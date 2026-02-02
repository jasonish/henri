// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::{Tool, ToolDefinition, ToolResult};

pub(crate) struct ListDir;

#[derive(Debug, Deserialize)]
struct ListDirInput {
    path: Option<String>,
    #[serde(default)]
    include_hidden: bool,
}

fn summary_from_message(message: &str) -> Option<String> {
    let line = message.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return None;
    }
    Some(line.to_string())
}

fn error_with_summary(tool_use_id: &str, message: impl Into<String>) -> ToolResult {
    let message = message.into();
    let mut result = ToolResult::error(tool_use_id, message.clone());
    if let Some(summary) = summary_from_message(&message) {
        result.summary = Some(summary);
    }
    result
}

fn attach_summary_if_missing(mut result: ToolResult) -> ToolResult {
    if result.is_error
        && result.summary.is_none()
        && let Some(summary) = summary_from_message(&result.content)
    {
        result.summary = Some(summary);
    }
    result
}

fn format_file_count(count: usize) -> String {
    if count == 1 {
        "1 file".to_string()
    } else {
        format!("{} files", count)
    }
}

fn format_dir_count(count: usize) -> String {
    if count == 1 {
        "1 directory".to_string()
    } else {
        format!("{} directories", count)
    }
}

impl Tool for ListDir {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_dir".to_string(),
            description: "List the contents of a directory. Returns files and subdirectories in the specified path.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list (default: current directory)"
                    },
                    "include_hidden": {
                        "type": "boolean",
                        "description": "Include hidden files and directories (default: false)"
                    }
                },
                "required": []
            }),
        }
    }

    async fn execute(
        &self,
        tool_use_id: &str,
        input: serde_json::Value,
        output: &crate::output::OutputContext,
        _services: &crate::services::Services,
    ) -> ToolResult {
        let input: ListDirInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return attach_summary_if_missing(*e),
        };

        let dir_path = super::expand_tilde(input.path.as_deref().unwrap_or("."));
        let path = Path::new(&dir_path);

        if let Err(e) = super::validate_path_exists(tool_use_id, path, &dir_path) {
            return attach_summary_if_missing(*e);
        }
        if let Err(e) = super::validate_is_directory(tool_use_id, path, &dir_path) {
            return attach_summary_if_missing(*e);
        }

        let entries = match fs::read_dir(path) {
            Ok(e) => e,
            Err(e) => {
                return error_with_summary(tool_use_id, format!("Failed to read directory: {}", e));
            }
        };

        let mut files = Vec::new();
        let mut dirs = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files unless include_hidden is true
            if !input.include_hidden && name.starts_with('.') {
                continue;
            }

            if let Ok(file_type) = entry.file_type() {
                if file_type.is_dir() {
                    dirs.push(name);
                } else {
                    files.push(name);
                }
            }
        }

        files.sort();
        dirs.sort();

        // Emit preview: first few directories and files (up to 3 total)
        let mut preview_count = 0;
        for dir in &dirs {
            if preview_count >= 3 {
                break;
            }
            crate::output::emit_tool_output(output, &format!("{}/\n", dir));
            preview_count += 1;
        }
        for file in &files {
            if preview_count >= 3 {
                break;
            }
            crate::output::emit_tool_output(output, &format!("{}\n", file));
            preview_count += 1;
        }

        let mut output_buf = format!(
            "Contents of {} ({} files, {} directories)\n\n",
            dir_path,
            files.len(),
            dirs.len()
        );

        if !dirs.is_empty() {
            output_buf.push_str("Directories:\n");
            for dir in &dirs {
                output_buf.push_str(&format!("  {}/\n", dir));
            }
            output_buf.push('\n');
        }

        if !files.is_empty() {
            output_buf.push_str("Files:\n");
            for file in &files {
                output_buf.push_str(&format!("  {}\n", file));
            }
        }

        if files.is_empty() && dirs.is_empty() {
            output_buf.push_str("(empty directory)\n");
        }

        let summary = if files.is_empty() && dirs.is_empty() {
            "Empty directory".to_string()
        } else {
            format!(
                "Found {}, {}",
                format_file_count(files.len()),
                format_dir_count(dirs.len())
            )
        };

        ToolResult::success(tool_use_id, output_buf).with_summary(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_list_current_directory() {
        let tool = ListDir;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({}),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("Contents of ."));
    }

    #[tokio::test]
    async fn test_list_specific_path() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create test files and directories
        fs::create_dir(temp_path.join("subdir")).unwrap();
        fs::write(temp_path.join("file.txt"), "content").unwrap();

        let tool = ListDir;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "path": temp_path.to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("1 files, 1 directories"));
        assert!(result.content.contains("subdir/"));
        assert!(result.content.contains("file.txt"));
    }

    #[tokio::test]
    async fn test_list_nonexistent_path() {
        let tool = ListDir;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "path": "/nonexistent/path"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("Path not found"));
    }

    #[tokio::test]
    async fn test_list_file_not_directory() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("file.txt");
        fs::write(&file_path, "content").unwrap();

        let tool = ListDir;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "path": file_path.to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("not a directory"));
    }

    #[tokio::test]
    async fn test_hidden_files_excluded_by_default() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        fs::write(temp_path.join("visible.txt"), "").unwrap();
        fs::write(temp_path.join(".hidden"), "").unwrap();
        fs::create_dir(temp_path.join(".hidden_dir")).unwrap();

        let tool = ListDir;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "path": temp_path.to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("visible.txt"));
        assert!(!result.content.contains(".hidden"));
        assert!(!result.content.contains(".hidden_dir"));
    }

    #[tokio::test]
    async fn test_hidden_files_included_with_flag() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        fs::write(temp_path.join("visible.txt"), "").unwrap();
        fs::write(temp_path.join(".hidden"), "").unwrap();
        fs::create_dir(temp_path.join(".hidden_dir")).unwrap();

        let tool = ListDir;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "path": temp_path.to_str().unwrap(),
                    "include_hidden": true
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("visible.txt"));
        assert!(result.content.contains(".hidden"));
        assert!(result.content.contains(".hidden_dir/"));
    }

    #[tokio::test]
    async fn test_empty_directory() {
        let temp_dir = TempDir::new().unwrap();

        let tool = ListDir;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "path": temp_dir.path().to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("0 files, 0 directories"));
        assert!(result.content.contains("(empty directory)"));
    }
}
