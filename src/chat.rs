// SPDX-License-Identifier: AGPL-3.0-only

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: MessageContent,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<crate::llm::ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
}

impl MessageContent {
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(text) => text.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone)]
pub struct ChatConversation {
    pub messages: VecDeque<ChatMessage>,
    pub max_messages: usize,
}

impl Default for ChatConversation {
    fn default() -> Self {
        Self {
            messages: VecDeque::new(),
            max_messages: 100, // Keep last 100 messages for context
        }
    }
}

impl ChatConversation {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_system_message(&mut self, content: String) {
        let message = ChatMessage {
            role: MessageRole::System,
            content: MessageContent::Text(content),
            timestamp: chrono::Utc::now().timestamp(),
            tool_calls: None,
            tool_call_id: None,
        };

        self.add_message(message);
    }

    pub fn add_user_message(&mut self, content: String) {
        let message = ChatMessage {
            role: MessageRole::User,
            content: MessageContent::Text(content),
            timestamp: chrono::Utc::now().timestamp(),
            tool_calls: None,
            tool_call_id: None,
        };

        self.add_message(message);
    }

    pub fn add_assistant_message(&mut self, content: String) {
        let message = ChatMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text(content),
            timestamp: chrono::Utc::now().timestamp(),
            tool_calls: None,
            tool_call_id: None,
        };

        self.add_message(message);
    }

    pub fn add_assistant_message_with_tool_calls(
        &mut self,
        content: String,
        tool_calls: Vec<crate::llm::ToolCall>,
    ) {
        let message = ChatMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text(content),
            timestamp: chrono::Utc::now().timestamp(),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        };

        self.add_message(message);
    }

    pub fn add_tool_message(&mut self, content: String, tool_call_id: String) {
        let message = ChatMessage {
            role: MessageRole::Tool,
            content: MessageContent::Text(content),
            timestamp: chrono::Utc::now().timestamp(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id),
        };

        self.add_message(message);
    }

    fn add_message(&mut self, message: ChatMessage) {
        // Remove oldest messages if we exceed max_messages
        while self.messages.len() >= self.max_messages {
            self.messages.pop_front();
        }

        self.messages.push_back(message);
    }

    pub fn get_messages(&self) -> &VecDeque<ChatMessage> {
        &self.messages
    }

    pub fn clear(&mut self) {
        // Preserve the initial system message if it exists
        let message = self
            .messages
            .iter()
            .find(|msg| matches!(msg.role, MessageRole::System))
            .cloned();

        self.messages.clear();

        // Re-add the system message if it existed
        if let Some(msg) = message {
            self.messages.push_back(msg);
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}
