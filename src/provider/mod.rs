// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

pub(crate) mod anthropic;
pub(crate) mod antigravity;
pub(crate) mod copilot;
pub(crate) mod openai;
pub(crate) mod openai_compat;
pub(crate) mod openrouter;
pub(crate) mod transaction_log;
pub(crate) mod zen;

use crate::error::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Message {
    pub role: Role,
    pub content: MessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        data: Vec<u8>,
    },
    Thinking {
        thinking: String,
        /// Provider-specific data needed for round-tripping (signature, encrypted content, etc.)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_data: Option<serde_json::Value>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    Summary {
        summary: String,
        messages_compacted: usize,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Role {
    System,
    User,
    Assistant,
}

impl Message {
    pub(crate) fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Text(content.into()),
        }
    }

    pub(crate) fn assistant_blocks(blocks: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Blocks(blocks),
        }
    }

    pub(crate) fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: MessageContent::Text(content.into()),
        }
    }
}

/// A tool call requested by the model
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// Response from the chat API
#[derive(Debug)]
pub(crate) struct ChatResponse {
    /// Tool calls requested by the model
    pub tool_calls: Vec<ToolCall>,
    /// The content blocks to store in message history
    pub content_blocks: Vec<ContentBlock>,
    /// Whether the model stopped due to tool use
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Unknown,
}

/// Trait for AI providers
pub(crate) trait Provider: Send + Sync {
    fn chat(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> impl std::future::Future<Output = Result<ChatResponse>> + Send;

    /// Prepare and return the request that would be sent to the API.
    /// Returns a JSON value representing the full request body.
    fn prepare_request(
        &self,
        messages: Vec<Message>,
    ) -> impl std::future::Future<Output = Result<serde_json::Value>> + Send;

    /// Reset usage counters for a new turn/interaction.
    /// Called at the start of each user interaction to track accumulated usage.
    fn start_turn(&self) {
        // Default implementation: do nothing
    }
}

/// Get the context limit for a given provider and model name.
/// Returns None if the limit is unknown.
pub(crate) fn context_limit(provider: crate::providers::ModelProvider, model: &str) -> Option<u64> {
    use crate::providers::ModelProvider;

    match provider {
        ModelProvider::Antigravity => antigravity::AntigravityProvider::context_limit(model),
        ModelProvider::Claude => anthropic::AnthropicProvider::context_limit(model),
        ModelProvider::OpenAi => openai::OpenAiProvider::context_limit(model),
        ModelProvider::GitHubCopilot => copilot::CopilotProvider::context_limit(model),
        ModelProvider::OpenCodeZen => zen::ZenProvider::context_limit(model),
        ModelProvider::OpenAiCompat => openai_compat::OpenAiCompatProvider::context_limit(model),
        ModelProvider::OpenRouter => openrouter::OpenRouterProvider::context_limit(model),
    }
}

/// Transform thinking blocks to plain text when switching providers.
///
/// Thinking blocks contain provider-specific signatures that are not valid across providers.
/// When switching providers mid-conversation, thinking blocks from the old provider will
/// cause API errors (e.g., "Invalid signature in thinking block").
///
/// This function transforms thinking blocks to plain text wrapped in `<thinking>` tags,
/// preserving the reasoning content while discarding the provider-specific signature.
pub(crate) fn transform_thinking_for_provider_switch(messages: &mut [Message]) {
    for message in messages.iter_mut() {
        if let MessageContent::Blocks(blocks) = &mut message.content {
            for block in blocks.iter_mut() {
                if let ContentBlock::Thinking { thinking, .. } = block {
                    *block = ContentBlock::Text {
                        text: format!("<thinking>\n{}\n</thinking>", thinking),
                    };
                }
            }
        }
    }
}

/// Check if an HTTP status code indicates a retryable error.
/// Returns true for:
/// - 408 Request Timeout
/// - 429 Too Many Requests
/// - 502 Bad Gateway
/// - 503 Service Unavailable
/// - 529 Overloaded (Anthropic-specific)
pub(crate) fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 502 | 503 | 529)
}

/// Check if an error message indicates a retryable error.
/// Looks for common timeout/overload messages in the response body.
pub(crate) fn is_retryable_message(message: &str) -> bool {
    let msg_lower = message.to_lowercase();
    msg_lower.contains("timeout")
        || msg_lower.contains("overloaded")
        || msg_lower.contains("too many requests")
        || msg_lower.contains("rate limit")
        || msg_lower.contains("service unavailable")
        || msg_lower.contains("bad gateway")
}

/// Create the appropriate error for an API response based on status and message.
/// Returns `Error::Retryable` if the error appears to be transient, otherwise `Error::Api`.
pub(crate) fn api_error(status: u16, message: String) -> crate::error::Error {
    if is_retryable_status(status) || is_retryable_message(&message) {
        crate::error::Error::Retryable { status, message }
    } else {
        crate::error::Error::Api { status, message }
    }
}
