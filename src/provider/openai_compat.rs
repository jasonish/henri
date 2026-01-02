// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! OpenAI-compatible API provider implementation.
//!
//! This module contains both the OpenAiCompatProvider and shared chat logic
//! that can be reused by other providers following the OpenAI API format
//! (e.g., OpenRouterProvider).

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures::StreamExt;
use reqwest::Client;
use reqwest::header::HeaderMap;
use serde::Serialize;

use crate::config::ConfigFile;
use crate::error::{Error, Result};
use crate::output;
use crate::prompts;
use crate::provider::{
    ChatResponse, ContentBlock, Message, MessageContent, Provider, Role, StopReason, ToolCall,
};
use crate::tools;
use crate::usage;

const MAX_CONTINUATION_DEPTH: u8 = 255;

/// Configuration for an OpenAI-compatible chat client.
pub(crate) struct OpenAiChatConfig {
    pub provider_name: String,
    pub client: Client,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub usage_tracker: &'static usage::Usage,
    pub custom_headers: Option<HeaderMap>,
}

/// Trait for accessing model-specific configuration.
pub trait ModelConfigProvider: Send + Sync {
    fn get_model_config(&self, model_name: &str) -> Option<&crate::config::ModelConfig>;
}

#[derive(Serialize)]
pub(crate) struct OpenAiRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
}

#[derive(Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OpenAiFunction,
}

#[derive(Serialize)]
struct OpenAiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct OpenAiChunk {
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(serde::Deserialize)]
struct OpenAiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(serde::Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

#[derive(serde::Deserialize)]
struct OpenAiChoice {
    delta: OpenAiDelta,
    finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct OpenAiDelta {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAiToolCallDelta>>,
    reasoning: Option<String>,
    reasoning_content: Option<String>,
}

#[derive(serde::Deserialize)]
struct OpenAiToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    function: Option<OpenAiFunctionDelta>,
}

#[derive(serde::Deserialize)]
struct OpenAiFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Execute a chat request using the OpenAI-compatible API format.
pub async fn execute_chat(
    config: &OpenAiChatConfig,
    model_config: &dyn ModelConfigProvider,
    messages: &[Message],
    output: &crate::output::OutputContext,
    reasoning_effort_override: Option<&str>,
) -> Result<ChatResponse> {
    execute_chat_inner(
        config,
        model_config,
        messages,
        0,
        output,
        reasoning_effort_override,
    )
    .await
}

async fn execute_chat_inner(
    config: &OpenAiChatConfig,
    model_config: &dyn ModelConfigProvider,
    messages: &[Message],
    continuation_depth: u8,
    output: &crate::output::OutputContext,
    reasoning_effort_override: Option<&str>,
) -> Result<ChatResponse> {
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let request = build_request(config, model_config, messages, reasoning_effort_override).await?;

    let mut req_headers = HashMap::new();
    req_headers.insert(
        "Authorization".to_string(),
        format!("Bearer {}", config.api_key),
    );
    req_headers.insert("Content-Type".to_string(), "application/json".to_string());

    let mut builder = config
        .client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json");

    // Add custom headers if provided
    if let Some(ref custom_headers) = config.custom_headers {
        for (k, v) in custom_headers.iter() {
            req_headers.insert(k.to_string(), v.to_str().unwrap_or("<binary>").to_string());
        }
        builder = builder.headers(custom_headers.clone());
    }

    // Record TX bytes
    let body_bytes = serde_json::to_vec(&request)?;
    crate::usage::network_stats().record_tx(body_bytes.len() as u64);

    let response = builder.body(body_bytes).send().await.map_err(|e| {
        Error::Other(format!(
            "Failed to connect to {} ({}): {}",
            config.provider_name, url, e
        ))
    })?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let message = response.text().await.unwrap_or_default();
        return Err(Error::Api { status, message });
    }

    let mut full_text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut stop_reason = StopReason::Unknown;
    let mut pending_tools: HashMap<usize, PendingToolCall> = HashMap::new();
    let mut thinking = output::ThinkingState::new(output);
    let streaming_start = std::time::Instant::now();
    let mut char_count = 0usize;
    let mut received_reasoning = false;
    let mut usage_recorded = false;
    let mut raw_events: Vec<serde_json::Value> = Vec::new();

    let resp_headers = crate::provider::transaction_log::header_map_to_hash_map(response.headers());

    let mut sse = crate::sse::SseStream::new(response.bytes_stream().map(|chunk| {
        if let Ok(ref bytes) = chunk {
            crate::usage::network_stats().record_rx(bytes.len() as u64);
        }
        chunk
    }));

    while let Some(result) = sse.next_event().await {
        let data = result.map_err(Error::Http)?;
        raw_events.push(serde_json::from_str(&data).unwrap_or(serde_json::json!({ "raw": data })));

        // Try structured parsing first
        let chunk = match serde_json::from_str::<OpenAiChunk>(&data) {
            Ok(c) => c,
            Err(_) => {
                // If structured parsing fails, try flexible JSON parsing
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some(choices) = event.get("choices").and_then(|c| c.as_array())
                {
                    for choice in choices {
                        // Check delta.reasoning_content
                        if let Some(reasoning) = choice
                            .get("delta")
                            .and_then(|d| d.get("reasoning_content"))
                            .and_then(|r| r.as_str())
                        {
                            if !reasoning.is_empty() {
                                thinking.emit(reasoning);
                                received_reasoning = true;
                            }
                        }
                        // Check delta.reasoning
                        else if let Some(reasoning) = choice
                            .get("delta")
                            .and_then(|d| d.get("reasoning"))
                            .and_then(|r| r.as_str())
                            && !reasoning.is_empty()
                        {
                            thinking.emit(reasoning);
                            received_reasoning = true;
                        }
                    }
                }
                continue;
            }
        };

        for choice in chunk.choices {
            // Handle reasoning/thinking tokens - check both field names
            if let Some(reasoning) = choice
                .delta
                .reasoning_content
                .as_ref()
                .or(choice.delta.reasoning.as_ref())
                && !reasoning.is_empty()
            {
                thinking.emit(reasoning);
                received_reasoning = true;
            }

            if let Some(content) = &choice.delta.content
                && !content.is_empty()
            {
                thinking.end();
                output::print_text(output, content);
                full_text.push_str(content);
                char_count += content.chars().count();

                // Emit progress every ~50 chars (roughly every 12-13 tokens)
                if char_count.is_multiple_of(50) {
                    let duration = streaming_start.elapsed().as_secs_f64();
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
            }

            if let Some(tool_call_deltas) = &choice.delta.tool_calls {
                for tc in tool_call_deltas {
                    let index = tc.index.unwrap_or(0);
                    let pending = pending_tools.entry(index).or_default();

                    if let Some(id) = &tc.id
                        && !id.is_empty()
                    {
                        pending.id = id.clone();
                    }
                    if let Some(func) = &tc.function {
                        if let Some(name) = &func.name
                            && !name.is_empty()
                        {
                            pending.name = name.clone();
                        }
                        if let Some(args) = &func.arguments {
                            pending.arguments.push_str(args);
                        }
                    }
                }
            }

            if let Some(reason) = &choice.finish_reason {
                stop_reason = match reason.as_str() {
                    "stop" => StopReason::EndTurn,
                    "tool_calls" => StopReason::ToolUse,
                    "length" => StopReason::MaxTokens,
                    _ => StopReason::Unknown,
                };
            }
        }

        // Capture usage information (only once, from the final chunk that contains it)
        if !usage_recorded && let Some(usage_data) = &chunk.usage {
            if let Some(prompt_tokens) = usage_data.prompt_tokens {
                config.usage_tracker.record_input(prompt_tokens);
            }
            if let Some(completion_tokens) = usage_data.completion_tokens {
                config.usage_tracker.record_output(completion_tokens);

                // Emit progress with turn total (accumulated across all API calls)
                let duration = streaming_start.elapsed().as_secs_f64();
                if duration > 0.0 {
                    let turn_total = config.usage_tracker.turn_total();
                    let tokens_per_sec = completion_tokens as f64 / duration;
                    output::emit_working_progress(output, turn_total, duration, tokens_per_sec);
                }
            }
            // Track cached tokens if available
            if let Some(details) = &usage_data.prompt_tokens_details
                && let Some(cached) = details.cached_tokens
            {
                config.usage_tracker.add_cache_read(cached);
            }
            usage_recorded = true;
        }
    }

    thinking.end();
    output::print_text_end(output);

    if crate::provider::transaction_log::is_active() {
        crate::provider::transaction_log::log(
            &url,
            req_headers,
            serde_json::to_value(&request).unwrap_or(serde_json::json!({})),
            resp_headers,
            serde_json::Value::Array(raw_events),
        );
    }

    // OpenAI reasoning models stream thinking content separately and expect
    // an empty assistant message to continue and produce the final response.
    if stop_reason == StopReason::Unknown
        && received_reasoning
        && full_text.is_empty()
        && pending_tools.is_empty()
        && continuation_depth < MAX_CONTINUATION_DEPTH
    {
        let mut extended_messages = messages.to_vec();
        extended_messages.push(Message {
            role: Role::Assistant,
            content: MessageContent::Text(String::new()),
        });

        return Box::pin(execute_chat_inner(
            config,
            model_config,
            &extended_messages,
            continuation_depth + 1,
            output,
            reasoning_effort_override,
        ))
        .await;
    }

    // Add text block first (if present) to maintain correct order
    if !full_text.is_empty() {
        content_blocks.push(ContentBlock::Text {
            text: full_text.clone(),
        });
    }

    // Then add tool use blocks (if any)
    for (_index, pending) in pending_tools {
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
    }

    Ok(ChatResponse {
        tool_calls,
        content_blocks,
        stop_reason,
    })
}

/// Build an OpenAI-compatible request payload.
pub(crate) async fn build_request(
    config: &OpenAiChatConfig,
    model_config: &dyn ModelConfigProvider,
    messages: &[Message],
    reasoning_effort_override: Option<&str>,
) -> Result<OpenAiRequest> {
    let mut all_messages = vec![Message::system(prompts::system_prompt().join("\n\n"))];
    all_messages.extend(messages.iter().cloned());

    let tools: Vec<OpenAiTool> = tools::all_definitions()
        .await
        .into_iter()
        .map(|t| OpenAiTool {
            kind: "function",
            function: OpenAiFunction {
                name: t.name,
                description: t.description,
                parameters: t.input_schema,
            },
        })
        .collect();

    // Get model-specific config if available
    let model_params = model_config.get_model_config(&config.model);

    // Use override if provided, otherwise fall back to model config
    let reasoning_effort = reasoning_effort_override
        .map(|s| s.to_string())
        .or_else(|| model_params.and_then(|c| c.reasoning_effort.clone()));

    Ok(OpenAiRequest {
        model: config.model.clone(),
        messages: build_messages(&all_messages),
        stream: true,
        tools,
        temperature: model_params.and_then(|c| c.temperature),
        max_tokens: model_params.and_then(|c| c.max_tokens),
        stop: model_params.and_then(|c| c.stop_sequences.clone()),
        reasoning_effort,
    })
}

/// Convert provider Message structs to OpenAI API format.
fn build_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .flat_map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };

            match &m.content {
                MessageContent::Text(text) => {
                    vec![serde_json::json!({"role": role, "content": text})]
                }
                MessageContent::Blocks(blocks) => {
                    let mut tool_calls = Vec::new();
                    let mut tool_results = Vec::new();
                    let mut content_parts = Vec::new();

                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                content_parts.push(serde_json::json!({
                                    "type": "text",
                                    "text": text
                                }));
                            }
                            ContentBlock::Image { mime_type, data } => {
                                let base64_data = STANDARD.encode(data);
                                content_parts.push(serde_json::json!({
                                    "type": "image_url",
                                    "image_url": {
                                        "url": format!("data:{};base64,{}", mime_type, base64_data)
                                    }
                                }));
                            }
                            ContentBlock::Thinking { .. } => {}
                            ContentBlock::ToolUse { id, name, input, .. } => {
                                tool_calls.push(serde_json::json!({
                                    "id": id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": serde_json::to_string(input).unwrap_or_default()
                                    }
                                }));
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                ..
                            } => {
                                tool_results.push((tool_use_id.clone(), content.clone()));
                            }
                            ContentBlock::Summary {
                                summary,
                                messages_compacted,
                            } => {
                                content_parts.push(serde_json::json!({
                                    "type": "text",
                                    "text": format!("[Summary of {} previous messages]\n\n{}", messages_compacted, summary)
                                }));
                            }
                        }
                    }

                    // Tool results: each becomes a separate message
                    if !tool_results.is_empty() {
                        return tool_results
                            .into_iter()
                            .map(|(id, content)| {
                                serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": content
                                })
                            })
                            .collect();
                    }

                    if !tool_calls.is_empty() {
                        let content = if content_parts.is_empty() {
                            serde_json::Value::Null
                        } else if content_parts.len() == 1 {
                            if let Some(text) = content_parts[0].get("text") {
                                text.clone()
                            } else {
                                serde_json::json!(content_parts)
                            }
                        } else {
                            serde_json::json!(content_parts)
                        };
                        return vec![serde_json::json!({
                            "role": role,
                            "content": content,
                            "tool_calls": tool_calls
                        })];
                    }

                    // If we have mixed content (text + images), use array format
                    if content_parts.len() > 1 {
                        vec![serde_json::json!({
                            "role": role,
                            "content": content_parts
                        })]
                    } else if content_parts.len() == 1 {
                        let content = if let Some(text) = content_parts[0].get("text") {
                            text.clone()
                        } else {
                            serde_json::json!(content_parts)
                        };
                        vec![serde_json::json!({
                            "role": role,
                            "content": content
                        })]
                    } else {
                        vec![serde_json::json!({
                            "role": role,
                            "content": ""
                        })]
                    }
                }
            }
        })
        .collect()
}

pub(crate) struct OpenAiCompatProvider {
    config: OpenAiChatConfig,
    provider_config: crate::config::OpenAiCompatProviderConfig,
    /// Dynamic override for reasoning_effort (takes precedence over model config)
    reasoning_effort: Option<String>,
}

impl OpenAiCompatProvider {
    pub(crate) fn try_new(provider_name: &str) -> Result<Self> {
        let config = ConfigFile::load()?;
        let openai_compat = config
            .get_provider(provider_name)
            .and_then(|p| p.as_openai_compat())
            .ok_or_else(|| {
                Error::Auth(format!(
                    "OpenAI Compatible provider '{}' not configured.",
                    provider_name
                ))
            })?;

        if !openai_compat.enabled {
            return Err(Error::Auth(format!(
                "OpenAI Compatible provider '{}' is disabled.",
                provider_name
            )));
        }

        if openai_compat.api_key.is_empty() {
            return Err(Error::Auth(format!(
                "OpenAI Compatible provider '{}' API key is not set.",
                provider_name
            )));
        }

        if openai_compat.base_url.is_empty() {
            return Err(Error::Auth(format!(
                "OpenAI Compatible provider '{}' base URL is not set.",
                provider_name
            )));
        }

        // Convert to OpenAiCompatProviderConfig for internal use
        let provider_config = crate::config::OpenAiCompatProviderConfig {
            enabled: openai_compat.enabled,
            api_key: openai_compat.api_key.clone(),
            base_url: openai_compat.base_url.clone(),
            models: openai_compat.models.clone(),
            model_configs: openai_compat.model_configs.clone(),
        };

        let chat_config = OpenAiChatConfig {
            provider_name: provider_name.to_string(),
            client: Client::new(),
            api_key: openai_compat.api_key.clone(),
            base_url: openai_compat.base_url.clone(),
            model: "default".to_string(),
            usage_tracker: usage::openai_compat(),
            custom_headers: None,
        };

        Ok(Self {
            config: chat_config,
            provider_config,
            reasoning_effort: None,
        })
    }

    pub(crate) fn with_config(
        provider_name: &str,
        config: crate::config::OpenAiCompatProviderConfig,
        usage_tracker: &'static usage::Usage,
    ) -> Self {
        let chat_config = OpenAiChatConfig {
            provider_name: provider_name.to_string(),
            client: Client::new(),
            api_key: config.api_key.clone(),
            base_url: config.base_url.clone(),
            model: "default".to_string(),
            usage_tracker,
            custom_headers: None,
        };

        Self {
            config: chat_config,
            provider_config: config,
            reasoning_effort: None,
        }
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.config.model = model;
    }

    pub(crate) fn set_reasoning_effort(&mut self, effort: Option<String>) {
        self.reasoning_effort = effort;
    }

    /// Get context limit for a given model name
    /// Returns None since we don't know limits of arbitrary OpenAI-compatible providers
    pub(crate) fn context_limit(_model: &str) -> Option<u64> {
        None
    }
}

impl Provider for OpenAiCompatProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        execute_chat(
            &self.config,
            &self.provider_config,
            &messages,
            output,
            self.reasoning_effort.as_deref(),
        )
        .await
    }

    async fn prepare_request(&self, messages: Vec<Message>) -> Result<serde_json::Value> {
        let request = build_request(
            &self.config,
            &self.provider_config,
            &messages,
            self.reasoning_effort.as_deref(),
        )
        .await?;
        Ok(serde_json::to_value(&request)?)
    }

    fn start_turn(&self) {
        crate::usage::openai_compat().start_turn();
    }
}

// Implement ModelConfigProvider for OpenAiCompatProviderConfig
impl ModelConfigProvider for crate::config::OpenAiCompatProviderConfig {
    fn get_model_config(&self, model_name: &str) -> Option<&crate::config::ModelConfig> {
        self.get_model_config(model_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_messages_simple_text() {
        let messages = vec![Message::system("Hello")];
        let result = build_messages(&messages);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "system");
        assert_eq!(result[0]["content"], "Hello");
    }

    #[test]
    fn test_build_messages_user_assistant() {
        let messages = vec![Message::user("test")];
        let result = build_messages(&messages);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "user");
    }
}
