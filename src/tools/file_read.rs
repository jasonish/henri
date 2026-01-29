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
//! The tool handles non-UTF8 text using lossy conversion.
//! Binary files (like images) are returned as base64 with metadata.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use base64::{Engine, engine::general_purpose::STANDARD};
use image::GenericImageView;
use image::ImageFormat;
use image::imageops::FilterType;
use serde::Deserialize;

use super::{Tool, ToolDefinition, ToolResult};

/// Maximum number of lines to return before requiring pagination.
const MAX_LINES: usize = 2000;

/// Maximum output size in bytes before requiring pagination.
const MAX_OUTPUT_SIZE: usize = 50 * 1024; // 50KB

/// Maximum raw bytes read for binary files. Set to None to read entire file.
const MAX_BINARY_BYTES: Option<usize> = None;

/// Image resizing limits (Codex-compatible).
const MAX_IMAGE_WIDTH: u32 = 2048;
const MAX_IMAGE_HEIGHT: u32 = 768;

/// Maximum base64 length to include in tool output for images.
///
/// Even resized screenshots can be too large to embed directly in a tool result without
/// overflowing a model's context window.
const MAX_IMAGE_BASE64_LEN: usize = 200_000;

/// Byte window size for detecting binary files.
const BINARY_DETECT_BYTES: usize = 1024;

/// If more than this fraction of bytes are non-printable, treat as binary.
const BINARY_DETECT_RATIO: f32 = 0.30;

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
            description: "Read the contents of a file. Text files and images are supported. Returns text for normal files and base64 for binary files like images. Do not provide offset or limit for images."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "filename": {
                        "type": "string",
                        "description": "The path to the file to read (REQUIRED)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "0-based line number to start reading from (default: 0) (OPTIONAL)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read (default: read all lines) (OPTIONAL)"
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
            Err(e) => return *e,
        };

        let expanded_filename = super::expand_tilde(&input.filename);
        let path = Path::new(&expanded_filename);

        if let Err(e) = super::validate_path_exists(tool_use_id, path, &input.filename) {
            return *e;
        }
        if let Err(e) = super::validate_is_file(tool_use_id, path, &input.filename) {
            return *e;
        }

        let mut file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => return ToolResult::error(tool_use_id, format!("Failed to open file: {}", e)),
        };

        let size_bytes = file.metadata().ok().map(|m| m.len());

        let mut sniff_buf = vec![0u8; BINARY_DETECT_BYTES];
        let sniff_len = match file.read(&mut sniff_buf) {
            Ok(n) => n,
            Err(e) => return ToolResult::error(tool_use_id, format!("Failed to read file: {}", e)),
        };
        sniff_buf.truncate(sniff_len);

        // Check for image files first and handle separately.
        if let Some(image_mime) = detect_image_mime(&sniff_buf, path) {
            return read_image_file(
                tool_use_id,
                &input.filename,
                file,
                sniff_buf,
                size_bytes,
                image_mime,
                output,
            );
        }

        // Handle other binary files.
        if is_probably_binary(&sniff_buf) {
            let (bytes, truncated) = match read_binary_bytes(file, sniff_buf) {
                Ok(result) => result,
                Err(e) => {
                    return ToolResult::error(tool_use_id, format!("Failed to read file: {}", e));
                }
            };

            let base64_data = STANDARD.encode(&bytes);
            let output_buf = render_binary_output(
                &input.filename,
                "application/octet-stream",
                size_bytes,
                bytes.len(),
                truncated,
                &base64_data,
            );

            for line in output_buf.lines().take(3) {
                let formatted_line = format!("{}\n", line);
                crate::output::emit_file_read_output(output, &input.filename, &formatted_line);
            }

            let summary = build_binary_summary(
                size_bytes,
                bytes.len(),
                bytes.len(),
                base64_data.len(),
                truncated,
            );

            return ToolResult::success(tool_use_id, output_buf).with_summary(summary);
        }

        if file.seek(SeekFrom::Start(0)).is_err() {
            file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(e) => {
                    return ToolResult::error(tool_use_id, format!("Failed to open file: {}", e));
                }
            };
        }

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
            "[Read {} lines, {} bytes]",
            output_lines.len(),
            output_buf.len()
        );
        ToolResult::success(tool_use_id, output_buf).with_summary(tool_summary)
    }
}

/// Handle reading an image file, including resizing and encoding for tool output.
fn read_image_file(
    tool_use_id: &str,
    filename: &str,
    file: std::fs::File,
    sniff_buf: Vec<u8>,
    size_bytes: Option<u64>,
    mime_type: &'static str,
    output: &crate::output::OutputContext,
) -> ToolResult {
    let (bytes, truncated) = match read_binary_bytes(file, sniff_buf) {
        Ok(result) => result,
        Err(e) => {
            return ToolResult::error(tool_use_id, format!("Failed to read file: {}", e));
        }
    };

    let (final_bytes, final_mime, original_meta, final_meta) =
        match prepare_image_for_tool_output(&bytes, mime_type) {
            Ok(ImageToolOutput::AsIs { meta }) => (bytes, mime_type.to_string(), meta, None),
            Ok(ImageToolOutput::Processed {
                bytes: out,
                mime,
                original,
                final_meta,
            }) => (out, mime, original, Some(final_meta)),
            // If we cannot decode it as an image, fall back to returning the original bytes.
            Err(_e) => (
                bytes.clone(),
                mime_type.to_string(),
                ImageMeta {
                    width: 0,
                    height: 0,
                },
                None,
            ),
        };

    let base64_data = STANDARD.encode(&final_bytes);

    // Build content string for the model (full details).
    let disk_bytes = size_bytes
        .map(|b| b.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let trunc_note = if truncated { "; Truncated" } else { "" };

    let content = format!(
        "[Filename=\"{}\"; OriginalBytes={}; OriginalSize={}x{}; Type={}{}]",
        filename, disk_bytes, original_meta.width, original_meta.height, final_mime, trunc_note
    );

    // Build shorter summary for UI display.
    let summary = if let Some(resized) = final_meta {
        format!(
            "[Type={}; ResizedTo={}x{}]",
            final_mime, resized.width, resized.height
        )
    } else {
        format!("[Type={}]", final_mime)
    };

    // Emit image preview for terminals that support inline images (e.g., Kitty).
    crate::output::emit_image_preview(output, base64_data.clone(), final_mime.clone());

    ToolResult::success(tool_use_id, content)
        .with_summary(summary)
        .with_data(base64_data)
        .with_mime_type(final_mime)
}

fn is_heif_brand(brand: &[u8; 4]) -> bool {
    matches!(
        brand,
        b"heic" | b"heix" | b"hevc" | b"heim" | b"heis" | b"hevm" | b"hevs" | b"mif1" | b"msf1"
    )
}

fn is_iso_bmff_heif(bytes: &[u8]) -> bool {
    // ISO BMFF: [size:4][type:4] where type == "ftyp"
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }

    let Ok(major_brand) = bytes[8..12].try_into() else {
        return false;
    };

    if is_heif_brand(&major_brand) {
        return true;
    }

    // Scan a small window of compatible brands.
    let mut i = 16;
    let max = bytes.len().min(64);
    while i + 4 <= max {
        let Ok(brand) = bytes[i..i + 4].try_into() else {
            break;
        };
        if is_heif_brand(&brand) {
            return true;
        }
        i += 4;
    }

    false
}

fn detect_image_mime(bytes: &[u8], path: &Path) -> Option<&'static str> {
    if bytes.len() >= 8 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }

    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some("image/jpeg");
    }

    if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some("image/gif");
    }

    if is_iso_bmff_heif(bytes) {
        return Some("image/heic");
    }

    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        "tif" | "tiff" => Some("image/tiff"),
        _ => None,
    }
}

fn image_format_from_mime(mime_type: &str) -> Option<ImageFormat> {
    match mime_type {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/bmp" => Some(ImageFormat::Bmp),
        "image/tiff" => Some(ImageFormat::Tiff),
        "image/webp" => Some(ImageFormat::WebP),
        _ => None,
    }
}

fn fit_within_bounds(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }

    if width <= max_w && height <= max_h {
        return (width, height);
    }

    let scale_w = max_w as f64 / width as f64;
    let scale_h = max_h as f64 / height as f64;
    let scale = scale_w.min(scale_h);

    let new_w = ((width as f64) * scale).round().max(1.0) as u32;
    let new_h = ((height as f64) * scale).round().max(1.0) as u32;
    (new_w.min(max_w), new_h.min(max_h))
}

#[derive(Debug, Clone, Copy)]
struct ImageMeta {
    width: u32,
    height: u32,
}

enum ImageToolOutput {
    AsIs {
        meta: ImageMeta,
    },
    Processed {
        bytes: Vec<u8>,
        mime: String,
        original: ImageMeta,
        final_meta: ImageMeta,
    },
}

fn prepare_image_for_tool_output(bytes: &[u8], mime_type: &str) -> Result<ImageToolOutput, String> {
    let Some(input_format) = image_format_from_mime(mime_type) else {
        return Err("unknown image format".to_string());
    };

    let img = image::load_from_memory_with_format(bytes, input_format)
        .map_err(|e| format!("decode failed: {}", e))?;

    let (w, h) = img.dimensions();
    let original = ImageMeta {
        width: w,
        height: h,
    };

    let (bounded_w, bounded_h) = fit_within_bounds(w, h, MAX_IMAGE_WIDTH, MAX_IMAGE_HEIGHT);
    let mut cur = if bounded_w != w || bounded_h != h {
        img.resize(bounded_w, bounded_h, FilterType::Triangle)
    } else {
        img
    };

    let needs_resize = bounded_w != w || bounded_h != h;

    // If the original was PNG/JPEG and we didn't resize, returning as-is is fine.
    if !needs_resize && matches!(input_format, ImageFormat::Png | ImageFormat::Jpeg) {
        return Ok(ImageToolOutput::AsIs { meta: original });
    }

    for _ in 0..6 {
        let mut out = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        cur.write_with_encoder(encoder)
            .map_err(|e| format!("encode failed: {}", e))?;

        let b64_len = out.len().div_ceil(3) * 4;
        if b64_len <= MAX_IMAGE_BASE64_LEN {
            let (fw, fh) = cur.dimensions();
            let final_meta = ImageMeta {
                width: fw,
                height: fh,
            };
            return Ok(ImageToolOutput::Processed {
                bytes: out,
                mime: "image/jpeg".to_string(),
                original,
                final_meta,
            });
        }

        let (cw, ch) = cur.dimensions();
        if cw <= 1 || ch <= 1 {
            break;
        }

        let next_w = ((cw as f64) * 0.75).round().max(1.0) as u32;
        let next_h = ((ch as f64) * 0.75).round().max(1.0) as u32;
        cur = cur.resize(next_w, next_h, FilterType::Triangle);
    }

    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 70);
    cur.write_with_encoder(encoder)
        .map_err(|e| format!("encode failed: {}", e))?;

    let (fw, fh) = cur.dimensions();
    let final_meta = ImageMeta {
        width: fw,
        height: fh,
    };

    Ok(ImageToolOutput::Processed {
        bytes: out,
        mime: "image/jpeg".to_string(),
        original,
        final_meta,
    })
}

fn is_probably_binary(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }

    let mut non_printable = 0usize;
    for &b in bytes {
        if b == 0 {
            non_printable += 1;
            continue;
        }
        if b.is_ascii()
            && !(b.is_ascii_graphic() || b == b'\n' || b == b'\r' || b == b'\t' || b == b' ')
        {
            non_printable += 1;
        }
    }

    (non_printable as f32 / bytes.len() as f32) >= BINARY_DETECT_RATIO
}

fn read_binary_bytes(mut file: std::fs::File, prefix: Vec<u8>) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = prefix;

    // If no limit, read the entire file.
    if MAX_BINARY_BYTES.is_none() {
        file.read_to_end(&mut bytes)?;
        return Ok((bytes, false));
    }

    let max_bytes = MAX_BINARY_BYTES.unwrap();
    if bytes.len() >= max_bytes {
        bytes.truncate(max_bytes);
        return Ok((bytes, true));
    }

    let remaining = max_bytes - bytes.len();
    let mut rest = vec![0u8; remaining];
    let read_len = file.read(&mut rest)?;
    rest.truncate(read_len);
    bytes.extend_from_slice(&rest);

    if read_len < remaining {
        return Ok((bytes, false));
    }

    let truncated = file.read(&mut [0u8; 1])? > 0;
    Ok((bytes, truncated))
}

fn render_binary_output(
    filename: &str,
    mime_type: &str,
    size_bytes: Option<u64>,
    bytes_read: usize,
    truncated: bool,
    base64_data: &str,
) -> String {
    let mut out = String::new();
    out.push_str("[binary file]\n");
    out.push_str(&format!("File: {}\n", filename));
    out.push_str(&format!("MIME: {}\n", mime_type));
    if let Some(size) = size_bytes {
        out.push_str(&format!("Size: {} bytes\n", size));
    }
    out.push_str(&format!("Bytes read: {}\n", bytes_read));
    if truncated {
        out.push_str("Truncated: true\n");
    }
    out.push_str("Encoding: base64\n");
    out.push_str("Data:\n");
    out.push_str(base64_data);
    out.push('\n');
    out
}

fn build_binary_summary(
    size_bytes: Option<u64>,
    disk_bytes_read: usize,
    tool_bytes: usize,
    base64_len: usize,
    truncated: bool,
) -> String {
    let size = size_bytes
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let trunc = if truncated { " truncated" } else { "" };
    format!(
        "[binary {} bytes, read {} bytes, tool {} bytes, base64 {} bytes{}]",
        size, disk_bytes_read, tool_bytes, base64_len, trunc
    )
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

    use base64::Engine;

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
    async fn test_read_png_returns_base64() {
        let mut tmp = NamedTempFile::new().unwrap();

        // Write a valid 1x1 PNG.
        let img = image::RgbImage::from_fn(1, 1, |_x, _y| image::Rgb([1, 2, 3]));
        let dyn_img = image::DynamicImage::ImageRgb8(img);
        let mut bytes = Vec::new();
        dyn_img
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        tmp.write_all(&bytes).unwrap();

        let tool = FileRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filename": tmp.path().to_string_lossy().to_string(),
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);

        let expected = STANDARD.encode(&bytes);
        // Content has full details for the model.
        assert!(result.content.starts_with("[Filename="));
        assert!(result.content.contains("Type=image/png"));
        assert_eq!(result.data.as_deref().unwrap(), expected);
        assert_eq!(result.mime_type.as_deref().unwrap(), "image/png");
        // Summary is shorter for UI display.
        assert!(
            result
                .summary
                .as_deref()
                .unwrap_or("")
                .starts_with("[Type=")
        );
    }

    #[tokio::test]
    async fn test_bmff_mp4_is_not_detected_as_heic() {
        let mut tmp = NamedTempFile::new().unwrap();

        // Minimal ISO BMFF ftyp box for an MP4-like file (major brand "isom").
        // Old HEIC sniffing would incorrectly classify this as image/heic.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x18]);
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"isom");
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        bytes.extend_from_slice(b"isom");
        bytes.extend_from_slice(b"mp42");

        tmp.write_all(&bytes).unwrap();

        let tool = FileRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filename": tmp.path().to_string_lossy().to_string(),
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(result.data.is_none());
        assert_ne!(result.mime_type.as_deref(), Some("image/heic"));
    }

    #[tokio::test]
    async fn test_read_large_image_is_resized_not_cropped() {
        let mut tmp = NamedTempFile::new().unwrap();

        // Create a mostly-uniform image so resampling doesn't blend corners away.
        let img = image::RgbImage::from_fn(4000, 1000, |x, y| {
            if x < 2000 && y < 500 {
                image::Rgb([255, 0, 0])
            } else if x >= 2000 && y < 500 {
                image::Rgb([0, 255, 0])
            } else if x < 2000 {
                image::Rgb([0, 0, 255])
            } else {
                image::Rgb([255, 255, 255])
            }
        });

        let dyn_img = image::DynamicImage::ImageRgb8(img);
        let mut png_bytes = Vec::new();
        dyn_img
            .write_to(&mut std::io::Cursor::new(&mut png_bytes), ImageFormat::Png)
            .unwrap();

        tmp.write_all(&png_bytes).unwrap();

        let tool = FileRead;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "filename": tmp.path().to_string_lossy().to_string(),
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        // Content is now a simple bracket format, not JSON.
        assert!(result.content.starts_with("[Filename="));
        assert!(
            result
                .mime_type
                .as_deref()
                .unwrap_or("")
                .starts_with("image/")
        );

        let data = result.data.as_deref().unwrap();
        let decoded = STANDARD.decode(data).unwrap();

        let out_img = image::load_from_memory(&decoded).unwrap();
        let (w, h) = out_img.dimensions();

        // Fit within 2048x768 should preserve aspect ratio: 4000x1000 -> 2048x512.
        assert_eq!(w, 2048);
        assert_eq!(h, 512);

        let px_tl = out_img.get_pixel(0, 0);
        let px_tr = out_img.get_pixel(w - 1, 0);
        let px_bl = out_img.get_pixel(0, h - 1);
        let px_br = out_img.get_pixel(w - 1, h - 1);

        // JPEG introduces minor artifacts; use threshold checks.
        assert!(px_tl.0[0] > 200 && px_tl.0[1] < 60 && px_tl.0[2] < 60);
        assert!(px_tr.0[1] > 200 && px_tr.0[0] < 60 && px_tr.0[2] < 60);
        assert!(px_bl.0[2] > 200 && px_bl.0[0] < 60 && px_bl.0[1] < 60);
        assert!(px_br.0[0] > 200 && px_br.0[1] > 200 && px_br.0[2] > 200);
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
