// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Session state persistence for Henri.
//!
//! Supports multiple sessions per directory. Each session is stored in a
//! separate file within a directory named after the hash of the working directory.
//!
//! Structure:
//! ```text
//! ~/.cache/henri/sessions/
//!   {dir_hash}/                    # Directory per working directory
//!     {session_id}.json            # One file per session
//! ```

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::provider::{ContentBlock, Message, MessageContent, Role};
use crate::providers::ModelProvider;
use crate::tools::TodoItem;

use crate::tools::format_tool_call_description;
use crate::tools::todo::{clear_todos, get_todos, set_todos};

const SESSION_VERSION: u32 = 2;

/// Session metadata stored as the first line of the JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionMeta {
    /// Version for future compatibility
    pub version: u32,

    /// Unique session identifier (timestamp-based)
    /// Defaults to empty string for backward compatibility with v1 sessions.
    #[serde(default)]
    pub session_id: String,

    /// Working directory this session is for
    pub working_directory: PathBuf,

    /// When the session was last saved
    pub saved_at: DateTime<Utc>,

    /// Provider identifier (e.g., "claude", "copilot")
    pub provider: String,

    /// Model identifier (e.g., "claude-opus-4-5")
    pub model_id: String,

    /// Whether thinking/reasoning is enabled
    pub thinking_enabled: bool,

    /// Whether read-only mode is enabled
    #[serde(default)]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,

    /// Current todo list state
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todos: Option<Vec<TodoItem>>,
}

/// Summary info for session listing (without loading full messages)
#[derive(Debug, Clone)]
pub(crate) struct SessionInfo {
    pub id: String,
    pub saved_at: DateTime<Utc>,
    pub _model_id: String,
    pub _message_count: usize,
    /// First user message (truncated) for preview
    pub preview: Option<String>,
}

/// Session state loaded from disk (metadata + messages).
#[derive(Debug, Clone)]
pub(crate) struct SessionState {
    pub meta: SessionMeta,
    pub messages: Vec<SerializableMessage>,
}

/// Restored session ready to be used by CLI.
/// Contains the converted messages and settings.
#[derive(Debug, Clone)]
pub(crate) struct RestoredSession {
    pub session_id: String,
    pub messages: Vec<Message>,
    pub provider: String,
    pub model_id: String,
    pub thinking_enabled: bool,
    pub read_only: bool,
    pub _state: SessionState, // Keep original state for replay
}

impl RestoredSession {
    /// Create from a loaded SessionState.
    pub(crate) fn from_state(state: &SessionState) -> Self {
        // Restore todo state if present
        if let Some(todos) = &state.meta.todos {
            set_todos(todos.clone());
        }

        Self {
            session_id: state.meta.session_id.clone(),
            messages: restore_messages(state),
            provider: state.meta.provider.clone(),
            model_id: state.meta.model_id.clone(),
            thinking_enabled: state.meta.thinking_enabled,
            read_only: state.meta.read_only,
            _state: state.clone(),
        }
    }
}

/// A message that can be serialized to JSON efficiently.
/// Images are stored as base64 strings instead of byte arrays.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SerializableMessage {
    pub role: Role,
    pub content: SerializableContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SerializableContent {
    Text(String),
    Blocks(Vec<SerializableContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum SerializableContentBlock {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        data: String, // Base64 encoded
    },
    Thinking {
        thinking: String,
        // Legacy field for backward compatibility with v1 sessions
        #[serde(default, skip_serializing)]
        signature: Option<String>,
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
        #[serde(default)]
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    Summary {
        summary: String,
        messages_compacted: usize,
    },
}

impl From<&Message> for SerializableMessage {
    fn from(msg: &Message) -> Self {
        SerializableMessage {
            role: msg.role,
            content: (&msg.content).into(),
        }
    }
}

impl From<&MessageContent> for SerializableContent {
    fn from(content: &MessageContent) -> Self {
        match content {
            MessageContent::Text(s) => SerializableContent::Text(s.clone()),
            MessageContent::Blocks(blocks) => {
                SerializableContent::Blocks(blocks.iter().map(|b| b.into()).collect())
            }
        }
    }
}

impl From<&ContentBlock> for SerializableContentBlock {
    fn from(block: &ContentBlock) -> Self {
        match block {
            ContentBlock::Text { text } => SerializableContentBlock::Text { text: text.clone() },
            ContentBlock::Image { mime_type, data } => SerializableContentBlock::Image {
                mime_type: mime_type.clone(),
                data: base64::engine::general_purpose::STANDARD.encode(data),
            },
            ContentBlock::Thinking {
                thinking,
                provider_data,
            } => SerializableContentBlock::Thinking {
                thinking: thinking.clone(),
                signature: None,
                provider_data: provider_data.clone(),
            },
            ContentBlock::ToolUse {
                id,
                name,
                input,
                thought_signature,
            } => SerializableContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                thought_signature: thought_signature.clone(),
            },
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => SerializableContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                is_error: *is_error,
            },
            ContentBlock::Summary {
                summary,
                messages_compacted,
            } => SerializableContentBlock::Summary {
                summary: summary.clone(),
                messages_compacted: *messages_compacted,
            },
        }
    }
}

impl From<&SerializableMessage> for Message {
    fn from(msg: &SerializableMessage) -> Self {
        Message {
            role: msg.role,
            content: (&msg.content).into(),
        }
    }
}

impl From<&SerializableContent> for MessageContent {
    fn from(content: &SerializableContent) -> Self {
        match content {
            SerializableContent::Text(s) => MessageContent::Text(s.clone()),
            SerializableContent::Blocks(blocks) => {
                MessageContent::Blocks(blocks.iter().map(|b| b.into()).collect())
            }
        }
    }
}

impl From<&SerializableContentBlock> for ContentBlock {
    fn from(block: &SerializableContentBlock) -> Self {
        match block {
            SerializableContentBlock::Text { text } => ContentBlock::Text { text: text.clone() },
            SerializableContentBlock::Image { mime_type, data } => ContentBlock::Image {
                mime_type: mime_type.clone(),
                data: base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .unwrap_or_default(),
            },
            SerializableContentBlock::Thinking {
                thinking,
                signature,
                provider_data,
            } => {
                // Migrate legacy signature field to provider_data if needed
                let provider_data = if provider_data.is_some() {
                    provider_data.clone()
                } else if let Some(sig) = signature
                    && !sig.is_empty()
                {
                    Some(serde_json::json!({"signature": sig}))
                } else {
                    None
                };
                ContentBlock::Thinking {
                    thinking: thinking.clone(),
                    provider_data,
                }
            }
            SerializableContentBlock::ToolUse {
                id,
                name,
                input,
                thought_signature,
            } => ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                thought_signature: thought_signature.clone(),
            },
            SerializableContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => ContentBlock::ToolResult {
                tool_use_id: tool_use_id.clone(),
                content: content.clone(),
                is_error: *is_error,
            },
            SerializableContentBlock::Summary {
                summary,
                messages_compacted,
            } => ContentBlock::Summary {
                summary: summary.clone(),
                messages_compacted: *messages_compacted,
            },
        }
    }
}

/// Get the base sessions directory path.
fn sessions_base_dir() -> PathBuf {
    // Allow override via environment variable (used by tests)
    if let Ok(dir) = std::env::var("HENRI_SESSIONS_DIR") {
        return PathBuf::from(dir);
    }

    dirs::home_dir()
        .map(|home| home.join(".cache").join("henri").join("sessions"))
        .unwrap_or_else(|| PathBuf::from(".cache/henri/sessions"))
}

/// Get the sessions directory for a specific working directory.
fn sessions_dir_for_path(dir: &Path) -> PathBuf {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let path_str = canonical.to_string_lossy();

    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let hash = hasher.finalize();
    let hash_str = format!("{:x}", hash);
    let short_hash = &hash_str[..16];

    sessions_base_dir().join(short_hash)
}

/// Get the path for a specific session file.
fn get_session_path(dir: &Path, session_id: &str) -> PathBuf {
    sessions_dir_for_path(dir).join(format!("{}.json", session_id))
}

/// Generate a new session ID.
///
/// Uses UUIDv7 for time-sortable uniqueness.
pub(crate) fn generate_session_id() -> String {
    Uuid::now_v7().to_string()
}

/// Save session state to disk in JSONL format.
/// Line 1: Session metadata
/// Lines 2+: One message per line
///
/// If `session_id` is None, generates a new session ID.
/// Returns the session ID used.
pub(crate) fn save_session(
    working_directory: &Path,
    messages: &[Message],
    provider: &ModelProvider,
    model_id: &str,
    thinking_enabled: bool,
    read_only: bool,
    session_id: Option<&str>,
) -> std::io::Result<String> {
    let session_id = session_id
        .map(|s| s.to_string())
        .unwrap_or_else(generate_session_id);
    let session_path = get_session_path(working_directory, &session_id);

    // Ensure the sessions directory exists
    if let Some(parent) = session_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Get current todos
    let todos = get_todos();
    let todos = if todos.is_empty() { None } else { Some(todos) };

    let meta = SessionMeta {
        version: SESSION_VERSION,
        session_id: session_id.clone(),
        working_directory: working_directory.to_path_buf(),
        saved_at: Utc::now(),
        provider: provider.id().to_string(),
        model_id: model_id.to_string(),
        thinking_enabled,
        read_only,
        todos,
    };

    let mut file = File::create(&session_path)?;

    // Write metadata as first line
    let meta_json = serde_json::to_string(&meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(file, "{}", meta_json)?;

    // Write each message as a separate line
    for msg in messages {
        let serializable: SerializableMessage = msg.into();
        let msg_json = serde_json::to_string(&serializable)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(file, "{}", msg_json)?;
    }

    Ok(session_id)
}

/// Load the most recent session for a directory.
/// Returns None if no sessions exist.
pub(crate) fn load_session(dir: &Path) -> Option<SessionState> {
    let sessions = list_sessions(dir);
    if sessions.is_empty() {
        return None;
    }
    // Sessions are sorted newest first
    load_session_by_id(dir, &sessions[0].id)
}

/// Load a specific session by ID.
pub(crate) fn load_session_by_id(dir: &Path, session_id: &str) -> Option<SessionState> {
    let session_path = get_session_path(dir, session_id);
    load_session_from_path(&session_path)
}

/// Load session state from a specific path.
fn load_session_from_path(path: &Path) -> Option<SessionState> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // First line is metadata
    let meta_line = lines.next()?.ok()?;
    let meta: SessionMeta = serde_json::from_str(&meta_line).ok()?;

    // Validate version
    if meta.version > SESSION_VERSION {
        eprintln!(
            "Warning: Session file has newer version ({}), ignoring.",
            meta.version
        );
        return None;
    }

    // Remaining lines are messages
    let mut messages = Vec::new();
    for line in lines {
        let line = line.ok()?;
        if line.is_empty() {
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<SerializableMessage>(&line) {
            messages.push(msg);
        }
    }

    Some(SessionState { meta, messages })
}

/// List all sessions for a directory, sorted by recency (newest first).
pub(crate) fn list_sessions(dir: &Path) -> Vec<SessionInfo> {
    let sessions_dir = sessions_dir_for_path(dir);
    let mut sessions = Vec::new();

    let Ok(entries) = fs::read_dir(&sessions_dir) else {
        return sessions;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && let Some(info) = load_session_info(&path)
        {
            sessions.push(info);
        }
    }

    // Sort by saved_at descending (newest first)
    sessions.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
    sessions
}

/// Load just the session info (metadata + message count + preview) without loading all messages.
fn load_session_info(path: &Path) -> Option<SessionInfo> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // First line is metadata
    let meta_line = lines.next()?.ok()?;
    let meta: SessionMeta = serde_json::from_str(&meta_line).ok()?;

    // For v1 sessions, derive ID from filename (strip extension)
    let session_id = if meta.session_id.is_empty() {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        meta.session_id
    };

    // Count messages and find first user message for preview
    let mut message_count = 0;
    let mut preview = None;

    for line in lines {
        let Ok(line) = line else { continue };
        if line.is_empty() {
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<SerializableMessage>(&line) {
            message_count += 1;
            // Get first user message as preview
            if preview.is_none() && msg.role == Role::User {
                preview = extract_preview(&msg.content);
            }
        }
    }

    Some(SessionInfo {
        id: session_id,
        saved_at: meta.saved_at,
        _model_id: meta.model_id,
        _message_count: message_count,
        preview,
    })
}

/// Extract a preview string from message content, truncated to ~60 chars.
fn extract_preview(content: &SerializableContent) -> Option<String> {
    let text = match content {
        SerializableContent::Text(s) => s.clone(),
        SerializableContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                SerializableContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
    };

    if text.is_empty() {
        return None;
    }

    // Take first line and truncate (char-safe)
    let first_line = text.lines().next().unwrap_or(&text);
    Some(truncate_str(first_line, 60))
}

/// Truncate a string to at most `max_chars` characters, appending "…" if truncated.
pub(crate) fn truncate_str(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{}…", truncated)
    }
}

/// Delete a specific session by ID.
#[cfg(test)]
pub(crate) fn delete_session(dir: &Path, session_id: &str) -> std::io::Result<()> {
    let session_path = get_session_path(dir, session_id);
    if session_path.exists() {
        fs::remove_file(&session_path)?;
    }
    Ok(())
}

fn restore_messages(state: &SessionState) -> Vec<Message> {
    state.messages.iter().map(|m| m.into()).collect()
}

/// Format the session age in a human-readable way.
pub(crate) fn format_age(saved_at: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(*saved_at);

    if duration.num_days() > 0 {
        let days = duration.num_days();
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{} days ago", days)
        }
    } else if duration.num_hours() > 0 {
        let hours = duration.num_hours();
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else if duration.num_minutes() > 0 {
        let minutes = duration.num_minutes();
        if minutes == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{} minutes ago", minutes)
        }
    } else {
        "just now".to_string()
    }
}

/// Replay session messages into the CLI output history so the UI matches as closely
/// as possible what the user would have seen if the session had continued live.
///
/// This is intended to be called from the interactive CLI (TUI) after the prompt is
/// visible, so output is rendered above the prompt and is resizable.
pub(crate) fn replay_session_into_output(state: &SessionState) {
    use std::collections::HashMap;

    use crate::cli::history::{self, HistoryEvent};
    // Use the same marker format as the interactive CLI ("Image#1", "Image#2", ...).
    const IMAGE_MARKER_PREFIX: &str = "Image#";

    // Clear existing history and todos so the replay is the authoritative view.
    history::clear();
    clear_todos();
    if let Some(todos) = &state.meta.todos {
        set_todos(todos.clone());
    }

    // Header similar to the old stdout replay, but rendered as Info events.
    history::push(HistoryEvent::Info(
        format!(
            "Model: {} (thinking {})",
            state.meta.model_id,
            if state.meta.thinking_enabled {
                "enabled"
            } else {
                "disabled"
            }
        )
        .dimmed()
        .to_string(),
    ));
    history::push(HistoryEvent::Info(
        format!(
            "Saved: {} · Messages: {}",
            format_age(&state.meta.saved_at),
            state.messages.len()
        )
        .dimmed()
        .to_string(),
    ));
    history::push(HistoryEvent::Info("".to_string()));

    let mut in_tool_block = false;
    let mut idx = 0;

    while idx < state.messages.len() {
        let msg = &state.messages[idx];
        match msg.role {
            Role::System => {
                idx += 1;
                continue;
            }
            Role::User => {
                // Tool-result-only messages are processed with the preceding assistant message,
                // so skip them here.
                if is_tool_result_only_serializable(msg) {
                    idx += 1;
                    continue;
                }

                // Close any dangling tool block boundary.
                if in_tool_block {
                    history::push(HistoryEvent::ToolEnd);
                    in_tool_block = false;
                }

                let (text, images) = user_text_and_images(msg, IMAGE_MARKER_PREFIX);
                history::push_user_prompt(&text, images);
            }
            Role::Assistant => {
                // Look ahead for a tool-result-only user message to match results with uses.
                let tool_results: HashMap<String, (bool, String)> = if let Some(next_msg) =
                    state.messages.get(idx + 1)
                    && is_tool_result_only_serializable(next_msg)
                {
                    collect_tool_results(next_msg)
                } else {
                    HashMap::new()
                };

                // Render blocks in the same order as live output.
                match &msg.content {
                    SerializableContent::Text(text) => {
                        if in_tool_block {
                            history::push(HistoryEvent::ToolEnd);
                            in_tool_block = false;
                        }
                        history::push(HistoryEvent::AssistantText {
                            text: text.clone(),
                            is_streaming: false,
                        });
                        history::push(HistoryEvent::ResponseEnd);
                    }
                    SerializableContent::Blocks(blocks) => {
                        for block in blocks {
                            match block {
                                SerializableContentBlock::Thinking { thinking, .. } => {
                                    if in_tool_block {
                                        history::push(HistoryEvent::ToolEnd);
                                        in_tool_block = false;
                                    }
                                    history::push(HistoryEvent::Thinking {
                                        text: thinking.clone(),
                                        is_streaming: false,
                                    });
                                    history::push(HistoryEvent::ThinkingEnd);
                                }
                                SerializableContentBlock::Text { text } => {
                                    if in_tool_block {
                                        history::push(HistoryEvent::ToolEnd);
                                        in_tool_block = false;
                                    }
                                    history::push(HistoryEvent::AssistantText {
                                        text: text.clone(),
                                        is_streaming: false,
                                    });
                                }
                                SerializableContentBlock::ToolUse {
                                    id, name, input, ..
                                } => {
                                    if !in_tool_block {
                                        history::push(HistoryEvent::ToolStart);
                                        in_tool_block = true;
                                    }
                                    let description = format_tool_call_description(name, input);
                                    history::push(HistoryEvent::ToolUse { description });

                                    // Push matching ToolResult from the lookahead if available.
                                    if let Some((is_error, content)) = tool_results.get(id) {
                                        let output = if *is_error {
                                            content.lines().next().unwrap_or("").to_string()
                                        } else {
                                            String::new()
                                        };
                                        history::push(HistoryEvent::ToolResult {
                                            output,
                                            is_error: *is_error,
                                        });
                                    }
                                }
                                SerializableContentBlock::ToolResult {
                                    is_error, content, ..
                                } => {
                                    // Tool results in saved sessions include full content; live UI only shows
                                    // the ✓/✗ indicator. Store just the first line for error previews.
                                    let output = if *is_error {
                                        content.lines().next().unwrap_or("").to_string()
                                    } else {
                                        String::new()
                                    };
                                    history::push(HistoryEvent::ToolResult {
                                        output,
                                        is_error: *is_error,
                                    });
                                }
                                SerializableContentBlock::Summary {
                                    summary,
                                    messages_compacted,
                                } => {
                                    if in_tool_block {
                                        history::push(HistoryEvent::ToolEnd);
                                        in_tool_block = false;
                                    }
                                    // Mirror the old replay display as an Info block.
                                    history::push(HistoryEvent::Info(
                                        format!(
                                            "── Compacted {} messages ──\n{}",
                                            messages_compacted, summary
                                        )
                                        .dimmed()
                                        .to_string(),
                                    ));
                                }
                                SerializableContentBlock::Image { .. } => {
                                    // Assistant images are not currently displayed in the CLI history.
                                }
                            }
                        }

                        // If we ended a tool loop in this assistant message, close the block.
                        if in_tool_block {
                            history::push(HistoryEvent::ToolEnd);
                            in_tool_block = false;
                        }

                        // End of assistant message block.
                        history::push(HistoryEvent::ResponseEnd);
                    }
                }
            }
        }
        idx += 1;
    }

    // Keep history in a consistent state.
    if in_tool_block {
        history::push(HistoryEvent::ToolEnd);
    }

    // If we restored todos, emit them into history so redraw-from-history includes them.
    let current_todos = get_todos();
    if !current_todos.is_empty() {
        use crate::cli::history::{TodoItem as HistoryTodoItem, TodoStatus as HistoryTodoStatus};
        let items = current_todos
            .iter()
            .map(|t| HistoryTodoItem {
                content: if matches!(t.status, crate::tools::TodoStatus::InProgress) {
                    t.active_form.clone()
                } else {
                    t.content.clone()
                },
                status: match t.status {
                    crate::tools::TodoStatus::Pending => HistoryTodoStatus::Pending,
                    crate::tools::TodoStatus::InProgress => HistoryTodoStatus::InProgress,
                    crate::tools::TodoStatus::Completed => HistoryTodoStatus::Completed,
                },
            })
            .collect();
        history::push(HistoryEvent::TodoList { items });
    }
}

/// Collect tool results from a tool-result-only user message into a map of tool_use_id -> (is_error, content).
fn collect_tool_results(
    msg: &SerializableMessage,
) -> std::collections::HashMap<String, (bool, String)> {
    let mut results = std::collections::HashMap::new();
    if let SerializableContent::Blocks(blocks) = &msg.content {
        for block in blocks {
            if let SerializableContentBlock::ToolResult {
                tool_use_id,
                is_error,
                content,
            } = block
            {
                results.insert(tool_use_id.clone(), (*is_error, content.clone()));
            }
        }
    }
    results
}

fn is_tool_result_only_serializable(msg: &SerializableMessage) -> bool {
    if msg.role != Role::User {
        return false;
    }
    match &msg.content {
        SerializableContent::Text(_) => false,
        SerializableContent::Blocks(blocks) => {
            !blocks.is_empty()
                && blocks
                    .iter()
                    .all(|b| matches!(b, SerializableContentBlock::ToolResult { .. }))
        }
    }
}

fn user_text_and_images(
    msg: &SerializableMessage,
    image_marker_prefix: &str,
) -> (String, Vec<crate::cli::history::ImageMeta>) {
    match &msg.content {
        SerializableContent::Text(text) => (text.clone(), vec![]),
        SerializableContent::Blocks(blocks) => {
            let mut text_parts = Vec::new();
            let mut images: Vec<crate::cli::history::ImageMeta> = Vec::new();

            for block in blocks {
                match block {
                    SerializableContentBlock::Text { text } => text_parts.push(text.clone()),
                    SerializableContentBlock::Image { mime_type, data } => {
                        images.push(crate::cli::history::ImageMeta {
                            _marker: format!("{}{}", image_marker_prefix, images.len() + 1),
                            _mime_type: mime_type.clone(),
                            _size_bytes: data.len(),
                        });
                    }
                    // User-side Summary blocks are preserved in sessions; render them as part of the prompt.
                    SerializableContentBlock::Summary { summary, .. } => {
                        text_parts.push(summary.clone())
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Mutex to serialize tests that use HENRI_SESSIONS_DIR environment variable
    static SESSION_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Helper to set up a temp sessions directory for tests.
    /// Returns a guard that will clear the env var when dropped.
    struct TestSessionsDir {
        _temp_dir: TempDir,
    }

    impl TestSessionsDir {
        fn new() -> Self {
            let temp_dir = TempDir::new().unwrap();
            // SAFETY: We hold SESSION_TEST_LOCK, so no concurrent env access in session tests
            unsafe {
                std::env::set_var("HENRI_SESSIONS_DIR", temp_dir.path());
            }
            Self {
                _temp_dir: temp_dir,
            }
        }
    }

    impl Drop for TestSessionsDir {
        fn drop(&mut self) {
            // SAFETY: We hold SESSION_TEST_LOCK, so no concurrent env access in session tests
            unsafe {
                std::env::remove_var("HENRI_SESSIONS_DIR");
            }
        }
    }

    #[test]
    fn test_replay_into_output_builds_history_events() {
        // Ensure replay populates the CLI history with user/assistant/tool events.
        // This is a unit test for the semantic event generation, not terminal rendering.
        crate::cli::history::clear();
        crate::tools::todo::clear_todos();

        let state = SessionState {
            meta: SessionMeta {
                version: SESSION_VERSION,
                session_id: "test".to_string(),
                working_directory: PathBuf::from("/tmp"),
                saved_at: Utc::now(),
                provider: "claude".to_string(),
                model_id: "claude-sonnet-4".to_string(),
                thinking_enabled: true,
                read_only: false,
                todos: Some(vec![TodoItem {
                    content: "Do the thing".to_string(),
                    status: crate::tools::TodoStatus::Pending,
                    active_form: "Doing the thing".to_string(),
                }]),
            },
            messages: vec![
                SerializableMessage {
                    role: Role::User,
                    content: SerializableContent::Text("Hello".to_string()),
                },
                SerializableMessage {
                    role: Role::Assistant,
                    content: SerializableContent::Blocks(vec![
                        SerializableContentBlock::Thinking {
                            thinking: "Thinking...".to_string(),
                            signature: None,
                            provider_data: None,
                        },
                        SerializableContentBlock::ToolUse {
                            id: "tool1".to_string(),
                            name: "bash".to_string(),
                            input: serde_json::json!({"command": "echo hi"}),
                            thought_signature: None,
                        },
                        SerializableContentBlock::ToolResult {
                            tool_use_id: "tool1".to_string(),
                            content: "hi\n".to_string(),
                            is_error: false,
                        },
                        SerializableContentBlock::Text {
                            text: "Done".to_string(),
                        },
                    ]),
                },
            ],
        };

        replay_session_into_output(&state);

        let events = crate::cli::history::snapshot();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, crate::cli::history::HistoryEvent::UserPrompt { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, crate::cli::history::HistoryEvent::AssistantText { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, crate::cli::history::HistoryEvent::ToolUse { .. }))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, crate::cli::history::HistoryEvent::TodoList { .. }))
        );
    }

    #[test]
    fn test_session_path_consistency() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap();
        let _sessions_dir = TestSessionsDir::new();
        let dir = Path::new("/tmp/test-project");
        let path1 = get_session_path(dir, "20251231T120000");
        let path2 = get_session_path(dir, "20251231T120000");
        assert_eq!(path1, path2);
    }

    #[test]
    fn test_session_path_different_dirs() {
        let dir1 = Path::new("/tmp/project-a");
        let dir2 = Path::new("/tmp/project-b");
        let path1 = get_session_path(dir1, "20251231T120000");
        let path2 = get_session_path(dir2, "20251231T120000");
        assert_ne!(path1, path2);
    }

    #[test]
    fn test_save_and_load_session() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap();
        let _sessions_dir = TestSessionsDir::new();
        let temp_dir = TempDir::new().unwrap();
        let working_dir = temp_dir.path();

        let messages = vec![
            Message::user("Hello"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "Hi there!".to_string(),
            }]),
        ];

        let session_id = save_session(
            working_dir,
            &messages,
            &ModelProvider::Claude,
            "claude-opus-4-5",
            true,
            false,
            None,
        )
        .unwrap();

        let loaded = load_session(working_dir).unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.meta.provider, "claude");
        assert_eq!(loaded.meta.model_id, "claude-opus-4-5");
        assert_eq!(loaded.meta.session_id, session_id);
        assert!(loaded.meta.thinking_enabled);
        assert!(!loaded.meta.read_only);

        delete_session(working_dir, &session_id).unwrap();
    }

    #[test]
    fn test_multiple_sessions() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap();
        let _sessions_dir = TestSessionsDir::new();
        let temp_dir = TempDir::new().unwrap();
        let working_dir = temp_dir.path();

        // Create first session
        let messages1 = vec![Message::user("First session")];
        let id1 = save_session(
            working_dir,
            &messages1,
            &ModelProvider::Claude,
            "model1",
            true,
            false,
            Some("20251231T100000"),
        )
        .unwrap();

        // Create second session (newer)
        let messages2 = vec![Message::user("Second session")];
        let id2 = save_session(
            working_dir,
            &messages2,
            &ModelProvider::Claude,
            "model2",
            false,
            false,
            Some("20251231T110000"),
        )
        .unwrap();

        // List sessions - should be sorted newest first
        let sessions = list_sessions(working_dir);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, id2); // Newer first
        assert_eq!(sessions[1].id, id1);

        // Load most recent should get second session
        let loaded = load_session(working_dir).unwrap();
        assert_eq!(loaded.meta.session_id, id2);
        assert_eq!(loaded.meta.model_id, "model2");

        // Load specific session by ID
        let loaded1 = load_session_by_id(working_dir, &id1).unwrap();
        assert_eq!(loaded1.meta.model_id, "model1");

        // Delete one session
        delete_session(working_dir, &id1).unwrap();
        let sessions = list_sessions(working_dir);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, id2);

        delete_session(working_dir, &id2).unwrap();
    }

    #[test]
    fn test_save_and_load_session_read_only() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap();
        let _sessions_dir = TestSessionsDir::new();
        let temp_dir = TempDir::new().unwrap();
        let working_dir = temp_dir.path();

        let messages = vec![Message::user("Hello")];

        let session_id = save_session(
            working_dir,
            &messages,
            &ModelProvider::Claude,
            "claude-opus-4-5",
            true,
            true,
            None,
        )
        .unwrap();

        let loaded = load_session(working_dir).unwrap();
        assert_eq!(loaded.meta.session_id, session_id);
        assert!(loaded.meta.read_only);

        delete_session(working_dir, &session_id).unwrap();
    }

    #[test]
    fn test_session_preview() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap();
        let _sessions_dir = TestSessionsDir::new();
        let temp_dir = TempDir::new().unwrap();
        let working_dir = temp_dir.path();

        let messages = vec![
            Message::user("What is the meaning of life, the universe, and everything?"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "42".to_string(),
            }]),
        ];

        let session_id = save_session(
            working_dir,
            &messages,
            &ModelProvider::Claude,
            "test",
            true,
            false,
            None,
        )
        .unwrap();

        let sessions = list_sessions(working_dir);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]._message_count, 2);
        assert!(sessions[0].preview.is_some());
        assert!(
            sessions[0]
                .preview
                .as_ref()
                .unwrap()
                .contains("meaning of life")
        );

        delete_session(working_dir, &session_id).unwrap();
    }

    #[test]
    fn test_format_age() {
        let now = Utc::now();

        assert_eq!(format_age(&now), "just now");

        let one_hour_ago = now - chrono::Duration::hours(1);
        assert_eq!(format_age(&one_hour_ago), "1 hour ago");

        let two_hours_ago = now - chrono::Duration::hours(2);
        assert_eq!(format_age(&two_hours_ago), "2 hours ago");

        let one_day_ago = now - chrono::Duration::days(1);
        assert_eq!(format_age(&one_day_ago), "1 day ago");

        let three_days_ago = now - chrono::Duration::days(3);
        assert_eq!(format_age(&three_days_ago), "3 days ago");
    }

    #[test]
    fn test_tool_result_deserialize_without_is_error() {
        // When is_error is false, it's not serialized. Verify it can still be deserialized.
        let json = r#"{"type": "tool_result", "tool_use_id": "toolu_123", "content": "success"}"#;
        let block: SerializableContentBlock = serde_json::from_str(json).unwrap();
        match block {
            SerializableContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_use_id, "toolu_123");
                assert_eq!(content, "success");
                assert!(!is_error); // Should default to false
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[test]
    fn test_save_and_load_session_with_tool_results() {
        let _lock = SESSION_TEST_LOCK.lock().unwrap();
        let _sessions_dir = TestSessionsDir::new();
        let temp_dir = TempDir::new().unwrap();
        let working_dir = temp_dir.path();

        // Simulate a conversation with tool use
        let messages = vec![
            Message::user("What files are here?"),
            Message::assistant_blocks(vec![ContentBlock::ToolUse {
                id: "toolu_123".to_string(),
                name: "file_list".to_string(),
                input: serde_json::json!({"path": "."}),
                thought_signature: None,
            }]),
            Message {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "toolu_123".to_string(),
                    content: "file1.txt\nfile2.txt".to_string(),
                    is_error: false,
                }]),
            },
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "I found 2 files.".to_string(),
            }]),
        ];

        let session_id = save_session(
            working_dir,
            &messages,
            &ModelProvider::Claude,
            "claude-opus-4-5",
            true,
            false,
            None,
        )
        .unwrap();

        let loaded = load_session(working_dir).unwrap();
        assert_eq!(
            loaded.messages.len(),
            4,
            "All 4 messages should be loaded including tool_result"
        );

        // Verify the tool_result message was preserved
        let tool_result_msg = &loaded.messages[2];
        assert_eq!(tool_result_msg.role, Role::User);
        if let SerializableContent::Blocks(blocks) = &tool_result_msg.content {
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                SerializableContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    assert_eq!(tool_use_id, "toolu_123");
                    assert_eq!(content, "file1.txt\nfile2.txt");
                    assert!(!is_error);
                }
                _ => panic!("Expected ToolResult block"),
            }
        } else {
            panic!("Expected Blocks content");
        }

        delete_session(working_dir, &session_id).unwrap();
    }
}
