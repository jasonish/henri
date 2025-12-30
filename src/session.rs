// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Session state persistence for Henri.
//!
//! Automatically saves conversation state after each interaction, allowing
//! users to continue where they left off when restarting Henri.

use base64::Engine;
use chrono::{DateTime, Utc};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::provider::{ContentBlock, Message, MessageContent, Role};
use crate::providers::ModelProvider;
use crate::tools::todo::{get_todos, set_todos};
use crate::tools::{TodoItem, format_tool_call_description};

const SESSION_VERSION: u32 = 1;

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

    /// Current todo list state
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todos: Option<Vec<TodoItem>>,
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
    pub messages: Vec<Message>,
    pub provider: String,
    pub model_id: String,
    pub thinking_enabled: bool,
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
            messages: restore_messages(state),
            provider: state.meta.provider.clone(),
            model_id: state.meta.model_id.clone(),
            thinking_enabled: state.meta.thinking_enabled,
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
        // Legacy field for backward compatibility
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        // New unified provider data field
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
                signature: None, // Don't use legacy field
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
                // Handle migration from old format (signature field) to new format (provider_data)
                let provider_data = if let Some(data) = provider_data {
                    Some(data.clone())
                } else if let Some(sig) = signature
                    && !sig.is_empty()
                {
                    // Migrate old signature field to provider_data
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

/// Get the sessions directory path.
fn sessions_dir() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".cache").join("henri").join("sessions"))
        .unwrap_or_else(|| PathBuf::from(".cache/henri/sessions"))
}

fn get_session_path(dir: &Path) -> PathBuf {
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let path_str = canonical.to_string_lossy();

    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let hash = hasher.finalize();
    let hash_str = format!("{:x}", hash);
    let short_hash = &hash_str[..16];

    sessions_dir().join(format!("{}.json", short_hash))
}

/// Save session state to disk in JSONL format.
/// Line 1: Session metadata
/// Lines 2+: One message per line
pub(crate) fn save_session(
    working_directory: &Path,
    messages: &[Message],
    provider: &ModelProvider,
    model_id: &str,
    thinking_enabled: bool,
) -> std::io::Result<()> {
    let session_path = get_session_path(working_directory);

    // Ensure the sessions directory exists
    if let Some(parent) = session_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Get current todos
    let todos = get_todos();
    let todos = if todos.is_empty() { None } else { Some(todos) };

    let meta = SessionMeta {
        version: SESSION_VERSION,
        working_directory: working_directory.to_path_buf(),
        saved_at: Utc::now(),
        provider: provider.id().to_string(),
        model_id: model_id.to_string(),
        thinking_enabled,
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

    Ok(())
}

/// Load session state from disk (JSONL format).
/// Returns None if no session exists or if the session is invalid.
pub(crate) fn load_session(dir: &Path) -> Option<SessionState> {
    let session_path = get_session_path(dir);
    let file = File::open(&session_path).ok()?;
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

/// Delete the session file for a given directory.
pub(crate) fn delete_session(dir: &Path) -> std::io::Result<()> {
    let session_path = get_session_path(dir);
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
    let mut tool_results: std::collections::HashMap<String, (usize, bool, Option<String>)> =
        std::collections::HashMap::new();

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
    tool_results: &std::collections::HashMap<String, (usize, bool, Option<String>)>,
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
    use tempfile::TempDir;

    #[test]
    fn test_session_path_consistency() {
        let dir = Path::new("/tmp/test-project");
        let path1 = get_session_path(dir);
        let path2 = get_session_path(dir);
        assert_eq!(path1, path2);
    }

    #[test]
    fn test_session_path_different_dirs() {
        let dir1 = Path::new("/tmp/project-a");
        let dir2 = Path::new("/tmp/project-b");
        let path1 = get_session_path(dir1);
        let path2 = get_session_path(dir2);
        assert_ne!(path1, path2);
    }

    #[test]
    fn test_save_and_load_session() {
        let temp_dir = TempDir::new().unwrap();
        let working_dir = temp_dir.path();

        let messages = vec![
            Message::user("Hello"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "Hi there!".to_string(),
            }]),
        ];

        save_session(
            working_dir,
            &messages,
            &ModelProvider::Claude,
            "claude-opus-4-5",
            true,
        )
        .unwrap();

        let loaded = load_session(working_dir).unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.meta.provider, "claude");
        assert_eq!(loaded.meta.model_id, "claude-opus-4-5");
        assert!(loaded.meta.thinking_enabled);

        delete_session(working_dir).unwrap();
    }

    #[test]
    fn test_delete_session() {
        let temp_dir = TempDir::new().unwrap();
        let working_dir = temp_dir.path();

        let messages = vec![Message::user("Hello")];
        save_session(working_dir, &messages, &ModelProvider::Claude, "test", true).unwrap();

        assert!(load_session(working_dir).is_some());

        delete_session(working_dir).unwrap();

        assert!(load_session(working_dir).is_none());
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

        save_session(
            working_dir,
            &messages,
            &ModelProvider::Claude,
            "claude-opus-4-5",
            true,
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

        delete_session(working_dir).unwrap();
    }
}
