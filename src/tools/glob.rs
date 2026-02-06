// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use serde::Deserialize;
use std::path::Path;

use super::{Tool, ToolDefinition, ToolResult};

const DEFAULT_LIMIT: usize = 1000;

pub(crate) struct Glob;

#[derive(Debug, Deserialize)]
struct GlobInput {
    pattern: String,
    path: Option<String>,
    #[serde(default, deserialize_with = "super::deserialize_optional_usize")]
    limit: Option<usize>,
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

impl Tool for Glob {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "glob".to_string(),
            description: "Find files using glob patterns. Supports ** for recursive matching, * for wildcards, ? for single characters, and [a-z] for character classes. Use this for powerful file discovery and codebase exploration.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files (e.g., '**/*.rs', 'src/**/*.rs', '**/*.toml')"
                    },
                    "path": {
                        "type": "string",
                        "description": "Base directory for search (default: current directory)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of files to return (default: 1000)"
                    },
                    "include_hidden": {
                        "type": "boolean",
                        "description": "Include hidden files/directories (default: false)"
                    }
                },
                "required": ["pattern"]
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
        let input: GlobInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return attach_summary_if_missing(*e),
        };

        let base_path = super::expand_tilde(input.path.as_deref().unwrap_or("."));
        let path = Path::new(&base_path);
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT);

        if let Err(e) = super::validate_path_exists(tool_use_id, path, &base_path) {
            return attach_summary_if_missing(*e);
        }
        if let Err(e) = super::validate_is_directory(tool_use_id, path, &base_path) {
            return attach_summary_if_missing(*e);
        }

        // Build the full pattern by combining path and pattern
        let full_pattern = if base_path == "." {
            input.pattern.clone()
        } else {
            format!("{}/{}", base_path.trim_end_matches('/'), input.pattern)
        };

        // Configure glob options
        let mut options = glob::MatchOptions::new();
        options.case_sensitive = true;
        options.require_literal_leading_dot = !input.include_hidden;

        let mut files = Vec::new();
        let mut truncated = false;

        match glob::glob_with(&full_pattern, options) {
            Ok(entries) => {
                for entry in entries.take(limit) {
                    match entry {
                        Ok(path) => {
                            if path.is_file() {
                                let display_path = path
                                    .strip_prefix(&base_path)
                                    .unwrap_or(&path)
                                    .to_string_lossy()
                                    .to_string();
                                files.push(display_path);
                            }
                        }
                        Err(_) => continue, // Skip invalid entries
                    }

                    if files.len() >= limit {
                        truncated = true;
                        break;
                    }
                }
            }
            Err(e) => {
                return error_with_summary(
                    tool_use_id,
                    format!("Invalid glob pattern '{}': {}", input.pattern, e),
                );
            }
        }

        files.sort();

        // Emit first 5 files as preview
        for file in files.iter().take(5) {
            crate::output::emit_tool_output(output, &format!("{}\n", file));
        }

        let summary = if files.is_empty() {
            format!("No files matching '{}'", input.pattern)
        } else if truncated {
            format!("Found {} files (truncated)", files.len())
        } else {
            format!("Found {} files", files.len())
        };

        let mut output_buf = String::new();

        for file in &files {
            output_buf.push_str(file);
            output_buf.push('\n');
        }

        if truncated {
            output_buf.push_str(&format!(
                "\n(truncated: showing {} of more files, use a more specific pattern)\n",
                limit
            ));
        }

        if output_buf.is_empty() {
            output_buf = format!(
                "No files matching pattern '{}' found in {}\n",
                input.pattern, base_path
            );
        } else {
            let output_header = format!(
                "Found {} files matching '{}' in {}\n\n",
                files.len(),
                input.pattern,
                base_path
            );
            output_buf = output_header + &output_buf;
        }

        ToolResult::success(tool_use_id, output_buf).with_summary(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_glob_nonexistent_path() {
        let tool = Glob;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "*.rs",
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
    async fn test_glob_file_not_directory() {
        let tool = Glob;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "*.rs",
                    "path": "/etc/passwd"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not a directory"));
    }

    #[tokio::test]
    async fn test_glob_current_directory() {
        let tool = Glob;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "*.rs"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        // Should find at least main.rs in the project root
        assert!(result.content.contains(".rs"));
    }

    #[tokio::test]
    async fn test_glob_recursive_pattern() {
        let tool = Glob;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "**/*.rs"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        // Should find multiple Rust files recursively
        assert!(result.content.contains(".rs"));
    }

    #[tokio::test]
    async fn test_glob_character_classes() {
        let tool = Glob;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "**/*.[rt]s"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        // Should find .rs or .ts files if they exist
        // Note: This test might not find .ts files in a Rust project
    }

    #[tokio::test]
    async fn test_glob_with_temp_dir() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create test files
        std::fs::create_dir_all(temp_path.join("src")).unwrap();
        std::fs::write(temp_path.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(temp_path.join("src/lib.rs"), "pub fn hello() {}").unwrap();
        std::fs::write(temp_path.join("test.py"), "print('test')").unwrap();
        std::fs::write(temp_path.join(".hidden"), "hidden").unwrap();

        let tool = Glob;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "**/*.rs",
                    "path": temp_path.to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"));
        assert!(result.content.contains("src/lib.rs"));
        assert!(!result.content.contains("test.py"));
        assert!(!result.content.contains(".hidden")); // Hidden files excluded by default
    }

    #[tokio::test]
    async fn test_glob_include_hidden() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create test files including hidden
        std::fs::write(temp_path.join("visible.rs"), "pub fn visible() {}").unwrap();
        std::fs::write(temp_path.join(".hidden.rs"), "pub fn hidden() {}").unwrap();

        let tool = Glob;

        // Without include_hidden (default)
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "*.rs",
                    "path": temp_path.to_str().unwrap(),
                    "include_hidden": false
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("visible.rs"));
        assert!(!result.content.contains(".hidden.rs"));

        // With include_hidden: true
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "*.rs",
                    "path": temp_path.to_str().unwrap(),
                    "include_hidden": true
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("visible.rs"));
        assert!(result.content.contains(".hidden.rs"));
    }

    #[tokio::test]
    async fn test_glob_invalid_pattern() {
        let tool = Glob;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "[invalid" // Invalid bracket expression
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("Invalid glob pattern"));
    }

    #[tokio::test]
    async fn test_glob_limit() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        // Create many test files
        for i in 0..10 {
            std::fs::write(temp_path.join(format!("file{}.rs", i)), "").unwrap();
        }

        let tool = Glob;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "*.rs",
                    "path": temp_path.to_str().unwrap(),
                    "limit": 3
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("Found 3 files"));
        assert!(result.content.contains("truncated"));
    }
}
