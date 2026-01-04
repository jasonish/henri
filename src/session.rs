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
//!     {session_id}.jsonl           # One file per session
//! ```

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::provider::{ContentBlock, Message, MessageContent, Role};
use crate::providers::ModelProvider;
use crate::tools::todo::{get_todos, set_todos};
use crate::tools::{TodoItem, format_tool_call_description};

const SESSION_VERSION: u32 = 2;

/// Rich intermediate representation for session replay
#[derive(Debug, Clone)]
pub(crate) struct SessionReplayMessage {
    pub role: Role,
    pub segments: Vec<ReplaySegment>,
}

#[derive(Debug, Clone)]
pub(crate) enum ReplaySegment {
    UserText {
        text: String,
        has_images: bool,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        description: String,
        status: ToolStatus,
    },
    ToolResult {
        is_error: bool,
        error_preview: Option<String>,
    },
    Text {
        text: String,
    },
    Summary {
        summary: String,
        messages_compacted: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ToolStatus {
    Pending,
    Success,
    Error,
}

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
    pub model_id: String,
    pub _message_count: usize,
    /// First user message (truncated) for preview
    pub preview: Option<String>,
}

impl SessionInfo {
    /// Format this session info as a display string for menus.
    pub(crate) fn display_string(&self) -> String {
        let preview = self.preview.as_deref().unwrap_or("No preview");
        format!("{} ({})", preview, format_age(&self.saved_at))
    }
}

/// Session state loaded from disk (metadata + messages).
#[derive(Debug, Clone)]
pub(crate) struct SessionState {
    pub meta: SessionMeta,
    pub messages: Vec<SerializableMessage>,
}

/// Restored session ready to be used by CLI or TUI.
/// Contains the converted messages and settings.
#[derive(Debug, Clone)]
pub(crate) struct RestoredSession {
    pub session_id: String,
    pub messages: Vec<Message>,
    pub provider: String,
    pub model_id: String,
    pub thinking_enabled: bool,
    pub read_only: bool,
    pub state: SessionState, // Keep original state for replay
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
            state: state.clone(),
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
    sessions_dir_for_path(dir).join(format!("{}.jsonl", session_id))
}

/// Generate a new session ID based on current timestamp.
/// Format: YYYYMMDDTHHMMSS (compact ISO-8601 for natural sorting)
pub(crate) fn generate_session_id() -> String {
    Utc::now().format("%Y%m%dT%H%M%S").to_string()
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
        if path.extension().is_some_and(|ext| ext == "jsonl")
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

    // For v1 sessions, derive ID from filename (strip .jsonl extension)
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
        model_id: meta.model_id,
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

/// Extract rich replay messages from session state
pub(crate) fn extract_replay_messages(state: &SessionState) -> Vec<SessionReplayMessage> {
    let mut result = Vec::new();
    let mut tool_results: HashMap<String, (usize, bool, Option<String>)> = HashMap::new();

    // First pass: collect all tool results
    for msg in &state.messages {
        if let SerializableContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                if let SerializableContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = block
                {
                    let bytes = content.len();
                    let error_preview = if *is_error {
                        // Get first line of error as preview
                        content.lines().next().map(|s| s.to_string())
                    } else {
                        None
                    };
                    tool_results.insert(tool_use_id.clone(), (bytes, *is_error, error_preview));
                }
            }
        }
    }

    // Second pass: build replay messages
    for msg in &state.messages {
        let segments = match msg.role {
            Role::User => extract_user_segments(&msg.content),
            Role::Assistant => extract_assistant_segments(&msg.content, &tool_results),
            Role::System => continue, // Skip system messages
        };

        if !segments.is_empty() {
            result.push(SessionReplayMessage {
                role: msg.role,
                segments,
            });
        }
    }

    result
}

fn extract_user_segments(content: &SerializableContent) -> Vec<ReplaySegment> {
    let mut segments = Vec::new();

    match content {
        SerializableContent::Text(text) => {
            segments.push(ReplaySegment::UserText {
                text: text.clone(),
                has_images: false,
            });
        }
        SerializableContent::Blocks(blocks) => {
            let mut text_parts = Vec::new();
            let mut has_images = false;

            for block in blocks {
                match block {
                    SerializableContentBlock::Text { text } => {
                        text_parts.push(text.clone());
                    }
                    SerializableContentBlock::Image { .. } => {
                        has_images = true;
                    }
                    SerializableContentBlock::Summary {
                        summary,
                        messages_compacted,
                    } => {
                        segments.push(ReplaySegment::Summary {
                            summary: summary.clone(),
                            messages_compacted: *messages_compacted,
                        });
                    }
                    _ => {}
                }
            }

            if !text_parts.is_empty() {
                segments.push(ReplaySegment::UserText {
                    text: text_parts.join("\n"),
                    has_images,
                });
            }
        }
    }

    segments
}

fn extract_assistant_segments(
    content: &SerializableContent,
    tool_results: &HashMap<String, (usize, bool, Option<String>)>,
) -> Vec<ReplaySegment> {
    let mut segments = Vec::new();

    match content {
        SerializableContent::Text(text) => {
            segments.push(ReplaySegment::Text { text: text.clone() });
        }
        SerializableContent::Blocks(blocks) => {
            for block in blocks {
                match block {
                    SerializableContentBlock::Thinking { thinking, .. } => {
                        segments.push(ReplaySegment::Thinking {
                            text: thinking.clone(),
                        });
                    }
                    SerializableContentBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        let description = format_tool_call_description(name, input);
                        let status = if let Some((_, is_error, _)) = tool_results.get(id) {
                            if *is_error {
                                ToolStatus::Error
                            } else {
                                ToolStatus::Success
                            }
                        } else {
                            ToolStatus::Pending
                        };

                        segments.push(ReplaySegment::ToolCall {
                            description,
                            status,
                        });

                        // Add tool result if available
                        if let Some((_bytes, is_error, error_preview)) = tool_results.get(id) {
                            segments.push(ReplaySegment::ToolResult {
                                is_error: *is_error,
                                error_preview: error_preview.clone(),
                            });
                        }
                    }
                    SerializableContentBlock::Text { text } => {
                        segments.push(ReplaySegment::Text { text: text.clone() });
                    }
                    _ => {}
                }
            }
        }
    }

    segments
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

/// Replay session messages for display purposes.
/// Shows a rich view matching the live session appearance.
pub(crate) fn replay_session(state: &SessionState) {
    // Show session metadata
    println!(
        "{}",
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
    );
    println!(
        "{}\n",
        format!(
            "Saved: {} · Messages: {}",
            format_age(&state.meta.saved_at),
            state.messages.len()
        )
        .dimmed()
    );

    let replay_messages = extract_replay_messages(state);

    for msg in replay_messages {
        match msg.role {
            Role::User => {
                print!("{} ", "❯".bold().green());
                for segment in msg.segments {
                    match segment {
                        ReplaySegment::UserText { text, has_images } => {
                            println!("{}", text);
                            if has_images {
                                println!("{}", "  [images attached]".dimmed());
                            }
                        }
                        ReplaySegment::Summary {
                            summary,
                            messages_compacted,
                        } => {
                            println!(
                                "{}",
                                format!("── Compacted {} messages ──", messages_compacted).dimmed()
                            );
                            for line in summary.lines() {
                                println!("{}", format!("  {}", line).dimmed().italic());
                            }
                        }
                        _ => {}
                    }
                }
                println!();
            }
            Role::Assistant => {
                for segment in msg.segments {
                    match segment {
                        ReplaySegment::Thinking { text } => {
                            // Same format as CliListener - indented with grey color
                            for line in text.lines() {
                                print!("  ");
                                println!("{}", line.bright_black());
                            }
                        }
                        ReplaySegment::ToolCall {
                            description,
                            status,
                            ..
                        } => {
                            let indicator = match status {
                                ToolStatus::Pending => "▶",
                                ToolStatus::Success => "✓",
                                ToolStatus::Error => "✗",
                            };
                            let colored_indicator = match status {
                                ToolStatus::Success => indicator.green().dimmed(),
                                ToolStatus::Error => indicator.red().dimmed(),
                                ToolStatus::Pending => indicator.dimmed(),
                            };
                            println!("{} {}", colored_indicator, description.dimmed());
                        }
                        ReplaySegment::ToolResult {
                            is_error,
                            error_preview,
                        } => {
                            if is_error && let Some(preview) = error_preview {
                                println!(
                                    "{} {}",
                                    "✗".red().dimmed(),
                                    format!("Error: {}", preview).dimmed()
                                );
                            }
                        }
                        ReplaySegment::Text { text } => {
                            println!("{}", text);
                        }
                        _ => {}
                    }
                }
                println!();
            }
            Role::System => {}
        }
    }

    println!();
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
    fn test_session_path_consistency() {
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
