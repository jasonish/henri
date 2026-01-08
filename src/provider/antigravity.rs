// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use futures::StreamExt;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::auth::{GOOGLE_TOKEN_URL, get_antigravity_client_id, get_antigravity_client_secret};
use crate::config::{AntigravityProviderConfig, ConfigFile, ProviderConfig};
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

// API endpoints
const ANTIGRAVITY_ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:streamGenerateContent",
    "https://autopush-cloudcode-pa.sandbox.googleapis.com/v1internal:streamGenerateContent",
    "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent",
];

// Available models
const ANTIGRAVITY_MODELS: &[&str] = &[
    "gemini-3-flash",
    "gemini-3-pro-high",
    "claude-sonnet-4-5-thinking",
    "claude-opus-4-5-thinking",
];

struct AuthState {
    local_id: String,
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    project_id: Option<String>,
}

/// Antigravity system instruction prompt embedded at compile time.
const ANTIGRAVITY_SYSTEM_INSTRUCTION: &str = include_str!("../prompts/antigravity.md");

/// Tracks streaming progress for periodic updates
struct ProgressTracker {
    start: Option<Instant>,
    last_update: Instant,
    char_count: usize,
}

impl ProgressTracker {
    fn new() -> Self {
        Self {
            start: None,
            last_update: Instant::now(),
            char_count: 0,
        }
    }

    fn add_chars(&mut self, count: usize) {
        if self.start.is_none() {
            self.start = Some(Instant::now());
            self.last_update = Instant::now();
        }
        self.char_count += count;
    }

    fn maybe_emit(&mut self, output: &output::OutputContext) {
        if self.last_update.elapsed().as_secs_f64() >= 0.5 {
            if let Some(start) = self.start {
                let duration = start.elapsed().as_secs_f64();
                let estimated_tokens = (self.char_count / 4) as u64;
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
            self.last_update = Instant::now();
        }
    }

    fn final_emit(&self, output: &output::OutputContext, output_tokens: u64) {
        if let Some(start) = self.start {
            let duration = start.elapsed().as_secs_f64();
            if duration > 0.0 {
                let turn_total = usage::antigravity().turn_total();
                let tokens_per_sec = output_tokens as f64 / duration;
                output::emit_working_progress(output, turn_total, duration, tokens_per_sec);
            }
        }
    }
}

pub(crate) struct AntigravityProvider {
    client: Client,
    state: Mutex<AuthState>,
    model: String,
    thinking_enabled: bool,
    thinking_mode: Option<String>,
    services: Services,
}

impl AntigravityProvider {
    pub(crate) fn try_new(provider_name: &str, services: Services) -> Result<Self> {
        let config = ConfigFile::load()?;

        let antigravity = config
            .get_provider(provider_name)
            .and_then(|p| p.as_antigravity())
            .ok_or_else(|| {
                Error::Auth(format!(
                    "Antigravity provider '{}' not configured. Run 'henri provider add'.",
                    provider_name
                ))
            })?;

        if !antigravity.enabled {
            return Err(Error::Auth(format!(
                "Antigravity provider '{}' is disabled.",
                provider_name
            )));
        }

        Ok(Self {
            client: Client::new(),
            state: Mutex::new(AuthState {
                local_id: provider_name.to_string(),
                access_token: antigravity.access_token.clone(),
                refresh_token: antigravity.refresh_token.clone(),
                expires_at: antigravity.expires_at,
                project_id: antigravity.project_id.clone(),
            }),
            model: "gemini-3-flash".to_string(),
            thinking_enabled: true,
            thinking_mode: None,
            services,
        })
    }

    pub(crate) fn set_thinking_enabled(&mut self, enabled: bool) {
        self.thinking_enabled = enabled;
        if !enabled {
            self.thinking_mode = None;
        }
    }

    pub(crate) fn set_thinking_mode(&mut self, mode: Option<String>) {
        self.thinking_mode = mode;
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.model = model;
    }

    /// Returns the available thinking modes for the given model.
    pub(crate) fn thinking_modes(model: &str) -> &'static [&'static str] {
        if model.starts_with("claude-") {
            &["off", "low", "medium", "high"]
        } else if model == "gemini-3-flash" {
            &["minimal", "low", "medium", "high"]
        } else if model.starts_with("gemini-") {
            &["low", "high"]
        } else {
            &["off", "on"]
        }
    }

    /// Returns the default thinking state for the given model.
    pub(crate) fn default_thinking_state(model: &str) -> crate::providers::ThinkingState {
        if model.starts_with("claude-") {
            crate::providers::ThinkingState::new(true, Some("medium".to_string()))
        } else if model.starts_with("gemini-3-pro") {
            crate::providers::ThinkingState::new(true, Some("high".to_string()))
        } else if model.starts_with("gemini-3-flash") {
            crate::providers::ThinkingState::new(true, Some("medium".to_string()))
        } else {
            crate::providers::ThinkingState::new(true, None)
        }
    }

    pub(crate) fn models() -> &'static [&'static str] {
        ANTIGRAVITY_MODELS
    }

    pub(crate) fn context_limit(model: &str) -> Option<u64> {
        if model.starts_with("claude-") {
            Some(200_000)
        } else if model.starts_with("gemini-") {
            Some(1_000_000)
        } else {
            None
        }
    }

    async fn ensure_access_token(&self) -> Result<String> {
        self.refresh_access_token(false).await
    }

    async fn force_refresh(&self) -> Result<String> {
        self.refresh_access_token(true).await
    }

    async fn refresh_access_token(&self, force: bool) -> Result<String> {
        let mut state = self.state.lock().await;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| Error::Auth(e.to_string()))?
            .as_millis() as u64;

        // Check if token is still valid (with 30-minute buffer)
        if !force
            && (now < state.expires_at.saturating_sub(30 * 60 * 1000)
                || state.refresh_token.is_empty())
        {
            return Ok(state.access_token.clone());
        }

        // Refresh the token
        let response = self
            .client
            .post(GOOGLE_TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", &get_antigravity_client_id()),
                ("client_secret", &get_antigravity_client_secret()),
                ("refresh_token", state.refresh_token.as_str()),
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
                "Antigravity token refresh failed: {} - {}",
                status, text
            )));
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;

        let access_token = json["access_token"]
            .as_str()
            .ok_or_else(|| Error::Auth("Missing access_token".to_string()))?;
        let expires_in = json["expires_in"]
            .as_u64()
            .ok_or_else(|| Error::Auth("Missing expires_in".to_string()))?;

        state.access_token = access_token.to_string();
        state.expires_at = now + expires_in * 1000;

        // Persist updated tokens to config
        if let Ok(mut config) = ConfigFile::load() {
            let enabled = config
                .get_provider(&state.local_id)
                .map(|p| p.is_enabled())
                .unwrap_or(true);

            // Get existing config to preserve other fields
            let existing = config
                .get_provider(&state.local_id)
                .and_then(|p| p.as_antigravity().cloned());

            config.set_provider(
                state.local_id.clone(),
                ProviderConfig::Antigravity(AntigravityProviderConfig {
                    enabled,
                    access_token: state.access_token.clone(),
                    refresh_token: state.refresh_token.clone(),
                    expires_at: state.expires_at,
                    email: existing.as_ref().and_then(|e| e.email.clone()),
                    tier: existing.as_ref().and_then(|e| e.tier.clone()),
                    project_id: state.project_id.clone(),
                }),
            );
            let _ = config.save();
        }

        Ok(state.access_token.clone())
    }

    fn build_messages(&self, messages: &[Message]) -> Vec<serde_json::Value> {
        let mut gemini_messages = Vec::new();

        for msg in messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "model",
                Role::System => continue, // System messages handled separately
            };

            let parts = match &msg.content {
                MessageContent::Text(text) => {
                    vec![serde_json::json!({"text": text})]
                }
                MessageContent::Blocks(blocks) => {
                    let mut parts = Vec::new();
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                parts.push(serde_json::json!({"text": text}));
                            }
                            ContentBlock::Image { mime_type, data } => {
                                let base64_data =
                                    base64::engine::general_purpose::STANDARD.encode(data);
                                parts.push(serde_json::json!({
                                    "inlineData": {
                                        "mimeType": mime_type,
                                        "data": base64_data
                                    }
                                }));
                            }
                            ContentBlock::Thinking {
                                thinking,
                                provider_data,
                            } => {
                                // Include thinking as thought block with signature if available
                                let mut thought_block = serde_json::json!({
                                    "thought": true,
                                    "text": thinking
                                });
                                if let Some(data) = provider_data
                                    && let Some(sig) =
                                        data.get("signature").and_then(|s| s.as_str())
                                {
                                    thought_block["thoughtSignature"] = sig.into();
                                }
                                parts.push(thought_block);
                            }
                            ContentBlock::ToolUse {
                                id,
                                name,
                                input,
                                thought_signature,
                            } => {
                                // Use Gemini-style functionCall for all models
                                // Antigravity translates internally for Claude
                                let mut fc_part = serde_json::json!({
                                    "functionCall": {
                                        "name": name,
                                        "args": input,
                                        "id": id
                                    }
                                });
                                if let Some(sig) = thought_signature {
                                    fc_part["thoughtSignature"] = sig.clone().into();
                                }
                                parts.push(fc_part);
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => {
                                // Use Gemini-style functionResponse for all models
                                // Include id field for Claude model translation
                                parts.push(serde_json::json!({
                                    "functionResponse": {
                                        "id": tool_use_id,
                                        "name": tool_use_id,
                                        "response": {
                                            "result": content,
                                            "error": is_error
                                        }
                                    }
                                }));
                            }
                            ContentBlock::Summary {
                                summary,
                                messages_compacted,
                            } => {
                                parts.push(serde_json::json!({
                                    "text": format!("[Summary of {} previous messages]\n\n{}", messages_compacted, summary)
                                }));
                            }
                        }
                    }
                    parts
                }
            };

            gemini_messages.push(serde_json::json!({
                "role": role,
                "parts": parts
            }));
        }

        gemini_messages
    }

    /// Build the inner Gemini-style request (contents, systemInstruction, tools, etc.)
    async fn build_inner_request(&self, messages: &[Message]) -> serde_json::Value {
        let tools: Vec<serde_json::Value> = tools::all_definitions(&self.services)
            .await
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema
                })
            })
            .collect();

        let mut system_parts = vec![
            serde_json::json!({"text": ANTIGRAVITY_SYSTEM_INSTRUCTION}),
            serde_json::json!({"text": "Your name is Henri."}),
        ];

        for part in system_prompt() {
            system_parts.push(serde_json::json!({"text": part}));
        }

        // Generate session ID: -{random_number}
        let session_id = format!("-{}", rand::random::<u64>());

        let mut request = serde_json::json!({
            "contents": self.build_messages(messages),
            "systemInstruction": {
                "role": "user",
                "parts": system_parts
            },
            "tools": [{
                "functionDeclarations": tools
            }],
            "generationConfig": {
                "maxOutputTokens": 64000,
                "temperature": 1.0
            },
            "sessionId": session_id,
            "safetySettings": [
                {"category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF"},
                {"category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF"},
                {"category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF"},
                {"category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF"},
                {"category": "HARM_CATEGORY_CIVIC_INTEGRITY", "threshold": "BLOCK_NONE"}
            ]
        });

        if self.model.starts_with("claude-") {
            let budget = match self.thinking_mode.as_deref() {
                Some("low") => 4000,
                Some("medium") => 16000,
                Some("high") => 32000,
                _ => 0,
            };
            if budget > 0 {
                request["generationConfig"]["thinkingConfig"] = serde_json::json!({
                    "includeThoughts": true,
                    "thinkingBudget": budget,
                });
            }
        } else if self.model.starts_with("gemini-")
            && let Some(level) = &self.thinking_mode
        {
            request["generationConfig"]["thinkingConfig"] = serde_json::json!({
                "thinkingLevel": level,
                "includeThoughts": true
            });
        }

        request
    }

    /// Build the full Antigravity envelope wrapping the inner request
    fn build_antigravity_envelope(
        &self,
        inner_request: serde_json::Value,
        project_id: &str,
    ) -> serde_json::Value {
        // Generate request ID: agent-{uuid}
        let request_id = format!("agent-{}", uuid::Uuid::new_v4());

        serde_json::json!({
            "project": project_id,
            "userAgent": "antigravity",
            "requestId": request_id,
            "model": &self.model,
            "request": inner_request,
            "requestType": "agent"
        })
    }

    async fn send_chat_request(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        let access_token = self.ensure_access_token().await?;
        let project_id = {
            let state = self.state.lock().await;
            state.project_id.clone().unwrap_or_default()
        };

        // Build the inner Gemini request
        let inner_request = self.build_inner_request(&messages).await;

        // Wrap in Antigravity envelope
        let request = self.build_antigravity_envelope(inner_request, &project_id);

        // Record TX bytes
        let body_bytes = serde_json::to_vec(&request)?;
        usage::network_stats().record_tx(body_bytes.len() as u64);

        let client_metadata = serde_json::json!({
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        });

        let mut headers = std::collections::HashMap::new();
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Accept".to_string(), "text/event-stream".to_string());
        headers.insert(
            "Authorization".to_string(),
            format!("Bearer {}", access_token),
        );
        headers.insert(
            "User-Agent".to_string(),
            "antigravity/1.11.5 windows/amd64".to_string(),
        );
        headers.insert(
            "X-Goog-Api-Client".to_string(),
            "google-cloud-sdk vscode_cloudshelleditor/0.1".to_string(),
        );
        headers.insert("Client-Metadata".to_string(), client_metadata.to_string());
        if !project_id.is_empty() {
            headers.insert("X-Goog-User-Project".to_string(), project_id.clone());
        }

        let mut last_error = None;
        let mut final_response = None;

        for base_url in ANTIGRAVITY_ENDPOINTS {
            // Build URL - model is in request body, not URL path
            let url = format!("{}?alt=sse", base_url);

            let mut request_builder = self.client.post(&url);
            for (key, value) in &headers {
                request_builder = request_builder.header(key, value);
            }

            match request_builder.body(body_bytes.clone()).send().await {
                Ok(response) => {
                    if response.status().is_success() {
                        final_response = Some(response);
                        break;
                    }

                    let status = response.status();
                    let status_code = status.as_u16();

                    // Capture headers before consuming the response body
                    let error_headers = crate::provider::transaction_log::header_map_to_hash_map(
                        response.headers(),
                    );

                    // If Unauthorized, fail immediately to trigger refresh in outer loop
                    if status_code == 401 {
                        let text = response.text().await.unwrap_or_default();

                        crate::provider::transaction_log::log(
                            &url,
                            headers.clone(),
                            request.clone(),
                            error_headers,
                            serde_json::json!({
                                "error": true,
                                "status": status_code,
                                "body": text
                            }),
                        );

                        return Err(Error::Unauthorized(format!(
                            "Antigravity chat failed: {} - {}",
                            status, text
                        )));
                    }

                    // For other errors, store and try next endpoint
                    let text = response.text().await.unwrap_or_default();

                    crate::provider::transaction_log::log(
                        &url,
                        headers.clone(),
                        request.clone(),
                        error_headers,
                        serde_json::json!({
                            "error": true,
                            "status": status_code,
                            "body": text
                        }),
                    );

                    // Check for retryable errors (timeouts, overloaded, rate limits)
                    if super::is_retryable_status(status_code) || super::is_retryable_message(&text)
                    {
                        last_error = Some(Error::Retryable {
                            status: status_code,
                            message: text,
                        });
                    } else {
                        last_error = Some(Error::Auth(format!(
                            "Antigravity chat failed: {} - {}",
                            status, text
                        )));
                    }
                }
                Err(e) => {
                    last_error = Some(Error::Other(format!(
                        "Failed to connect to Antigravity API ({}): {}",
                        base_url, e
                    )));
                }
            }
        }

        let response = final_response.ok_or_else(|| {
            last_error.unwrap_or_else(|| {
                Error::Other("Failed to connect to any Antigravity endpoint".to_string())
            })
        })?;

        let resp_headers =
            crate::provider::transaction_log::header_map_to_hash_map(response.headers());
        // Use primary endpoint URL for logging (actual endpoint used is not tracked)
        let url = ANTIGRAVITY_ENDPOINTS[0].to_string();

        // Process streaming response
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut stop_reason = StopReason::Unknown;
        let mut raw_events: Vec<serde_json::Value> = Vec::new();

        let mut current_text = String::new();
        let mut current_thinking = String::new();
        let mut current_thought_signature: Option<String> = None;
        let mut thinking = output::ThinkingState::new(output);
        let mut progress = ProgressTracker::new();
        // Track usage metadata - only record final values (API sends cumulative counts with each chunk)
        let mut final_prompt_tokens: Option<u64> = None;
        let mut final_output_tokens: Option<u64> = None;

        let mut sse = sse::SseStream::new(response.bytes_stream().map(|chunk| {
            if let Ok(ref bytes) = chunk {
                usage::network_stats().record_rx(bytes.len() as u64);
            }
            chunk
        }));

        while let Some(result) = sse.next_event().await {
            let data = result.map_err(Error::Http)?;

            let Ok(event): std::result::Result<serde_json::Value, _> = serde_json::from_str(&data)
            else {
                continue;
            };
            raw_events.push(event.clone());

            // Parse Gemini streaming response format
            // Response may be wrapped in a "response" key (Antigravity format) or direct (Gemini format)
            let response_obj = event.get("response").unwrap_or(&event);
            if let Some(candidates) = response_obj.get("candidates").and_then(|c| c.as_array()) {
                for candidate in candidates {
                    if let Some(content) = candidate.get("content")
                        && let Some(parts) = content.get("parts").and_then(|p| p.as_array())
                    {
                        for part in parts {
                            // Handle thought/thinking blocks
                            if part.get("thought").and_then(|t| t.as_bool()) == Some(true) {
                                // Capture thoughtSignature if present (comes at end of thinking)
                                if let Some(sig) =
                                    part.get("thoughtSignature").and_then(|s| s.as_str())
                                {
                                    current_thought_signature = Some(sig.to_string());
                                }

                                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                    progress.add_chars(text.len());
                                    current_thinking.push_str(text);
                                    thinking.emit(text);
                                    progress.maybe_emit(output);
                                }
                            }
                            // Handle regular text
                            else if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                // Finalize thinking if we have any
                                if !current_thinking.is_empty() {
                                    thinking.end();
                                    content_blocks.push(ContentBlock::Thinking {
                                        thinking: current_thinking.clone(),
                                        provider_data: current_thought_signature
                                            .take()
                                            .map(|sig| serde_json::json!({"signature": sig})),
                                    });
                                    current_thinking.clear();
                                }

                                progress.add_chars(text.len());
                                current_text.push_str(text);
                                output::print_text(output, text);
                                progress.maybe_emit(output);
                            }
                            // Handle function calls
                            else if let Some(fc) = part.get("functionCall") {
                                // Finalize text and thinking
                                if !current_thinking.is_empty() {
                                    thinking.end();
                                    content_blocks.push(ContentBlock::Thinking {
                                        thinking: current_thinking.clone(),
                                        provider_data: current_thought_signature
                                            .clone()
                                            .map(|sig| serde_json::json!({"signature": sig})),
                                    });
                                    current_thinking.clear();
                                }
                                if !current_text.is_empty() {
                                    content_blocks.push(ContentBlock::Text {
                                        text: current_text.clone(),
                                    });
                                    current_text.clear();
                                }

                                let name = fc
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let args = fc.get("args").cloned().unwrap_or(serde_json::json!({}));

                                // Use ID from API if available, otherwise generate one
                                let id = fc
                                    .get("id")
                                    .and_then(|i| i.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| {
                                        format!(
                                            "call_{}_{:x}",
                                            std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap()
                                                .as_nanos(),
                                            tool_calls.len()
                                        )
                                    });

                                // Get thoughtSignature - may be sibling of functionCall in same part,
                                // or from earlier thinking block
                                let thought_sig = part
                                    .get("thoughtSignature")
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.to_string())
                                    .or_else(|| current_thought_signature.clone());

                                tool_calls.push(ToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    input: args.clone(),
                                    thought_signature: thought_sig.clone(),
                                });

                                content_blocks.push(ContentBlock::ToolUse {
                                    id,
                                    name,
                                    input: args,
                                    thought_signature: thought_sig,
                                });
                            }
                        }
                    }

                    // Check finish reason
                    if let Some(finish_reason) =
                        candidate.get("finishReason").and_then(|r| r.as_str())
                    {
                        stop_reason = match finish_reason {
                            "STOP" => StopReason::EndTurn,
                            "MAX_TOKENS" => StopReason::MaxTokens,
                            _ => stop_reason,
                        };
                    }
                }
            }

            // Track usage metadata - just capture values, record after loop
            // (API sends cumulative counts with each streaming chunk)
            if let Some(usage_meta) = response_obj.get("usageMetadata") {
                if let Some(input) = usage_meta.get("promptTokenCount").and_then(|t| t.as_u64()) {
                    final_prompt_tokens = Some(input);
                }
                if let Some(output_tokens) = usage_meta
                    .get("candidatesTokenCount")
                    .and_then(|t| t.as_u64())
                {
                    final_output_tokens = Some(output_tokens);
                }
            }
        }

        // Record final usage (only once, after streaming completes)
        if let Some(input) = final_prompt_tokens {
            usage::antigravity().record_input(input);
            let limit = Self::context_limit(&self.model);
            output::emit_context_update(output, input, limit);
        }
        if let Some(output_tokens) = final_output_tokens {
            usage::antigravity().record_output(output_tokens);
            progress.final_emit(output, output_tokens);
        }

        // Finalize any remaining content
        if !current_thinking.is_empty() {
            thinking.end();
            content_blocks.push(ContentBlock::Thinking {
                thinking: current_thinking,
                provider_data: current_thought_signature
                    .take()
                    .map(|sig| serde_json::json!({"signature": sig})),
            });
        }
        if !current_text.is_empty() {
            content_blocks.push(ContentBlock::Text { text: current_text });
        }

        // Ensure stop_reason reflects tool calls (API may send STOP even with function calls)
        if !tool_calls.is_empty() {
            stop_reason = StopReason::ToolUse;
        }

        output::print_text_end(output);

        if crate::provider::transaction_log::is_active() {
            crate::provider::transaction_log::log(
                &url,
                headers,
                request,
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

impl Provider for AntigravityProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        match self.send_chat_request(messages.clone(), output).await {
            Ok(response) => Ok(response),
            Err(Error::Unauthorized(_)) => {
                self.force_refresh().await?;
                self.send_chat_request(messages, output).await
            }
            Err(e) => Err(e),
        }
    }

    async fn prepare_request(&self, messages: Vec<Message>) -> Result<serde_json::Value> {
        let project_id = {
            let state = self.state.lock().await;
            state.project_id.clone().unwrap_or_default()
        };
        let inner_request = self.build_inner_request(&messages).await;
        let request = self.build_antigravity_envelope(inner_request, &project_id);
        Ok(request)
    }

    fn start_turn(&self) {
        crate::usage::antigravity().start_turn();
    }
}
