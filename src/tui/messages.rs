// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

pub(crate) enum Message {
    Text(String),
    Error(String),
    Warning(String),
    User(UserMessage),
    AssistantThinking(ThinkingMessage),
    AssistantToolCalls(ToolCallsMessage),
    AssistantText(TextMessage),
    Shell(ShellMessage),
    Usage(UsageDisplay),
    TodoList(TodoListDisplay),
    FileDiff(DiffMessage),
}

pub(crate) struct UsageDisplay {
    pub limits: crate::usage::RateLimits,
    pub display_text: String,
}

pub(crate) struct TodoListDisplay {
    pub todos: Vec<crate::tools::TodoItem>,
    pub display_text: String,
}

pub(crate) struct DiffMessage {
    pub _path: String,
    pub diff: String,
    pub _lines_added: usize,
    pub _lines_removed: usize,
    /// Language for syntax highlighting, derived from file extension
    pub language: Option<String>,
}

pub(crate) struct ThinkingMessage {
    pub text: String,
    pub is_streaming: bool,
}

pub(crate) struct ToolCallsMessage {
    pub calls: Vec<String>,
    pub is_streaming: bool,
}

pub(crate) struct TextMessage {
    pub text: String,
    pub is_streaming: bool,
}

pub(crate) struct UserMessage {
    pub display_text: String,
}

pub(crate) struct ShellMessage {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub status: Option<i32>,
    pub display: String,
    pub running: bool, // true if command is still running
}

/// Events sent from the shell command thread
pub(crate) enum ShellEvent {
    /// A line of output (stdout or stderr)
    Output { message_idx: usize, line: String },
    /// The command has completed with the given exit status
    Done {
        message_idx: usize,
        status: Option<i32>,
    },
    /// Usage rate limits fetched successfully
    UsageData(crate::usage::RateLimits),
    /// Usage fetch failed
    UsageError(String),
    /// Context ready - open in external pager (bat)
    ContextReady(String, crate::providers::ProviderManager),
    /// Context preparation failed with provider returned
    ContextError(String, crate::providers::ProviderManager),
    /// Token count result - display inline
    TokenCount(String, crate::providers::ProviderManager),
    /// Token count failed
    TokenCountError(String, crate::providers::ProviderManager),
}

/// Format an error message for display, pretty-printing any embedded JSON
pub(crate) fn format_error_message(err: &str) -> String {
    // Check if there's a JSON object in the error message
    if let Some(json_start) = err.find('{')
        && let Some(json_end) = err.rfind('}')
    {
        let json_str = &err[json_start..=json_end];
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(json_str) {
            let prefix = err[..json_start].trim();
            let suffix = err[json_end + 1..].trim();
            let pretty_json =
                serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| json_str.to_string());

            let mut result = String::new();
            if !prefix.is_empty() {
                result.push_str(prefix);
                result.push('\n');
            }
            result.push_str(&pretty_json);
            if !suffix.is_empty() {
                result.push('\n');
                result.push_str(suffix);
            }
            return result;
        }
    }
    err.to_string()
}

/// Convert text to bulleted format for display
pub(crate) fn bulletify(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= 1 {
        return text.to_string();
    }

    let mut result = String::with_capacity(text.len() + lines.len() * 2);
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        result.push_str(line);
    }
    result
}

/// Format shell command output for display
pub(crate) fn format_shell_display(
    command: &str,
    stdout: &str,
    stderr: &str,
    status: Option<i32>,
    running: bool,
) -> String {
    let mut display = format!("! {command}");
    if !stdout.is_empty() {
        display.push('\n');
        for line in stdout.lines() {
            display.push_str("  ");
            display.push_str(line);
            display.push('\n');
        }
        // Remove trailing newline
        display.pop();
    }
    if !stderr.is_empty() {
        display.push('\n');
        for line in stderr.lines() {
            display.push_str("  ");
            display.push_str(line);
            display.push('\n');
        }
        display.pop();
    }
    if running {
        display.push_str("\n  ...");
    } else if let Some(code) = status
        && code != 0
    {
        display.push_str(&format!("\n  [exit {}]", code));
    }
    display
}
