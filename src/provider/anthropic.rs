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
use crate::prompts::system_prompt;
use crate::provider::{
    ChatResponse, ContentBlock, Message, MessageContent, Provider, Role, StopReason, ToolCall,
};
use crate::services::Services;
use crate::sse;
use crate::tools;
use crate::usage;

pub(crate) const API_URL: &str = "https://api.anthropic.com/v1/messages";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";
pub(crate) const ANTHROPIC_BETA: &str = "oauth-2025-04-20,claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14";

const ANTHROPIC_MODELS: &[&str] = &["claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5"];

const DEFAULT_MODEL: &str = ANTHROPIC_MODELS[2];

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
struct CacheControl {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
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
        Ok(Self {
            client: AnthropicClient::try_new()?,
            model: DEFAULT_MODEL.to_string(),
            thinking_mode: Some("medium".to_string()),
            services: services.clone(),
        })
    }

    pub(crate) fn set_thinking_mode(&mut self, mode: Option<String>) {
        self.thinking_mode = mode;
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.model = model;
    }

    /// Returns the available thinking modes for Claude models.
    pub(crate) fn thinking_modes() -> &'static [&'static str] {
        &["off", "low", "medium", "high"]
    }

    /// Returns the default thinking state for Claude models.
    pub(crate) fn default_thinking_state() -> crate::providers::ThinkingState {
        crate::providers::ThinkingState::new(true, Some("medium".to_string()))
    }

    pub(crate) fn models() -> &'static [&'static str] {
        ANTHROPIC_MODELS
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
        messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "user",
                };

                let content = match &m.content {
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
                                    serde_json::json!({"type": "thinking", "thinking": thinking, "signature": signature})
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
                                } => {
                                    // Anthropic API requires non-empty content when is_error is true
                                    let api_content = if *is_error && content.is_empty() {
                                        "(Tool execution failed with no output)"
                                    } else {
                                        content.as_str()
                                    };

                                    let mut obj = serde_json::json!({
                                        "type": "tool_result",
                                        "tool_use_id": tool_use_id,
                                        "content": api_content
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

                serde_json::json!({
                    "role": role,
                    "content": content
                })
            })
            .collect()
    }

    /// Build the request struct for the Anthropic API
    async fn build_request(&self, messages: &[Message]) -> AnthropicRequest {
        let mut tools: Vec<AnthropicTool> = tools::all_definitions(&self.services)
            .await
            .into_iter()
            .map(|t| AnthropicTool {
                name: t.name,
                description: t.description,
                input_schema: t.input_schema,
                cache_control: None,
            })
            .collect();

        // Add cache control to the last tool to enable prompt caching
        if let Some(last) = tools.last_mut() {
            last.cache_control = Some(CacheControl {
                kind: "ephemeral".to_string(),
            });
        }

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
            _ => None,
        };

        // Claude-specific identity
        let mut system = vec![
            serde_json::json!({"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}),
            serde_json::json!({"type": "text", "text": "Your name is Henri"}),
        ];

        for part in system_prompt() {
            system.push(serde_json::json!({"type": "text", "text": part}));
        }

        // Add cache control to the last system block to enable prompt caching
        if let Some(last) = system.last_mut() {
            last["cache_control"] = serde_json::json!({"type": "ephemeral"});
        }

        let built_messages = self.build_messages(messages);

        AnthropicRequest {
            model: self.model.clone(),
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

        let mut req_headers = std::collections::HashMap::new();
        req_headers.insert("Content-Type".to_string(), "application/json".to_string());
        req_headers.insert(
            "anthropic-version".to_string(),
            ANTHROPIC_VERSION.to_string(),
        );
        req_headers.insert("anthropic-beta".to_string(), ANTHROPIC_BETA.to_string());
        req_headers.insert("Accept".to_string(), "text/event-stream".to_string());
        req_headers.insert(
            "Authorization".to_string(),
            format!("Bearer {}", access_token),
        );

        let response = self
            .client
            .http_client()
            .post(API_URL)
            .header("Content-Type", "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA)
            .header("Accept", "text/event-stream")
            .header("Authorization", format!("Bearer {}", access_token))
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
        let mut char_count = 0usize;
        let mut last_progress_update = Instant::now();

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
                        if let Some(input) = u.input_tokens {
                            usage::anthropic().record_input(input);
                            let limit = Self::context_limit(&self.model);
                            output::emit_context_update(output, input, limit);
                        }
                        if let Some(cache_create) = u.cache_creation_input_tokens {
                            usage::anthropic().add_cache_creation(cache_create);
                        }
                        if let Some(cache_read) = u.cache_read_input_tokens {
                            usage::anthropic().add_cache_read(cache_read);
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
                                    last_progress_update = Instant::now();
                                }
                                char_count += thinking_text.len();
                                pending.text.push_str(thinking_text);
                                thinking.emit(thinking_text);

                                // Emit progress update every 0.5 seconds
                                if last_progress_update.elapsed().as_secs_f64() >= 0.5 {
                                    if let Some(start) = streaming_start {
                                        let duration = start.elapsed().as_secs_f64();
                                        // Rough estimate: 4 characters per token
                                        let estimated_tokens = (char_count / 4) as u64;
                                        if duration > 0.0 && estimated_tokens > 0 {
                                            let tokens_per_sec = estimated_tokens as f64 / duration;
                                            output::emit_working_progress(
                                                output,
                                                estimated_tokens,
                                                duration,
                                                tokens_per_sec,
                                            );
                                        }
                                    }
                                    last_progress_update = Instant::now();
                                }
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
                                last_progress_update = Instant::now();
                            }
                            char_count += text.len();
                            output::print_text(output, text);

                            // Emit progress update every 0.5 seconds
                            if last_progress_update.elapsed().as_secs_f64() >= 0.5 {
                                if let Some(start) = streaming_start {
                                    let duration = start.elapsed().as_secs_f64();
                                    // Rough estimate: 4 characters per token
                                    let estimated_tokens = (char_count / 4) as u64;
                                    if duration > 0.0 && estimated_tokens > 0 {
                                        let tokens_per_sec = estimated_tokens as f64 / duration;
                                        output::emit_working_progress(
                                            output,
                                            estimated_tokens,
                                            duration,
                                            tokens_per_sec,
                                        );
                                    }
                                }
                                last_progress_update = Instant::now();
                            }
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

                                tool_calls.push(ToolCall {
                                    id: tool.id.clone(),
                                    name: tool.name.clone(),
                                    input: input.clone(),
                                    thought_signature: None,
                                });

                                content_blocks.push(ContentBlock::ToolUse {
                                    id: tool.id,
                                    name: tool.name,
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

        output::print_text_end(output);

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
            .header("Content-Type", "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA)
            .header("Authorization", format!("Bearer {}", access_token))
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
