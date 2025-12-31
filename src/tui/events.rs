// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Output event handling for the TUI App.

use crate::output::OutputEvent;
use crate::tools::TodoStatus;

use super::app::App;
use super::messages::{
    DiffMessage, Message, TextMessage, ThinkingMessage, TodoListDisplay, ToolCallsMessage,
};

impl App {
    /// Ensure there's a streaming tool calls message, creating one if needed
    fn ensure_tool_calls_message(&mut self) -> &mut ToolCallsMessage {
        let needs_new = !matches!(
            self.messages.last(),
            Some(Message::AssistantToolCalls(msg)) if msg.is_streaming
        );
        if needs_new {
            self.messages
                .push(Message::AssistantToolCalls(ToolCallsMessage {
                    calls: Vec::new(),
                    is_streaming: true,
                }));
        }
        match self.messages.last_mut() {
            Some(Message::AssistantToolCalls(msg)) => msg,
            _ => unreachable!(),
        }
    }

    /// Ensure there's a streaming thinking message, creating one if needed
    fn ensure_thinking_message(&mut self) -> &mut ThinkingMessage {
        let needs_new = !matches!(
            self.messages.last(),
            Some(Message::AssistantThinking(msg)) if msg.is_streaming
        );
        if needs_new {
            self.messages
                .push(Message::AssistantThinking(ThinkingMessage {
                    text: String::new(),
                    is_streaming: true,
                }));
        }
        match self.messages.last_mut() {
            Some(Message::AssistantThinking(msg)) => msg,
            _ => unreachable!(),
        }
    }

    /// Ensure there's a streaming text message, creating one if needed
    fn ensure_text_message(&mut self) -> &mut TextMessage {
        let needs_new = !matches!(
            self.messages.last(),
            Some(Message::AssistantText(msg)) if msg.is_streaming
        );
        if needs_new {
            self.messages.push(Message::AssistantText(TextMessage {
                text: String::new(),
                is_streaming: true,
            }));
        }
        match self.messages.last_mut() {
            Some(Message::AssistantText(msg)) => msg,
            _ => unreachable!(),
        }
    }

    /// Mark the last streaming assistant message as done
    fn finish_last_assistant_message(&mut self) {
        match self.messages.last_mut() {
            Some(Message::AssistantThinking(msg)) => msg.is_streaming = false,
            Some(Message::AssistantToolCalls(msg)) => msg.is_streaming = false,
            Some(Message::AssistantText(msg)) => msg.is_streaming = false,
            _ => {}
        }
    }

    /// Poll for all output events (unified channel)
    pub(crate) fn poll_output_events(&mut self) -> bool {
        let mut updated = false;
        let width = self.layout_cache.width;

        while let Ok(event) = self.event_rx.try_recv() {
            updated = true;
            match event {
                OutputEvent::Waiting => {
                    // Latch currently displayed stats into accumulated stats
                    if let Some(tokens) = self.streaming_tokens {
                        self.accumulated_tokens = tokens;
                    }
                    if let Some(duration) = self.streaming_duration {
                        self.accumulated_duration = duration;
                    }
                    // Reset start time for next streaming phase
                    self.streaming_start_time = Some(std::time::Instant::now());

                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::ToolCall { description } => {
                    // Skip display for todo tools - they emit their own TodoList event
                    if description.starts_with("Updating todo list")
                        || description.starts_with("Reading todo list")
                    {
                        continue;
                    }

                    let last_msg = self.messages.last();

                    // Check if we need a new message (current one is text or thinking)
                    let needs_new_message = matches!(
                        last_msg,
                        Some(Message::AssistantText(_)) | Some(Message::AssistantThinking(_))
                    );

                    if needs_new_message {
                        // Mark previous message as done streaming
                        self.finish_last_assistant_message();
                    }

                    let msg = self.ensure_tool_calls_message();
                    msg.calls.push(format!("▶ {}", description));

                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::ToolResult {
                    is_error,
                    error_preview,
                } => {
                    // Find the last tool calls message and update it
                    // Skip if the last tool call was a todo tool (they don't create call messages)
                    if let Some(Message::AssistantToolCalls(msg)) = self.messages.last_mut()
                        && let Some(last_tool) = msg.calls.last_mut()
                    {
                        // Don't update tool result markers for todo tools
                        if !last_tool.contains("todo_") {
                            if is_error {
                                *last_tool = last_tool.replace("▶", "✗");
                                // Add error preview as a new line
                                if let Some(preview) = error_preview {
                                    msg.calls.push(format!("  ✗ Error: {}", preview));
                                }
                            } else {
                                *last_tool = last_tool.replace("▶", "✓");
                            }
                            self.layout_cache.invalidate();
                            if width > 0 {
                                self.adjust_scroll_for_content_change(width);
                            }
                            self.mark_new_content_if_scrolled();
                        }
                    }
                }
                OutputEvent::Thinking(text) => {
                    // Check if we need a new message (current one is tool calls or text)
                    let needs_new_message = matches!(
                        self.messages.last(),
                        Some(Message::AssistantToolCalls(_)) | Some(Message::AssistantText(_))
                    );

                    if needs_new_message {
                        // Mark previous message as done streaming
                        self.finish_last_assistant_message();
                    }

                    // Append to existing thinking message or create new one
                    let msg = self.ensure_thinking_message();
                    msg.text.push_str(&text);

                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::Text(text) => {
                    // Check if we need a new message (current one is tool calls or thinking)
                    let needs_new_message = matches!(
                        self.messages.last(),
                        Some(Message::AssistantToolCalls(_)) | Some(Message::AssistantThinking(_))
                    );

                    if needs_new_message {
                        // Mark previous message as done streaming
                        self.finish_last_assistant_message();
                    }

                    // Append text to the current text message, creating one if needed
                    let msg = self.ensure_text_message();
                    msg.text.push_str(&text);

                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::Done => {
                    self.finish_last_assistant_message();

                    // Handle compaction completion
                    if self.is_compacting {
                        self.finalize_compaction();
                        // Clear context tokens - the last API call was the summarization
                        // request, not the actual conversation. The count will be updated
                        // on the next actual chat.
                        self.last_context_tokens = None;
                    } else {
                        // Store context stats for display in working indicator
                        // (only for normal chats, not compaction)
                        if let Some(ref model) = self.current_model {
                            self.last_context_tokens = get_last_input_for_provider(model.provider);
                            self.context_limit =
                                crate::provider::context_limit(model.provider, &model.model_id);
                        }

                        // Auto-save session after successful chat
                        if let Some(ref model) = self.current_model {
                            let _ = crate::session::save_session(
                                &self.working_dir,
                                &self.chat_messages,
                                &model.provider,
                                &model.model_id,
                                self.thinking_enabled,
                            );
                        }

                        // Check for queued prompts
                        if !self.pending_prompts.is_empty() {
                            let next = self.pending_prompts.pop_front().unwrap();
                            self.start_chat(next.input, next.images, next.display_text);
                        }
                    }

                    self.is_chatting = false;
                    self.is_compacting = false;
                    self.streaming_start_time = None;
                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::Error(err) => {
                    self.messages
                        .push(Message::Error(format!("Error: {}", err)));

                    // Clear queued prompts on error
                    self.pending_prompts.clear();

                    // Rollback compaction on error
                    if self.is_compacting {
                        self.rollback_compaction();
                    }

                    self.is_chatting = false;
                    self.is_compacting = false;
                    self.streaming_start_time = None;
                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::Interrupted => {
                    // Add [Interrupted] marker to a text message
                    match self.messages.last_mut() {
                        Some(Message::AssistantText(msg)) => {
                            if !msg.text.is_empty() {
                                msg.text.push_str("\n[Interrupted]");
                            } else {
                                msg.text.push_str("[Interrupted]");
                            }
                            msg.is_streaming = false;
                        }
                        Some(Message::AssistantThinking(_))
                        | Some(Message::AssistantToolCalls(_)) => {
                            // Mark current message as done and create a text message with [Interrupted]
                            self.finish_last_assistant_message();
                            self.messages.push(Message::AssistantText(TextMessage {
                                text: "[Interrupted]".to_string(),
                                is_streaming: false,
                            }));
                        }
                        _ => {
                            // No assistant message exists, create one
                            self.messages.push(Message::AssistantText(TextMessage {
                                text: "[Interrupted]".to_string(),
                                is_streaming: false,
                            }));
                        }
                    }

                    // Clear queued prompts on interrupt
                    self.pending_prompts.clear();

                    // Rollback compaction on interrupt
                    if self.is_compacting {
                        self.rollback_compaction();
                    }

                    self.is_chatting = false;
                    self.is_compacting = false;
                    self.streaming_start_time = None;
                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::WorkingProgress { total_tokens } => {
                    // Update streaming stats for display in working indicator
                    // total_tokens is already accumulated across all API calls in this turn
                    self.streaming_tokens = Some(total_tokens);
                    // Initialize display value on first progress event
                    if self.streaming_tokens_display == 0 {
                        self.streaming_tokens_display = total_tokens;
                    }
                }
                OutputEvent::TodoList { todos } => {
                    // Format display text for the todo list
                    let display_text = if todos.is_empty() {
                        "Todo list cleared.".to_string()
                    } else {
                        let mut lines = vec!["Todo List:".to_string()];
                        for item in &todos {
                            let (indicator, text) = match item.status {
                                TodoStatus::Pending => ("[ ]", &item.content),
                                TodoStatus::InProgress => ("[-]", &item.active_form),
                                TodoStatus::Completed => ("[✓]", &item.content),
                            };
                            lines.push(format!("  {} {}", indicator, text));
                        }
                        lines.join("\n")
                    };

                    self.messages.push(Message::TodoList(TodoListDisplay {
                        todos,
                        display_text,
                    }));

                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::FileDiff {
                    path,
                    diff,
                    lines_added,
                    lines_removed,
                    language,
                } => {
                    if self.show_diffs {
                        self.messages.push(Message::FileDiff(DiffMessage {
                            _path: path,
                            diff,
                            _lines_added: lines_added,
                            _lines_removed: lines_removed,
                            language,
                        }));

                        self.layout_cache.invalidate();
                        if width > 0 {
                            self.adjust_scroll_for_content_change(width);
                        }
                        self.mark_new_content_if_scrolled();
                    }
                }
                OutputEvent::ThinkingStart => {
                    self.is_thinking = true;

                    // Check if we need a new message (current one is tool calls or text)
                    let needs_new_message = matches!(
                        self.messages.last(),
                        Some(Message::AssistantToolCalls(_)) | Some(Message::AssistantText(_))
                    );

                    if needs_new_message {
                        // Mark previous message as done streaming
                        self.finish_last_assistant_message();
                    }

                    // Create or ensure a thinking message exists
                    self.ensure_thinking_message();

                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::ThinkingEnd => {
                    self.is_thinking = false;
                }
                OutputEvent::AutoCompactStarting {
                    current_usage,
                    limit,
                } => {
                    let pct = (current_usage as f64 / limit as f64) * 100.0;
                    self.messages.push(Message::Text(format!(
                        "Context at {:.0}% ({}/{}) - auto-compacting...",
                        pct, current_usage, limit
                    )));
                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::AutoCompactCompleted { messages_compacted } => {
                    self.messages.push(Message::Text(format!(
                        "Compacted {} messages into summary.",
                        messages_compacted
                    )));
                    self.layout_cache.invalidate();
                    if width > 0 {
                        self.adjust_scroll_for_content_change(width);
                    }
                    self.mark_new_content_if_scrolled();
                }
                OutputEvent::TextEnd
                | OutputEvent::SpinnerStart(_)
                | OutputEvent::SpinnerStop
                | OutputEvent::Info(_) => {}
            }
        }

        updated
    }
}

/// Get last input tokens for a provider, if usage tracking is available
fn get_last_input_for_provider(provider: crate::providers::ModelProvider) -> Option<u64> {
    let input = match provider {
        crate::providers::ModelProvider::Claude => crate::usage::anthropic().last_input(),
        crate::providers::ModelProvider::OpenCodeZen => crate::usage::zen().last_input(),
        crate::providers::ModelProvider::OpenAiCompat => crate::usage::openai_compat().last_input(),
        crate::providers::ModelProvider::OpenAi => crate::usage::openai().last_input(),
        _ => return None,
    };
    // Only return if we actually have usage data
    if input > 0 { Some(input) } else { None }
}
