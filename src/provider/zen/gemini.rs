// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::time::Instant;

use base64::Engine;
use futures::StreamExt;
use reqwest::header::HeaderMap;
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

use super::{ChatContext, ZEN_BASE_URL, strip_unsupported_schema_fields};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GeminiRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiSystemInstruction>,
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<GeminiToolDeclaration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    candidate_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking_config: Option<GeminiThinkingConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiThinkingConfig {
    thinking_level: String,
}

#[derive(Serialize)]
struct GeminiToolDeclaration {
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Serialize, Clone)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Serialize, Clone)]
struct GeminiSystemInstruction {
    parts: Vec<GeminiPart>,
}

#[derive(Serialize, Clone)]
#[serde(untagged)]
enum GeminiPart {
    Text {
        text: String,
    },
    InlineData {
        #[serde(rename = "inlineData")]
        inline_data: GeminiInlineData,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
        #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: GeminiFunctionResponse,
    },
}

#[derive(Serialize, Deserialize, Clone)]
struct GeminiFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(Serialize, Clone)]
struct GeminiInlineData {
    mime_type: String,
    data: String,
}

#[derive(Serialize, Clone)]
struct GeminiFunctionResponse {
    name: String,
    response: serde_json::Value,
}

#[derive(Deserialize)]
struct GeminiResponse {
    #[serde(default)]
    candidates: Vec<GeminiCandidate>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Deserialize)]
struct GeminiUsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u64>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u64>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiResponseContent,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct GeminiResponseContent {
    parts: Vec<GeminiResponsePart>,
}

#[derive(Deserialize)]
struct GeminiResponsePart {
    #[serde(default)]
    text: String,
    #[serde(default)]
    thought: bool,
    #[serde(rename = "thoughtSignature")]
    thought_signature: Option<String>,
    #[serde(rename = "functionCall")]
    function_call: Option<GeminiFunctionCall>,
}

fn build_contents(messages: &[Message]) -> Vec<GeminiContent> {
    messages
        .iter()
        .filter(|m| !matches!(m.role, Role::System))
        .map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "model",
                Role::System => "user",
            };

            let parts = match &m.content {
                MessageContent::Text(text) => {
                    vec![GeminiPart::Text { text: text.clone() }]
                }
                MessageContent::Blocks(blocks) => {
                    let mut parts = Vec::new();
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                parts.push(GeminiPart::Text { text: text.clone() });
                            }
                            ContentBlock::Image { mime_type, data } => {
                                let base64_data =
                                    base64::engine::general_purpose::STANDARD.encode(data);
                                parts.push(GeminiPart::InlineData {
                                    inline_data: GeminiInlineData {
                                        mime_type: mime_type.clone(),
                                        data: base64_data,
                                    },
                                });
                            }
                            ContentBlock::Thinking { .. } => {}
                            ContentBlock::ToolUse {
                                name,
                                input,
                                thought_signature,
                                ..
                            } => {
                                parts.push(GeminiPart::FunctionCall {
                                    function_call: GeminiFunctionCall {
                                        name: name.clone(),
                                        args: input.clone(),
                                    },
                                    thought_signature: thought_signature.clone(),
                                });
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                ..
                            } => {
                                parts.push(GeminiPart::FunctionResponse {
                                    function_response: GeminiFunctionResponse {
                                        name: tool_use_id.clone(),
                                        response: serde_json::json!({"result": content}),
                                    },
                                });
                            }
                            ContentBlock::Summary {
                                summary,
                                messages_compacted,
                            } => {
                                parts.push(GeminiPart::Text {
                                    text: format!(
                                        "[Summary of {} previous messages]\n\n{}",
                                        messages_compacted, summary
                                    ),
                                });
                            }
                        }
                    }
                    parts
                }
            };

            GeminiContent {
                role: role.to_string(),
                parts,
            }
        })
        .collect()
}

pub(super) async fn build_request(
    _model: &str,
    messages: &[Message],
    _thinking_mode: Option<&str>,
    services: &Services,
) -> GeminiRequest {
    let tools: Vec<GeminiToolDeclaration> = vec![GeminiToolDeclaration {
        function_declarations: tools::all_definitions(services)
            .await
            .into_iter()
            .map(|t| GeminiFunctionDeclaration {
                name: t.name,
                description: t.description,
                parameters: strip_unsupported_schema_fields(t.input_schema),
            })
            .collect(),
    }];

    let generation_config = Some(GeminiGenerationConfig {
        max_output_tokens: Some(8192),
        candidate_count: Some(1),
        thinking_config: None,
    });

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let now = chrono::Local::now();
    let datetime_str = format!(
        "Current date and time: {}",
        now.format("%Y-%m-%d %H:%M:%S %Z")
    );

    GeminiRequest {
        system_instruction: Some(GeminiSystemInstruction {
            parts: vec![
                GeminiPart::Text {
                    text: prompts::default_system_prompt().to_string(),
                },
                GeminiPart::Text { text: datetime_str },
                GeminiPart::Text {
                    text: format!("Current working directory: {}", cwd),
                },
                GeminiPart::Text {
                    text: "You support images, do not use external tools to view images."
                        .to_string(),
                },
            ],
        }),
        contents: build_contents(messages),
        tools,
        generation_config,
    }
}

pub(super) async fn chat(
    ctx: &ChatContext<'_>,
    messages: Vec<Message>,
    output: &crate::output::OutputContext,
) -> Result<ChatResponse> {
    let url = format!(
        "{}/models/{}:streamGenerateContent?alt=sse",
        ZEN_BASE_URL, ctx.model
    );
    let request = build_request(ctx.model, &messages, ctx.thinking_mode, ctx.services).await;

    // Record TX bytes
    let body_bytes = serde_json::to_vec(&request)?;
    usage::network_stats().record_tx(body_bytes.len() as u64);

    let mut headers = HeaderMap::new();
    headers.insert(
        "x-goog-api-key",
        ctx.api_key.parse().expect("API key should be valid header"),
    );
    headers.insert(
        "content-type",
        "application/json".parse().expect("static header value"),
    );

    let mut req_headers = std::collections::HashMap::new();
    req_headers.insert("x-goog-api-key".to_string(), ctx.api_key.to_string());
    req_headers.insert("Content-Type".to_string(), "application/json".to_string());

    let response = ctx
        .client
        .post(&url)
        .headers(headers)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            Error::Other(format!(
                "Failed to connect to Zen (Gemini API - {}) ({}): {}",
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
    let mut thinking = output::ThinkingState::new(output);
    let mut streaming_start: Option<Instant> = None;
    let mut char_count = 0usize;
    let mut last_progress_update = Instant::now();
    let mut has_seen_usage = false;

    let mut sse = sse::SseStream::new(response.bytes_stream().map(|chunk| {
        if let Ok(ref bytes) = chunk {
            usage::network_stats().record_rx(bytes.len() as u64);
        }
        chunk
    }));
    while let Some(result) = sse.next_event().await {
        let data = result.map_err(Error::Http)?;

        let Ok(resp) = serde_json::from_str::<GeminiResponse>(&data) else {
            continue;
        };

        for candidate in resp.candidates {
            for part in candidate.content.parts {
                if part.thought && !part.text.is_empty() {
                    if streaming_start.is_none() {
                        streaming_start = Some(Instant::now());
                        last_progress_update = Instant::now();
                    }
                    char_count += part.text.len();
                    thinking.emit(&part.text);

                    // Emit progress update every 0.5 seconds
                    if !has_seen_usage && last_progress_update.elapsed().as_secs_f64() >= 0.5 {
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
                } else if part.thought_signature.is_none() && !part.text.is_empty() {
                    if streaming_start.is_none() {
                        streaming_start = Some(Instant::now());
                        last_progress_update = Instant::now();
                    }
                    thinking.end();
                    char_count += part.text.len();
                    output::print_text(output, &part.text);
                    full_text.push_str(&part.text);

                    // Emit progress update every 0.5 seconds
                    if !has_seen_usage && last_progress_update.elapsed().as_secs_f64() >= 0.5 {
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

                if let Some(fc) = &part.function_call {
                    let id = format!("call_{}", tool_calls.len());
                    tool_calls.push(ToolCall {
                        id: id.clone(),
                        name: fc.name.clone(),
                        input: fc.args.clone(),
                        thought_signature: None,
                    });

                    content_blocks.push(ContentBlock::ToolUse {
                        id,
                        name: fc.name.clone(),
                        input: fc.args.clone(),
                        thought_signature: part.thought_signature.clone(),
                    });
                }
            }

            if let Some(reason) = &candidate.finish_reason {
                stop_reason = match reason.as_str() {
                    "STOP" => StopReason::EndTurn,
                    "MAX_TOKENS" => StopReason::MaxTokens,
                    _ => {
                        if !tool_calls.is_empty() {
                            StopReason::ToolUse
                        } else {
                            StopReason::Unknown
                        }
                    }
                };
            }
        }

        // Handle usage metadata
        if let Some(u) = &resp.usage_metadata {
            if let Some(prompt_tokens) = u.prompt_token_count {
                usage::zen().record_input(prompt_tokens);
            }
            if let Some(completion_tokens) = u.candidates_token_count {
                usage::zen().record_output(completion_tokens);

                has_seen_usage = true;
                if let Some(start) = streaming_start {
                    let duration = start.elapsed().as_secs_f64();
                    if duration > 0.0 {
                        let turn_total = usage::zen().turn_total();
                        let tokens_per_sec = completion_tokens as f64 / duration;
                        output::emit_working_progress(output, turn_total, duration, tokens_per_sec);
                    }
                }
            }
        }
    }

    // Only end the text block if we actually streamed any text.
    if !full_text.is_empty() {
        output::print_text_end(output);
    }

    if !full_text.is_empty() {
        content_blocks.insert(
            0,
            ContentBlock::Text {
                text: full_text.clone(),
            },
        );
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

pub(super) fn prepare_request_value(request: &GeminiRequest) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(request)?)
}
