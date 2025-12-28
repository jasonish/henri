// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::sync::Arc;

use tokio::sync::mpsc;

/// Unified output events for CLI and TUI rendering
#[derive(Debug, Clone)]
pub(crate) enum OutputEvent {
    /// Thinking/reasoning started
    ThinkingStart,
    /// Thinking/reasoning text delta
    Thinking(String),
    /// Thinking/reasoning ended
    ThinkingEnd,
    /// Response text delta
    Text(String),
    /// Response text ended
    TextEnd,
    /// Tool is being called
    ToolCall { description: String },
    /// Tool execution completed
    ToolResult {
        is_error: bool,
        error_preview: Option<String>,
    },
    /// Start spinner with message
    SpinnerStart(String),
    /// Stop spinner
    SpinnerStop,
    /// Informational message
    Info(String),
    /// Error message
    Error(String),
    /// Waiting for model response
    Waiting,
    /// Model finished responding
    Done,
    /// Interaction was interrupted
    Interrupted,
    /// Progress update during streaming
    WorkingProgress { total_tokens: u64 },
    /// Todo list updated
    TodoList { todos: Vec<crate::tools::TodoItem> },
    /// File was modified, here's the diff
    FileDiff {
        path: String,
        diff: String,
        lines_added: usize,
        lines_removed: usize,
    },
    /// Auto-compaction is starting
    AutoCompactStarting { current_usage: u64, limit: u64 },
    /// Auto-compaction completed
    AutoCompactCompleted { messages_compacted: usize },
}

/// Trait for listening to output events
pub(crate) trait OutputListener: Send + Sync {
    fn on_event(&self, event: &OutputEvent);
}

/// Context for emitting output events. Cheap to clone (Arc internally).
#[derive(Clone)]
pub(crate) struct OutputContext {
    listener: Option<Arc<dyn OutputListener>>,
    event_sender: Option<mpsc::UnboundedSender<OutputEvent>>,
}

impl OutputContext {
    /// Create a new output context for CLI mode with a listener
    pub(crate) fn new_cli(listener: Arc<dyn OutputListener>) -> Self {
        Self {
            listener: Some(listener),
            event_sender: None,
        }
    }

    /// Create a new output context for TUI mode with an event channel
    pub(crate) fn new_tui(event_sender: mpsc::UnboundedSender<OutputEvent>) -> Self {
        Self {
            listener: None,
            event_sender: Some(event_sender),
        }
    }

    /// Create a quiet output context (errors only)
    pub(crate) fn new_quiet() -> Self {
        Self {
            listener: Some(Arc::new(crate::cli::listener::QuietListener::new())),
            event_sender: None,
        }
    }

    /// Create a null output context that discards all events (for tests)
    #[cfg(test)]
    pub(crate) fn null() -> Self {
        Self {
            listener: None,
            event_sender: None,
        }
    }

    /// Emit an output event to both listener and channel
    pub(crate) fn emit(&self, event: OutputEvent) {
        // Call listener synchronously
        if let Some(listener) = &self.listener {
            listener.on_event(&event);
        }

        // Send to channel for TUI
        if let Some(sender) = &self.event_sender {
            let _ = sender.send(event);
        }
    }
}

pub(crate) fn menu_page_size() -> usize {
    use terminal_size::terminal_size;
    terminal_size()
        .map(|(_, h)| (h.0 as usize) / 3)
        .unwrap_or(10)
        .max(5)
}

pub(crate) fn print_thinking_start(ctx: &OutputContext) {
    ctx.emit(OutputEvent::ThinkingStart);
}

pub(crate) fn print_thinking(ctx: &OutputContext, text: &str) {
    ctx.emit(OutputEvent::Thinking(text.to_string()));
}

pub(crate) fn print_thinking_end(ctx: &OutputContext) {
    ctx.emit(OutputEvent::ThinkingEnd);
}

/// Tracks thinking/reasoning state during streaming with automatic cleanup.
pub(crate) struct ThinkingState<'a> {
    ctx: &'a OutputContext,
    active: bool,
}

impl<'a> ThinkingState<'a> {
    pub(crate) fn new(ctx: &'a OutputContext) -> Self {
        Self { ctx, active: false }
    }

    /// Emit thinking text. Starts the block automatically if needed.
    pub(crate) fn emit(&mut self, text: &str) {
        if !self.active {
            print_thinking_start(self.ctx);
            self.active = true;
        }
        print_thinking(self.ctx, text);
    }

    /// End the thinking block if active. Safe to call multiple times.
    pub(crate) fn end(&mut self) {
        if self.active {
            print_thinking_end(self.ctx);
            self.active = false;
        }
    }
}

impl<'a> Drop for ThinkingState<'a> {
    fn drop(&mut self) {
        self.end();
    }
}

pub(crate) fn print_text(ctx: &OutputContext, text: &str) {
    ctx.emit(OutputEvent::Text(text.to_string()));
}

pub(crate) fn print_text_end(ctx: &OutputContext) {
    ctx.emit(OutputEvent::TextEnd);
}

pub(crate) fn start_spinner(ctx: &OutputContext, message: &str) {
    ctx.emit(OutputEvent::SpinnerStart(message.to_string()));
}

pub(crate) fn stop_spinner(ctx: &OutputContext) {
    ctx.emit(OutputEvent::SpinnerStop);
}

/// Print a tool call announcement
pub(crate) fn print_tool_call(ctx: &OutputContext, _name: &str, description: &str) {
    ctx.emit(OutputEvent::ToolCall {
        description: description.to_string(),
    });
}

/// Print a tool result
pub(crate) fn print_tool_result(
    ctx: &OutputContext,
    is_error: bool,
    error_preview: Option<String>,
) {
    ctx.emit(OutputEvent::ToolResult {
        is_error,
        error_preview,
    });
}

/// Emit waiting for model response
pub(crate) fn emit_waiting(ctx: &OutputContext) {
    ctx.emit(OutputEvent::Waiting);
}

/// Emit model finished responding
pub(crate) fn emit_done(ctx: &OutputContext) {
    ctx.emit(OutputEvent::Done);
}

/// Emit interaction interrupted
pub(crate) fn emit_interrupted(ctx: &OutputContext) {
    ctx.emit(OutputEvent::Interrupted);
}

/// Emit working progress update with streaming stats
pub(crate) fn emit_working_progress(
    ctx: &OutputContext,
    total_tokens: u64,
    _duration_secs: f64,
    _tokens_per_sec: f64,
) {
    ctx.emit(OutputEvent::WorkingProgress { total_tokens });
}

/// Emit error message
pub(crate) fn emit_error(ctx: &OutputContext, message: &str) {
    ctx.emit(OutputEvent::Error(message.to_string()));
}

/// Emit todo list update
pub(crate) fn emit_todo_list(ctx: &OutputContext, todos: Vec<crate::tools::TodoItem>) {
    ctx.emit(OutputEvent::TodoList { todos });
}

/// Emit auto-compaction starting
pub(crate) fn emit_auto_compact_starting(ctx: &OutputContext, current_usage: u64, limit: u64) {
    ctx.emit(OutputEvent::AutoCompactStarting {
        current_usage,
        limit,
    });
}

/// Emit auto-compaction completed
pub(crate) fn emit_auto_compact_completed(ctx: &OutputContext, messages_compacted: usize) {
    ctx.emit(OutputEvent::AutoCompactCompleted { messages_compacted });
}
