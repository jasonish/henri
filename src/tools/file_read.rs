// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use serde::Deserialize;

use super::{Tool, ToolDefinition, ToolResult};

/// Maximum number of characters to return per line before truncation.
const MAX_LINE_LENGTH: usize = 2048;

/// Maximum number of lines to return before requiring pagination.
const MAX_LINES: usize = 2000;

/// Maximum output size in bytes before requiring pagination.
const MAX_OUTPUT_SIZE: usize = 50 * 1024; // 50KB

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

        let expanded_filename = super::expand_tilde(&input.filename);
        let path = Path::new(&expanded_filename);

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
        let user_limit = input.limit;
        let mut output = String::new();
        let mut line_count = 0;
        // Tracks where we stopped if a defensive limit was hit: (line_num, reason)
        let mut truncated_at: Option<(usize, &str)> = None;

        for (line_num, line_result) in BoundedLineReader::new(reader, MAX_LINE_LENGTH).enumerate() {
            if line_num < offset {
                continue;
            }

            // User-specified limit takes priority (no pagination hint needed)
            if let Some(limit) = user_limit
                && line_count >= limit
            {
                break;
            }

            if line_count >= MAX_LINES {
                truncated_at = Some((line_num, "line limit"));
                break;
            }

            match line_result {
                Ok((line, truncated_total)) => {
                    let formatted_line = if let Some(total_len) = truncated_total {
                        format!(
                            "{:6}\t{}... <truncated, total length: {}>\n",
                            line_num + 1,
                            line,
                            total_len
                        )
                    } else {
                        format!("{:6}\t{}\n", line_num + 1, line)
                    };

                    if output.len() + formatted_line.len() > MAX_OUTPUT_SIZE {
                        truncated_at = Some((line_num, "size limit"));
                        break;
                    }

                    output.push_str(&formatted_line);
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

        if let Some((line_num, reason)) = truncated_at {
            let size_kb = output.len() / 1024;
            output.push_str(&format!(
                "\n--- Output truncated ({reason}, {line_count} lines, ~{size_kb}KB) ---\n\
                 Use offset={line_num} to continue reading\n"
            ));
        }

        ToolResult::success(tool_use_id, output)
    }
}

/// An iterator that reads lines with bounded memory usage.
///
/// Each line is limited to `max_len` characters. If a line exceeds this limit,
/// it is truncated and the total original length is reported.
struct BoundedLineReader<R> {
    reader: BufReader<R>,
    max_len: usize,
    done: bool,
}

impl<R: Read> BoundedLineReader<R> {
    fn new(reader: BufReader<R>, max_len: usize) -> Self {
        Self {
            reader,
            max_len,
            done: false,
        }
    }

    /// Reads the next line, returning (content, Option<total_length_if_truncated>)
    fn read_next_line(&mut self) -> std::io::Result<Option<(String, Option<usize>)>> {
        if self.done {
            return Ok(None);
        }

        let mut line = Vec::with_capacity(self.max_len.min(1024));
        let mut total_len = 0usize;
        let mut found_newline = false;

        // Phase 1: Read up to max_len bytes, looking for newline
        while line.len() < self.max_len {
            let buf = self.reader.fill_buf()?;
            if buf.is_empty() {
                self.done = true;
                break;
            }

            let remaining = self.max_len - line.len();
            let search_len = buf.len().min(remaining);

            if let Some(pos) = buf[..search_len].iter().position(|&b| b == b'\n') {
                // Found newline within our limit
                line.extend_from_slice(&buf[..pos]);
                total_len += pos;
                self.reader.consume(pos + 1); // consume including newline
                found_newline = true;
                break;
            } else if search_len < buf.len() {
                // We hit our max_len limit before finding newline
                line.extend_from_slice(&buf[..search_len]);
                total_len += search_len;
                self.reader.consume(search_len);
                break;
            } else {
                // Consume entire buffer, keep looking
                line.extend_from_slice(buf);
                total_len += buf.len();
                let to_consume = buf.len();
                self.reader.consume(to_consume);
            }
        }

        // Check if we have anything
        if line.is_empty() && self.done && total_len == 0 {
            return Ok(None);
        }

        // Phase 2: If we didn't find a newline, check if there's more content to skip
        let truncated_total = if !found_newline && !self.done {
            // Check if the next character is a newline (line exactly at limit)
            let buf = self.reader.fill_buf()?;
            if buf.is_empty() {
                self.done = true;
                None // Line ended at EOF, exactly at limit - not truncated
            } else if buf[0] == b'\n' {
                // Next char is newline - line was exactly at limit, not truncated
                self.reader.consume(1);
                None
            } else {
                // There's more content - this line is truly truncated
                loop {
                    let buf = self.reader.fill_buf()?;
                    if buf.is_empty() {
                        self.done = true;
                        break;
                    }

                    if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                        total_len += pos;
                        self.reader.consume(pos + 1);
                        break;
                    } else {
                        total_len += buf.len();
                        let to_consume = buf.len();
                        self.reader.consume(to_consume);
                    }
                }
                Some(total_len)
            }
        } else {
            None
        };

        let line_str = String::from_utf8_lossy(&line).into_owned();
        Ok(Some((line_str, truncated_total)))
    }
}

impl<R: Read> Iterator for BoundedLineReader<R> {
    type Item = std::io::Result<(String, Option<usize>)>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.read_next_line() {
            Ok(Some(line)) => Some(Ok(line)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
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

    #[test]
    fn test_bounded_line_reader_normal_lines() {
        let content = "line one\nline two\nline three\n";
        let reader = BufReader::new(content.as_bytes());
        let lines: Vec<_> = BoundedLineReader::new(reader, 100)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], ("line one".to_string(), None));
        assert_eq!(lines[1], ("line two".to_string(), None));
        assert_eq!(lines[2], ("line three".to_string(), None));
    }

    #[test]
    fn test_bounded_line_reader_truncation() {
        // Create a line that exceeds the limit
        let long_line = "a".repeat(5000);
        let content = format!("{}\nshort line\n", long_line);
        let reader = BufReader::new(content.as_bytes());
        let lines: Vec<_> = BoundedLineReader::new(reader, 100)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(lines.len(), 2);
        // First line should be truncated
        assert_eq!(lines[0].0.len(), 100);
        assert_eq!(lines[0].1, Some(5000)); // Original length
        // Second line should be normal
        assert_eq!(lines[1], ("short line".to_string(), None));
    }

    #[test]
    fn test_bounded_line_reader_exact_limit() {
        let line = "a".repeat(100);
        let content = format!("{}\n", line);
        let reader = BufReader::new(content.as_bytes());
        let lines: Vec<_> = BoundedLineReader::new(reader, 100)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(lines.len(), 1);
        // Exactly at limit should not be truncated
        assert_eq!(lines[0], (line, None));
    }

    #[test]
    fn test_bounded_line_reader_one_over_limit() {
        let line = "a".repeat(101);
        let content = format!("{}\n", line);
        let reader = BufReader::new(content.as_bytes());
        let lines: Vec<_> = BoundedLineReader::new(reader, 100)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(lines.len(), 1);
        // One over limit should be truncated
        assert_eq!(lines[0].0.len(), 100);
        assert_eq!(lines[0].1, Some(101));
    }

    #[test]
    fn test_bounded_line_reader_no_trailing_newline() {
        let content = "line one\nline two";
        let reader = BufReader::new(content.as_bytes());
        let lines: Vec<_> = BoundedLineReader::new(reader, 100)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], ("line one".to_string(), None));
        assert_eq!(lines[1], ("line two".to_string(), None));
    }

    #[test]
    fn test_bounded_line_reader_empty_file() {
        let content = "";
        let reader = BufReader::new(content.as_bytes());
        let lines: Vec<_> = BoundedLineReader::new(reader, 100)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(lines.is_empty());
    }

    #[test]
    fn test_bounded_line_reader_empty_lines() {
        let content = "\n\n\n";
        let reader = BufReader::new(content.as_bytes());
        let lines: Vec<_> = BoundedLineReader::new(reader, 100)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], ("".to_string(), None));
        assert_eq!(lines[1], ("".to_string(), None));
        assert_eq!(lines[2], ("".to_string(), None));
    }
}
