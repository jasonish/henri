// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::{ClaudeAuth, ClaudeProviderConfig, ConfigFile, ProviderConfig, ProviderType};
use crate::error::{Error, Result};
use crate::output;
use crate::provider::model_utils;
use crate::provider::{
    ChatResponse, ContentBlock, Message, MessageContent, Provider, Role, StopReason, ToolCall,
};
use crate::services::Services;
use crate::sse;
use crate::tools;
use crate::usage;

pub(crate) const API_URL: &str = "https://api.anthropic.com/v1/messages?beta=true";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";
pub(crate) const ANTHROPIC_BETA: &str = "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14,interleaved-thinking-2025-05-14";

const CLAUDE_CODE_VERSION: &str = "2.1.2";

/// Map Henri tool names to Claude Code's exact tool names.
/// Claude Code uses PascalCase names for its tools.
fn to_claude_code_name(name: &str) -> String {
    match name {
        "file_read" => "Read".to_string(),
        "file_write" => "Write".to_string(),
        "file_edit" => "Edit".to_string(),
        "file_delete" => "FileDelete".to_string(),
        "bash" => "Bash".to_string(),
        "grep" => "Grep".to_string(),
        "glob" => "Glob".to_string(),
        "list_dir" => "LS".to_string(),
        "fetch" => "Fetch".to_string(),
        "todo_read" => "TodoRead".to_string(),
        "todo_write" => "TodoWrite".to_string(),
        other => other.to_string(),
    }
}

/// Map Claude Code tool names back to Henri's tool names.
fn from_claude_code_name(name: &str) -> String {
    match name {
        "Read" => "file_read".to_string(),
        "Write" => "file_write".to_string(),
        "Edit" => "file_edit".to_string(),
        "FileDelete" => "file_delete".to_string(),
        "Bash" => "bash".to_string(),
        "Grep" => "grep".to_string(),
        "Glob" => "glob".to_string(),
        "LS" => "list_dir".to_string(),
        "Fetch" => "fetch".to_string(),
        "TodoRead" => "todo_read".to_string(),
        "TodoWrite" => "todo_write".to_string(),
        other => other.to_string(),
    }
}

const ANTHROPIC_MODELS: &[&str] = &[
    "claude-opus-4-5#off",
    "claude-opus-4-5#low",
    "claude-opus-4-5#medium",
    "claude-opus-4-5#high",
    "claude-sonnet-4-5#off",
    "claude-sonnet-4-5#low",
    "claude-sonnet-4-5#medium",
    "claude-sonnet-4-5#high",
    "claude-haiku-4-5#off",
    "claude-haiku-4-5#low",
    "claude-haiku-4-5#medium",
    "claude-haiku-4-5#high",
];

const DEFAULT_MODEL: &str = "claude-haiku-4-5#medium";

fn thinking_level_from_model(model: &str) -> Option<&str> {
    model_utils::model_variant(model)
}

struct AuthState {
    local_id: String,
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

pub(crate) struct AnthropicClient {
    client: Client,
    state: Mutex<AuthState>,
}

impl AnthropicClient {
    pub(crate) fn try_new() -> Result<Self> {
        let config = ConfigFile::load()?;

        // Find the first enabled Claude provider
        let (local_id, claude) = config
            .providers_of_type(ProviderType::Claude)
            .into_iter()
            .find(|(_, p)| p.is_enabled())
            .and_then(|(id, p)| p.as_claude().map(|c| (id.clone(), c.clone())))
            .ok_or_else(|| Error::Auth("Anthropic not configured. Run /login.".to_string()))?;

        Ok(Self {
            client: Client::new(),
            state: Mutex::new(AuthState {
                local_id,
                access_token: claude.auth.access_token,
                refresh_token: claude.auth.refresh_token,
                expires_at: claude.auth.expires_at,
            }),
        })
    }

    pub(crate) fn http_client(&self) -> &Client {
        &self.client
    }

    pub async fn get_access_token(&self) -> Result<String> {
        self.refresh_access_token(false).await
    }

    pub async fn force_refresh(&self) -> Result<String> {
        self.refresh_access_token(true).await
    }

    async fn refresh_access_token(&self, force: bool) -> Result<String> {
        let mut state = self.state.lock().await;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Auth(e.to_string()))?
            .as_millis() as u64;

        if !force
            && (now < state.expires_at.saturating_sub(5 * 60 * 1000)
                || state.refresh_token.is_empty())
        {
            return Ok(state.access_token.clone());
        }

        // Try refresh, potentially retrying with tokens from config if our in-memory
        // tokens are stale (another client instance may have refreshed them).
        let mut retry_with_config = false;
        loop {
            let body = serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": state.refresh_token,
                "client_id": CLIENT_ID,
            });

            let response = self
                .client
                .post(TOKEN_URL)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Auth(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                if text.contains("invalid_grant") {
                    // Check if config has newer tokens (another client may have refreshed).
                    if !retry_with_config
                        && let Some(updated) = self.reload_tokens_from_config(&state.local_id)
                        && updated.refresh_token != state.refresh_token
                    {
                        state.access_token = updated.access_token;
                        state.refresh_token = updated.refresh_token;
                        state.expires_at = updated.expires_at;

                        // Check if the reloaded access token is still valid
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        if now < state.expires_at.saturating_sub(5 * 60 * 1000) {
                            return Ok(state.access_token.clone());
                        }

                        // Access token expired, retry refresh with new refresh token
                        retry_with_config = true;
                        continue;
                    }
                    return Err(Error::RefreshTokenExpired);
                }
                return Err(Error::Auth(format!(
                    "Anthropic token refresh failed: {} - {}",
                    status, text
                )));
            }

            let text = response
                .text()
                .await
                .map_err(|e| Error::Auth(e.to_string()))?;
            let json: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| Error::Auth(e.to_string()))?;

            let access_token = json["access_token"]
                .as_str()
                .ok_or_else(|| Error::Auth("Missing access_token".to_string()))?;
            let expires_in = json["expires_in"]
                .as_u64()
                .ok_or_else(|| Error::Auth("Missing expires_in".to_string()))?;
            let new_refresh = json["refresh_token"]
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| state.refresh_token.clone());

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| Error::Auth(e.to_string()))?
                .as_millis() as u64;

            state.access_token = access_token.to_string();
            state.refresh_token = new_refresh;
            state.expires_at = now + expires_in * 1000;

            if let Ok(mut config) = ConfigFile::load() {
                // Get the current enabled state from the existing provider
                let enabled = config
                    .get_provider(&state.local_id)
                    .map(|p| p.is_enabled())
                    .unwrap_or(true);
                config.set_provider(
                    state.local_id.clone(),
                    ProviderConfig::Claude(ClaudeProviderConfig {
                        enabled,
                        auth: ClaudeAuth {
                            refresh_token: state.refresh_token.clone(),
                            access_token: state.access_token.clone(),
                            expires_at: state.expires_at,
                        },
                    }),
                );
                let _ = config.save();
            }

            return Ok(state.access_token.clone());
        }
    }

    fn reload_tokens_from_config(&self, local_id: &str) -> Option<ClaudeAuth> {
        let config = ConfigFile::load().ok()?;
        let provider = config.get_provider(local_id)?;
        let claude = provider.as_claude()?;
        Some(claude.auth.clone())
    }
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    system: Vec<serde_json::Value>,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
}

#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    kind: String,
    budget_tokens: u32,
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    delta: Option<StreamDelta>,
    content_block: Option<StreamContentBlock>,
    message: Option<StreamMessage>,
    usage: Option<StreamUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamMessage {
    usage: Option<StreamUsage>,
}

#[derive(Debug, Deserialize)]
struct StreamUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    text: Option<String>,
    thinking: Option<String>,
    signature: Option<String>,
    partial_json: Option<String>,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamContentBlock {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    name: Option<String>,
}

/// Tracks a tool use block being accumulated from stream
#[derive(Debug, Default)]
struct PendingToolUse {
    id: String,
    name: String,
    input_json: String,
}

#[derive(Debug)]
struct PendingThinking {
    text: String,
    signature: String,
}

#[derive(Debug)]
struct PendingText {
    text: String,
}

#[derive(Debug)]
enum PendingBlock {
    Thinking(PendingThinking),
    ToolUse(PendingToolUse),
    Text(PendingText),
}

pub(crate) struct AnthropicProvider {
    client: AnthropicClient,
    model: String,
    thinking_mode: Option<String>,
    services: Services,
}

impl AnthropicProvider {
    pub(crate) fn try_new(services: Services) -> Result<Self> {
        let model = DEFAULT_MODEL.to_string();
        Ok(Self {
            client: AnthropicClient::try_new()?,
            thinking_mode: thinking_level_from_model(&model).map(|s| s.to_string()),
            model,
            services: services.clone(),
        })
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.model = model;
        self.thinking_mode = thinking_level_from_model(&self.model).map(|s| s.to_string());
    }

    /// Returns the available thinking modes for Claude models.
    pub(crate) fn thinking_modes() -> &'static [&'static str] {
        &[]
    }

    /// Returns the default thinking state for Claude models.
    pub(crate) fn default_thinking_state(model: &str) -> crate::providers::ThinkingState {
        if let Some(variant) = thinking_level_from_model(model) {
            crate::providers::ThinkingState::new(variant != "off", Some(variant.to_string()))
        } else {
            crate::providers::ThinkingState::new(true, Some("medium".to_string()))
        }
    }

    pub(crate) fn models() -> &'static [&'static str] {
        ANTHROPIC_MODELS
    }

    /// Get the available variants (thinking levels) for a given model.
    /// Returns the variant suffixes like "high", "medium", "low", "off".
    pub(crate) fn model_variants(model: &str) -> Vec<&'static str> {
        let base = model_utils::base_model_name(model);
        model_utils::get_model_variants(base, ANTHROPIC_MODELS)
            .iter()
            .filter_map(|m| model_utils::model_variant(m))
            .collect()
    }

    /// Cycle to the next variant for the given model.
    /// Returns the new full model string with the next variant.
    pub(crate) fn cycle_model_variant(model: &str) -> String {
        model_utils::cycle_model_variant(model, ANTHROPIC_MODELS, None)
    }

    async fn ensure_access_token(&self) -> Result<String> {
        self.client.get_access_token().await
    }

    async fn force_refresh_token(&self) -> Result<String> {
        let token = self.client.force_refresh().await?;
        eprintln!("[Session refreshed - cleared corrupted server state]");
        Ok(token)
    }

    fn build_messages(&self, messages: &[Message]) -> Vec<serde_json::Value> {
        let mut params: Vec<serde_json::Value> = Vec::new();
        let mut i = 0;

        while i < messages.len() {
            let msg = &messages[i];

            // Check if this is a user message with tool results, and subsequent messages are also tool results
            if msg.is_tool_result_only() {
                let mut tool_results: Vec<serde_json::Value> = Vec::new();

                // Process current message's tool results
                if let MessageContent::Blocks(blocks) = &msg.content {
                    for block in blocks {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                            data,
                            mime_type,
                        } = block
                        {
                            let api_content = if *is_error && content.is_empty() {
                                "(Tool execution failed with no output)"
                            } else {
                                content.as_str()
                            };

                            // Build content - either a string or an array with text + image
                            let content_value =
                                if let (Some(image_data), Some(mime)) = (data, mime_type) {
                                    serde_json::json!([
                                        {
                                            "type": "text",
                                            "text": api_content
                                        },
                                        {
                                            "type": "image",
                                            "source": {
                                                "type": "base64",
                                                "media_type": mime,
                                                "data": image_data
                                            }
                                        }
                                    ])
                                } else {
                                    serde_json::json!(api_content)
                                };

                            let mut obj = serde_json::json!({
                                "type": "tool_result",
                                "tool_use_id": tool_use_id,
                                "content": content_value
                            });
                            if *is_error {
                                obj["is_error"] = serde_json::json!(true);
                            }
                            tool_results.push(obj);
                        }
                    }
                }

                // Look ahead for consecutive tool result messages
                let mut j = i + 1;
                while j < messages.len() && messages[j].is_tool_result_only() {
                    if let MessageContent::Blocks(blocks) = &messages[j].content {
                        for block in blocks {
                            if let ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                data,
                                mime_type,
                            } = block
                            {
                                let api_content = if *is_error && content.is_empty() {
                                    "(Tool execution failed with no output)"
                                } else {
                                    content.as_str()
                                };

                                // Build content - either a string or an array with text + image
                                let content_value =
                                    if let (Some(image_data), Some(mime)) = (data, mime_type) {
                                        serde_json::json!([
                                            {
                                                "type": "text",
                                                "text": api_content
                                            },
                                            {
                                                "type": "image",
                                                "source": {
                                                    "type": "base64",
                                                    "media_type": mime,
                                                    "data": image_data
                                                }
                                            }
                                        ])
                                    } else {
                                        serde_json::json!(api_content)
                                    };

                                let mut obj = serde_json::json!({
                                    "type": "tool_result",
                                    "tool_use_id": tool_use_id,
                                    "content": content_value
                                });
                                if *is_error {
                                    obj["is_error"] = serde_json::json!(true);
                                }
                                tool_results.push(obj);
                            }
                        }
                    }
                    j += 1;
                }

                // Skip the messages we've already processed
                i = j;

                // Add a single user message with all tool results
                params.push(serde_json::json!({
                    "role": "user",
                    "content": tool_results
                }));
                continue;
            }

            // Normal message processing
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "user",
            };

            let content = match &msg.content {
                MessageContent::Text(text) => {
                    serde_json::json!([{"type": "text", "text": text}])
                }
                MessageContent::Blocks(blocks) => {
                    let content_blocks: Vec<serde_json::Value> = blocks
                        .iter()
                        .map(|b| match b {
                            ContentBlock::Text { text } => {
                                serde_json::json!({"type": "text", "text": text})
                            }
                            ContentBlock::Image { mime_type, data } => {
                                let base64_data = base64::engine::general_purpose::STANDARD.encode(data);
                                serde_json::json!({
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": mime_type,
                                        "data": base64_data
                                    }
                                })
                            }
                            ContentBlock::Thinking { thinking, provider_data } => {
                                let signature = provider_data
                                    .as_ref()
                                    .and_then(|d| d.get("signature"))
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("");

                                if signature.is_empty() {
                                    // If signature is empty (e.g. from aborted stream), convert to text
                                    // to avoid API rejection
                                    serde_json::json!({"type": "text", "text": thinking})
                                } else {
                                    serde_json::json!({"type": "thinking", "thinking": thinking, "signature": signature})
                                }
                            }
                            ContentBlock::ToolUse { id, name, input, .. } => {
                                serde_json::json!({
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                    "input": input
                                })
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                data,
                                mime_type,
                            } => {
                                // Anthropic API requires non-empty content when is_error is true
                                let api_content = if *is_error && content.is_empty() {
                                    "(Tool execution failed with no output)"
                                } else {
                                    content.as_str()
                                };

                                // Build content - either a string or an array with text + image
                                let content_value = if let (Some(image_data), Some(mime)) =
                                    (data, mime_type)
                                {
                                    // Include both text and image in content array
                                    serde_json::json!([
                                        {
                                            "type": "text",
                                            "text": api_content
                                        },
                                        {
                                            "type": "image",
                                            "source": {
                                                "type": "base64",
                                                "media_type": mime,
                                                "data": image_data
                                            }
                                        }
                                    ])
                                } else {
                                    serde_json::json!(api_content)
                                };

                                let mut obj = serde_json::json!({
                                    "type": "tool_result",
                                    "tool_use_id": tool_use_id,
                                    "content": content_value
                                });
                                if *is_error {
                                    obj["is_error"] = serde_json::json!(true);
                                }
                                obj
                            }
                            ContentBlock::Summary { summary, messages_compacted } => {
                                // Convert summary to text block for API compatibility
                                serde_json::json!({
                                    "type": "text",
                                    "text": format!("[Summary of {} previous messages]\n\n{}", messages_compacted, summary)
                                })
                            }
                        })
                        .collect();

                    serde_json::Value::Array(content_blocks)
                }
            };

            params.push(serde_json::json!({
                "role": role,
                "content": content
            }));

            i += 1;
        }

        params
    }

    /// Build the request struct for the Anthropic API
    async fn build_request(&self, messages: &[Message]) -> AnthropicRequest {
        let tools: Vec<AnthropicTool> = tools::all_definitions(&self.services)
            .await
            .into_iter()
            .map(|t| AnthropicTool {
                name: to_claude_code_name(&t.name),
                description: t.description,
                input_schema: t.input_schema,
            })
            .collect();

        let (model, _) = model_utils::split_model(&self.model);
        let thinking = match self.thinking_mode.as_deref() {
            Some("low") => Some(ThinkingConfig {
                kind: "enabled".to_string(),
                budget_tokens: 4000,
            }),
            Some("medium") => Some(ThinkingConfig {
                kind: "enabled".to_string(),
                budget_tokens: 16000,
            }),
            Some("high") => Some(ThinkingConfig {
                kind: "enabled".to_string(),
                budget_tokens: 32000,
            }),
            Some("xhigh") => Some(ThinkingConfig {
                kind: "enabled".to_string(),
                budget_tokens: 48000,
            }),
            _ => None,
        };

        // OAuth mode: MUST start with Claude Code identity
        let mut system = vec![serde_json::json!({
            "type": "text",
            "text": "You are Claude Code, Anthropic's official CLI for Claude.",
            "cache_control": {"type": "ephemeral"}
        })];

        // Add Henri-specific system prompts
        for part in crate::prompts::system_prompt_with_services(Some(&self.services)) {
            system.push(serde_json::json!({"type": "text", "text": part}));
        }

        // Add cache control to the second-to-last system block to enable prompt caching.
        // The last block is the timestamp which changes on every request, so we cache
        // everything before it.
        let len = system.len();
        if len >= 2 {
            system[len - 2]["cache_control"] = serde_json::json!({"type": "ephemeral"});
        }

        let mut built_messages = self.build_messages(messages);

        // Add cache control to the last user message to enable conversation history caching.
        if let Some(last_msg) = built_messages.last_mut()
            && last_msg["role"] == "user"
            && let Some(content) = last_msg.get_mut("content")
            && let Some(blocks) = content.as_array_mut()
            && let Some(last_block) = blocks.last_mut()
        {
            last_block["cache_control"] = serde_json::json!({"type": "ephemeral"});
        }

        AnthropicRequest {
            model: model.to_string(),
            messages: built_messages,
            system,
            max_tokens: 16000,
            stream: true,
            tools,
            thinking,
        }
    }

    /// Send a chat request and stream the response, accumulating tool calls
    async fn send_chat_request(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        let access_token = self.ensure_access_token().await?;
        let request = self.build_request(&messages).await;

        // Record TX bytes
        let body_bytes = serde_json::to_vec(&request)?;
        usage::network_stats().record_tx(body_bytes.len() as u64);

        // Build headers for transaction logging
        let mut req_headers = std::collections::HashMap::new();
        req_headers.insert("content-type".to_string(), "application/json".to_string());
        req_headers.insert("accept".to_string(), "application/json".to_string());
        req_headers.insert(
            "anthropic-version".to_string(),
            ANTHROPIC_VERSION.to_string(),
        );
        req_headers.insert("anthropic-beta".to_string(), ANTHROPIC_BETA.to_string());

        // OAuth mode: Add headers to mimic Claude Code exactly
        req_headers.insert(
            "user-agent".to_string(),
            format!("claude-cli/{} (external, cli)", CLAUDE_CODE_VERSION),
        );
        req_headers.insert("x-app".to_string(), "cli".to_string());
        req_headers.insert(
            "anthropic-dangerous-direct-browser-access".to_string(),
            "true".to_string(),
        );

        req_headers.insert(
            "authorization".to_string(),
            format!("Bearer {}", access_token),
        );

        // Build HTTP request with the same headers
        let response = self
            .client
            .http_client()
            .post(API_URL)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA)
            .header(
                "user-agent",
                format!("claude-cli/{} (external, cli)", CLAUDE_CODE_VERSION),
            )
            .header("x-app", "cli")
            .header("anthropic-dangerous-direct-browser-access", "true")
            .header("authorization", format!("Bearer {}", access_token))
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| {
                Error::Other(format!(
                    "Failed to connect to Anthropic API ({}): {}",
                    API_URL, e
                ))
            })?;

        let resp_headers =
            crate::provider::transaction_log::header_map_to_hash_map(response.headers());

        if !response.status().is_success() {
            let status = response.status();
            let status_code = status.as_u16();
            let text = response.text().await.unwrap_or_default();

            crate::provider::transaction_log::log(
                API_URL,
                req_headers.clone(),
                serde_json::to_value(&request).unwrap_or_default(),
                resp_headers,
                serde_json::json!({
                    "error": true,
                    "status": status_code,
                    "body": text
                }),
            );

            // Check for corrupted session state error (tool_use without tool_result)
            // This happens when a previous session crashed mid-tool-loop
            if text.contains("tool_use") && text.contains("tool_result") {
                return Err(Error::SessionCorrupted(format!(
                    "Anthropic chat failed: {} - {}",
                    status, text
                )));
            }

            if status_code == 401 {
                return Err(Error::Unauthorized(format!(
                    "Anthropic chat failed: {} - {}",
                    status, text
                )));
            }

            // Check for retryable errors (timeouts, overloaded, rate limits)
            if super::is_retryable_status(status_code) || super::is_retryable_message(&text) {
                return Err(Error::Retryable {
                    status: status_code,
                    message: text,
                });
            }

            return Err(Error::Auth(format!(
                "Anthropic chat failed: {} - {}",
                status, text
            )));
        }

        // Process the streaming response
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut stop_reason = StopReason::Unknown;
        let mut raw_events: Vec<serde_json::Value> = Vec::new();

        let mut pending_block: Option<PendingBlock> = None;
        let mut thinking = output::ThinkingState::new(output);
        let mut streaming_start: Option<Instant> = None;

        let mut sse = sse::SseStream::new(response.bytes_stream().map(|chunk| {
            if let Ok(ref bytes) = chunk {
                usage::network_stats().record_rx(bytes.len() as u64);
            }
            chunk
        }));
        while let Some(result) = sse.next_event().await {
            let data = result.map_err(Error::Http)?;

            let Ok(event) = serde_json::from_str::<StreamEvent>(&data) else {
                continue;
            };
            raw_events
                .push(serde_json::from_str(&data).unwrap_or(serde_json::json!({ "raw": data })));

            match event.event_type.as_str() {
                "message_start" => {
                    if let Some(msg) = &event.message
                        && let Some(u) = &msg.usage
                    {
                        let mut total_context = 0u64;
                        let mut input_tokens = 0u64;
                        let mut cache_read_tokens = 0u64;
                        if let Some(input) = u.input_tokens {
                            input_tokens = input;
                        }
                        if let Some(cache_create) = u.cache_creation_input_tokens {
                            usage::anthropic().add_cache_creation(cache_create);
                        }
                        if let Some(cache_read) = u.cache_read_input_tokens {
                            usage::anthropic().add_cache_read(cache_read);
                            cache_read_tokens = cache_read;
                        }
                        let input_total = input_tokens + cache_read_tokens;
                        if input_total > 0 {
                            usage::anthropic().record_input(input_total);
                            total_context += input_total;
                            output::emit_usage_update(output, input_total, 0, cache_read_tokens);
                        }
                        if total_context > 0 {
                            let limit = Self::context_limit(&self.model);
                            output::emit_context_update(output, total_context, limit);
                        }
                    }
                }
                "content_block_start" => {
                    if let Some(block) = &event.content_block {
                        match block.kind.as_str() {
                            "thinking" => {
                                pending_block = Some(PendingBlock::Thinking(PendingThinking {
                                    text: String::new(),
                                    signature: String::new(),
                                }));
                            }
                            "tool_use" => {
                                pending_block = Some(PendingBlock::ToolUse(PendingToolUse {
                                    id: block.id.clone().unwrap_or_default(),
                                    name: block.name.clone().unwrap_or_default(),
                                    input_json: String::new(),
                                }));
                            }
                            "text" => {
                                pending_block = Some(PendingBlock::Text(PendingText {
                                    text: String::new(),
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                "content_block_delta" => {
                    if let Some(delta) = &event.delta {
                        if let Some(PendingBlock::Thinking(ref mut pending)) = pending_block {
                            if let Some(thinking_text) = &delta.thinking {
                                if streaming_start.is_none() {
                                    streaming_start = Some(Instant::now());
                                }
                                pending.text.push_str(thinking_text);
                                thinking.emit(thinking_text);
                            }
                            if let Some(sig) = &delta.signature {
                                pending.signature = sig.clone();
                            }
                        }
                        if let Some(PendingBlock::Text(ref mut pending)) = pending_block
                            && let Some(text) = &delta.text
                        {
                            pending.text.push_str(text);
                        }
                        if let Some(text) = &delta.text {
                            if streaming_start.is_none() {
                                streaming_start = Some(Instant::now());
                            }
                            output::print_text(output, text);
                        }
                        if let Some(partial_json) = &delta.partial_json
                            && let Some(PendingBlock::ToolUse(ref mut tool)) = pending_block
                        {
                            tool.input_json.push_str(partial_json);
                        }
                    }
                }
                "content_block_stop" => {
                    if let Some(block) = pending_block.take() {
                        match block {
                            PendingBlock::Thinking(pending) => {
                                thinking.end();
                                let provider_data = if pending.signature.is_empty() {
                                    None
                                } else {
                                    Some(serde_json::json!({"signature": pending.signature}))
                                };
                                content_blocks.push(ContentBlock::Thinking {
                                    thinking: pending.text,
                                    provider_data,
                                });
                            }
                            PendingBlock::ToolUse(tool) => {
                                let input: serde_json::Value =
                                    serde_json::from_str(&tool.input_json)
                                        .unwrap_or(serde_json::json!({}));
                                let stripped_name = from_claude_code_name(&tool.name);

                                tool_calls.push(ToolCall {
                                    id: tool.id.clone(),
                                    name: stripped_name.clone(),
                                    input: input.clone(),
                                    thought_signature: None,
                                });

                                content_blocks.push(ContentBlock::ToolUse {
                                    id: tool.id,
                                    name: stripped_name,
                                    input,
                                    thought_signature: None,
                                });
                            }
                            PendingBlock::Text(pending) => {
                                if !pending.text.is_empty() {
                                    content_blocks.push(ContentBlock::Text { text: pending.text });
                                }
                            }
                        }
                    }
                }
                "message_delta" => {
                    if let Some(delta) = &event.delta
                        && let Some(reason) = &delta.stop_reason
                    {
                        stop_reason = match reason.as_str() {
                            "end_turn" => StopReason::EndTurn,
                            "tool_use" => StopReason::ToolUse,
                            "max_tokens" => StopReason::MaxTokens,
                            _ => StopReason::Unknown,
                        };
                    }
                    if let Some(u) = &event.usage
                        && let Some(output_tokens) = u.output_tokens
                    {
                        usage::anthropic().record_output(output_tokens);
                        output::emit_usage_update(output, 0, output_tokens, 0);

                        // Emit final progress with turn total (accumulated across all API calls)
                        if let Some(start) = streaming_start {
                            let duration = start.elapsed().as_secs_f64();
                            if duration > 0.0 {
                                let turn_total = usage::anthropic().turn_total();
                                let tokens_per_sec = output_tokens as f64 / duration;
                                output::emit_working_progress(
                                    output,
                                    turn_total,
                                    duration,
                                    tokens_per_sec,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if !content_blocks.is_empty() {
            // Only end the text block if we actually streamed any text.
            if content_blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { .. }))
            {
                output::print_text_end(output);
            }
        }

        if crate::provider::transaction_log::is_active() {
            crate::provider::transaction_log::log(
                API_URL,
                req_headers,
                serde_json::to_value(&request).unwrap_or(serde_json::json!({})),
                resp_headers,
                serde_json::Value::Array(raw_events),
            );
        }

        Ok(ChatResponse {
            tool_calls,
            content_blocks,
            stop_reason,
        })
    }
}

impl AnthropicProvider {
    /// Count tokens for the current message state without making a chat request.
    /// Returns raw JSON response from the API.
    pub async fn count_tokens(&self, messages: &[Message]) -> Result<serde_json::Value> {
        let messages = if messages.is_empty() {
            // At least one message is required.
            &[Message::user(".")]
        } else {
            messages
        };

        let access_token = self.ensure_access_token().await?;
        let request = self.build_request(messages).await;

        // Filter out thinking blocks (not supported by token counting API)
        let filtered_messages: Vec<serde_json::Value> = request
            .messages
            .into_iter()
            .map(|mut msg| {
                if let Some(content) = msg.get_mut("content")
                    && let Some(blocks) = content.as_array_mut()
                {
                    blocks.retain(|block| {
                        block.get("type").and_then(|t| t.as_str()) != Some("thinking")
                    });
                }
                msg
            })
            .collect();

        // Build a simplified request for token counting (no stream, no thinking)
        let count_request = serde_json::json!({
            "model": request.model,
            "messages": filtered_messages,
            "system": request.system,
            "tools": request.tools,
        });

        let url = "https://api.anthropic.com/v1/messages/count_tokens";

        let response = self
            .client
            .http_client()
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA)
            .header(
                "user-agent",
                format!("claude-cli/{} (external, cli)", CLAUDE_CODE_VERSION),
            )
            .header("x-app", "cli")
            .header("anthropic-dangerous-direct-browser-access", "true")
            .header("authorization", format!("Bearer {}", access_token))
            .json(&count_request)
            .send()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Auth(format!(
                "Token count failed: {} - {}",
                status, text
            )));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;

        Ok(json)
    }

    /// Get the context limit for a given model name
    pub(crate) fn context_limit(model: &str) -> Option<u64> {
        if model.contains("opus") || model.contains("sonnet") || model.contains("haiku") {
            Some(200_000)
        } else {
            None
        }
    }
}

impl Provider for AnthropicProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        match self.send_chat_request(messages.clone(), output).await {
            Ok(response) => Ok(response),
            Err(Error::SessionCorrupted(_)) | Err(Error::Unauthorized(_)) => {
                self.force_refresh_token().await?;
                self.send_chat_request(messages, output).await
            }
            Err(e) => Err(e),
        }
    }

    async fn prepare_request(&self, messages: Vec<Message>) -> Result<serde_json::Value> {
        let request = self.build_request(&messages).await;
        Ok(serde_json::to_value(&request)?)
    }

    fn start_turn(&self) {
        crate::usage::anthropic().start_turn();
    }
}
