// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use super::sandbox;
use super::{Tool, ToolDefinition, ToolResult};

pub(crate) struct FileDelete;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileDeleteInput {
    file_path: String,
}

impl Tool for FileDelete {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file_delete".to_string(),
            description: "Delete a file from the filesystem.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "filePath": {
                        "type": "string",
                        "description": "The path to the file to delete"
                    }
                },
                "required": ["filePath"]
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

        let input: FileDeleteInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return *e,
        };

        let expanded_path = super::expand_tilde(&input.file_path);
        let path = Path::new(&expanded_path);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        if let Err(message) = sandbox::check_write_access(path, &cwd, services.is_sandbox_enabled())
        {
            return ToolResult::error(tool_use_id, message);
        }

        if let Err(e) = super::validate_path_exists(tool_use_id, path, &input.file_path) {
            return *e;
        }

        if path.is_dir() {
            return ToolResult::error(
                tool_use_id,
                format!(
                    "Path is a directory, not a file: {}. Use a different method to remove directories.",
                    input.file_path
                ),
            );
        }

        if let Err(e) = fs::remove_file(path) {
            return ToolResult::error(tool_use_id, format!("Failed to delete file: {}", e));
        }

        ToolResult::success(tool_use_id, format!("Deleted {}", input.file_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[tokio::test]
    async fn test_delete_existing_file() {
        let mut temp = NamedTempFile::new().unwrap();
        write!(temp, "content to delete").unwrap();

        // Keep the file on disk by persisting it
        let (_, persisted_path) = temp.keep().unwrap();

        let tool = FileDelete;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": persisted_path.to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("Deleted"));
        assert!(!persisted_path.exists());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_file() {
        // Use a path under /tmp so it passes sandbox checks but doesn't exist
        let nonexistent_path = "/tmp/henri-test-nonexistent-file-12345.txt";
        let tool = FileDelete;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": nonexistent_path
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("Path not found"));
    }

    #[tokio::test]
    async fn test_delete_directory_fails() {
        let dir = TempDir::new().unwrap();

        let tool = FileDelete;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": dir.path().to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(result.is_error);
        assert!(result.content.contains("is a directory"));
    }
}
