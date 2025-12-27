// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

pub mod anthropic;
pub mod antigravity;
pub mod copilot;
pub mod openai;
pub mod openai_compat;
pub mod openrouter;
pub mod transaction_log;
pub mod zen;

use crate::error::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
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
pub enum Role {
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
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// Response from the chat API
#[derive(Debug)]
pub struct ChatResponse {
    /// Tool calls requested by the model
    pub tool_calls: Vec<ToolCall>,
    /// The content blocks to store in message history
    pub content_blocks: Vec<ContentBlock>,
    /// Whether the model stopped due to tool use
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Unknown,
}

/// Trait for AI providers
pub trait Provider: Send + Sync {
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
