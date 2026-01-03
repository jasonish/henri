// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use base64::Engine;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use super::sandbox;
use super::{Tool, ToolDefinition, ToolResult};

pub(crate) struct FileWrite;

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum ContentEncoding {
    #[default]
    Text,
    Base64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileWriteInput {
    file_path: String,
    content: String,
    #[serde(default)]
    encoding: ContentEncoding,
    #[serde(default)]
    create_directories: bool,
}

impl Tool for FileWrite {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file_write".to_string(),
            description: "Write content to a file. Creates the file if it doesn't exist, \
                or overwrites it if it does."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "filePath": {
                        "type": "string",
                        "description": "The path to the file to write"
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file"
                    },
                    "encoding": {
                        "type": "string",
                        "enum": ["text", "base64"],
                        "description": "How the content is encoded. Use 'base64' for binary files like images. (default: text)"
                    },
                    "createDirectories": {
                        "type": "boolean",
                        "description": "Create parent directories if they don't exist (default: false)"
                    }
                },
                "required": ["filePath", "content"]
            }),
        }
    }

    async fn execute(
        &self,
        tool_use_id: &str,
        input: serde_json::Value,
        _output: &crate::output::OutputContext,
        services: &crate::services::Services,
    ) -> ToolResult {
        if services.is_read_only() {
            return ToolResult::error(tool_use_id, "Read-only mode is enabled");
        }

        let input: FileWriteInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        let expanded_path = super::expand_tilde(&input.file_path);
        let path = Path::new(&expanded_path);

        if path.is_dir() {
            return ToolResult::error(
                tool_use_id,
                format!("Path is a directory: {}", input.file_path),
            );
        }

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        if let Err(message) = sandbox::check_write_access(path, &cwd, services.is_sandbox_enabled())
        {
            return ToolResult::error(tool_use_id, message);
        }

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            if input.create_directories {
                if let Err(e) = fs::create_dir_all(parent) {
                    return ToolResult::error(
                        tool_use_id,
                        format!("Failed to create directories: {}", e),
                    );
                }
            } else {
                return ToolResult::error(
                    tool_use_id,
                    format!(
                        "Parent directory does not exist: {}. Use createDirectories: true to create it.",
                        parent.display()
                    ),
                );
            }
        }

        let bytes_to_write: Vec<u8> = match input.encoding {
            ContentEncoding::Text => input.content.into_bytes(),
            ContentEncoding::Base64 => {
                match base64::engine::general_purpose::STANDARD.decode(&input.content) {
                    Ok(decoded) => decoded,
                    Err(e) => {
                        return ToolResult::error(
                            tool_use_id,
                            format!("Failed to decode base64 content: {}", e),
                        );
                    }
                }
            }
        };

        let bytes_written = bytes_to_write.len();
        let file_existed = path.exists();

        let old_content = if file_existed {
            fs::read_to_string(path).unwrap_or_default()
        } else {
            String::new()
        };

        if let Err(e) = fs::write(path, &bytes_to_write) {
            return ToolResult::error(tool_use_id, format!("Failed to write file: {}", e));
        }

        if input.encoding == ContentEncoding::Text {
            let new_content = String::from_utf8_lossy(&bytes_to_write);
            let diff = crate::diff::unified_diff(path, &old_content, &new_content, 3);
            if diff.has_changes {
                _output.emit(crate::output::OutputEvent::FileDiff {
                    path: input.file_path.clone(),
                    diff: diff.unified_diff,
                    lines_added: diff.lines_added,
                    lines_removed: diff.lines_removed,
                    language: crate::syntax::language_from_path(&input.file_path),
                });
            }
        }

        let action = if file_existed { "Overwrote" } else { "Created" };
        let msg = format!("{} {} ({} bytes)", action, input.file_path, bytes_written);

        // Notify LSP of the change and get diagnostics immediately for text files
        if input.encoding == ContentEncoding::Text {
            let diagnostics = if services.lsp.handles_file(path).await {
                let content_str = String::from_utf8_lossy(&bytes_to_write);
                let _ = services.lsp.notify_file_changed(path, &content_str).await;
                services.lsp.get_diagnostics_with_wait(path).await
            } else {
                Vec::new()
            };

            // Include diagnostics in the tool result if any
            let final_msg = if !diagnostics.is_empty() {
                if let Some(summary) = crate::lsp::diagnostic_summary(&diagnostics) {
                    _output.emit(crate::output::OutputEvent::Info(summary));
                }
                format!("{}{}", msg, crate::lsp::format_diagnostics(&diagnostics))
            } else {
                msg
            };

            ToolResult::success(tool_use_id, final_msg)
        } else {
            ToolResult::success(tool_use_id, msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[tokio::test]
    async fn test_write_new_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("new_file.txt");

        let tool = FileWrite;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": file_path.to_str().unwrap(),
                    "content": "hello world"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("Created"));
        assert!(result.content.contains("11 bytes"));

        let contents = fs::read_to_string(&file_path).unwrap();
        assert_eq!(contents, "hello world");
    }

    #[tokio::test]
    async fn test_overwrite_existing_file() {
        let mut temp = NamedTempFile::new().unwrap();
        write!(temp, "old content").unwrap();
        let path = temp.path().to_str().unwrap().to_string();

        let tool = FileWrite;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": path,
                    "content": "new content"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("Overwrote"));

        let contents = fs::read_to_string(temp.path()).unwrap();
        assert_eq!(contents, "new content");
    }

    #[tokio::test]
    async fn test_write_empty_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("empty.txt");

        let tool = FileWrite;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": file_path.to_str().unwrap(),
                    "content": ""
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("0 bytes"));

        let contents = fs::read_to_string(&file_path).unwrap();
        assert_eq!(contents, "");
    }

    #[tokio::test]
    async fn test_write_to_directory_path() {
        let dir = TempDir::new().unwrap();

        let tool = FileWrite;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": dir.path().to_str().unwrap(),
                    "content": "test"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("is a directory"));
    }

    #[tokio::test]
    async fn test_write_missing_parent_dir() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("nonexistent").join("file.txt");

        let tool = FileWrite;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": file_path.to_str().unwrap(),
                    "content": "test"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("does not exist"));
        assert!(result.content.contains("createDirectories"));
    }

    #[tokio::test]
    async fn test_write_create_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("a").join("b").join("c").join("file.txt");

        let tool = FileWrite;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": file_path.to_str().unwrap(),
                    "content": "nested content",
                    "createDirectories": true
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("Created"));

        let contents = fs::read_to_string(&file_path).unwrap();
        assert_eq!(contents, "nested content");
    }

    #[tokio::test]
    async fn test_write_base64_binary() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("binary.bin");

        // Binary data with null bytes and non-UTF8 sequences
        let binary_data: Vec<u8> = vec![0x00, 0x01, 0x02, 0xFF, 0xFE, 0x89, 0x50, 0x4E, 0x47];
        let base64_content = base64::engine::general_purpose::STANDARD.encode(&binary_data);

        let tool = FileWrite;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": file_path.to_str().unwrap(),
                    "content": base64_content,
                    "encoding": "base64"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("9 bytes"));

        let contents = fs::read(&file_path).unwrap();
        assert_eq!(contents, binary_data);
    }

    #[tokio::test]
    async fn test_write_base64_invalid() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("invalid.bin");

        let tool = FileWrite;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": file_path.to_str().unwrap(),
                    "content": "not valid base64!!!",
                    "encoding": "base64"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("Failed to decode base64"));
    }
}
