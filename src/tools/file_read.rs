// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::path::Path;

use super::{Tool, ToolDefinition, ToolResult};

/// Tool for reading file contents
pub(crate) struct FileRead;

#[derive(Debug, Deserialize)]
struct FileReadInput {
    filename: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

impl Tool for FileRead {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file_read".to_string(),
            description:
                "Read the contents of a file. Returns the file contents as text with line numbers."
                    .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "filename": {
                        "type": "string",
                        "description": "The path to the file to read"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "0-based line number to start reading from (default: 0)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read (default: read all lines)"
                    }
                },
                "required": ["filename"]
            }),
        }
    }

    async fn execute(
        &self,
        tool_use_id: &str,
        input: serde_json::Value,
        _output: &crate::output::OutputContext,
        _services: &crate::services::Services,
    ) -> ToolResult {
        let input: FileReadInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        let path = Path::new(&input.filename);

        if let Err(e) = super::validate_path_exists(tool_use_id, path, &input.filename) {
            return e;
        }
        if let Err(e) = super::validate_is_file(tool_use_id, path, &input.filename) {
            return e;
        }

        // Open and read the file
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => return ToolResult::error(tool_use_id, format!("Failed to open file: {}", e)),
        };

        let reader = BufReader::new(file);
        let offset = input.offset.unwrap_or(0);
        let mut output = String::new();
        let mut line_count = 0;

        for (line_num, line_result) in reader.lines().enumerate() {
            // Skip lines before offset
            if line_num < offset {
                continue;
            }

            // Check if we've hit the limit
            if let Some(limit) = input.limit
                && line_count >= limit
            {
                break;
            }

            match line_result {
                Ok(line) => {
                    // Format with 1-based line numbers for display
                    output.push_str(&format!("{:6}\t{}\n", line_num + 1, line));
                    line_count += 1;
                }
                Err(e) => {
                    return ToolResult::error(
                        tool_use_id,
                        format!("Error reading line {}: {}", line_num + 1, e),
                    );
                }
            }
        }

        if output.is_empty() {
            if offset > 0 {
                return ToolResult::error(
                    tool_use_id,
                    format!("Offset {} is beyond the end of the file", offset),
                );
            }
            output = "(empty file)\n".to_string();
        }

        ToolResult::success(tool_use_id, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let tool = FileRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filename": "/nonexistent/path/file.txt"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("Path not found"));
    }

    #[tokio::test]
    async fn test_read_directory() {
        let tool = FileRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filename": "/tmp"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("not a file"));
    }
}
