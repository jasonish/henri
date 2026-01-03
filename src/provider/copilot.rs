// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::{ConfigFile, CopilotProviderConfig, ProviderConfig, ProviderType};
use crate::error::{Error, Result};
use crate::output;
use crate::prompts;
use crate::provider::{
    ChatResponse, ContentBlock, Message, MessageContent, Provider, Role, StopReason, ToolCall,
};
use crate::services::Services;
use crate::sse;
use crate::tools;

const TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const CHAT_URL: &str = "https://api.githubcopilot.com/chat/completions";
const RESPONSES_URL: &str = "https://api.githubcopilot.com/responses";

// User agent strings for Copilot API requests
const EDITOR_VERSION: &str = "vscode/1.99.3";
const EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.26.7";
const USER_AGENT: &str = "GitHubCopilotChat/0.26.7";

const COPILOT_MODELS: &[&str] = &[
    "claude-haiku-4.5",
    "claude-sonnet-4.5",
    "claude-opus-4.5",
    "gpt-5.1-codex",
    "grok-code-fast-1",
];

#[derive(Debug)]
struct CopilotState {
    local_id: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
    copilot_token: Option<String>,
    copilot_expires_at: Option<u64>,
}

pub(crate) struct CopilotProvider {
    client: Client,
    state: Mutex<CopilotState>,
    model: String,
    thinking_enabled: bool,
    services: Services,
}

#[derive(Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: u64,
}

#[derive(Serialize)]
struct CopilotChatRequest {
    model: String,
    messages: Vec<CopilotMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
}

#[derive(Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    kind: String,
    function: OpenAiFunction,
}

#[derive(Serialize)]
struct OpenAiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Serialize, Clone)]
struct OpenAiToolCallMessage {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: OpenAiToolCallFunction,
}

#[derive(Serialize, Clone)]
struct OpenAiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct CopilotMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCallMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
struct CopilotResponseMessage {
    role: String,
    content: Vec<CopilotResponseContent>,
}

#[derive(Serialize)]
struct CopilotResponseContent {
    #[serde(rename = "type")]
    kind: String,
    text: String,
}

#[derive(Deserialize, Debug)]
struct CopilotChunk {
    choices: Vec<CopilotChoice>,
}

#[derive(Deserialize, Debug)]
struct CopilotChoice {
    delta: CopilotDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct CopilotDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<CopilotToolCallDelta>>,
}

#[derive(Deserialize, Debug, Clone)]
struct CopilotToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<CopilotFunctionDelta>,
}

#[derive(Deserialize, Debug, Clone)]
struct CopilotFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

impl CopilotProvider {
    pub(crate) fn try_new(services: Services) -> Result<Self> {
        let config = ConfigFile::load()?;

        // Find the first enabled GitHub Copilot provider
        let (local_id, github) = config
            .providers_of_type(ProviderType::GithubCopilot)
            .into_iter()
            .find(|(_, p)| p.is_enabled())
            .and_then(|(id, p)| p.as_copilot().map(|c| (id.clone(), c.clone())))
            .ok_or_else(|| Error::Auth("GitHub Copilot not configured. Run /login.".to_string()))?;

        Ok(Self {
            client: Client::new(),
            state: Mutex::new(CopilotState {
                local_id,
                access_token: github.access_token,
                refresh_token: github.refresh_token,
                expires_at: github.expires_at,
                copilot_token: github.copilot_token,
                copilot_expires_at: github.copilot_expires_at,
            }),
            model: "claude-haiku-4.5".to_string(),
            thinking_enabled: true,
            services,
        })
    }

    pub(crate) fn set_thinking_enabled(&mut self, enabled: bool) {
        self.thinking_enabled = enabled;
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.model = model;
    }

    pub(crate) fn models() -> &'static [&'static str] {
        COPILOT_MODELS
    }

    fn use_responses_api(&self) -> bool {
        self.model.starts_with("gpt-5")
    }

    async fn ensure_copilot_token(&self, state: &mut CopilotState) -> Result<String> {
        if let (Some(token), Some(expires)) = (&state.copilot_token, state.copilot_expires_at) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| Error::Auth(e.to_string()))?
                .as_secs();

            if now < expires.saturating_sub(300) {
                return Ok(token.clone());
            }
        }

        let response = self
            .client
            .get(TOKEN_URL)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {}", state.access_token))
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", EDITOR_PLUGIN_VERSION)
            .header("User-Agent", USER_AGENT)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Auth(format!(
                "Copilot token request failed: {} - {}",
                status, body
            )));
        }

        let copilot_response: CopilotTokenResponse = response
            .json()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;

        state.copilot_token = Some(copilot_response.token.clone());
        state.copilot_expires_at = Some(copilot_response.expires_at);

        // Persist updated token info if possible.
        if let Ok(mut config) = ConfigFile::load() {
            let enabled = config
                .get_provider(&state.local_id)
                .map(|p| p.is_enabled())
                .unwrap_or(true);
            config.set_provider(
                state.local_id.clone(),
                ProviderConfig::GithubCopilot(CopilotProviderConfig {
                    enabled,
                    access_token: state.access_token.clone(),
                    refresh_token: state.refresh_token.clone(),
                    expires_at: state.expires_at,
                    copilot_token: Some(copilot_response.token.clone()),
                    copilot_expires_at: Some(copilot_response.expires_at),
                }),
            );
            let _ = config.save();
        }

        Ok(copilot_response.token)
    }

    fn build_messages(&self, messages: Vec<Message>) -> Vec<CopilotMessage> {
        let mut payload = vec![CopilotMessage {
            role: "system".to_string(),
            content: Some(prompts::system_prompt().join("\n\n")),
            tool_calls: None,
            tool_call_id: None,
        }];

        for msg in messages {
            match &msg.content {
                MessageContent::Text(text) => {
                    let role = match msg.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::System => "system",
                    };
                    payload.push(CopilotMessage {
                        role: role.to_string(),
                        content: Some(text.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                MessageContent::Blocks(blocks) => {
                    // Check if this is an assistant message with tool calls
                    let tool_calls: Vec<_> = blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolUse {
                                id, name, input, ..
                            } = b
                            {
                                Some(OpenAiToolCallMessage {
                                    id: id.clone(),
                                    kind: "function".to_string(),
                                    function: OpenAiToolCallFunction {
                                        name: name.clone(),
                                        arguments: serde_json::to_string(input).unwrap_or_default(),
                                    },
                                })
                            } else {
                                None
                            }
                        })
                        .collect();

                    // Check for tool results (user messages)
                    let tool_results: Vec<_> = blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                ..
                            } = b
                            {
                                Some((tool_use_id.clone(), content.clone()))
                            } else {
                                None
                            }
                        })
                        .collect();

                    // Get any text content
                    let text_content: String = blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");

                    if !tool_calls.is_empty() {
                        // Assistant message with tool calls
                        payload.push(CopilotMessage {
                            role: "assistant".to_string(),
                            content: if text_content.is_empty() {
                                None
                            } else {
                                Some(text_content)
                            },
                            tool_calls: Some(tool_calls),
                            tool_call_id: None,
                        });
                    } else if !tool_results.is_empty() {
                        // Tool result messages
                        for (tool_use_id, content) in tool_results {
                            payload.push(CopilotMessage {
                                role: "tool".to_string(),
                                content: Some(content),
                                tool_calls: None,
                                tool_call_id: Some(tool_use_id),
                            });
                        }
                    } else if !text_content.is_empty() {
                        // Just text content
                        let role = match msg.role {
                            Role::User => "user",
                            Role::Assistant => "assistant",
                            Role::System => "system",
                        };
                        payload.push(CopilotMessage {
                            role: role.to_string(),
                            content: Some(text_content),
                            tool_calls: None,
                            tool_call_id: None,
                        });
                    }
                }
            }
        }

        payload
    }

    fn build_responses_input(&self, messages: Vec<Message>) -> Vec<ResponsesInput> {
        let mut payload = vec![ResponsesInput::Message(CopilotResponseMessage {
            role: "system".to_string(),
            content: vec![CopilotResponseContent {
                kind: "input_text".to_string(),
                text: prompts::system_prompt().join("\n\n"),
            }],
        })];

        for msg in messages {
            match &msg.content {
                MessageContent::Text(text) => {
                    let role = match msg.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::System => "system",
                    };
                    payload.push(ResponsesInput::Message(CopilotResponseMessage {
                        role: role.to_string(),
                        content: vec![CopilotResponseContent {
                            kind: "input_text".to_string(),
                            text: text.clone(),
                        }],
                    }));
                }
                MessageContent::Blocks(blocks) => {
                    // Get any text content from assistant
                    let text_content: String = blocks
                        .iter()
                        .filter_map(|b| {
                            if let ContentBlock::Text { text } = b {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");

                    if !text_content.is_empty() {
                        let (role, kind) = match msg.role {
                            Role::User => ("user", "input_text"),
                            Role::Assistant => ("assistant", "output_text"),
                            Role::System => ("system", "input_text"),
                        };
                        payload.push(ResponsesInput::Message(CopilotResponseMessage {
                            role: role.to_string(),
                            content: vec![CopilotResponseContent {
                                kind: kind.to_string(),
                                text: text_content,
                            }],
                        }));
                    }

                    // Include function calls from assistant (ToolUse blocks)
                    for block in blocks {
                        if let ContentBlock::ToolUse {
                            id, name, input, ..
                        } = block
                        {
                            payload.push(ResponsesInput::FunctionCall(
                                ResponsesFunctionCallInput {
                                    kind: "function_call".to_string(),
                                    call_id: id.clone(),
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            ));
                        }
                    }

                    // Include function call outputs (ToolResult blocks)
                    for block in blocks {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } = block
                        {
                            payload.push(ResponsesInput::FunctionCallOutput(
                                ResponsesFunctionCallOutput {
                                    kind: "function_call_output".to_string(),
                                    call_id: tool_use_id.clone(),
                                    output: content.clone(),
                                },
                            ));
                        }
                    }
                }
            }
        }

        payload
    }

    async fn build_chat_request(&self, messages: &[Message]) -> CopilotChatRequest {
        let openai_tools: Vec<OpenAiTool> = tools::all_definitions(&self.services)
            .await
            .into_iter()
            .map(|t| OpenAiTool {
                kind: "function".to_string(),
                function: OpenAiFunction {
                    name: t.name,
                    description: t.description,
                    parameters: t.input_schema,
                },
            })
            .collect();

        CopilotChatRequest {
            model: self.model.clone(),
            messages: self.build_messages(messages.to_vec()),
            stream: true,
            tools: openai_tools,
        }
    }

    async fn build_responses_request(&self, messages: &[Message]) -> CopilotResponsesRequest {
        let tools: Vec<ResponsesTool> = tools::all_definitions(&self.services)
            .await
            .into_iter()
            .map(|t| ResponsesTool {
                kind: "function".to_string(),
                name: t.name,
                description: t.description,
                parameters: t.input_schema,
            })
            .collect();

        let reasoning = if self.model.starts_with("gpt-5") && self.thinking_enabled {
            Some(CopilotReasoningConfig {
                effort: "medium".to_string(),
                summary: "auto".to_string(),
            })
        } else {
            None
        };

        CopilotResponsesRequest {
            model: self.model.clone(),
            input: self.build_responses_input(messages.to_vec()),
            stream: true,
            reasoning,
            tools,
        }
    }

    /// Get the context limit for a given model name
    pub(crate) fn context_limit(model: &str) -> Option<u64> {
        // Copilot GPT-5 models have 400k context
        if model.starts_with("gpt-5") {
            Some(400_000)
        } else {
            None
        }
    }
}

impl Provider for CopilotProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        if self.use_responses_api() {
            return self.chat_responses_api(messages, output).await;
        }

        let mut state = self.state.lock().await;
        let copilot_token = self.ensure_copilot_token(&mut state).await?;
        drop(state);

        let request = self.build_chat_request(&messages).await;

        // Record TX bytes
        let body_bytes = serde_json::to_vec(&request)?;
        crate::usage::network_stats().record_tx(body_bytes.len() as u64);

        let mut req_headers = std::collections::HashMap::new();
        req_headers.insert(
            "Authorization".to_string(),
            format!("Bearer {copilot_token}"),
        );
        req_headers.insert(
            "User-Agent".to_string(),
            "GitHubCopilotChat/1.0".to_string(),
        );
        req_headers.insert("Editor-Version".to_string(), EDITOR_VERSION.to_string());
        req_headers.insert(
            "Editor-Plugin-Version".to_string(),
            EDITOR_PLUGIN_VERSION.to_string(),
        );
        req_headers.insert("Content-Type".to_string(), "application/json".to_string());
        req_headers.insert("Accept".to_string(), "text/event-stream".to_string());
        req_headers.insert("X-GitHub-Api-Version".to_string(), "2023-07-07".to_string());

        let response = self
            .client
            .post(CHAT_URL)
            .header("Authorization", format!("Bearer {copilot_token}"))
            .header("User-Agent", "GitHubCopilotChat/1.0")
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("X-GitHub-Api-Version", "2023-07-07")
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| {
                Error::Other(format!(
                    "Failed to connect to GitHub Copilot API ({}): {}",
                    CHAT_URL, e
                ))
            })?;

        let resp_headers =
            crate::provider::transaction_log::header_map_to_hash_map(response.headers());

        if !response.status().is_success() {
            let status = response.status();
            let status_code = status.as_u16();
            let text = response.text().await.unwrap_or_default();

            crate::provider::transaction_log::log(
                CHAT_URL,
                req_headers.clone(),
                serde_json::to_value(&request).unwrap_or_default(),
                resp_headers,
                serde_json::json!({
                    "error": true,
                    "status": status_code,
                    "body": text
                }),
            );

            // Check for retryable errors (timeouts, overloaded, rate limits)
            if super::is_retryable_status(status_code) || super::is_retryable_message(&text) {
                return Err(Error::Retryable {
                    status: status_code,
                    message: text,
                });
            }

            return Err(Error::Auth(format!(
                "Copilot chat failed: {} - {}",
                status, text
            )));
        }

        // Process streaming response, accumulating text and tool calls
        let mut full_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut pending_tool_calls: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();
        let mut stop_reason = StopReason::EndTurn;
        let mut thinking = output::ThinkingState::new(output);
        let mut raw_events: Vec<serde_json::Value> = Vec::new();

        let mut sse = sse::SseStream::new(response.bytes_stream().map(|chunk| {
            if let Ok(ref bytes) = chunk {
                crate::usage::network_stats().record_rx(bytes.len() as u64);
            }
            chunk
        }));
        while let Some(result) = sse.next_event().await {
            let data = result.map_err(Error::Http)?;

            let Ok(chunk) = serde_json::from_str::<CopilotChunk>(&data) else {
                continue;
            };
            raw_events
                .push(serde_json::from_str(&data).unwrap_or(serde_json::json!({ "raw": data })));

            for choice in &chunk.choices {
                if let Some(reasoning) = &choice.delta.reasoning_content {
                    thinking.emit(reasoning);
                }

                if let Some(content) = &choice.delta.content {
                    thinking.end();
                    output::print_text(output, content);
                    full_text.push_str(content);
                }

                // Handle tool calls
                if let Some(tc_deltas) = &choice.delta.tool_calls {
                    for tc_delta in tc_deltas {
                        let entry = pending_tool_calls
                            .entry(tc_delta.index)
                            .or_insert_with(|| (String::new(), String::new(), String::new()));

                        if let Some(id) = &tc_delta.id {
                            entry.0 = id.clone();
                        }
                        if let Some(func) = &tc_delta.function {
                            if let Some(name) = &func.name {
                                entry.1 = name.clone();
                            }
                            if let Some(args) = &func.arguments {
                                entry.2.push_str(args);
                            }
                        }
                    }
                }

                // Check finish reason
                if let Some(reason) = &choice.finish_reason {
                    stop_reason = match reason.as_str() {
                        "stop" => StopReason::EndTurn,
                        "tool_calls" => StopReason::ToolUse,
                        "length" => StopReason::MaxTokens,
                        _ => StopReason::Unknown,
                    };
                }
            }
        }

        // Finalize tool calls
        for (_index, (id, name, arguments)) in pending_tool_calls {
            if !id.is_empty() && !name.is_empty() {
                let input: serde_json::Value =
                    serde_json::from_str(&arguments).unwrap_or(serde_json::json!({}));
                tool_calls.push(ToolCall {
                    id,
                    name,
                    input,
                    thought_signature: None,
                });
            }
        }

        // Build content blocks
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        if !full_text.is_empty() {
            content_blocks.push(ContentBlock::Text {
                text: full_text.clone(),
            });
        }
        for tc in &tool_calls {
            content_blocks.push(ContentBlock::ToolUse {
                id: tc.id.clone(),
                name: tc.name.clone(),
                input: tc.input.clone(),
                thought_signature: None,
            });
        }

        output::print_text_end(output);

        if crate::provider::transaction_log::is_active() {
            let url = if self.use_responses_api() {
                RESPONSES_URL
            } else {
                CHAT_URL
            };
            crate::provider::transaction_log::log(
                url,
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

    async fn prepare_request(&self, messages: Vec<Message>) -> Result<serde_json::Value> {
        if self.use_responses_api() {
            let request = self.build_responses_request(&messages).await;
            Ok(serde_json::to_value(&request)?)
        } else {
            let request = self.build_chat_request(&messages).await;
            Ok(serde_json::to_value(&request)?)
        }
    }
}

#[derive(Serialize)]
struct CopilotResponsesRequest {
    model: String,
    input: Vec<ResponsesInput>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<CopilotReasoningConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ResponsesTool>,
}

#[derive(Debug, Clone, Serialize)]
struct CopilotReasoningConfig {
    effort: String,
    summary: String,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ResponsesInput {
    Message(CopilotResponseMessage),
    FunctionCall(ResponsesFunctionCallInput),
    FunctionCallOutput(ResponsesFunctionCallOutput),
}

#[derive(Serialize)]
struct ResponsesFunctionCallInput {
    #[serde(rename = "type")]
    kind: String,
    call_id: String,
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct ResponsesFunctionCallOutput {
    #[serde(rename = "type")]
    kind: String,
    call_id: String,
    output: String,
}

#[derive(Serialize)]
struct ResponsesTool {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// Streaming event types for Responses API
#[derive(Deserialize, Debug)]
struct ResponsesStreamEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    item: Option<ResponsesStreamItem>,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    output_index: Option<usize>,
}

#[derive(Deserialize, Debug)]
struct ResponsesStreamItem {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl CopilotProvider {
    async fn chat_responses_api(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        let mut state = self.state.lock().await;
        let copilot_token = self.ensure_copilot_token(&mut state).await?;
        drop(state);

        let request = self.build_responses_request(&messages).await;

        // Record TX bytes
        let body_bytes = serde_json::to_vec(&request)?;
        crate::usage::network_stats().record_tx(body_bytes.len() as u64);

        let mut req_headers = std::collections::HashMap::new();
        req_headers.insert(
            "Authorization".to_string(),
            format!("Bearer {copilot_token}"),
        );
        req_headers.insert(
            "User-Agent".to_string(),
            "GitHubCopilotChat/1.0".to_string(),
        );
        req_headers.insert("Editor-Version".to_string(), EDITOR_VERSION.to_string());
        req_headers.insert(
            "Editor-Plugin-Version".to_string(),
            EDITOR_PLUGIN_VERSION.to_string(),
        );
        req_headers.insert("Content-Type".to_string(), "application/json".to_string());
        req_headers.insert("Accept".to_string(), "text/event-stream".to_string());
        req_headers.insert("X-GitHub-Api-Version".to_string(), "2023-07-07".to_string());

        let response = self
            .client
            .post(RESPONSES_URL)
            .header("Authorization", format!("Bearer {copilot_token}"))
            .header("User-Agent", "GitHubCopilotChat/1.0")
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("X-GitHub-Api-Version", "2023-07-07")
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;

        let resp_headers =
            crate::provider::transaction_log::header_map_to_hash_map(response.headers());

        if !response.status().is_success() {
            let status = response.status();
            let status_code = status.as_u16();
            let text = response.text().await.unwrap_or_default();

            crate::provider::transaction_log::log(
                RESPONSES_URL,
                req_headers.clone(),
                serde_json::to_value(&request).unwrap_or_default(),
                resp_headers,
                serde_json::json!({
                    "error": true,
                    "status": status_code,
                    "body": text
                }),
            );

            // Check for retryable errors (timeouts, overloaded, rate limits)
            if super::is_retryable_status(status_code) || super::is_retryable_message(&text) {
                return Err(Error::Retryable {
                    status: status_code,
                    message: text,
                });
            }

            return Err(Error::Auth(format!(
                "Copilot responses failed: {} - {}",
                status, text
            )));
        }

        // Process streaming response
        let mut full_text = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut thinking = output::ThinkingState::new(output);
        let mut raw_events: Vec<serde_json::Value> = Vec::new();

        // Track pending function calls by output_index
        let mut pending_functions: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();

        let mut sse = sse::SseStream::new(response.bytes_stream().map(|chunk| {
            if let Ok(ref bytes) = chunk {
                crate::usage::network_stats().record_rx(bytes.len() as u64);
            }
            chunk
        }));
        while let Some(result) = sse.next_event().await {
            let data = result.map_err(Error::Http)?;

            if data.is_empty() {
                continue;
            }

            let Ok(event) = serde_json::from_str::<ResponsesStreamEvent>(&data) else {
                continue;
            };
            raw_events
                .push(serde_json::from_str(&data).unwrap_or(serde_json::json!({ "raw": data })));

            match event.kind.as_str() {
                "response.output_item.added" => {
                    // Track new function call items
                    if let (Some(item), Some(idx)) = (&event.item, event.output_index)
                        && item.kind == "function_call"
                    {
                        let call_id = item.call_id.clone().unwrap_or_default();
                        let name = item.name.clone().unwrap_or_default();
                        pending_functions.insert(idx, (call_id, name, String::new()));
                    }
                }
                "response.output_text.delta" => {
                    thinking.end();
                    if let Some(delta) = &event.delta {
                        output::print_text(output, delta);
                        full_text.push_str(delta);
                    }
                }
                "response.reasoning_summary_text.delta" => {
                    if let Some(delta) = &event.delta {
                        thinking.emit(delta);
                    }
                }
                "response.function_call_arguments.delta" => {
                    if let (Some(delta), Some(idx)) = (&event.delta, event.output_index)
                        && let Some(entry) = pending_functions.get_mut(&idx)
                    {
                        entry.2.push_str(delta);
                    }
                }
                "response.output_item.done" => {
                    // Finalize function calls when their item is done
                    if let Some(idx) = event.output_index
                        && let Some((call_id, name, arguments)) = pending_functions.remove(&idx)
                        && !call_id.is_empty()
                        && !name.is_empty()
                    {
                        let input: serde_json::Value =
                            serde_json::from_str(&arguments).unwrap_or(serde_json::json!({}));
                        tool_calls.push(ToolCall {
                            id: call_id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                            thought_signature: None,
                        });
                        content_blocks.push(ContentBlock::ToolUse {
                            id: call_id,
                            name,
                            input,
                            thought_signature: None,
                        });
                    }
                }
                _ => {}
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

        let stop_reason = if tool_calls.is_empty() {
            StopReason::EndTurn
        } else {
            StopReason::ToolUse
        };

        output::print_text_end(output);

        if crate::provider::transaction_log::is_active() {
            let url = if self.use_responses_api() {
                RESPONSES_URL
            } else {
                CHAT_URL
            };
            crate::provider::transaction_log::log(
                url,
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
