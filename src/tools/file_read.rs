// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! File reading tool for LLM agents.
//!
//! Reads file contents with pagination support for large files. Output is
//! limited to prevent excessive token usage:
//!
//! - **Line limit**: Maximum of 2000 lines per read
//! - **Size limit**: Maximum of 50KB output per read
//!
//! When either limit is reached, output is truncated and a footer indicates
//! the next offset to continue reading. Long lines that would exceed the
//! size budget are truncated to fit.
//!
//! The tool handles non-UTF8 content gracefully using lossy conversion.

use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;

use super::{Tool, ToolDefinition, ToolResult};

/// Maximum number of lines to return before requiring pagination.
const MAX_LINES: usize = 2000;

/// Maximum output size in bytes before requiring pagination.
const MAX_OUTPUT_SIZE: usize = 50 * 1024; // 50KB

/// Tool for reading file contents.
pub(crate) struct FileRead;

#[derive(Debug, Deserialize)]
struct FileReadInput {
    filename: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Debug, Clone)]
struct OutputLine {
    /// 0-based line number in the file.
    idx: usize,
    /// The (possibly truncated) line content, with trailing newlines removed.
    content: String,
    /// True if this line's content was truncated due to output/size constraints.
    truncated: bool,
}

impl Tool for FileRead {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "file_read".to_string(),
            description: "Read the contents of a file. Returns the file contents as text."
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
        output: &crate::output::OutputContext,
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

        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => return ToolResult::error(tool_use_id, format!("Failed to open file: {}", e)),
        };

        let mut reader = BufReader::new(file);
        let offset = input.offset.unwrap_or(0);
        let user_limit = input.limit;

        // Skip to offset without allocating unbounded memory.
        let mut line_idx: usize = 0;
        while line_idx < offset {
            let advanced = match skip_one_line(&mut reader) {
                Ok(a) => a,
                Err(e) => {
                    return ToolResult::error(
                        tool_use_id,
                        format!("Error reading line {}: {}", line_idx + 1, e),
                    );
                }
            };

            if !advanced {
                break;
            }

            line_idx += 1;
        }

        let mut output_lines: Vec<OutputLine> = Vec::new();
        let mut output_bytes: usize = 0;

        // (reason, suggested_next_offset)
        let mut stop_reason: Option<(&'static str, usize)> = None;

        loop {
            // Apply limits BEFORE reading the next line to avoid allocating/processing another line.
            if let Some(limit) = user_limit
                && output_lines.len() >= limit
            {
                stop_reason = Some(("user limit", offset + limit));
                break;
            }

            if output_lines.len() >= MAX_LINES {
                stop_reason = Some(("line limit", line_idx));
                break;
            }

            // Each rendered line ends with a single '\n'.
            const NEWLINE_OVERHEAD: usize = 1;

            if output_bytes >= MAX_OUTPUT_SIZE {
                stop_reason = Some(("size limit", line_idx));
                break;
            }

            let remaining = MAX_OUTPUT_SIZE - output_bytes;
            if remaining < NEWLINE_OVERHEAD {
                stop_reason = Some(("size limit", line_idx));
                break;
            }

            let max_line_output_bytes = remaining - NEWLINE_OVERHEAD;

            let line = match read_one_line_lossy(&mut reader, max_line_output_bytes) {
                Ok(l) => l,
                Err(e) => {
                    return ToolResult::error(
                        tool_use_id,
                        format!("Error reading line {}: {}", line_idx + 1, e),
                    );
                }
            };

            let Some(line) = line else {
                break;
            };

            output_bytes += line.content.len() + NEWLINE_OVERHEAD;

            output_lines.push(OutputLine {
                idx: line_idx,
                content: line.content.clone(),
                truncated: line.truncated,
            });

            if output_lines.len() <= 3 {
                let formatted_line = format!("{}\n", line.content);
                crate::output::emit_file_read_output(output, &input.filename, &formatted_line);
            }

            line_idx += 1;

            if line.truncated {
                // If we had to truncate the current line to fit, we have hit the output budget.
                stop_reason = Some(("size limit", line_idx));
                break;
            }

            if output_bytes >= MAX_OUTPUT_SIZE {
                stop_reason = Some(("size limit", line_idx));
                break;
            }
        }

        if output_lines.is_empty() && offset > 0 {
            return ToolResult::error(
                tool_use_id,
                format!("Offset {} is beyond the end of the file", offset),
            );
        }

        let mut output_buf = if output_lines.is_empty() {
            "(empty file)\n".to_string()
        } else {
            render_lines(&output_lines)
        };

        // Append footer (allowed to push output a bit beyond MAX_OUTPUT_SIZE).
        let summary = build_bracket_summary(offset, &output_lines, stop_reason);
        output_buf.push_str(&summary);

        let tool_summary = format!(
            "(read {} lines, {} bytes)",
            output_lines.len(),
            output_buf.len()
        );
        ToolResult::success(tool_use_id, output_buf).with_summary(tool_summary)
    }
}

#[derive(Debug)]
struct LineRead {
    content: String,
    truncated: bool,
}

/// Skip exactly one line (up to and including a trailing '\n'), without allocating.
///
/// Returns `Ok(true)` if any bytes were consumed (including consuming a single '\n' for an empty
/// line). Returns `Ok(false)` if already at EOF.
fn skip_one_line<R: BufRead>(reader: &mut R) -> std::io::Result<bool> {
    let mut consumed_any = false;

    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(consumed_any);
        }
        consumed_any = true;

        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            reader.consume(pos + 1);
            return Ok(true);
        }

        let len = buf.len();
        reader.consume(len);
    }
}

/// Read one full line (up to and including a trailing '\n' if present), but only keep at most
/// `max_output_bytes` worth of line content in memory.
///
/// The returned `content` is UTF-8 via `from_utf8_lossy` and has no trailing newlines.
///
/// This function always advances the reader to the end of the line (newline or EOF), even when the
/// kept content is truncated.
fn read_one_line_lossy<R: BufRead>(
    reader: &mut R,
    max_output_bytes: usize,
) -> std::io::Result<Option<LineRead>> {
    let mut stored: Vec<u8> = Vec::with_capacity(max_output_bytes.min(1024));
    let mut total_len: usize = 0;
    let mut last_byte_was_cr = false;

    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            if total_len == 0 {
                return Ok(None);
            }

            // EOF terminates a line without a trailing newline.
            break;
        }

        if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            // We found a newline; consume up to and including it.
            if pos == 0 {
                // Empty line, but handle CRLF split across buffers.
                if last_byte_was_cr {
                    // Previous buffer ended with '\r' and this buffer starts with '\n' => CRLF.
                    total_len = total_len.saturating_sub(1);
                    if stored.last() == Some(&b'\r') {
                        stored.pop();
                    }
                }
            } else {
                let mut slice = &buf[..pos];

                // Strip CR in CRLF when both are in the same buffer.
                if slice.ends_with(b"\r") {
                    slice = &slice[..slice.len() - 1];
                }

                total_len += slice.len();

                if stored.len() < max_output_bytes {
                    let remaining = max_output_bytes - stored.len();
                    let to_copy = slice.len().min(remaining);
                    stored.extend_from_slice(&slice[..to_copy]);
                }
            }

            reader.consume(pos + 1);
            break;
        }

        // No newline in this buffer.
        total_len += buf.len();

        if stored.len() < max_output_bytes {
            let remaining = max_output_bytes - stored.len();
            let to_copy = buf.len().min(remaining);
            stored.extend_from_slice(&buf[..to_copy]);
        }

        last_byte_was_cr = buf.last() == Some(&b'\r');
        let len = buf.len();
        reader.consume(len);
    }

    let mut content = String::from_utf8_lossy(&stored).into_owned();
    let mut truncated = stored.len() < total_len;

    // Defensive: lossy conversion can expand invalid sequences (e.g., '\xFF' -> "\u{FFFD}").
    // Ensure we still honor the per-line output byte budget.
    if content.len() > max_output_bytes {
        content = truncate_utf8_to_bytes(&content, max_output_bytes);
        truncated = true;
    }

    Ok(Some(LineRead { content, truncated }))
}

fn truncate_utf8_to_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }

    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }

    s[..idx].to_string()
}

fn render_lines(lines: &[OutputLine]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(&line.content);
        out.push('\n');
    }
    out
}

fn build_bracket_summary(
    offset: usize,
    lines: &[OutputLine],
    stop_reason: Option<(&'static str, usize)>,
) -> String {
    let start = offset;
    let end = lines.last().map(|l| l.idx).unwrap_or(offset);

    let mut parts = Vec::new();
    parts.push(format!("Read lines {}..{}", start, end));

    for line in lines {
        if line.truncated {
            parts.push(format!(
                "Line {} truncated to {} bytes",
                line.idx,
                line.content.len()
            ));
        }
    }

    if let Some((_reason, next_offset)) = stop_reason {
        parts.push(format!("Next offset={}", next_offset));
    }

    format!("[{}]\n", parts.join("; "))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::NamedTempFile;

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

    #[tokio::test]
    async fn test_read_non_utf8_does_not_error() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&[0xff, 0xfe, 0xfd, b'\n']).unwrap();

        let tool = FileRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filename": tmp.path().to_string_lossy().to_string(),
                    "offset": 0,
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains('\u{FFFD}'));
    }

    #[tokio::test]
    async fn test_read_long_line_is_not_truncated_per_line_if_under_output_cap() {
        let mut tmp = NamedTempFile::new().unwrap();
        let long_line = "a".repeat(5000);
        writeln!(tmp, "{}", long_line).unwrap();

        let tool = FileRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filename": tmp.path().to_string_lossy().to_string(),
                    "offset": 0,
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains(&long_line));
    }

    #[tokio::test]
    async fn test_size_limit_truncates_last_line_to_fit() {
        let mut tmp = NamedTempFile::new().unwrap();
        let long_line = "a".repeat(60 * 1024);
        writeln!(tmp, "{}", long_line).unwrap();

        let tool = FileRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filename": tmp.path().to_string_lossy().to_string(),
                    "offset": 0,
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);

        let first_line = result.content.split('\n').next().unwrap_or("");
        assert!(first_line.len() <= MAX_OUTPUT_SIZE);
        assert!(first_line.len() < long_line.len());

        assert!(result.content.contains("[Read lines 0..0"));
        assert!(result.content.contains("Line 0 truncated to"));
    }

    #[tokio::test]
    async fn test_limit_does_not_read_extra_lines() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "line1").unwrap();
        writeln!(tmp, "line2").unwrap();

        let tool = FileRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filename": tmp.path().to_string_lossy().to_string(),
                    "offset": 0,
                    "limit": 1,
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.starts_with("line1\n"));
        assert!(!result.content.contains("line2"));
        assert!(result.content.contains("Next offset=1"));
    }
}
