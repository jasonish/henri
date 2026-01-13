// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use base64::Engine;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::output;
use crate::prompts;
use crate::provider::{
    ChatResponse, ContentBlock, Message, MessageContent, Role, StopReason, ToolCall,
};
use crate::services::Services;
use crate::sse;
use crate::tools;
use crate::usage;

use super::{ChatContext, ZEN_BASE_URL, get_model_spec};

#[derive(Serialize)]
pub(super) struct AnthropicRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<serde_json::Value>,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<AnthropicThinkingConfig>,
}

#[derive(Serialize)]
struct AnthropicThinkingConfig {
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

#[derive(Deserialize)]
struct AnthropicEvent {
    #[serde(rename = "type")]
    event_type: String,
    delta: Option<AnthropicDelta>,
    content_block: Option<AnthropicContentBlock>,
    message: Option<AnthropicMessage>,
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicMessage {
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct AnthropicDelta {
    text: Option<String>,
    thinking: Option<String>,
    signature: Option<String>,
    partial_json: Option<String>,
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Default)]
struct PendingToolUse {
    id: String,
    name: String,
    input_json: String,
}

#[derive(Debug, Default)]
struct PendingThinking {
    text: String,
    signature: String,
}

#[derive(Debug)]
enum PendingBlock {
    Thinking(PendingThinking),
    ToolUse(PendingToolUse),
}

pub(super) fn build_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .filter(|m| !matches!(m.role, Role::System))
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
                                let base64_data =
                                    base64::engine::general_purpose::STANDARD.encode(data);
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
                            ContentBlock::ToolUse {
                                id, name, input, ..
                            } => serde_json::json!({
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": input
                            }),
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

            serde_json::json!({"role": role, "content": content})
        })
        .collect()
}

pub(super) async fn build_request(
    model: &str,
    messages: &[Message],
    thinking_enabled: bool,
    services: &Services,
) -> AnthropicRequest {
    let tools: Vec<AnthropicTool> = tools::all_definitions(services)
        .await
        .into_iter()
        .map(|t| AnthropicTool {
            name: t.name,
            description: t.description,
            input_schema: t.input_schema,
        })
        .collect();

    let model_supports_thinking = get_model_spec(model)
        .map(|m| m.supports_thinking)
        .unwrap_or(false);

    let thinking = if thinking_enabled && model_supports_thinking {
        Some(AnthropicThinkingConfig {
            kind: "enabled".to_string(),
            budget_tokens: 10000,
        })
    } else {
        None
    };

    AnthropicRequest {
        model: model.to_string(),
        system: Some(prompts::system_prompt_with_services(Some(services)).join("\n\n")),
        messages: build_messages(messages),
        max_tokens: 16000,
        stream: true,
        tools,
        thinking,
    }
}

pub(super) async fn chat(
    ctx: &ChatContext<'_>,
    messages: Vec<Message>,
    output: &crate::output::OutputContext,
) -> Result<ChatResponse> {
    let url = format!("{}/messages", ZEN_BASE_URL);
    let request = build_request(ctx.model, &messages, ctx.thinking_enabled, ctx.services).await;

    // Record TX bytes
    let body_bytes = serde_json::to_vec(&request)?;
    usage::network_stats().record_tx(body_bytes.len() as u64);

    let mut req_headers = std::collections::HashMap::new();
    req_headers.insert("x-api-key".to_string(), ctx.api_key.to_string());
    req_headers.insert("Content-Type".to_string(), "application/json".to_string());
    req_headers.insert("anthropic-version".to_string(), "2023-06-01".to_string());

    let response = ctx
        .client
        .post(&url)
        .header("x-api-key", ctx.api_key)
        .header("Content-Type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            Error::Other(format!(
                "Failed to connect to Zen (Anthropic API - {}) ({}): {}",
                ctx.model, url, e
            ))
        })?;

    let resp_headers = crate::provider::transaction_log::header_map_to_hash_map(response.headers());

    if !response.status().is_success() {
        let status = response.status().as_u16();
        let message = response.text().await.unwrap_or_default();

        crate::provider::transaction_log::log(
            &url,
            req_headers,
            serde_json::to_value(&request).unwrap_or_default(),
            resp_headers,
            serde_json::json!({
                "error": true,
                "status": status,
                "body": message
            }),
        );

        return Err(crate::provider::api_error(status, message));
    }

    let mut full_text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut content_blocks: Vec<ContentBlock> = Vec::new();
    let mut stop_reason = StopReason::Unknown;
    let mut pending_block: Option<PendingBlock> = None;
    let mut thinking = output::ThinkingState::new(output);

    let mut sse = sse::SseStream::new(response.bytes_stream().map(|chunk| {
        if let Ok(ref bytes) = chunk {
            usage::network_stats().record_rx(bytes.len() as u64);
        }
        chunk
    }));
    while let Some(result) = sse.next_event().await {
        let data = result.map_err(Error::Http)?;

        let Ok(event) = serde_json::from_str::<AnthropicEvent>(&data) else {
            continue;
        };

        match event.event_type.as_str() {
            "message_start" => {
                if let Some(msg) = &event.message
                    && let Some(u) = &msg.usage
                {
                    if let Some(input) = u.input_tokens {
                        usage::zen().record_input(input);
                    }
                    if let Some(cache_create) = u.cache_creation_input_tokens {
                        usage::zen().add_cache_creation(cache_create);
                    }
                    if let Some(cache_read) = u.cache_read_input_tokens {
                        usage::zen().add_cache_read(cache_read);
                    }
                }
            }
            "content_block_start" => {
                if let Some(block) = &event.content_block {
                    match block.kind.as_str() {
                        "thinking" => {
                            pending_block =
                                Some(PendingBlock::Thinking(PendingThinking::default()));
                        }
                        "tool_use" => {
                            pending_block = Some(PendingBlock::ToolUse(PendingToolUse {
                                id: block.id.clone().unwrap_or_default(),
                                name: block.name.clone().unwrap_or_default(),
                                input_json: String::new(),
                            }));
                        }
                        _ => {}
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = &event.delta {
                    if let Some(thinking_text) = &delta.thinking {
                        thinking.emit(thinking_text);
                        if let Some(PendingBlock::Thinking(ref mut pending)) = pending_block {
                            pending.text.push_str(thinking_text);
                        }
                    }
                    if let Some(sig) = &delta.signature
                        && let Some(PendingBlock::Thinking(ref mut pending)) = pending_block
                    {
                        pending.signature.push_str(sig);
                    }
                    if let Some(text) = &delta.text {
                        output::print_text(output, text);
                        full_text.push_str(text);
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
                            let input: serde_json::Value = serde_json::from_str(&tool.input_json)
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
                    usage::zen().record_output(output_tokens);
                }
            }
            _ => {}
        }
    }

    // Only end the text block if we actually streamed any text.
    if !full_text.is_empty() {
        output::print_text_end(output);
    }

    // Ensure correct block order: thinking blocks must come first if present
    if !full_text.is_empty() {
        let has_thinking = content_blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Thinking { .. }));

        if has_thinking {
            let insert_pos = content_blocks
                .iter()
                .rposition(|b| matches!(b, ContentBlock::Thinking { .. }))
                .map(|i| i + 1)
                .unwrap_or(0);

            content_blocks.insert(
                insert_pos,
                ContentBlock::Text {
                    text: full_text.clone(),
                },
            );
        } else {
            content_blocks.insert(
                0,
                ContentBlock::Text {
                    text: full_text.clone(),
                },
            );
        }
    }

    Ok(ChatResponse {
        tool_calls,
        content_blocks,
        stop_reason,
    })
}

pub(super) fn prepare_request_value(request: &AnthropicRequest) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(request)?)
}
