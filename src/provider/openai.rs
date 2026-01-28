// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures::StreamExt;
use reqwest::Client;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::config::{ConfigFile, OpenAiProviderConfig, ProviderConfig, ProviderType};
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

const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_CODEX_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const OPENAI_DEFAULT_MODEL: &str = "gpt-5.2-codex#medium";
const OPENAI_MODELS: &[&str] = &[
    "gpt-5.2-codex#low",
    "gpt-5.2-codex#medium",
    "gpt-5.2-codex#high",
    "gpt-5.2-codex#xhigh",
    "gpt-5.2#low",
    "gpt-5.2#medium",
    "gpt-5.2#high",
    "gpt-5.2#xhigh",
    "gpt-5.1-codex-max#low",
    "gpt-5.1-codex-max#medium",
    "gpt-5.1-codex-max#high",
    "gpt-5.1-codex-max#xhigh",
    "gpt-5.1-codex-mini#medium",
    "gpt-5.1-codex-mini#high",
];
const CODEX_PROMPT_GPT_5_1: &str = include_str!("openai/gpt_5_1_prompt.md");
const CODEX_PROMPT_GPT_5_CODEX: &str = include_str!("openai/gpt_5_codex_prompt.md");
const CODEX_PROMPT_GPT_5_1_CODEX_MAX: &str = include_str!("openai/gpt-5.1-codex-max_prompt.md");
const CODEX_PROMPT_GPT_5_2_CODEX: &str = include_str!("openai/gpt-5.2-codex_prompt.md");
const CODEX_PROMPT_GPT_5_2: &str = include_str!("openai/gpt_5_2_prompt.md");

/// Maximum number of retries for transient errors.
const MAX_RETRIES: u32 = 3;

/// Initial retry delay (exponential backoff: 1s, 2s, 4s)
const INITIAL_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

fn reasoning_effort_from_model(model: &str) -> &str {
    model_utils::model_variant(model).unwrap_or_default()
}

#[derive(Debug)]
struct OpenAiState {
    local_id: String,
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

pub(crate) struct OpenAiProvider {
    client: Client,
    state: Mutex<OpenAiState>,
    model: String,
    project_id: Option<String>,
    client_id: String,
    audience: String,
    thinking_enabled: bool,
    usage_tracker: &'static usage::Usage,
    services: Services,
}

#[derive(Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    kind: &'static str,
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Serialize, Default, Clone)]
struct CodexInputItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct CodexRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    store: bool,
    stream: bool,
    input: Vec<CodexInputItem>,
    instructions: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<Vec<String>>,
}

#[derive(Debug, Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl OpenAiProvider {
    pub(crate) fn try_new(services: Services) -> Result<Self> {
        let config = ConfigFile::load()?;

        // Find the first enabled OpenAI provider
        let (local_id, openai) = config
            .providers_of_type(ProviderType::Openai)
            .into_iter()
            .find(|(_, p)| p.is_enabled())
            .and_then(|(id, p)| p.as_openai().map(|c| (id.clone(), c.clone())))
            .ok_or_else(|| Error::Auth("OpenAI not configured. Run /login.".to_string()))?;

        Ok(Self {
            client: Client::new(),
            state: Mutex::new(OpenAiState {
                local_id,
                access_token: openai.access_token,
                refresh_token: openai.refresh_token,
                expires_at: openai.expires_at,
            }),
            model: OPENAI_DEFAULT_MODEL.to_string(),
            project_id: openai.project_id,
            client_id: openai.client_id,
            audience: openai.audience,
            thinking_enabled: true,
            usage_tracker: usage::openai(),
            services,
        })
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.model = model;
    }

    pub(crate) fn set_thinking_enabled(&mut self, enabled: bool) {
        self.thinking_enabled = enabled;
    }

    pub(crate) fn models() -> &'static [&'static str] {
        OPENAI_MODELS
    }

    /// Get the available variants (reasoning effort levels) for a given model.
    /// Returns the variant suffixes like "xhigh", "high", "medium", "low".
    pub(crate) fn model_variants(model: &str) -> Vec<&'static str> {
        let base = model_utils::base_model_name(model);
        model_utils::get_model_variants(base, OPENAI_MODELS)
            .iter()
            .filter_map(|m| model_utils::model_variant(m))
            .collect()
    }

    /// Cycle to the next variant for the given model.
    /// Returns the new full model string with the next variant.
    pub(crate) fn cycle_model_variant(model: &str) -> String {
        model_utils::cycle_model_variant(model, OPENAI_MODELS, None)
    }

    fn build_codex_input(&self, messages: &[Message]) -> Vec<CodexInputItem> {
        let mut input_items = Vec::new();

        for message in messages {
            // Determine role and text type based on message role
            // User messages use "input_text", assistant messages use "output_text"
            let (role, text_type) = match message.role {
                Role::User => ("user", "input_text"),
                Role::Assistant => ("assistant", "output_text"),
                Role::System => {
                    // Codex Responses API rejects system messages in input; instructions already cover them.
                    continue;
                }
            };

            match &message.content {
                MessageContent::Text(text) => {
                    input_items.push(CodexInputItem {
                        kind: "message".to_string(),
                        role: Some(role.to_string()),
                        content: Some(vec![serde_json::json!({
                            "type": text_type,
                            "text": text
                        })]),
                        ..Default::default()
                    });
                }
                MessageContent::Blocks(blocks) => {
                    let mut content_parts = Vec::new();
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                if !text.trim().is_empty() {
                                    content_parts.push(serde_json::json!({
                                        "type": text_type,
                                        "text": text
                                    }));
                                }
                            }
                            ContentBlock::Image { mime_type, data } => {
                                let encoded = STANDARD.encode(data);
                                content_parts.push(serde_json::json!({
                                    "type": "input_image",
                                    "image_url": format!(
                                        "data:{};base64,{}",
                                        mime_type, encoded
                                    )
                                }));
                            }
                            ContentBlock::Thinking {
                                thinking,
                                provider_data,
                            } => {
                                // If we have encrypted reasoning content from a previous turn,
                                // include it in the input for round-tripping
                                if let Some(data) = provider_data
                                    && let Some(encrypted) = data.get("encrypted_content")
                                    && let Some(encrypted_str) = encrypted.as_str()
                                {
                                    // Add the reasoning summary as output_text
                                    if !thinking.trim().is_empty() {
                                        content_parts.push(serde_json::json!({
                                            "type": text_type,
                                            "text": thinking
                                        }));
                                    }

                                    // Flush any pending content
                                    if !content_parts.is_empty() {
                                        input_items.push(CodexInputItem {
                                            kind: "message".to_string(),
                                            role: Some(role.to_string()),
                                            content: Some(content_parts.clone()),
                                            ..Default::default()
                                        });
                                        content_parts.clear();
                                    }

                                    // Add reasoning item with encrypted content
                                    input_items.push(CodexInputItem {
                                        kind: "reasoning".to_string(),
                                        output: Some(serde_json::json!({
                                            "encrypted_content": encrypted_str
                                        })),
                                        ..Default::default()
                                    });
                                }
                                // Otherwise, thinking without encrypted content can be skipped
                                // (it's just display info, not needed for the API)
                            }
                            ContentBlock::ToolUse {
                                id, name, input, ..
                            } => {
                                if !content_parts.is_empty() {
                                    input_items.push(CodexInputItem {
                                        kind: "message".to_string(),
                                        role: Some(role.to_string()),
                                        content: Some(content_parts.clone()),
                                        ..Default::default()
                                    });
                                    content_parts.clear();
                                }
                                let arguments = serde_json::to_string(input)
                                    .unwrap_or_else(|_| "{}".to_string());
                                input_items.push(CodexInputItem {
                                    kind: "function_call".to_string(),
                                    name: Some(name.clone()),
                                    call_id: Some(id.clone()),
                                    arguments: Some(arguments),
                                    ..Default::default()
                                });
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                data,
                                mime_type,
                                ..
                            } => {
                                if !content_parts.is_empty() {
                                    input_items.push(CodexInputItem {
                                        kind: "message".to_string(),
                                        content: Some(content_parts.clone()),
                                        ..Default::default()
                                    });
                                    content_parts.clear();
                                }

                                // Build output with text content and optional image.
                                let mut output_parts: Vec<serde_json::Value> =
                                    vec![serde_json::json!({
                                        "type": "input_text",
                                        "text": content
                                    })];

                                // If there's image data, include it as an input_image part.
                                if let (Some(image_data), Some(mime)) = (data, mime_type) {
                                    output_parts.push(serde_json::json!({
                                        "type": "input_image",
                                        "detail": "auto",
                                        "image_url": format!("data:{};base64,{}", mime, image_data)
                                    }));
                                }

                                input_items.push(CodexInputItem {
                                    kind: "function_call_output".to_string(),
                                    call_id: Some(tool_use_id.clone()),
                                    output: Some(serde_json::json!(output_parts)),
                                    ..Default::default()
                                });
                            }
                            ContentBlock::Summary {
                                summary,
                                messages_compacted,
                            } => {
                                // Convert summary to text block
                                let text = format!(
                                    "[Summary of {} previous messages]\n\n{}",
                                    messages_compacted, summary
                                );
                                if !text.trim().is_empty() {
                                    content_parts.push(serde_json::json!({
                                        "type": text_type,
                                        "text": text
                                    }));
                                }
                            }
                        }
                    }

                    if !content_parts.is_empty() {
                        input_items.push(CodexInputItem {
                            kind: "message".to_string(),
                            role: Some(role.to_string()),
                            content: Some(content_parts),
                            ..Default::default()
                        });
                    }
                }
            }
        }

        input_items
    }

    async fn codex_instructions(&self) -> String {
        let base_model = model_utils::base_model_name(&self.model);
        let instructions = match base_model.to_lowercase().as_ref() {
            "gpt-5.2" => CODEX_PROMPT_GPT_5_2,
            "gpt-5.2-codex" => CODEX_PROMPT_GPT_5_2_CODEX,
            "gpt-5.1-codex" => CODEX_PROMPT_GPT_5_CODEX,
            "gpt-5.1" => CODEX_PROMPT_GPT_5_1,
            "gpt-5.1-codex-mini" => CODEX_PROMPT_GPT_5_CODEX,
            "gpt-5.1-codex-max" => CODEX_PROMPT_GPT_5_1_CODEX_MAX,
            _ => "",
        };
        instructions.trim().to_string()
    }

    async fn ensure_access_token(&self) -> Result<String> {
        let mut state = self.state.lock().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Auth(e.to_string()))?
            .as_millis() as u64;

        if now < state.expires_at.saturating_sub(5 * 60 * 1000) || state.refresh_token.is_empty() {
            return Ok(state.access_token.clone());
        }

        let response = self
            .client
            .post(OPENAI_TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", state.refresh_token.as_str()),
                ("client_id", self.client_id.as_str()),
            ])
            .send()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if text.contains("invalid_grant") {
                return Err(Error::RefreshTokenExpired);
            }
            return Err(Error::Auth(format!(
                "OpenAI token refresh failed: {} - {}",
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
        // OAuth servers may rotate refresh tokens - use new one if provided
        let new_refresh = json["refresh_token"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| state.refresh_token.clone());

        state.access_token = access_token.to_string();
        state.refresh_token = new_refresh;
        state.expires_at = now + expires_in * 1000;

        if let Ok(mut config) = ConfigFile::load() {
            let enabled = config
                .get_provider(&state.local_id)
                .map(|p| p.is_enabled())
                .unwrap_or(true);
            config.set_provider(
                state.local_id.clone(),
                ProviderConfig::Openai(OpenAiProviderConfig {
                    enabled,
                    client_id: self.client_id.clone(),
                    audience: self.audience.clone(),
                    refresh_token: state.refresh_token.clone(),
                    access_token: state.access_token.clone(),
                    expires_at: state.expires_at,
                    project_id: self.project_id.clone(),
                }),
            );
            let _ = config.save();
        }

        Ok(state.access_token.clone())
    }

    async fn build_request(&self, messages: &[Message]) -> CodexRequest {
        let instructions = self.codex_instructions().await;

        // OpenAI Codex Responses API: put application system prompt in `instructions`.
        // This avoids smuggling the system prompt in as a fake user message.
        let mut merged_instructions = Vec::new();
        let model_instructions = instructions.trim();
        if !model_instructions.is_empty() {
            merged_instructions.push(model_instructions.to_string());
        }

        let mut app_instructions =
            crate::prompts::system_prompt_with_services(Some(&self.services));
        app_instructions.push(
            "You do not have an `apply_patch` tool, instead use the `file_edit` tool.".to_string(),
        );
        merged_instructions.extend(app_instructions);

        let instructions = merged_instructions.join("\n\n");

        let input = self.build_codex_input(messages);
        let prompt_cache_key = self.services.session_id();

        let tools: Vec<OpenAiTool> = tools::all_definitions(&self.services)
            .await
            .into_iter()
            .map(|t| OpenAiTool {
                kind: "function",
                name: t.name,
                description: t.description,
                parameters: t.input_schema,
            })
            .collect();

        let model = model_utils::base_model_name(&self.model);
        let reasoning_effort = reasoning_effort_from_model(&self.model);

        let reasoning = if reasoning_effort.is_empty() {
            None
        } else {
            Some(serde_json::json!({"effort": reasoning_effort, "summary": "auto"}))
        };

        CodexRequest {
            model: model.to_string(),
            prompt_cache_key,
            store: false,
            stream: true,
            input,
            instructions,
            tools,
            reasoning,
            text: Some(serde_json::json!({ "verbosity": "medium" })),
            include: Some(vec!["reasoning.encrypted_content".to_string()]),
        }
    }

    async fn execute_chat_with_request(
        &self,
        request: CodexRequest,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        // Record TX bytes
        let body_bytes = serde_json::to_vec(&request)?;
        let body_len = body_bytes.len();
        crate::usage::network_stats().record_tx(body_len as u64);

        let access_token = self.ensure_access_token().await?;
        let account_id = chatgpt_account_id(&access_token)
            .ok_or_else(|| Error::Auth("Failed to extract ChatGPT account id".to_string()))?;

        let mut req_headers = std::collections::HashMap::new();
        req_headers.insert(
            "Authorization".to_string(),
            format!("Bearer {}", access_token),
        );
        req_headers.insert("Content-Type".to_string(), "application/json".to_string());
        req_headers.insert(
            "OpenAI-Beta".to_string(),
            "responses=experimental".to_string(),
        );
        req_headers.insert("originator".to_string(), "codex_cli_rs".to_string());
        req_headers.insert("chatgpt-account-id".to_string(), account_id.clone());
        req_headers.insert("accept".to_string(), "text/event-stream".to_string());

        let response = self
            .client
            .post(OPENAI_CODEX_URL)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("Content-Type", "application/json")
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("chatgpt-account-id", account_id)
            .header("accept", "text/event-stream")
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| {
                Error::Other(format!(
                    "Failed to connect to OpenAI API ({}): {}",
                    OPENAI_CODEX_URL, e
                ))
            })?;

        let resp_headers =
            crate::provider::transaction_log::header_map_to_hash_map(response.headers());

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let message = response.text().await.unwrap_or_default();

            crate::provider::transaction_log::log(
                OPENAI_CODEX_URL,
                req_headers.clone(),
                serde_json::to_value(&request).unwrap_or_default(),
                resp_headers,
                serde_json::json!({
                    "error": true,
                    "status": status,
                    "body": message
                }),
            );

            return Err(super::api_error(status, message));
        }

        let mut full_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut stop_reason = StopReason::Unknown;
        let mut pending_tools: HashMap<String, PendingToolCall> = HashMap::new();
        let mut thinking = output::ThinkingState::new(output);
        let mut raw_events: Vec<serde_json::Value> = Vec::new();
        let streaming_start = std::time::Instant::now();
        let mut char_count = 0usize;
        let mut reasoning_summary = String::new();
        let mut encrypted_content: Option<String> = None;

        let mut sse = sse::SseStream::new(response.bytes_stream().map(|chunk| {
            if let Ok(ref bytes) = chunk {
                crate::usage::network_stats().record_rx(bytes.len() as u64);
            }
            chunk
        }));
        while let Some(result) = sse.next_event().await {
            let data = result.map_err(Error::Http)?;

            let Ok(event) = serde_json::from_str::<serde_json::Value>(&data) else {
                continue;
            };
            raw_events.push(event.clone());

            let event_type = event
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string();

            if (event_type == "response.output_item.added"
                || event_type == "response.output_item.done")
                && let Some(item) = event.get("item")
                && item.get("type").and_then(|v| v.as_str()) == Some("function_call")
            {
                let call_id = item
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let arguments = item
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                if !call_id.is_empty() || !name.is_empty() || !arguments.is_empty() {
                    let pending = pending_tools.entry(call_id.clone()).or_default();
                    if !name.is_empty() {
                        pending.name = name;
                    }
                    if !call_id.is_empty() {
                        pending.id = call_id;
                    }
                    if !arguments.is_empty() {
                        pending.arguments = arguments;
                    }
                }
            }

            // Reasoning stream (best effort) - OpenAI may return either `reasoning`
            // or `reasoning_content` in the delta payload, or send separate
            // reasoning summary text events.
            if self.thinking_enabled {
                if let Some(reasoning_delta) = event
                    .get("delta")
                    .and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
                    .and_then(|r| r.as_str())
                {
                    thinking.emit(reasoning_delta);
                    reasoning_summary.push_str(reasoning_delta);
                } else if event_type.contains("reasoning_summary_text.delta")
                    && let Some(delta) = event.get("delta").and_then(|d| d.as_str())
                {
                    thinking.emit(delta);
                    reasoning_summary.push_str(delta);
                }
            }

            if event_type.contains("output_text.delta") {
                if let Some(delta) = event.get("delta").and_then(|d| d.as_str()) {
                    thinking.end();
                    output::print_text(output, delta);
                    full_text.push_str(delta);
                    char_count += delta.chars().count();

                    // Emit progress every ~50 chars
                    if char_count.is_multiple_of(50) {
                        let duration = streaming_start.elapsed().as_secs_f64();
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
                }
            } else if event_type.contains("output_text.done") {
                if let Some(text) = event.get("output_text").and_then(|d| d.as_str()) {
                    thinking.end();
                    output::print_text(output, text);
                    full_text.push_str(text);
                    char_count += text.chars().count();

                    let duration = streaming_start.elapsed().as_secs_f64();
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
            } else if event_type.contains("function_call_arguments.delta")
                || event_type.contains("output_tool_call")
            {
                let call_id = event
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let name = event
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let delta = event
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if !call_id.is_empty() || !name.is_empty() || !delta.is_empty() {
                    let pending = pending_tools.entry(call_id.to_string()).or_default();
                    if !name.is_empty() {
                        pending.name = name.to_string();
                    }
                    if !call_id.is_empty() {
                        pending.id = call_id.to_string();
                    }
                    pending.arguments.push_str(delta);
                }
            } else if event_type.contains("function_call_arguments.done") {
                let call_id = event
                    .get("call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let name = event
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let arguments = event
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if !call_id.is_empty() || !name.is_empty() || !arguments.is_empty() {
                    let pending = pending_tools.entry(call_id.to_string()).or_default();
                    if !name.is_empty() {
                        pending.name = name.to_string();
                    }
                    if !call_id.is_empty() {
                        pending.id = call_id.to_string();
                    }
                    if !arguments.is_empty() {
                        pending.arguments = arguments.to_string();
                    }
                }
            } else if event_type.contains("response.completed")
                || event_type.contains("response.done")
            {
                stop_reason = StopReason::EndTurn;
                if let Some(resp) = event.get("response") {
                    // Extract encrypted reasoning content if present
                    if let Some(reasoning) = resp.get("reasoning")
                        && let Some(encrypted) = reasoning.get("encrypted_content")
                        && let Some(encrypted_str) = encrypted.as_str()
                    {
                        encrypted_content = Some(encrypted_str.to_string());
                    }

                    // Extract actual usage data from the response
                    if let Some(usage) = resp.get("usage") {
                        if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
                            self.usage_tracker.record_input(input);
                            let limit = Self::context_limit(&self.model);
                            output::emit_context_update(output, input, limit);
                        }
                        if let Some(output_tokens) =
                            usage.get("output_tokens").and_then(|v| v.as_u64())
                        {
                            self.usage_tracker.record_output(output_tokens);
                            // Emit final progress update with turn total (accumulated across all API calls)
                            let duration = streaming_start.elapsed().as_secs_f64();
                            if duration > 0.0 {
                                let turn_total = self.usage_tracker.turn_total();
                                let tokens_per_sec = output_tokens as f64 / duration;
                                output::emit_working_progress(
                                    output,
                                    turn_total,
                                    duration,
                                    tokens_per_sec,
                                );
                            }
                        }
                        // Handle cached tokens
                        if let Some(details) = usage.get("input_tokens_details")
                            && let Some(cached) =
                                details.get("cached_tokens").and_then(|v| v.as_u64())
                        {
                            self.usage_tracker.add_cache_read(cached);
                        }
                    }

                    // Also extract output_text if present
                    if let Some(text) = resp.get("output_text").and_then(|t| t.as_str())
                        && !text.is_empty()
                    {
                        full_text.push_str(text);
                    }
                }
            }
        }

        // Only end the text block if we actually streamed any text.
        if !full_text.is_empty() {
            output::print_text_end(output);
        }

        if crate::provider::transaction_log::is_active() {
            crate::provider::transaction_log::log(
                OPENAI_CODEX_URL,
                req_headers,
                serde_json::to_value(&request).unwrap_or(serde_json::json!({})),
                resp_headers,
                serde_json::Value::Array(raw_events),
            );
        }

        // Add thinking block first (if reasoning was present)
        if !reasoning_summary.is_empty() {
            let provider_data =
                encrypted_content.map(|enc| serde_json::json!({"encrypted_content": enc}));
            content_blocks.push(ContentBlock::Thinking {
                thinking: reasoning_summary,
                provider_data,
            });
        }

        // Add text block (if present) to maintain correct order
        if !full_text.is_empty() {
            content_blocks.push(ContentBlock::Text {
                text: full_text.clone(),
            });
        }

        // Then add tool use blocks (if any)
        for (_, pending) in pending_tools {
            if !pending.id.is_empty() && !pending.name.is_empty() {
                let input: serde_json::Value =
                    serde_json::from_str(&pending.arguments).unwrap_or(serde_json::json!({}));
                tool_calls.push(ToolCall {
                    id: pending.id.clone(),
                    name: pending.name.clone(),
                    input: input.clone(),
                    thought_signature: None,
                });

                content_blocks.push(ContentBlock::ToolUse {
                    id: pending.id,
                    name: pending.name,
                    input,
                    thought_signature: None,
                });
            }
        }

        if !tool_calls.is_empty() {
            stop_reason = StopReason::ToolUse;
        } else if stop_reason == StopReason::Unknown {
            stop_reason = StopReason::EndTurn;
        }

        Ok(ChatResponse {
            tool_calls,
            content_blocks,
            stop_reason,
        })
    }

    async fn execute_chat(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        let mut attempts = 0;
        let mut delay = INITIAL_RETRY_DELAY;

        loop {
            let request = self.build_request(&messages).await;
            let result = self.execute_chat_with_request(request, output).await;

            match result {
                Ok(response) => return Ok(response),
                Err(ref e) if e.is_retryable() && attempts < MAX_RETRIES => {
                    attempts += 1;
                    output::emit_warning(
                        output,
                        &format!(
                            "{} (retrying in {}s, attempt {}/{})",
                            e.display_message(),
                            delay.as_secs(),
                            attempts,
                            MAX_RETRIES
                        ),
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Get the context limit for a given model name
    pub(crate) fn context_limit(_model: &str) -> Option<u64> {
        // All these models are 272,000 as of now.
        Some(272000)
    }
}

impl Provider for OpenAiProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        self.execute_chat(messages, output).await
    }

    async fn prepare_request(&self, messages: Vec<Message>) -> Result<serde_json::Value> {
        let request = self.build_request(&messages).await;
        Ok(serde_json::to_value(&request)?)
    }

    fn start_turn(&self) {
        crate::usage::openai().start_turn();
    }
}

fn chatgpt_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let rem = payload.len() % 4;
    let mut padded = payload.to_string();
    if rem != 0 {
        padded.push_str(&"=".repeat(4 - rem));
    }

    let decoded = URL_SAFE_NO_PAD
        .decode(padded.as_bytes())
        .ok()
        .or_else(|| STANDARD.decode(padded.as_bytes()).ok())?;

    let value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    value
        .get("https://api.openai.com/auth")
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_variants() {
        // gpt-5.2-codex has 4 variants (low to high)
        let variants = OpenAiProvider::model_variants("gpt-5.2-codex#medium");
        assert_eq!(variants, vec!["low", "medium", "high", "xhigh"]);

        // gpt-5.2 also has 4 variants
        let variants = OpenAiProvider::model_variants("gpt-5.2#high");
        assert_eq!(variants, vec!["low", "medium", "high", "xhigh"]);

        // gpt-5.1-codex-mini has only 2 variants
        let variants = OpenAiProvider::model_variants("gpt-5.1-codex-mini#medium");
        assert_eq!(variants, vec!["medium", "high"]);
    }

    #[test]
    fn test_cycle_model_variant() {
        // Cycle through gpt-5.2-codex variants (low to high, then wrap)
        assert_eq!(
            OpenAiProvider::cycle_model_variant("gpt-5.2-codex#low"),
            "gpt-5.2-codex#medium"
        );
        assert_eq!(
            OpenAiProvider::cycle_model_variant("gpt-5.2-codex#medium"),
            "gpt-5.2-codex#high"
        );
        assert_eq!(
            OpenAiProvider::cycle_model_variant("gpt-5.2-codex#high"),
            "gpt-5.2-codex#xhigh"
        );
        assert_eq!(
            OpenAiProvider::cycle_model_variant("gpt-5.2-codex#xhigh"),
            "gpt-5.2-codex#low" // wraps around
        );

        // Cycle through gpt-5.1-codex-mini variants (only 2)
        assert_eq!(
            OpenAiProvider::cycle_model_variant("gpt-5.1-codex-mini#medium"),
            "gpt-5.1-codex-mini#high"
        );
        assert_eq!(
            OpenAiProvider::cycle_model_variant("gpt-5.1-codex-mini#high"),
            "gpt-5.1-codex-mini#medium" // wraps around
        );
    }

    #[test]
    fn test_cycle_model_variant_unknown() {
        // Unknown model returns unchanged
        assert_eq!(
            OpenAiProvider::cycle_model_variant("unknown-model"),
            "unknown-model"
        );
    }
}
