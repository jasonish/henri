// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Semantic history storage for CLI mode.
//!
//! Stores events as semantic units rather than raw text, allowing
//! re-rendering at any width on terminal resize.

use std::sync::{Mutex, MutexGuard};

use super::TOOL_OUTPUT_MAX_BUFFER_LINES;

/// Global history storage
static HISTORY: Mutex<History> = Mutex::new(History::new());

fn lock_history() -> MutexGuard<'static, History> {
    match HISTORY.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

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
    ToolResult {
        output: String,
        is_error: bool,
        summary: Option<String>,
    },
    /// Tool output text (may be streamed)
    ToolOutput {
        text: String,
        total_lines: usize,
        /// Lines currently stored in `text` (for O(1) truncation checks)
        stored_lines: usize,
    },
    /// File read output with filename for syntax highlighting
    FileReadOutput {
        filename: String,
        text: String,
        total_lines: usize,
        /// Lines currently stored in `text` (for O(1) truncation checks)
        stored_lines: usize,
    },
    /// Image preview (for terminals that support inline images)
    ImagePreview { data: Vec<u8>, mime_type: String },
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
        /// Optional summary for display (e.g., lines added/removed)
        summary: Option<String>,
    },
    /// Auto-compaction notification
    AutoCompact { message: String },
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

    /// Undo the most recent turn from this history.
    ///
    /// A "turn" in the rendered CLI history starts at the most recent
    /// `UserPrompt` event and includes everything after it.
    pub(crate) fn undo_last_turn(&mut self) -> bool {
        let Some(idx) = self
            .events
            .iter()
            .rposition(|e| matches!(e, HistoryEvent::UserPrompt { .. }))
        else {
            return false;
        };

        self.events.truncate(idx);
        true
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

    /// Append tool output to the last ToolOutput event, or create a new one.
    /// Truncates to keep only the last TOOL_OUTPUT_MAX_BUFFER_LINES lines.
    pub(crate) fn append_tool_output(&mut self, text: &str) {
        let new_lines = text.bytes().filter(|&b| b == b'\n').count();

        if let Some(HistoryEvent::ToolOutput {
            text: existing,
            total_lines,
            stored_lines,
        }) = self.events.last_mut()
        {
            existing.push_str(text);
            *total_lines += new_lines;
            *stored_lines += new_lines;
            *stored_lines =
                truncate_to_last_lines(existing, *stored_lines, TOOL_OUTPUT_MAX_BUFFER_LINES);
        } else {
            let mut new_text = text.to_string();
            let stored =
                truncate_to_last_lines(&mut new_text, new_lines, TOOL_OUTPUT_MAX_BUFFER_LINES);
            self.push(HistoryEvent::ToolOutput {
                text: new_text,
                total_lines: new_lines,
                stored_lines: stored,
            });
        }
    }

    /// Append file read output to the last FileReadOutput event with the same filename, or create a new one.
    /// Truncates to keep only the last TOOL_OUTPUT_MAX_BUFFER_LINES lines.
    pub(crate) fn append_file_read_output(&mut self, filename: &str, text: &str) {
        let new_lines = text.bytes().filter(|&b| b == b'\n').count();

        if let Some(HistoryEvent::FileReadOutput {
            filename: existing_filename,
            text: existing,
            total_lines,
            stored_lines,
        }) = self.events.last_mut()
            && existing_filename == filename
        {
            existing.push_str(text);
            *total_lines += new_lines;
            *stored_lines += new_lines;
            *stored_lines =
                truncate_to_last_lines(existing, *stored_lines, TOOL_OUTPUT_MAX_BUFFER_LINES);
            return;
        }
        let mut new_text = text.to_string();
        let stored = truncate_to_last_lines(&mut new_text, new_lines, TOOL_OUTPUT_MAX_BUFFER_LINES);
        self.push(HistoryEvent::FileReadOutput {
            filename: filename.to_string(),
            text: new_text,
            total_lines: new_lines,
            stored_lines: stored,
        });
    }
}

/// Truncate a string to keep only the last `max_lines` lines.
/// Takes the current line count to avoid rescanning.
/// Returns the new line count after truncation.
fn truncate_to_last_lines(text: &mut String, line_count: usize, max_lines: usize) -> usize {
    if line_count <= max_lines {
        return line_count;
    }

    let lines_to_skip = line_count - max_lines;
    let mut skipped = 0;
    let mut start_offset = 0;

    for (i, c) in text.char_indices() {
        if c == '\n' {
            skipped += 1;
            if skipped >= lines_to_skip {
                start_offset = i + 1;
                break;
            }
        }
    }

    if start_offset > 0 && start_offset < text.len() {
        *text = text[start_offset..].to_string();
    }

    max_lines
}

// ============================================================================
// Global API
// ============================================================================

/// Push an event to the global history.
pub(crate) fn push(event: HistoryEvent) {
    let mut history = lock_history();
    history.push(event);
}

/// Clear the global history.
pub(crate) fn clear() {
    let mut history = lock_history();
    history.clear();
}

/// Get a snapshot of all events for rendering.
pub(crate) fn snapshot() -> Vec<HistoryEvent> {
    lock_history().events.clone()
}

pub(crate) fn has_events() -> bool {
    !lock_history().events.is_empty()
}

/// Undo the most recent turn in the global history.
///
/// This removes the most recent `UserPrompt` event and everything after it.
pub(crate) fn undo_last_turn() -> bool {
    let mut history = lock_history();
    history.undo_last_turn()
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
    let mut history = lock_history();
    history.append_assistant_text(text);
}

pub(crate) fn finish_assistant_text() {
    let mut history = lock_history();
    history.finish_assistant_text();
}

/// Append text to the last Thinking event, or create a new one.
pub(crate) fn append_thinking(text: &str) {
    let mut history = lock_history();
    history.append_thinking(text);
}

pub(crate) fn finish_thinking() {
    let mut history = lock_history();
    history.finish_thinking();
}

/// Append tool output to the last ToolOutput event, or create a new one.
pub(crate) fn append_tool_output(text: &str) {
    let mut history = lock_history();
    history.append_tool_output(text);
}

/// Append file read output to the last FileReadOutput event, or create a new one.
pub(crate) fn append_file_read_output(filename: &str, text: &str) {
    let mut history = lock_history();
    history.append_file_read_output(filename, text);
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
                                summary: None,
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

        let events: Vec<_> = history.events().iter().collect();
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
