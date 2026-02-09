// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::auth::{GOOGLE_TOKEN_URL, get_antigravity_client_id, get_antigravity_client_secret};
use crate::config::{AntigravityProviderConfig, ConfigFile, ProviderConfig};
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

// API endpoints
const ANTIGRAVITY_ENDPOINTS: &[&str] = &[
    "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal:streamGenerateContent?alt=sse",
    "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse",
];

// Available models with thinking level variants
const ANTIGRAVITY_MODELS: &[&str] = &[
    "gemini-3-flash#minimal",
    "gemini-3-flash#low",
    "gemini-3-flash#medium",
    "gemini-3-flash#high",
    "gemini-3-pro-high#low",
    "gemini-3-pro-high#high",
    "claude-sonnet-4-5-thinking#off",
    "claude-sonnet-4-5-thinking#low",
    "claude-sonnet-4-5-thinking#medium",
    "claude-sonnet-4-5-thinking#high",
    "claude-sonnet-4-5-thinking#xhigh",
    "claude-opus-4-6-thinking#off",
    "claude-opus-4-6-thinking#low",
    "claude-opus-4-6-thinking#medium",
    "claude-opus-4-6-thinking#high",
    "claude-opus-4-6-thinking#xhigh",
];

fn thinking_level_from_model(model: &str) -> Option<&str> {
    model_utils::model_variant(model)
}

/// Maximum number of retries for the internal "fast" retry loop.
/// This handles transient errors (network drops, 429s, 5xx).
const INTERNAL_MAX_RETRIES: u32 = 3;

/// Initial retry delay for internal loop (exponential: 1s, 2s, 4s)
const INTERNAL_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

struct AuthState {
    local_id: String,
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    project_id: Option<String>,
}

/// Antigravity system instruction prompt embedded at compile time.
const ANTIGRAVITY_SYSTEM_INSTRUCTION: &str = include_str!("../prompts/antigravity.md");

fn strip_unsupported_schema_fields(schema: serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(mut map) => {
            map.remove("$schema");
            map.remove("$id");
            map.remove("$comment");
            map.remove("$ref");
            map.remove("$defs");
            map.remove("definitions");
            map.remove("const");
            map.remove("additionalProperties");
            map.remove("propertyNames");
            map.remove("title");

            let cleaned: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .map(|(k, v)| (k, strip_unsupported_schema_fields(v)))
                .collect();

            serde_json::Value::Object(cleaned)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.into_iter()
                .map(strip_unsupported_schema_fields)
                .collect(),
        ),
        other => other,
    }
}

pub(crate) struct AntigravityProvider {
    state: Mutex<AuthState>,
    model: String,
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
                    "Antigravity provider '{}' not configured. Enter `/provider` to add a provider/model.",
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
            state: Mutex::new(AuthState {
                local_id: provider_name.to_string(),
                access_token: antigravity.access_token.clone(),
                refresh_token: antigravity.refresh_token.clone(),
                expires_at: antigravity.expires_at,
                project_id: antigravity.project_id.clone(),
            }),
            model: "gemini-3-flash#medium".to_string(),
            services,
        })
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.model = model;
    }

    /// Get the available variants (thinking levels) for a given model.
    /// Returns the variant suffixes like "high", "medium", "low".
    pub(crate) fn model_variants(model: &str) -> Vec<&'static str> {
        let base = model_utils::base_model_name(model);
        model_utils::get_model_variants(base, ANTIGRAVITY_MODELS)
            .iter()
            .filter_map(|m| model_utils::model_variant(m))
            .collect()
    }

    /// Cycle to the next variant for the given model.
    /// Returns the new full model string with the next variant.
    pub(crate) fn cycle_model_variant(model: &str) -> String {
        model_utils::cycle_model_variant(model, ANTIGRAVITY_MODELS, Some("medium"))
    }

    /// Returns the default thinking state for the given model.
    pub(crate) fn default_thinking_state(model: &str) -> crate::providers::ThinkingState {
        // Extract variant from model name
        if let Some(variant) = thinking_level_from_model(model) {
            crate::providers::ThinkingState::new(variant != "off", Some(variant.to_string()))
        } else {
            // Fallback for bare model names (shouldn't happen with new format)
            crate::providers::ThinkingState::new(true, Some("medium".to_string()))
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
        let client = Client::new();
        let response = client
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

            config.set_provider(
                state.local_id.clone(),
                ProviderConfig::Antigravity(AntigravityProviderConfig {
                    enabled,
                    access_token: state.access_token.clone(),
                    refresh_token: state.refresh_token.clone(),
                    expires_at: state.expires_at,
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
                                data,
                                mime_type,
                            } => {
                                // Use Gemini-style functionResponse for all models
                                // Include id field for Claude model translation
                                let mut func_response = serde_json::json!({
                                    "functionResponse": {
                                        "id": tool_use_id,
                                        "name": tool_use_id,
                                        "response": {
                                            "output": content
                                        }
                                    }
                                });

                                // For image results, add the inlineData parts.
                                if let (Some(image_data), Some(mime)) = (data, mime_type) {
                                    func_response["functionResponse"]["parts"] = serde_json::json!([{
                                        "inlineData": {
                                            "mimeType": mime,
                                            "data": image_data
                                        }
                                    }]);
                                }

                                // Add error field if this is an error result.
                                if *is_error {
                                    func_response["functionResponse"]["response"]["error"] =
                                        serde_json::json!(true);
                                }

                                parts.push(func_response);
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
                    "parameters": strip_unsupported_schema_fields(t.input_schema)
                })
            })
            .collect();

        let mut system_parts = vec![
            serde_json::json!({"text": ANTIGRAVITY_SYSTEM_INSTRUCTION}),
            serde_json::json!({"text": "Your name is Henri."}),
        ];

        for part in crate::prompts::system_prompt_with_services(Some(&self.services)) {
            system_parts.push(serde_json::json!({"text": part}));
        }

        // Generate session ID: 32-char hex string (UUID without hyphens)
        let session_id = uuid::Uuid::new_v4().simple().to_string();

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

        let (base_model, thinking_level) = model_utils::split_model(&self.model);

        if base_model.starts_with("claude-") {
            let budget = match thinking_level {
                Some("low") => 4000,
                Some("medium") => 16000,
                Some("high") => 32000,
                Some("xhigh") => 48000,
                _ => 0,
            };
            if budget > 0 {
                request["generationConfig"]["thinkingConfig"] = serde_json::json!({
                    "includeThoughts": true,
                    "thinkingBudget": budget,
                });
            }
        } else if base_model.starts_with("gemini-")
            && let Some(level) = thinking_level
        {
            request["generationConfig"]["thinkingConfig"] = serde_json::json!({
                "thinkingLevel": level,
                "includeThoughts": true
            });
        }

        request
    }

    fn generate_request_id() -> String {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let suffix: String = (0..9)
            .map(|_| {
                let idx = (rand::random::<u32>() % 36) as usize;
                if idx < 10 {
                    (b'0' + idx as u8) as char
                } else {
                    (b'a' + (idx - 10) as u8) as char
                }
            })
            .collect();
        format!("agent-{}-{}", timestamp, suffix)
    }

    /// Build the full Antigravity envelope wrapping the inner request
    fn build_antigravity_envelope(
        &self,
        inner_request: serde_json::Value,
        project_id: &str,
        request_id: String,
    ) -> serde_json::Value {
        // Use base model name without the #variant suffix for the API
        let api_model = model_utils::base_model_name(&self.model);
        serde_json::json!({
            "project": project_id,
            "userAgent": "antigravity",
            "requestId": request_id,
            "model": api_model,
            "request": inner_request,
            "requestType": "agent"
        })
    }

    async fn send_chat_request(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
        request_id: String,
    ) -> Result<ChatResponse> {
        let access_token = self.ensure_access_token().await?;
        let project_id = {
            let state = self.state.lock().await;
            state.project_id.clone().ok_or_else(|| {
                Error::Config(
                    "antigravity project_id not set. Enter `/provider` to re-authenticate."
                        .to_string(),
                )
            })?
        };

        // Build the inner Gemini request
        let inner_request = self.build_inner_request(&messages).await;

        // Wrap in Antigravity envelope
        let request = self.build_antigravity_envelope(inner_request, &project_id, request_id);

        // Record TX bytes
        let body_bytes = serde_json::to_vec(&request)?;

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
            "antigravity/1.15.8 windows/amd64".to_string(),
        );
        headers.insert(
            "X-Goog-Api-Client".to_string(),
            "google-cloud-sdk vscode_cloudshelleditor/0.1".to_string(),
        );
        headers.insert("Client-Metadata".to_string(), client_metadata.to_string());
        headers.insert(
            "anthropic-beta".to_string(),
            "interleaved-thinking-2025-05-14".to_string(),
        );

        let mut last_error = None;
        let mut final_response = None;

        for base_url in ANTIGRAVITY_ENDPOINTS {
            // Build URL - model is in request body, not URL path
            let url = base_url.to_string();

            // Create a fresh client for each request attempt
            let client = Client::new();
            let mut request_builder = client.post(&url);
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
        let mut streaming_start: Option<Instant> = None;
        // Track usage metadata - only record final values (API sends cumulative counts with each chunk)
        let mut final_prompt_tokens: Option<u64> = None;
        let mut final_output_tokens: Option<u64> = None;
        let mut final_cached_tokens: Option<u64> = None;
        let mut final_thought_tokens: Option<u64> = None;

        let mut sse = sse::SseStream::new(response.bytes_stream());

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
                                    if streaming_start.is_none() {
                                        streaming_start = Some(Instant::now());
                                    }
                                    current_thinking.push_str(text);
                                    thinking.emit(text);
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

                                if streaming_start.is_none() {
                                    streaming_start = Some(Instant::now());
                                }
                                current_text.push_str(text);
                                output::print_text(output, text);
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
                if let Some(cached) = usage_meta
                    .get("cachedContentTokenCount")
                    .and_then(|t| t.as_u64())
                {
                    final_cached_tokens = Some(cached);
                }
                if let Some(thoughts) = usage_meta
                    .get("thoughtsTokenCount")
                    .and_then(|t| t.as_u64())
                {
                    final_thought_tokens = Some(thoughts);
                }
            }
        }

        // Record final usage (only once, after streaming completes)
        let mut usage_input = None;
        let mut usage_output = None;
        let mut usage_cache = None;
        if let Some(input) = final_prompt_tokens {
            usage::antigravity().record_input(input);
            let limit = Self::context_limit(&self.model);
            output::emit_context_update(output, input, limit);
            usage_input = Some(input);
        }
        if let Some(cached) = final_cached_tokens {
            usage::antigravity().add_cache_read(cached);
            usage_cache = Some(cached);
        }
        if let Some(output_tokens) = final_output_tokens {
            let thought_tokens = final_thought_tokens.unwrap_or(0);
            let combined_output = output_tokens + thought_tokens;
            usage::antigravity().record_output(combined_output);
            usage_output = Some(combined_output);

            if let Some(start) = streaming_start {
                let duration = start.elapsed().as_secs_f64();
                if duration > 0.0 {
                    let turn_total = usage::antigravity().turn_total();
                    let tokens_per_sec = combined_output as f64 / duration;
                    output::emit_working_progress(output, turn_total, duration, tokens_per_sec);
                }
            }
        }
        if usage_input.is_some() || usage_output.is_some() || usage_cache.is_some() {
            output::emit_usage_update(
                output,
                usage_input.unwrap_or(0),
                usage_output.unwrap_or(0),
                usage_cache.unwrap_or(0),
                0,
            );
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

        // Only end the text block if we actually streamed any text.
        if content_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text { .. }))
        {
            output::print_text_end(output);
        }

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
        let mut attempts = 0;
        let mut delay = INTERNAL_INITIAL_DELAY;

        loop {
            let request_id = Self::generate_request_id();
            let result = self
                .send_chat_request(messages.clone(), output, request_id)
                .await;

            match result {
                Ok(response) => return Ok(response),
                Err(Error::Unauthorized(_)) => {
                    self.force_refresh().await?;
                    let request_id = Self::generate_request_id();
                    return self.send_chat_request(messages, output, request_id).await;
                }
                Err(ref e) if e.is_retryable() && attempts < INTERNAL_MAX_RETRIES => {
                    attempts += 1;
                    output::emit_warning(
                        output,
                        &format!(
                            "{} (retrying in {}ms, attempt {}/{})",
                            e.display_message(),
                            delay.as_millis(),
                            attempts,
                            INTERNAL_MAX_RETRIES
                        ),
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn prepare_request(&self, messages: Vec<Message>) -> Result<serde_json::Value> {
        let project_id = {
            let state = self.state.lock().await;
            state.project_id.clone().ok_or_else(|| {
                Error::Config(
                    "antigravity project_id not set. Enter `/provider` to re-authenticate."
                        .to_string(),
                )
            })?
        };
        let inner_request = self.build_inner_request(&messages).await;
        let request_id = Self::generate_request_id();
        let request = self.build_antigravity_envelope(inner_request, &project_id, request_id);
        Ok(request)
    }

    fn start_turn(&self) {
        crate::usage::antigravity().start_turn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_variants() {
        // gemini-3-flash has 4 variants (low to high)
        let variants = AntigravityProvider::model_variants("gemini-3-flash#medium");
        assert_eq!(variants, vec!["minimal", "low", "medium", "high"]);

        // gemini-3-pro-high has 2 variants
        let variants = AntigravityProvider::model_variants("gemini-3-pro-high#high");
        assert_eq!(variants, vec!["low", "high"]);

        // claude models have 5 variants
        let variants = AntigravityProvider::model_variants("claude-sonnet-4-5-thinking#medium");
        assert_eq!(variants, vec!["off", "low", "medium", "high", "xhigh"]);
    }

    #[test]
    fn test_cycle_model_variant() {
        // Cycle through gemini-3-flash variants (low to high, then wrap)
        assert_eq!(
            AntigravityProvider::cycle_model_variant("gemini-3-flash#minimal"),
            "gemini-3-flash#low"
        );
        assert_eq!(
            AntigravityProvider::cycle_model_variant("gemini-3-flash#low"),
            "gemini-3-flash#medium"
        );
        assert_eq!(
            AntigravityProvider::cycle_model_variant("gemini-3-flash#medium"),
            "gemini-3-flash#high"
        );
        assert_eq!(
            AntigravityProvider::cycle_model_variant("gemini-3-flash#high"),
            "gemini-3-flash#minimal" // wraps around
        );

        // Cycle through claude-opus-4-6-thinking variants
        assert_eq!(
            AntigravityProvider::cycle_model_variant("claude-opus-4-6-thinking#off"),
            "claude-opus-4-6-thinking#low"
        );
        assert_eq!(
            AntigravityProvider::cycle_model_variant("claude-opus-4-6-thinking#low"),
            "claude-opus-4-6-thinking#medium"
        );
        assert_eq!(
            AntigravityProvider::cycle_model_variant("claude-opus-4-6-thinking#medium"),
            "claude-opus-4-6-thinking#high"
        );
        assert_eq!(
            AntigravityProvider::cycle_model_variant("claude-opus-4-6-thinking#high"),
            "claude-opus-4-6-thinking#xhigh"
        );
        assert_eq!(
            AntigravityProvider::cycle_model_variant("claude-opus-4-6-thinking#xhigh"),
            "claude-opus-4-6-thinking#off" // wraps around
        );
    }

    #[test]
    fn test_cycle_model_variant_unknown() {
        // Unknown model returns unchanged
        assert_eq!(
            AntigravityProvider::cycle_model_variant("unknown-model"),
            "unknown-model"
        );
    }

    #[test]
    fn test_cycle_model_variant_bare_model() {
        // Bare model names (without variant) should be treated as #medium and cycle to #high
        assert_eq!(
            AntigravityProvider::cycle_model_variant("claude-opus-4-6-thinking"),
            "claude-opus-4-6-thinking#high"
        );
        assert_eq!(
            AntigravityProvider::cycle_model_variant("claude-sonnet-4-5-thinking"),
            "claude-sonnet-4-5-thinking#high"
        );
        // gemini-3-flash defaults to #medium which cycles to #high
        assert_eq!(
            AntigravityProvider::cycle_model_variant("gemini-3-flash"),
            "gemini-3-flash#high"
        );
    }

    #[test]
    fn test_thinking_level_from_model() {
        assert_eq!(
            thinking_level_from_model("gemini-3-flash#high"),
            Some("high")
        );
        assert_eq!(
            thinking_level_from_model("claude-sonnet-4-5-thinking#off"),
            Some("off")
        );
        assert_eq!(thinking_level_from_model("gemini-3-flash"), None);
    }
}
