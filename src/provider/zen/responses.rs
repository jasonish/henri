// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::time::Instant;

use base64::Engine;
use futures::StreamExt;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::output;

use crate::provider::{
    ChatResponse, ContentBlock, Message, MessageContent, Role, StopReason, ToolCall,
};
use crate::services::Services;
use crate::tools;
use crate::usage;

use super::{ChatContext, ZEN_BASE_URL, get_model_spec, strip_unsupported_schema_fields};

#[derive(Serialize)]
pub(super) struct OpenAiResponsesRequest {
    model: String,
    input: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiResponsesTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct OpenAiResponsesTool {
    #[serde(rename = "type")]
    tool_type: String,
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// SSE stream parser for OpenAI Responses API (event: + data: format)
struct ResponsesSseStream<S> {
    stream: S,
    buffer: String,
}

impl<S> ResponsesSseStream<S> {
    fn new(stream: S) -> Self {
        Self {
            stream,
            buffer: String::new(),
        }
    }
}

impl<S, B, E> ResponsesSseStream<S>
where
    S: futures::Stream<Item = std::result::Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    async fn next_event(&mut self) -> Option<std::result::Result<(String, String), E>> {
        use futures::StreamExt;

        loop {
            // Look for complete event block (ends with \n\n)
            if let Some(end_pos) = self.buffer.find("\n\n") {
                let event_block = self.buffer[..end_pos].to_string();
                self.buffer = self.buffer[end_pos + 2..].to_string();

                let mut event_type = String::new();
                let mut data = String::new();

                for line in event_block.lines() {
                    if let Some(stripped) = line.strip_prefix("event: ") {
                        event_type = stripped.to_string();
                    } else if let Some(stripped) = line.strip_prefix("data: ") {
                        data = stripped.to_string();
                    }
                }

                if !data.is_empty() && data != "[DONE]" {
                    return Some(Ok((event_type, data)));
                }
                continue;
            }

            // Need more data
            match self.stream.next().await {
                Some(Ok(chunk)) => {
                    let chunk_str = String::from_utf8_lossy(chunk.as_ref());
                    self.buffer.push_str(&chunk_str);
                }
                Some(Err(e)) => return Some(Err(e)),
                None => return None,
            }
        }
    }
}

fn build_input(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut input = Vec::new();

    for m in messages {
        match m.role {
            Role::System => {
                if let MessageContent::Text(text) = &m.content {
                    input.push(serde_json::json!({
                        "role": "system",
                        "content": text
                    }));
                }
            }
            Role::User => match &m.content {
                MessageContent::Text(text) => {
                    input.push(serde_json::json!({
                        "role": "user",
                        "content": [{"type": "input_text", "text": text}]
                    }));
                }
                MessageContent::Blocks(blocks) => {
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                input.push(serde_json::json!({
                                    "role": "user",
                                    "content": [{"type": "input_text", "text": text}]
                                }));
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                ..
                            } => {
                                input.push(serde_json::json!({
                                    "type": "function_call_output",
                                    "call_id": tool_use_id,
                                    "output": content
                                }));
                            }
                            ContentBlock::Image { mime_type, data } => {
                                let base64_data =
                                    base64::engine::general_purpose::STANDARD.encode(data);
                                input.push(serde_json::json!({
                                    "role": "user",
                                    "content": [{
                                        "type": "input_image",
                                        "image_url": format!("data:{};base64,{}", mime_type, base64_data)
                                    }]
                                }));
                            }
                            _ => {}
                        }
                    }
                }
            },
            Role::Assistant => {
                if let MessageContent::Blocks(blocks) = &m.content {
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                if !text.is_empty() {
                                    input.push(serde_json::json!({
                                        "role": "assistant",
                                        "content": [{"type": "output_text", "text": text}]
                                    }));
                                }
                            }
                            ContentBlock::ToolUse {
                                id,
                                name,
                                input: args,
                                ..
                            } => {
                                let args_str = serde_json::to_string(args).unwrap_or_default();
                                input.push(serde_json::json!({
                                    "type": "function_call",
                                    "call_id": id,
                                    "name": name,
                                    "arguments": args_str
                                }));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    input
}

pub(super) async fn build_request(
    model: &str,
    messages: &[Message],
    thinking_enabled: bool,
    services: &Services,
) -> OpenAiResponsesRequest {
    let tools: Vec<OpenAiResponsesTool> = tools::all_definitions(services)
        .await
        .into_iter()
        .map(|t| OpenAiResponsesTool {
            tool_type: "function".to_string(),
            name: t.name,
            description: t.description,
            parameters: strip_unsupported_schema_fields(t.input_schema),
        })
        .collect();

    let input = build_input(messages);

    let model_supports_thinking = get_model_spec(model)
        .map(|m| m.supports_thinking)
        .unwrap_or(false);

    let reasoning = if thinking_enabled && model_supports_thinking {
        Some(serde_json::json!({
            "effort": "medium",
            "summary": "auto"
        }))
    } else {
        None
    };

    OpenAiResponsesRequest {
        model: model.to_string(),
        input,
        max_output_tokens: Some(16384),
        stream: true,
        tools,
        tool_choice: Some("auto".to_string()),
        reasoning,
    }
}

pub(super) async fn chat(
    ctx: &ChatContext<'_>,
    messages: Vec<Message>,
    output: &crate::output::OutputContext,
) -> Result<ChatResponse> {
    let url = format!("{}/responses", ZEN_BASE_URL);
    let request = build_request(ctx.model, &messages, ctx.thinking_enabled, ctx.services).await;

    // Record TX bytes
    let body_bytes = serde_json::to_vec(&request)?;
    usage::network_stats().record_tx(body_bytes.len() as u64);

    let mut req_headers = std::collections::HashMap::new();
    req_headers.insert(
        "Authorization".to_string(),
        format!("Bearer {}", ctx.api_key),
    );
    req_headers.insert("Content-Type".to_string(), "application/json".to_string());

    let response = ctx
        .client
        .post(&url)
        .header("Authorization", format!("Bearer {}", ctx.api_key))
        .header("Content-Type", "application/json")
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            Error::Other(format!(
                "Failed to connect to Zen (OpenAI Responses API) ({}): {}",
                url, e
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

    let mut tool_calls = Vec::new();
    let mut content_blocks = Vec::new();
    let mut stop_reason = StopReason::EndTurn;
    let mut full_text = String::new();
    let mut pending_tools: Vec<PendingToolCall> = Vec::new();
    let mut thinking = output::ThinkingState::new(output);
    let mut streaming_start: Option<Instant> = None;
    let mut char_count = 0usize;
    let mut last_progress_update = Instant::now();
    let mut has_seen_usage = false;

    // Use custom SSE parser for event: + data: format
    let mut sse = ResponsesSseStream::new(response.bytes_stream().map(|chunk| {
        if let Ok(ref bytes) = chunk {
            usage::network_stats().record_rx(bytes.len() as u64);
        }
        chunk
    }));
    while let Some(result) = sse.next_event().await {
        let (event_type, data) = result.map_err(Error::Http)?;

        match event_type.as_str() {
            // Handle reasoning summary events (various formats)
            "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_part.delta"
            | "response.reasoning.delta" => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some(delta) = json.get("delta").and_then(|d| d.as_str())
                {
                    if streaming_start.is_none() {
                        streaming_start = Some(Instant::now());
                        last_progress_update = Instant::now();
                    }
                    char_count += delta.len();
                    thinking.emit(delta);

                    // Emit progress update every 0.5 seconds
                    if !has_seen_usage && last_progress_update.elapsed().as_secs_f64() >= 0.5 {
                        if let Some(start) = streaming_start {
                            let duration = start.elapsed().as_secs_f64();
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
            }
            "response.reasoning_summary_part.added" => {
                // Check if the part contains text
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some(part) = json.get("part")
                    && let Some(text) = part.get("text").and_then(|t| t.as_str())
                    && !text.is_empty()
                {
                    if streaming_start.is_none() {
                        streaming_start = Some(Instant::now());
                        last_progress_update = Instant::now();
                    }
                    char_count += text.len();
                    thinking.emit(text);
                }
            }
            "response.reasoning_summary_text.done"
            | "response.reasoning_summary_part.done"
            | "response.reasoning.done" => {
                thinking.end();
            }
            "response.output_item.done" => {
                // Check if this is a reasoning item with summary content
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some(item) = json.get("item")
                    && item.get("type").and_then(|t| t.as_str()) == Some("reasoning")
                    && let Some(summary) = item.get("summary").and_then(|s| s.as_array())
                {
                    for part in summary {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str())
                            && !text.is_empty()
                        {
                            if streaming_start.is_none() {
                                streaming_start = Some(Instant::now());
                                last_progress_update = Instant::now();
                            }
                            char_count += text.len();
                            thinking.emit(text);
                        }
                    }
                    thinking.end();
                }
            }
            "response.output_text.delta" => {
                thinking.end();
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some(delta) = json.get("delta").and_then(|d| d.as_str())
                {
                    if streaming_start.is_none() {
                        streaming_start = Some(Instant::now());
                        last_progress_update = Instant::now();
                    }
                    char_count += delta.len();
                    output::print_text(output, delta);
                    full_text.push_str(delta);

                    // Emit progress update every 0.5 seconds
                    if !has_seen_usage && last_progress_update.elapsed().as_secs_f64() >= 0.5 {
                        if let Some(start) = streaming_start {
                            let duration = start.elapsed().as_secs_f64();
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
            }
            "response.output_item.added" => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some(item) = json.get("item")
                    && item.get("type").and_then(|t| t.as_str()) == Some("function_call")
                {
                    let id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();

                    pending_tools.push(PendingToolCall {
                        id,
                        name,
                        arguments: String::new(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some(delta) = json.get("delta").and_then(|d| d.as_str())
                    && let Some(pending) = pending_tools.last_mut()
                {
                    pending.arguments.push_str(delta);
                }
            }
            "response.completed" => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data)
                    && let Some(response) = json.get("response")
                {
                    let reason = response
                        .get("stop_reason")
                        .and_then(|r| r.as_str())
                        .unwrap_or("stop");
                    stop_reason = match reason {
                        "stop" => StopReason::EndTurn,
                        "tool_call" | "tool_calls" => StopReason::ToolUse,
                        "max_output_tokens" | "length" => StopReason::MaxTokens,
                        _ => StopReason::Unknown,
                    };

                    // Parse usage information from the response
                    if let Some(usage_data) = response.get("usage") {
                        if let Some(input_tokens) =
                            usage_data.get("input_tokens").and_then(|t| t.as_u64())
                        {
                            usage::zen().record_input(input_tokens);
                        }
                        if let Some(output_tokens) =
                            usage_data.get("output_tokens").and_then(|t| t.as_u64())
                        {
                            usage::zen().record_output(output_tokens);

                            // Emit final progress with turn total (accumulated across all API calls)
                            has_seen_usage = true;
                            if let Some(start) = streaming_start {
                                let duration = start.elapsed().as_secs_f64();
                                if duration > 0.0 {
                                    let turn_total = usage::zen().turn_total();
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
                }
            }
            _ => {}
        }
    }

    // Only end the text block if we actually streamed any text.
    if !full_text.is_empty() {
        output::print_text_end(output);
    }

    // Process pending tool calls
    for pending in pending_tools {
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

pub(super) fn prepare_request_value(request: &OpenAiResponsesRequest) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(request)?)
}
