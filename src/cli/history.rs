// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Semantic history storage for CLI mode.
//!
//! Stores events as semantic units rather than raw text, allowing
//! re-rendering at any width on terminal resize.

use std::sync::Mutex;

/// Global history storage
static HISTORY: Mutex<History> = Mutex::new(History::new());

/// Metadata about an attached image (for display only, no data)
#[derive(Clone, Debug)]
pub(crate) struct ImageMeta {
    pub _marker: String,
    pub _mime_type: String,
    pub _size_bytes: usize,
}

/// A semantic event in the conversation history.
#[derive(Clone, Debug)]
pub(crate) enum HistoryEvent {
    /// User submitted a prompt
    UserPrompt {
        text: String,
        images: Vec<ImageMeta>,
    },
    /// Assistant text response (may be partial during streaming)
    AssistantText { text: String, is_streaming: bool },
    /// Assistant is thinking
    Thinking { text: String, is_streaming: bool },
    /// Tool invocation started - stores full description for display
    ToolUse { description: String },
    /// End of thinking block
    ThinkingEnd,
    /// End of response block
    ResponseEnd,
    /// Start of tool block
    ToolStart,
    /// End of tool block
    ToolEnd,
    /// Tool execution completed
    ToolResult { output: String, is_error: bool },
    /// An error occurred
    Error(String),
    /// Warning message
    Warning(String),
    /// Info message
    Info(String),
    /// File diff output
    FileDiff {
        diff: String,
        /// Language for syntax highlighting
        language: Option<String>,
    },
    /// Todo list update
    TodoList { items: Vec<TodoItem> },
    /// Auto-compaction notification
    AutoCompact { message: String },
}

/// A todo item for display
#[derive(Clone, Debug)]
pub(crate) struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// Todo item status
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// Stores the conversation history as semantic events.
pub(crate) struct History {
    events: Vec<HistoryEvent>,
}

impl History {
    /// Create a new empty history.
    pub(crate) const fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// Push a new event to the history.
    pub(crate) fn push(&mut self, event: HistoryEvent) {
        self.events.push(event);
    }

    /// Clear all events.
    pub(crate) fn clear(&mut self) {
        self.events.clear();
    }

    /// Iterate over all events.
    #[cfg(test)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = &HistoryEvent> {
        self.events.iter()
    }

    /// Get events as a slice.
    #[cfg(test)]
    pub(crate) fn events(&self) -> &[HistoryEvent] {
        &self.events
    }

    /// Append text to the last AssistantText event, or create a new one.
    pub(crate) fn append_assistant_text(&mut self, text: &str) {
        if let Some(HistoryEvent::AssistantText {
            text: existing,
            is_streaming,
        }) = self.events.last_mut()
        {
            existing.push_str(text);
            *is_streaming = true;
        } else {
            self.push(HistoryEvent::AssistantText {
                text: text.to_string(),
                is_streaming: true,
            });
        }
    }

    pub(crate) fn finish_assistant_text(&mut self) {
        if let Some(HistoryEvent::AssistantText { is_streaming, .. }) = self.events.last_mut() {
            *is_streaming = false;
        }
    }

    /// Append text to the last Thinking event, or create a new one.
    pub(crate) fn append_thinking(&mut self, text: &str) {
        if let Some(HistoryEvent::Thinking {
            text: existing,
            is_streaming,
        }) = self.events.last_mut()
        {
            existing.push_str(text);
            *is_streaming = true;
        } else {
            self.push(HistoryEvent::Thinking {
                text: text.to_string(),
                is_streaming: true,
            });
        }
    }

    pub(crate) fn finish_thinking(&mut self) {
        if let Some(HistoryEvent::Thinking { is_streaming, .. }) = self.events.last_mut() {
            *is_streaming = false;
        }
    }
}

// ============================================================================
// Global API
// ============================================================================

/// Push an event to the global history.
pub(crate) fn push(event: HistoryEvent) {
    if let Ok(mut history) = HISTORY.lock() {
        history.push(event);
    }
}

/// Clear the global history.
pub(crate) fn clear() {
    if let Ok(mut history) = HISTORY.lock() {
        history.clear();
    }
}

/// Get a snapshot of all events for rendering.
pub(crate) fn snapshot() -> Vec<HistoryEvent> {
    HISTORY.lock().map(|h| h.events.clone()).unwrap_or_default()
}

pub(crate) fn has_events() -> bool {
    HISTORY
        .lock()
        .map(|h| !h.events.is_empty())
        .unwrap_or(false)
}

/// Push a user prompt event.
pub(crate) fn push_user_prompt(text: &str, images: Vec<ImageMeta>) {
    push(HistoryEvent::UserPrompt {
        text: text.to_string(),
        images,
    });
}

/// Append text to the last AssistantText event, or create a new one.
pub(crate) fn append_assistant_text(text: &str) {
    if let Ok(mut history) = HISTORY.lock() {
        history.append_assistant_text(text);
    }
}

pub(crate) fn finish_assistant_text() {
    if let Ok(mut history) = HISTORY.lock() {
        history.finish_assistant_text();
    }
}

/// Append text to the last Thinking event, or create a new one.
pub(crate) fn append_thinking(text: &str) {
    if let Ok(mut history) = HISTORY.lock() {
        history.append_thinking(text);
    }
}

pub(crate) fn finish_thinking() {
    if let Ok(mut history) = HISTORY.lock() {
        history.finish_thinking();
    }
}

use crate::provider::{ContentBlock, Message, MessageContent, Role};

/// Push history events for a Message.
/// This converts a Message to the appropriate HistoryEvents for display.
pub(crate) fn push_message(message: &Message) {
    match message.role {
        Role::User => {
            let (text, images) = match &message.content {
                MessageContent::Text(text) => (text.clone(), vec![]),
                MessageContent::Blocks(blocks) => {
                    let mut text_parts = Vec::new();
                    let mut images = Vec::new();
                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => {
                                text_parts.push(text.clone());
                            }
                            ContentBlock::Image { mime_type, data } => {
                                images.push(ImageMeta {
                                    _marker: format!(
                                        "{}{}",
                                        crate::cli::input::IMAGE_MARKER_PREFIX,
                                        images.len() + 1
                                    ),
                                    _mime_type: mime_type.clone(),
                                    _size_bytes: data.len(),
                                });
                            }
                            _ => {}
                        }
                    }

                    let mut text = text_parts.join("\n");
                    if !images.is_empty() {
                        let missing_markers: Vec<&str> = images
                            .iter()
                            .map(|image| image._marker.as_str())
                            .filter(|marker| !text.contains(marker))
                            .collect();
                        if !missing_markers.is_empty() {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            for (idx, marker) in missing_markers.iter().enumerate() {
                                if idx > 0 {
                                    text.push('\n');
                                }
                                text.push_str(marker);
                            }
                        }
                    }

                    (text, images)
                }
            };
            push_user_prompt(&text, images);
        }
        Role::Assistant => {
            if let MessageContent::Blocks(blocks) = &message.content {
                for block in blocks {
                    match block {
                        ContentBlock::Thinking { thinking, .. } => {
                            push(HistoryEvent::Thinking {
                                text: thinking.clone(),
                                is_streaming: false,
                            });
                        }
                        ContentBlock::Text { text } => {
                            push(HistoryEvent::AssistantText {
                                text: text.clone(),
                                is_streaming: false,
                            });
                        }
                        ContentBlock::ToolUse { name, .. } => {
                            push(HistoryEvent::ToolStart);
                            push(HistoryEvent::ToolUse {
                                description: name.clone(),
                            });
                        }
                        ContentBlock::ToolResult {
                            content, is_error, ..
                        } => {
                            push(HistoryEvent::ToolResult {
                                output: content.clone(),
                                is_error: *is_error,
                            });
                            push(HistoryEvent::ToolEnd);
                        }
                        _ => {}
                    }
                }
            } else if let MessageContent::Text(text) = &message.content {
                push(HistoryEvent::AssistantText {
                    text: text.clone(),
                    is_streaming: false,
                });
            }
        }
        Role::System => {
            // System messages are not displayed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_history_push_and_iter() {
        let mut history = History::new();

        history.push(HistoryEvent::UserPrompt {
            text: "Hello".to_string(),
            images: Vec::new(),
        });
        history.push(HistoryEvent::AssistantText {
            text: "Hi there!".to_string(),
            is_streaming: false,
        });

        let events: Vec<_> = history.iter().collect();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_append_assistant_text() {
        let mut history = History::new();

        history.append_assistant_text("Hello ");
        history.append_assistant_text("world!");

        if let HistoryEvent::AssistantText { text, is_streaming } = &history.events()[0] {
            assert_eq!(text, "Hello world!");
            assert!(*is_streaming);
        } else {
            panic!("Expected AssistantText");
        }
    }

    #[test]
    fn test_append_thinking() {
        let mut history = History::new();

        history.append_thinking("Let me ");
        history.append_thinking("think...");

        if let HistoryEvent::Thinking { text, is_streaming } = &history.events()[0] {
            assert_eq!(text, "Let me think...");
            assert!(*is_streaming);
        } else {
            panic!("Expected Thinking");
        }
    }

    #[test]
    fn test_clear() {
        let mut history = History::new();
        history.push(HistoryEvent::Error("test".to_string()));

        history.clear();
    }
}
