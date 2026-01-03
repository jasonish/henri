// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use super::sandbox;
use super::{Tool, ToolDefinition, ToolResult};

/// Tool for performing exact string replacements in files
pub(crate) struct FileEdit;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileEditInput {
    file_path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

impl Tool for FileEdit {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file_edit".to_string(),
            description: "Performs exact string replacements in files. \
                The old_string must be unique in the file unless replace_all is true."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "filePath": {
                        "type": "string",
                        "description": "The absolute path to the file to modify"
                    },
                    "oldString": {
                        "type": "string",
                        "description": "The text to replace"
                    },
                    "newString": {
                        "type": "string",
                        "description": "The text to replace it with (must be different from oldString)"
                    },
                    "replaceAll": {
                        "type": "boolean",
                        "description": "Replace all occurrences of oldString (default false)"
                    }
                },
                "required": ["filePath", "oldString", "newString"]
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

        let input: FileEditInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        // Validate that old_string != new_string
        if input.old_string == input.new_string {
            return ToolResult::error(tool_use_id, "oldString and newString must be different");
        }

        // Validate old_string is not empty
        if input.old_string.is_empty() {
            return ToolResult::error(tool_use_id, "oldString cannot be empty");
        }

        let expanded_path = super::expand_tilde(&input.file_path);
        let path = Path::new(&expanded_path);

        if let Err(e) = super::validate_path_exists(tool_use_id, path, &input.file_path) {
            return e;
        }
        if let Err(e) = super::validate_is_file(tool_use_id, path, &input.file_path) {
            return e;
        }

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        if let Err(message) = sandbox::check_write_access(path, &cwd, services.is_sandbox_enabled())
        {
            return ToolResult::error(tool_use_id, message);
        }

        // Read the file contents
        let old_contents = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(tool_use_id, format!("Failed to read file: {}", e)),
        };

        // Count occurrences
        let count = old_contents.matches(&input.old_string).count();

        if count == 0 {
            return ToolResult::error(
                tool_use_id,
                "oldString not found in file. Make sure the string matches exactly, \
                including whitespace and line endings.",
            );
        }

        // If not replace_all and multiple occurrences, error
        if !input.replace_all && count > 1 {
            return ToolResult::error(
                tool_use_id,
                format!(
                    "oldString appears {} times in the file. Use replaceAll: true to replace \
                    all occurrences, or provide a more specific string that appears only once.",
                    count
                ),
            );
        }

        // Perform the replacement
        let new_contents = if input.replace_all {
            old_contents.replace(&input.old_string, &input.new_string)
        } else {
            old_contents.replacen(&input.old_string, &input.new_string, 1)
        };

        // Write the file back
        if let Err(e) = fs::write(path, &new_contents) {
            return ToolResult::error(tool_use_id, format!("Failed to write file: {}", e));
        }

        let diff = crate::diff::unified_diff(path, &old_contents, &new_contents, 3);
        if diff.has_changes {
            _output.emit(crate::output::OutputEvent::FileDiff {
                path: input.file_path.clone(),
                diff: diff.unified_diff,
                lines_added: diff.lines_added,
                lines_removed: diff.lines_removed,
                language: crate::syntax::language_from_path(&input.file_path),
            });
        }

        let msg = if input.replace_all && count > 1 {
            format!(
                "Successfully replaced {} occurrences in {}",
                count, input.file_path
            )
        } else {
            format!("Successfully edited {}", input.file_path)
        };

        // Notify LSP of the change and get diagnostics immediately
        let diagnostics = if services.lsp.handles_file(path).await {
            let _ = services.lsp.notify_file_changed(path, &new_contents).await;
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_edit_nonexistent_file() {
        let tool = FileEdit;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": "/nonexistent/path/file.txt",
                    "oldString": "foo",
                    "newString": "bar"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("Path not found"));
    }

    #[tokio::test]
    async fn test_edit_same_strings() {
        let tool = FileEdit;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": "/tmp/test.txt",
                    "oldString": "foo",
                    "newString": "foo"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("must be different"));
    }

    #[tokio::test]
    async fn test_edit_string_not_found() {
        let mut temp = NamedTempFile::new().unwrap();
        writeln!(temp, "hello world").unwrap();

        let tool = FileEdit;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": temp.path().to_str().unwrap(),
                    "oldString": "foo",
                    "newString": "bar"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[tokio::test]
    async fn test_edit_multiple_occurrences_without_replace_all() {
        let mut temp = NamedTempFile::new().unwrap();
        writeln!(temp, "foo bar foo").unwrap();

        let tool = FileEdit;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": temp.path().to_str().unwrap(),
                    "oldString": "foo",
                    "newString": "baz"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("appears 2 times"));
    }

    #[tokio::test]
    async fn test_edit_single_occurrence() {
        let mut temp = NamedTempFile::new().unwrap();
        write!(temp, "hello world").unwrap();

        let tool = FileEdit;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": temp.path().to_str().unwrap(),
                    "oldString": "world",
                    "newString": "rust"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);

        let contents = fs::read_to_string(temp.path()).unwrap();
        assert_eq!(contents, "hello rust");
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
        let mut temp = NamedTempFile::new().unwrap();
        write!(temp, "foo bar foo baz foo").unwrap();

        let tool = FileEdit;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filePath": temp.path().to_str().unwrap(),
                    "oldString": "foo",
                    "newString": "qux",
                    "replaceAll": true
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("3 occurrences"));

        let contents = fs::read_to_string(temp.path()).unwrap();
        assert_eq!(contents, "qux bar qux baz qux");
    }
}
