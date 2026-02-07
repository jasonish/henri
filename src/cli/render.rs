// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Rendering functions for CLI history events.
//!
//! Converts semantic HistoryEvents into styled, wrapped text lines
//! suitable for terminal display.

use std::borrow::Cow;

use colored::{Color, Colorize};
use unicode_width::UnicodeWidthChar;

use super::history::{HistoryEvent, ImageMeta};
use super::markdown::{align_markdown_tables, render_markdown_line};
use crate::cli::image_preview;
use crate::cli::spacing::{LastBlock, needs_blank_line_before};
use crate::syntax;

// Shared color constants for consistent styling

// ANSI background for user prompt lines (dark grey) using truecolor.
//
// We use this to fill the remainder of a prompt line using `\x1b[K`
// (erase-to-end-of-line) without printing trailing spaces. This avoids terminals
// auto-wrapping when the last column is filled.
pub(super) const BG_GREY_ANSI: &str = "\x1b[48;2;48;48;48m";
pub(super) const BG_DARK_GREEN: Color = Color::TrueColor { r: 0, g: 20, b: 0 };
pub(super) const BG_DARK_RED: Color = Color::TrueColor { r: 20, g: 0, b: 0 };

/// Render a single history event to styled text.
///
/// This renderer intentionally avoids inserting blank lines between events.
pub(crate) fn render_event(event: &HistoryEvent, width: usize) -> String {
    match event {
        HistoryEvent::UserPrompt { text, images } => render_user_prompt(text, images, width),
        HistoryEvent::AssistantText { text, is_streaming } => {
            render_assistant_text(text, *is_streaming, width)
        }
        HistoryEvent::Thinking { text, is_streaming } => {
            render_thinking(text, *is_streaming, width)
        }
        // No output for block boundaries.
        HistoryEvent::ThinkingEnd | HistoryEvent::ResponseEnd => String::new(),
        HistoryEvent::ToolStart | HistoryEvent::ToolEnd => String::new(),
        HistoryEvent::ToolUse { description } => render_tool_use(description),
        HistoryEvent::ToolResult {
            is_error,
            output,
            summary,
        } => render_tool_result(*is_error, output, summary.as_deref()),
        HistoryEvent::ToolOutput {
            text, total_lines, ..
        } => render_tool_output(text, *total_lines, width),
        HistoryEvent::FileReadOutput {
            filename,
            text,
            total_lines,
            ..
        } => render_file_read_output(filename, text, *total_lines, width),
        HistoryEvent::ImagePreview { data, mime_type } => render_image_preview(data, mime_type),
        HistoryEvent::Error(msg) => render_error(msg),
        HistoryEvent::Warning(msg) => render_warning(msg),
        HistoryEvent::Info(msg) => render_info(msg),
        HistoryEvent::FileDiff {
            diff,
            language,
            summary,
        } => render_file_diff(diff, language.as_deref(), summary.as_deref()),
        HistoryEvent::AutoCompact { message } => render_auto_compact(message),
    }
}

fn last_block_for_event(event: &HistoryEvent) -> Option<LastBlock> {
    match event {
        HistoryEvent::UserPrompt { .. } => Some(LastBlock::UserPrompt),
        HistoryEvent::AssistantText { .. } => Some(LastBlock::Text),
        HistoryEvent::Thinking { .. } => Some(LastBlock::Thinking),
        HistoryEvent::Info(_) | HistoryEvent::Error(_) | HistoryEvent::Warning(_) => {
            Some(LastBlock::Info)
        }
        HistoryEvent::ToolUse { .. } => Some(LastBlock::ToolCall),
        HistoryEvent::ToolResult { .. }
        | HistoryEvent::ToolOutput { .. }
        | HistoryEvent::FileReadOutput { .. }
        | HistoryEvent::ImagePreview { .. }
        | HistoryEvent::FileDiff { .. } => Some(LastBlock::ToolContent),
        HistoryEvent::ToolStart
        | HistoryEvent::ToolEnd
        | HistoryEvent::ThinkingEnd
        | HistoryEvent::ResponseEnd
        | HistoryEvent::AutoCompact { .. } => None,
    }
}

fn trailing_newlines(s: &str) -> usize {
    let mut count = 0usize;
    for &b in s.as_bytes().iter().rev() {
        match b {
            b'\n' => count += 1,
            b'\r' => {}
            _ => break,
        }
    }
    count
}

fn ensure_trailing_newlines(output: &mut String, min: usize) {
    let existing = trailing_newlines(output);
    if existing >= min {
        return;
    }
    output.push_str(&"\n".repeat(min - existing));
}

fn should_suppress_success_checkmark(diff_shown: &mut bool, event: &HistoryEvent) -> bool {
    match event {
        HistoryEvent::FileDiff { .. } => {
            *diff_shown = true;
            false
        }
        HistoryEvent::ToolResult {
            is_error: false, ..
        } if *diff_shown => {
            *diff_shown = false;
            true
        }
        // On errors, keep the flag set so the next successful ToolResult still suppresses.
        HistoryEvent::ToolResult { is_error: true, .. } => false,
        _ => false,
    }
}

/// Render all history events.
pub(crate) fn render_all(events: &[HistoryEvent], width: usize) -> String {
    let mut output = String::new();
    let mut last_block: Option<LastBlock> = None;

    // History includes ToolStart/ToolEnd boundaries so we can match live spacing rules.
    let mut in_tool_block = false;

    // Mirrors `CliListener`'s one-shot diff_shown behavior so we don't suppress multiple ✓'s.
    let mut diff_shown = false;

    for event in events.iter() {
        match event {
            HistoryEvent::ToolStart => {
                in_tool_block = true;
            }
            HistoryEvent::ToolEnd => {
                in_tool_block = false;
            }
            _ => {}
        }

        let suppressed_tool_result = should_suppress_success_checkmark(&mut diff_shown, event);

        let current_block = if suppressed_tool_result {
            None
        } else {
            last_block_for_event(event)
        };

        // Insert blank line between blocks where appropriate.
        if let Some(current_block) = current_block
            && needs_blank_line_before(last_block, current_block)
        {
            // Mirror live streaming behavior:
            // - Info blocks are separated.
            // - Tool results stay grouped with any preceding info emitted by that tool.
            if matches!(event, HistoryEvent::ToolResult { .. })
                && in_tool_block
                && last_block == Some(LastBlock::Info)
            {
                ensure_trailing_newlines(&mut output, 1);
            } else {
                ensure_trailing_newlines(&mut output, 2);
            }
        }

        // Ensure there is a blank line between tool output and an info message.
        // Skip if the previous rendered event already ends with a newline (e.g. FileDiff output)
        // so we don't accidentally create a double-blank separator.
        if matches!(event, HistoryEvent::Info(_)) && in_tool_block && trailing_newlines(&output) < 1
        {
            ensure_trailing_newlines(&mut output, 2);
        }

        let rendered = match event {
            HistoryEvent::ToolUse { description } => {
                let mut s = render_tool_use(description);
                s.push('\n');
                s
            }
            HistoryEvent::ToolResult {
                is_error,
                output,
                summary,
            } => {
                if suppressed_tool_result {
                    String::new()
                } else {
                    render_tool_result_with_context(*is_error, output, summary.as_deref(), false)
                }
            }
            _ => render_event(event, width),
        };

        output.push_str(&rendered);

        if let Some(current_block) = current_block
            && !rendered.is_empty()
        {
            last_block = Some(current_block);
        }
    }

    // Avoid leaving the output cursor on an empty trailing line above the prompt/status line.
    while output.ends_with(['\n', '\r']) {
        output.pop();
    }

    output
}

/// Render user prompt with grey background spanning full width
///
/// The text may contain inline image markers like `Image#1` which are colorized.
fn render_user_prompt(text: &str, _images: &[ImageMeta], width: usize) -> String {
    // Use a slightly reduced width to avoid terminals auto-wrapping when the last column
    // is filled. We still paint the rest of the line using `\x1b[K`.
    let safe_width = width.saturating_sub(1).max(1);
    let content_width = safe_width;

    let mut output = String::new();

    if text.is_empty() {
        // Empty prompt: still render a single prompt row.
        output.push_str(BG_GREY_ANSI);
        output.push_str("\x1b[K\x1b[0m\n");
        return output;
    }

    for line in text.lines() {
        let wrapped = wrap_text(line, content_width);
        for wrapped_line in &wrapped {
            // Colorize image markers in the line, restoring grey background after each marker.
            let styled_line = colorize_image_markers(wrapped_line, Some(BG_GREY_ANSI));

            // Full-width grey background without printing trailing spaces.
            output.push_str(BG_GREY_ANSI);
            output.push_str(&styled_line);
            output.push_str(BG_GREY_ANSI);
            output.push_str("\x1b[K\x1b[0m\n");
        }
    }

    output
}

// Background color for image markers - dark cyan/teal
const BG_IMAGE_MARKER: Color = Color::TrueColor { r: 0, g: 40, b: 50 };

use super::input::IMAGE_MARKER_PREFIX;

/// Colorize image markers like `Image#1` with cyan text on dark background.
/// Matches "Image#" followed by one or more digits.
///
/// If `restore_bg` is provided, the ANSI sequence is emitted after each marker
/// to restore the surrounding background color (e.g., the grey user-prompt background).
pub(super) fn colorize_image_markers(text: &str, restore_bg: Option<&str>) -> String {
    use colored::Colorize;

    let mut result = String::new();
    let chars: Vec<char> = text.chars().collect();
    let prefix_chars: Vec<char> = IMAGE_MARKER_PREFIX.chars().collect();
    let prefix_len = prefix_chars.len();
    let mut i = 0;

    while i < chars.len() {
        // Check for "Image#" followed by at least one digit
        // Need room for prefix + at least one digit
        let matches_prefix = if i + prefix_len < chars.len() {
            prefix_chars
                .iter()
                .enumerate()
                .all(|(j, &pc)| chars[i + j] == pc)
        } else {
            false
        };

        if matches_prefix && chars[i + prefix_len].is_ascii_digit() {
            // Found "Image#" + digit, collect the full marker
            let start = i;
            i += prefix_len; // Skip "Image#"

            // Collect all following digits
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }

            let marker: String = chars[start..i].iter().collect();
            result.push_str(&marker.cyan().on_color(BG_IMAGE_MARKER).to_string());

            // Restore surrounding background if provided (the marker styling resets all attributes)
            if let Some(bg) = restore_bg {
                result.push_str(bg);
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

fn strip_carriage_returns(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace('\r', ""))
    } else {
        Cow::Borrowed(text)
    }
}

/// Render assistant text with syntax highlighting for code blocks
fn render_assistant_text(text: &str, _is_streaming: bool, width: usize) -> String {
    // Streaming output ignores carriage returns; mirror that here so history
    // redraw/replay matches what was shown live.
    let text = strip_carriage_returns(text);

    // Match live streaming width (`CliListener::terminal_width`) so replay/redraw wraps
    // assistant markdown identically.
    let content_width = width.saturating_sub(2).max(1);

    // Align markdown tables if they fit within the width
    let aligned_text = align_markdown_tables(text.as_ref(), Some(content_width));

    let mut output = String::new();
    let mut pos = 0;

    // Find and process code blocks
    while let Some((block_start, block_end, highlighted)) = find_next_code_block(&aligned_text, pos)
    {
        // Render text before the code block
        let before = &aligned_text[pos..block_start];
        if !before.is_empty() {
            for line in wrap_text(before, content_width) {
                output.push_str(&render_markdown_line(&line));
                output.push('\n');
            }
        }

        // Render the highlighted code block (already includes newlines)
        output.push_str(&highlighted);

        pos = block_end;
    }

    // Render remaining text after last code block
    let remaining = &aligned_text[pos..];
    if !remaining.is_empty() {
        for line in wrap_text(remaining, content_width) {
            output.push_str(&render_markdown_line(&line));
            output.push('\n');
        }
    }

    output
}

/// Find the next code block and return its highlighted version.
/// Returns (block_start, block_end, highlighted_text) or None if no more blocks.
fn find_next_code_block(text: &str, start: usize) -> Option<(usize, usize, String)> {
    let remaining = &text[start..];

    // Find opening fence
    let fence_pos = remaining.find("```")?;
    let absolute_fence_start = start + fence_pos;

    // Find the end of the opening fence line
    let after_fence = &remaining[fence_pos + 3..];
    let line_end = after_fence.find('\n').unwrap_or(after_fence.len());
    let language = after_fence[..line_end].trim();
    let language = if language.is_empty() {
        None
    } else {
        Some(language)
    };

    let content_start = fence_pos + 3 + line_end + 1; // +1 for newline
    if start + content_start > text.len() {
        return None;
    }

    // Find closing fence
    let content_remaining = &remaining[content_start..];
    let closing_pos = find_closing_fence(content_remaining)?;
    let content_end = content_start + closing_pos;

    // Calculate block end (after the closing ``` line)
    let after_closing_fence = content_end + 3; // skip past ```
    let block_end = start
        + if after_closing_fence < remaining.len()
            && remaining.as_bytes()[after_closing_fence] == b'\n'
        {
            after_closing_fence + 1
        } else {
            after_closing_fence
        };

    // Extract code content and highlight it
    let code_content = &remaining[content_start..content_end];
    let highlighted = highlight_code_block(code_content, language);

    // Build the output with fence markers
    let mut result = format!("```{}\n", language.unwrap_or(""));
    result.push_str(&highlighted);
    // Ensure there's a newline before closing fence
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str("```\n");

    Some((absolute_fence_start, block_end, result))
}

/// Find the closing ``` fence
fn find_closing_fence(text: &str) -> Option<usize> {
    let mut pos = 0;
    for line in text.lines() {
        if line.trim().starts_with("```") {
            return Some(pos);
        }
        pos += line.len() + 1; // +1 for newline
    }
    None
}

/// Highlight a code block and return the highlighted string with ANSI colors.
fn highlight_code_block(code: &str, language: Option<&str>) -> String {
    let spans = syntax::highlight_code(code, language);

    if spans.is_empty() {
        return code.to_string();
    }

    let mut result = String::new();
    let mut last_end = 0;

    for span in spans {
        // Add any gap (shouldn't happen normally, but be safe)
        if span.start > last_end {
            result.push_str(&code[last_end..span.start]);
        }

        // Add the colored span using truecolor
        let text = &code[span.start..span.end];
        result.push_str(
            &text
                .truecolor(span.color.r, span.color.g, span.color.b)
                .to_string(),
        );

        last_end = span.end;
    }

    // Add any remaining text
    if last_end < code.len() {
        result.push_str(&code[last_end..]);
    }

    result
}

/// Render thinking text - dimmed and italic
fn render_thinking(text: &str, _is_streaming: bool, width: usize) -> String {
    let wrapped = wrap_text(text, width);
    let mut output = String::new();

    for line in &wrapped {
        output.push_str(&format!("{}\n", line.bright_black().italic()));
    }

    output
}

/// Render tool use - "{TOOL_USE_PREFIX}{description}".
fn render_tool_use(description: &str) -> String {
    format!("{}{}", super::TOOL_USE_PREFIX, description)
}

/// Format the optional tool summary for display after a ✓/✗.
///
/// Returns a leading-space suffix like ` [Read 3 lines]` or an empty string.
pub(super) fn format_summary_suffix(summary: Option<&str>) -> String {
    let Some(text) = summary else {
        return String::new();
    };

    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }

    let bracketed = if text.starts_with('[') && text.ends_with(']') {
        text.to_string()
    } else {
        format!("[{}]", text)
    };

    format!(" {}", bracketed.bright_black())
}

/// Render tool result - checkmark or X
fn render_tool_result(is_error: bool, _output: &str, summary: Option<&str>) -> String {
    let summary_suffix = format_summary_suffix(summary);
    if is_error {
        format!(" {}{}\n", "✗".red(), summary_suffix)
    } else {
        format!(" {}{}\n", "✓".green(), summary_suffix)
    }
}

/// Render tool result with context about whether it follows a ToolUse inline.
fn render_tool_result_with_context(
    is_error: bool,
    _output: &str,
    summary: Option<&str>,
    inline: bool,
) -> String {
    let summary_suffix = format_summary_suffix(summary);
    let symbol = if is_error {
        "✗".red().to_string()
    } else {
        "✓".green().to_string()
    };
    if inline {
        format!(" {}{}\n", symbol, summary_suffix)
    } else {
        format!("{}{}\n", symbol, summary_suffix)
    }
}

/// Render image preview using Kitty placeholders.
fn render_image_preview(data: &[u8], mime_type: &str) -> String {
    let Some(preview) = image_preview::get_image_preview(data, mime_type) else {
        return String::new();
    };

    if preview.placeholder_lines.is_empty() {
        return String::new();
    }

    let mut output = String::new();
    output.push_str(&preview.escape_sequence);
    for line in preview.placeholder_lines {
        output.push_str(&line);
        output.push('\n');
    }

    output
}

/// Render error message - red styled
fn render_error(msg: &str) -> String {
    format!("{}\n", msg.red())
}

/// Render warning message - yellow styled
fn render_warning(msg: &str) -> String {
    format!("{}\n", msg.yellow())
}

/// Render info message
fn render_info(msg: &str) -> String {
    format!("{}\n", msg.cyan())
}

/// Apply styling to tool output without adding extra visible characters.
///
/// We use a background color to visually distinguish tool output from the
/// assistant's normal conversation. This avoids adding prefix characters that
/// would get copied when selecting output from the terminal.
pub(crate) fn style_tool_output_line(line: &str) -> String {
    // Subtle dark-gray background using xterm 256-color palette.
    //
    // We use `EL` (erase to end-of-line) so the background fills the full
    // terminal width *without* padding with spaces, keeping copy/paste clean.
    //
    // Important: some command output includes leading `\t` characters (e.g. `git status`).
    // A terminal tab advances the cursor without painting cells, so if we only rely on
    // `EL` at the end, the left side can remain un-highlighted. To avoid that, we first
    // clear the *entire* line (`2K`) while the background is active.
    //
    // We also re-apply the background after any reset sequences emitted by the command
    // output.
    //
    // `100m` (bright-black background in 16-color ANSI) is often too harsh.
    const BG: &str = "\x1b[48;5;233m";
    const RESET: &str = "\x1b[0m";

    if line.is_empty() {
        return format!("{}\x1b[2K{}", BG, RESET);
    }

    // Fast path: if line has no special characters, skip expensive processing
    if !line.contains('\r') && !line.contains("\x1b[0m") && !line.contains("\x1b[m") {
        return format!("{}\x1b[2K{}{}\x1b[K{}", BG, line, BG, RESET);
    }

    // Slow path: process special characters in a single pass
    // Pre-allocate with reasonable capacity
    let mut result = String::with_capacity(line.len() + 64);
    result.push_str(BG);
    result.push_str("\x1b[2K");

    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Skip carriage returns
        if bytes[i] == b'\r' {
            i += 1;
            continue;
        }

        // Check for escape sequences that need background restoration
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Check for \x1b[0m (4 bytes) or \x1b[m (3 bytes)
            if i + 3 < bytes.len() && bytes[i + 2] == b'0' && bytes[i + 3] == b'm' {
                // \x1b[0m - emit it and restore background
                result.push_str("\x1b[0m\x1b[48;5;233m");
                i += 4;
                continue;
            }
            if i + 2 < bytes.len() && bytes[i + 2] == b'm' {
                // \x1b[m - emit it and restore background
                result.push_str("\x1b[m\x1b[48;5;233m");
                i += 3;
                continue;
            }
        }

        // Regular character - find next special character for batch copy
        let start = i;
        while i < bytes.len()
            && bytes[i] != b'\r'
            && !(bytes[i] == 0x1b
                && i + 2 < bytes.len()
                && bytes[i + 1] == b'['
                && (bytes[i + 2] == b'm'
                    || (i + 3 < bytes.len() && bytes[i + 2] == b'0' && bytes[i + 3] == b'm')))
        {
            i += 1;
        }
        if i > start {
            // SAFETY: We only split at ASCII byte boundaries (\r, ESC, [, 0, m are
            // all single-byte ASCII). ASCII bytes are always valid UTF-8 boundaries.
            result.push_str(unsafe { std::str::from_utf8_unchecked(&bytes[start..i]) });
        }
    }

    result.push_str(BG);
    result.push_str("\x1b[K");
    result.push_str(RESET);
    result
}

/// Format the scroll indicator shown when output is truncated.
/// `hidden` - lines hidden above the viewport (within the buffer)
/// `visible` - lines currently shown in viewport
/// `total_seen` - total lines ever seen (including truncated from memory)
pub(crate) fn format_scrolled_indicator(
    hidden: usize,
    visible: usize,
    total_seen: Option<usize>,
) -> String {
    let buffer_total = hidden + visible;
    if let Some(total) = total_seen {
        if total > buffer_total {
            // Lines have been truncated from memory
            format!(
                "(Showing last {} of {} lines, {} truncated)",
                visible,
                buffer_total,
                total - buffer_total
            )
            .bright_black()
            .to_string()
        } else {
            format!("(Showing last {} of {} lines)", visible, total)
                .bright_black()
                .to_string()
        }
    } else {
        format!("(Showing last {} of {} lines)", visible, buffer_total)
            .bright_black()
            .to_string()
    }
}

fn format_viewport_tail_indicator(hidden: usize, visible: usize, older_truncated: bool) -> String {
    let buffer_total = hidden + visible;
    let mut msg = if hidden > 0 {
        format!("(Showing last {} of {} lines)", visible, buffer_total)
    } else {
        format!("(Showing {} lines)", visible)
    };

    if older_truncated {
        msg.push_str(" (older output truncated)");
    }

    msg.bright_black().to_string()
}

/// Render tool output (tail or full, depending on Ctrl+O).
fn render_tool_output(text: &str, total_lines: usize, width: usize) -> String {
    // Skip rendering if tool output is hidden
    if super::listener::is_tool_output_hidden() {
        return String::new();
    }

    // Skip rendering in history if tool output is actively streaming *in viewport mode*.
    // The live viewport will render it instead, avoiding duplication.
    if super::listener::is_tool_output_viewport_active() {
        return String::new();
    }

    let wrapped = wrap_text(text, width);
    if wrapped.is_empty() {
        return String::new();
    }

    let stored_lines = text.bytes().filter(|&b| b == b'\n').count();
    let older_truncated = total_lines > stored_lines;
    let expanded = super::listener::is_tool_output_expanded();

    if expanded {
        let mut output = String::new();

        if older_truncated {
            let msg = format!(
                "(Older output truncated: {} lines dropped; showing last {} lines)",
                total_lines - stored_lines,
                stored_lines
            )
            .bright_black()
            .to_string();
            output.push_str(&style_tool_output_line(&msg));
            output.push('\n');
        }

        for line in &wrapped {
            output.push_str(&style_tool_output_line(line));
            output.push('\n');
        }

        return output;
    }

    let max_lines = super::listener::tool_output_viewport_lines();
    let start = wrapped.len().saturating_sub(max_lines);
    let visible = wrapped.len() - start;
    let mut output = String::new();

    for line in &wrapped[start..] {
        output.push_str(&style_tool_output_line(line));
        output.push('\n');
    }

    if start > 0 || older_truncated {
        output.push_str(&style_tool_output_line(&format_viewport_tail_indicator(
            start,
            visible,
            older_truncated,
        )));
        output.push('\n');
    }

    output
}

/// Render file read output with syntax highlighting based on file extension.
fn render_file_read_output(filename: &str, text: &str, total_lines: usize, width: usize) -> String {
    // Skip rendering if tool output is hidden
    if super::listener::is_tool_output_hidden() {
        return String::new();
    }

    // Skip rendering in history if tool output is actively streaming *in viewport mode*.
    // The live viewport will render it instead, avoiding duplication.
    if super::listener::is_tool_output_viewport_active() {
        return String::new();
    }

    let wrapped = wrap_text(text, width);
    if wrapped.is_empty() {
        return String::new();
    }

    let language = syntax::language_from_path(filename);
    let stored_lines = text.bytes().filter(|&b| b == b'\n').count();
    let older_truncated = total_lines > stored_lines;
    let expanded = super::listener::is_tool_output_expanded();

    if expanded {
        let mut output = String::new();

        if older_truncated {
            let msg = format!(
                "(Older output truncated: {} lines dropped; showing last {} lines)",
                total_lines - stored_lines,
                stored_lines
            )
            .bright_black()
            .to_string();
            output.push_str(&style_tool_output_line(&msg));
            output.push('\n');
        }

        for line in &wrapped {
            let styled = style_file_read_line(line, language.as_deref());
            output.push_str(&styled);
            output.push('\n');
        }

        return output;
    }

    let max_lines = super::listener::tool_output_viewport_lines();
    let start = wrapped.len().saturating_sub(max_lines);
    let visible = wrapped.len() - start;
    let mut output = String::new();

    for line in &wrapped[start..] {
        let styled = style_file_read_line(line, language.as_deref());
        output.push_str(&styled);
        output.push('\n');
    }

    if start > 0 || older_truncated {
        output.push_str(&style_tool_output_line(&format_viewport_tail_indicator(
            start,
            visible,
            older_truncated,
        )));
        output.push('\n');
    }

    output
}

/// Style a file read output line with syntax highlighting and background.
///
/// The line format is expected to be `{line_num:6}\t{content}` - only the content
/// portion after the tab is syntax highlighted.
pub(crate) fn style_file_read_line(line: &str, language: Option<&str>) -> String {
    const BG: &str = "\x1b[48;5;233m";
    const RESET: &str = "\x1b[0m";

    if line.is_empty() {
        return format!("{}\x1b[2K{}", BG, RESET);
    }

    // Strip carriage returns (CR) so cursor rewinds don't interact with our
    // line-clearing sequences (e.g. `\x1b[K`) and erase the rendered content.
    let line = line.replace('\r', "");

    // Split at the first tab to separate line number prefix from content.
    // Keep both as owned Strings so we can safely pass `&str` slices around.
    let (prefix, content) = if let Some(tab_pos) = line.find('\t') {
        (
            line[..=tab_pos].to_string(),
            line[tab_pos + 1..].to_string(),
        )
    } else {
        // No tab found - treat entire line as content
        (String::new(), line)
    };

    // Apply syntax highlighting only to the content portion
    let highlighted_content = if let Some(lang) = language {
        highlight_line_content(&content, lang)
    } else {
        content
    };

    // Re-apply background after any reset sequences from syntax highlighting
    let highlighted_content = highlighted_content.replace("\x1b[0m", "\x1b[0m\x1b[48;5;233m");
    let highlighted_content = highlighted_content.replace("\x1b[m", "\x1b[m\x1b[48;5;233m");

    format!(
        "{}\x1b[2K{}{}{}\x1b[K{}",
        BG, prefix, highlighted_content, BG, RESET
    )
}

/// Highlight a single line of code content (used for file read output).
pub(crate) fn highlight_line_content(line: &str, language: &str) -> String {
    // Strip carriage returns (CR) so cursor rewinds don't interact with our
    // line-clearing sequences (e.g. `\x1b[K`) and erase the rendered content.
    let line = line.replace('\r', "");

    let spans = syntax::highlight_code(&line, Some(language));

    if spans.is_empty() {
        return line;
    }

    let mut result = String::new();
    let mut last_end = 0;

    for span in spans {
        // Add any gap
        if span.start > last_end {
            result.push_str(&line[last_end..span.start]);
        }

        // Add the colored span using truecolor
        let text = &line[span.start..span.end];
        result.push_str(
            &text
                .truecolor(span.color.r, span.color.g, span.color.b)
                .to_string(),
        );

        last_end = span.end;
    }

    // Add any remaining text
    if last_end < line.len() {
        result.push_str(&line[last_end..]);
    }

    result
}

/// Render file diff with colored +/- lines and syntax highlighting
fn render_file_diff(diff: &str, language: Option<&str>, summary: Option<&str>) -> String {
    let summary_suffix = format_summary_suffix(summary);
    let checkmark = format!("{}{}\n", "✓".green(), summary_suffix);

    // Always show the checkmark, but skip the diff content if tool output is hidden
    if super::listener::is_tool_output_hidden() {
        return checkmark;
    }

    let mut output = String::new();
    let expanded = super::listener::is_tool_output_expanded();

    if expanded {
        let mut old_line_num = 0usize;
        let mut new_line_num = 0usize;

        for line in diff.lines() {
            // Skip --- and +++ header lines
            if line.starts_with("+++") || line.starts_with("---") {
                continue;
            }

            // Parse hunk headers to update line numbers, but don't render them
            if line.starts_with("@@") {
                if let Some((old_start, new_start)) = parse_hunk_header(line) {
                    old_line_num = old_start;
                    new_line_num = new_start;
                }
                continue;
            }

            output.push_str(&render_diff_line_with_gutter(
                line,
                language,
                &mut old_line_num,
                &mut new_line_num,
            ));
            output.push('\n');
        }
    } else {
        use std::collections::VecDeque;

        let max_lines = super::listener::tool_output_viewport_lines();
        let mut old_line_num = 0usize;
        let mut new_line_num = 0usize;
        let mut tail: VecDeque<String> = VecDeque::with_capacity(max_lines);
        let mut total_lines = 0usize;

        for line in diff.lines() {
            // Skip --- and +++ header lines
            if line.starts_with("+++") || line.starts_with("---") {
                continue;
            }

            // Parse hunk headers to update line numbers, but don't render them
            if line.starts_with("@@") {
                if let Some((old_start, new_start)) = parse_hunk_header(line) {
                    old_line_num = old_start;
                    new_line_num = new_start;
                }
                continue;
            }

            total_lines += 1;
            if tail.len() == max_lines {
                tail.pop_front();
            }
            tail.push_back(render_diff_line_with_gutter(
                line,
                language,
                &mut old_line_num,
                &mut new_line_num,
            ));
        }

        for line in tail {
            output.push_str(&line);
            output.push('\n');
        }

        let visible_count = total_lines.min(max_lines);
        let hidden = total_lines.saturating_sub(visible_count);
        if hidden > 0 {
            output.push_str(&style_tool_output_line(&format_scrolled_indicator(
                hidden,
                visible_count,
                Some(total_lines),
            )));
            output.push('\n');
        }
    }

    output.push_str(&checkmark);
    output
}

/// Parse hunk header like "@@ -1,3 +1,5 @@" to extract starting line numbers
fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let line = line.trim();
    if !line.starts_with("@@") {
        return None;
    }
    let line = line.strip_prefix("@@")?.trim_start();
    let line = line.split(" @@").next()?;

    let mut parts = line.split_whitespace();

    let old_part = parts.next()?.strip_prefix('-')?;
    let old_start: usize = old_part.split(',').next()?.parse().ok()?;

    let new_part = parts.next()?.strip_prefix('+')?;
    let new_start: usize = new_part.split(',').next()?.parse().ok()?;

    Some((old_start, new_start))
}

/// Render a single diff line with gutter (line number + prefix) and syntax highlighting.
fn render_diff_line_with_gutter(
    line: &str,
    language: Option<&str>,
    old_line_num: &mut usize,
    new_line_num: &mut usize,
) -> String {
    // Determine line type and update line numbers
    let (line_num, prefix, prefix_color, bg_color, code_content): (
        Option<usize>,
        &str,
        Option<Color>,
        Option<Color>,
        &str,
    ) = if let Some(stripped) = line.strip_prefix('+') {
        let num = *new_line_num;
        *new_line_num += 1;
        (
            Some(num),
            "+",
            Some(Color::Green),
            Some(BG_DARK_GREEN),
            stripped,
        )
    } else if let Some(stripped) = line.strip_prefix('-') {
        let num = *old_line_num;
        *old_line_num += 1;
        (
            Some(num),
            "-",
            Some(Color::Red),
            Some(BG_DARK_RED),
            stripped,
        )
    } else if let Some(stripped) = line.strip_prefix(' ') {
        let num = *new_line_num;
        *old_line_num += 1;
        *new_line_num += 1;
        (Some(num), " ", None, None, stripped)
    } else {
        // Unknown line format
        return line.to_string();
    };

    // Build the gutter: "  3 + " (3-digit right-aligned number + space + prefix + space)
    let line_num_str = line_num
        .map(|n| format!("{:>3}", n))
        .unwrap_or_else(|| "   ".to_string());

    let mut result = String::new();

    // Helper to apply optional background color to text
    fn with_bg(text: &str, bg: Option<Color>) -> String {
        match bg {
            Some(color) => text.on_color(color).to_string(),
            None => text.to_string(),
        }
    }

    // Line number in dim gray with optional background
    let line_num_display = format!("{} ", line_num_str);
    let styled_num = line_num_display.bright_black();
    result.push_str(&match bg_color {
        Some(bg) => styled_num.on_color(bg).to_string(),
        None => styled_num.to_string(),
    });

    // Prefix with diff color
    if let Some(color) = prefix_color {
        let styled_prefix = prefix.color(color);
        result.push_str(&match bg_color {
            Some(bg) => styled_prefix.on_color(bg).to_string(),
            None => styled_prefix.to_string(),
        });
    } else {
        result.push_str(&with_bg(prefix, bg_color));
    }

    // Space after prefix
    result.push_str(&with_bg(" ", bg_color));

    // Code content with syntax highlighting
    if let Some(lang) = language {
        let spans = syntax::highlight_code(code_content, Some(lang));
        let mut last_end = 0;

        for span in &spans {
            // Add any gap between spans
            if span.start > last_end {
                let gap_text = &code_content[last_end..span.start];
                result.push_str(&with_bg(gap_text, bg_color));
            }
            // Add the highlighted span with RGB color
            let syntax::Rgb { r, g, b } = span.color;
            let span_text = &code_content[span.start..span.end];
            let colored_span = span_text.truecolor(r, g, b);
            result.push_str(&match bg_color {
                Some(bg) => colored_span.on_color(bg).to_string(),
                None => colored_span.to_string(),
            });
            last_end = span.end;
        }

        // Add any remaining text
        if last_end < code_content.len() {
            let remaining = &code_content[last_end..];
            result.push_str(&with_bg(remaining, bg_color));
        }
    } else {
        // No language - just output the code content with optional background
        result.push_str(&with_bg(code_content, bg_color));
    }

    result
}

/// Render auto-compact notification
fn render_auto_compact(message: &str) -> String {
    format!("{}\n", message.yellow())
}

// ============================================================================
// Text wrapping utilities
// ============================================================================

/// Get display width of a character (accounting for wide chars)
fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Get display width of a string
pub(crate) fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Fast extraction of tail lines by scanning backwards from end.
///
/// Given a pre-computed line count, this finds the last `max_lines` by scanning
/// backwards to locate newline positions. This is O(output_size) for the visible
/// portion only, not O(total_buffer_size).
///
/// If `wrap_width` is provided, lines longer than that are hard-wrapped (no word
/// boundary logic, just character-based splitting). Lines containing ANSI escape
/// sequences are not wrapped to avoid breaking the sequences.
///
/// Returns `(visible_lines, hidden_count)` where `hidden_count` is the number of
/// wrapped lines not shown (for the scroll indicator).
pub(crate) fn tail_lines_fast(
    text: &str,
    total_line_count: usize,
    max_lines: usize,
    wrap_width: Option<usize>,
) -> (Vec<&str>, usize) {
    if text.is_empty() || max_lines == 0 {
        return (vec![], 0);
    }

    // If we have fewer lines than max, just return all (possibly wrapped)
    if total_line_count <= max_lines {
        let lines: Vec<&str> = text.lines().collect();
        return if let Some(width) = wrap_width {
            hard_wrap_lines(&lines, width, max_lines)
        } else {
            (lines, 0)
        };
    }

    // Scan backwards to find the byte offset where the visible portion starts.
    // We need enough raw lines that after wrapping we have at least max_lines.
    // Fetch extra to account for potential wrapping.
    let fetch_lines = if wrap_width.is_some() {
        max_lines * 2
    } else {
        max_lines
    };

    let bytes = text.as_bytes();

    // Find newlines from the end
    let mut newline_positions = Vec::with_capacity(fetch_lines + 1);
    for (i, &b) in bytes.iter().enumerate().rev() {
        if b == b'\n' {
            newline_positions.push(i);
            if newline_positions.len() > fetch_lines {
                break;
            }
        }
    }

    // Determine start offset
    let start_offset = if newline_positions.len() > fetch_lines {
        newline_positions[fetch_lines] + 1
    } else {
        0
    };

    // Extract the tail portion and split into lines
    let tail = &text[start_offset..];
    let raw_lines: Vec<&str> = tail.lines().collect();

    // Count lines we're not even fetching (before start_offset)
    let unfetched_lines = total_line_count.saturating_sub(raw_lines.len());

    // Apply hard wrapping if requested
    if let Some(width) = wrap_width {
        let (visible, wrapped_hidden) = hard_wrap_lines(&raw_lines, width, max_lines);
        (visible, unfetched_lines + wrapped_hidden)
    } else if raw_lines.len() > max_lines {
        let hidden = raw_lines.len() - max_lines + unfetched_lines;
        (raw_lines[raw_lines.len() - max_lines..].to_vec(), hidden)
    } else {
        (raw_lines, unfetched_lines)
    }
}

/// Hard-wrap lines at a fixed width (character-based, no word boundary logic).
/// Returns `(visible_lines, hidden_count)` where at most `max_lines` are visible.
/// Lines containing ANSI escape sequences are not wrapped.
fn hard_wrap_lines<'a>(lines: &[&'a str], width: usize, max_lines: usize) -> (Vec<&'a str>, usize) {
    if width == 0 {
        let visible: Vec<_> = lines.iter().take(max_lines).copied().collect();
        let hidden = lines.len().saturating_sub(max_lines);
        return (visible, hidden);
    }

    // First pass: count total wrapped lines to know how many to skip
    let mut total_wrapped = 0usize;
    for line in lines {
        total_wrapped += count_wrapped_chunks(line, width);
    }

    if total_wrapped <= max_lines {
        // No truncation needed, but still need to wrap
        let mut result = Vec::with_capacity(total_wrapped);
        for line in lines {
            if line.is_empty() {
                result.push(*line);
            } else {
                result.extend(wrap_line_chunks(line, width));
            }
        }
        return (result, 0);
    }

    // Need to skip some wrapped lines from the beginning
    let skip = total_wrapped - max_lines;
    let mut skipped = 0usize;
    let mut result = Vec::with_capacity(max_lines);

    for line in lines {
        let chunks = if line.is_empty() {
            vec![*line]
        } else {
            wrap_line_chunks(line, width)
        };
        for chunk in chunks {
            if skipped < skip {
                skipped += 1;
            } else {
                result.push(chunk);
                if result.len() >= max_lines {
                    return (result, skip);
                }
            }
        }
    }

    (result, skip)
}

/// Count how many visual lines a string produces when wrapped.
fn count_wrapped_chunks(line: &str, width: usize) -> usize {
    if line.is_empty() {
        return 1;
    }
    // Don't wrap lines with ANSI escapes (would break sequences)
    if line.contains('\x1b') {
        return 1;
    }
    let line_width = display_width(line);
    line_width.div_ceil(width).max(1)
}

/// Split a single line into width-sized chunks.
/// Lines containing ANSI escape sequences are returned as-is.
fn wrap_line_chunks(line: &str, width: usize) -> Vec<&str> {
    if line.is_empty() || width == 0 {
        return vec![line];
    }

    // Don't wrap lines with ANSI escapes - would break the sequences
    if line.contains('\x1b') {
        return vec![line];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    let mut current_width = 0;

    for (i, c) in line.char_indices() {
        let char_width = UnicodeWidthChar::width(c).unwrap_or(0);

        if current_width + char_width > width && current_width > 0 {
            chunks.push(&line[start..i]);
            start = i;
            current_width = char_width;
        } else {
            current_width += char_width;
        }
    }

    if start < line.len() {
        chunks.push(&line[start..]);
    }

    if chunks.is_empty() {
        vec![line]
    } else {
        chunks
    }
}

/// Wrap text to fit within a given width, preserving whitespace and newlines.
pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0;

    let mut word = String::new();
    let mut word_width = 0;

    let push_long_word = |lines: &mut Vec<String>,
                          current_line: &mut String,
                          current_width: &mut usize,
                          word_text: String| {
        if word_text.is_empty() {
            return;
        }

        let mut chunk = String::new();
        let mut chunk_width = 0;

        for ch in word_text.chars() {
            let ch_width = char_width(ch);
            if chunk_width + ch_width > width && chunk_width > 0 {
                lines.push(std::mem::take(&mut chunk));
                chunk_width = 0;
            }
            chunk.push(ch);
            chunk_width += ch_width;
            if chunk_width == width {
                lines.push(std::mem::take(&mut chunk));
                chunk_width = 0;
            }
        }

        if !chunk.is_empty() {
            current_line.push_str(&chunk);
            *current_width = chunk_width;
        } else {
            *current_width = 0;
        }
    };

    let flush_word = |lines: &mut Vec<String>,
                      current_line: &mut String,
                      current_width: &mut usize,
                      word: &mut String,
                      word_width: &mut usize| {
        if word.is_empty() {
            return;
        }

        let word_text = std::mem::take(word);
        let word_len = *word_width;
        *word_width = 0;

        if word_len > width {
            if *current_width > 0 {
                lines.push(std::mem::take(current_line));
                *current_width = 0;
            }
            push_long_word(lines, current_line, current_width, word_text);
            return;
        }

        if *current_width == 0 {
            current_line.push_str(&word_text);
            *current_width = word_len;
        } else if *current_width + word_len <= width {
            current_line.push_str(&word_text);
            *current_width += word_len;
        } else {
            lines.push(std::mem::take(current_line));
            current_line.push_str(&word_text);
            *current_width = word_len;
        }
    };

    let mut skipping_leading_ws_after_wrap = false;
    for ch in text.chars() {
        match ch {
            '\n' => {
                flush_word(
                    &mut lines,
                    &mut current_line,
                    &mut current_width,
                    &mut word,
                    &mut word_width,
                );
                lines.push(std::mem::take(&mut current_line));
                current_width = 0;
                skipping_leading_ws_after_wrap = false;
            }
            ' ' | '\t' => {
                flush_word(
                    &mut lines,
                    &mut current_line,
                    &mut current_width,
                    &mut word,
                    &mut word_width,
                );

                let ch_width = display_width(&ch.to_string());

                if current_width + ch_width > width {
                    lines.push(std::mem::take(&mut current_line));
                    current_width = 0;
                    skipping_leading_ws_after_wrap = true;
                    continue;
                }

                if skipping_leading_ws_after_wrap && current_width == 0 && current_line.is_empty() {
                    continue;
                }

                current_line.push(ch);
                current_width += ch_width;
                skipping_leading_ws_after_wrap = false;
            }
            _ => {
                skipping_leading_ws_after_wrap = false;
                word.push(ch);
                word_width += char_width(ch);
            }
        }
    }

    flush_word(
        &mut lines,
        &mut current_line,
        &mut current_width,
        &mut word,
        &mut word_width,
    );

    if !current_line.is_empty() || lines.is_empty() {
        lines.push(current_line);
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // Force colors to be enabled for tests (colored crate disables colors when not on a TTY)
    fn enable_colors() {
        colored::control::set_override(true);
    }

    #[test]
    fn test_wrap_text_simple() {
        let result = wrap_text("hello world", 20);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn test_wrap_text_break() {
        let result = wrap_text("hello world", 8);
        assert_eq!(result, vec!["hello ", "world"]);
    }

    #[test]
    fn test_wrap_text_does_not_start_wrapped_line_with_spaces() {
        let result = wrap_text("abcd ef", 4);
        assert_eq!(result, vec!["abcd", "ef"]);

        let result = wrap_text("abcd  ef", 4);
        assert_eq!(result, vec!["abcd", "ef"]);
    }

    #[test]
    fn test_wrap_text_long_sentence() {
        let result = wrap_text("The quick brown fox jumps over the lazy dog", 20);
        assert_eq!(
            result,
            vec!["The quick brown fox ", "jumps over the lazy ", "dog"]
        );
    }

    #[test]
    fn test_wrap_text_preserves_spaces() {
        let result = wrap_text("a  b", 10);
        assert_eq!(result, vec!["a  b"]);
    }

    #[test]
    fn test_wrap_text_long_word() {
        let result = wrap_text("supercalifragilisticexpialidocious", 10);
        assert_eq!(
            result,
            vec!["supercalif", "ragilistic", "expialidoc", "ious"]
        );
    }

    #[test]
    fn test_wrap_text_long_word_with_prefix() {
        let result = wrap_text("prefix supercalifragilisticexpialidocious", 10);
        assert_eq!(
            result,
            vec!["prefix ", "supercalif", "ragilistic", "expialidoc", "ious"]
        );
    }

    #[test]
    fn test_wrap_text_long_word_with_space() {
        let result = wrap_text("supercalifragilisticexpialidocious tail", 10);
        assert_eq!(
            result,
            vec!["supercalif", "ragilistic", "expialidoc", "ious tail"]
        );
    }

    #[test]
    fn test_wrap_text_long_word_preserves_newlines() {
        let result = wrap_text("supercalifragilisticexpialidocious\nnext", 10);
        assert_eq!(
            result,
            vec!["supercalif", "ragilistic", "expialidoc", "ious", "next"]
        );
    }

    #[test]
    fn test_wrap_text_preserves_newlines() {
        let result = wrap_text("a\n\n b", 10);
        assert_eq!(result, vec!["a", "", " b"]);
    }

    #[test]
    fn test_display_width() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("你好"), 4); // Each CJK char is 2 wide
    }

    #[test]
    fn test_render_error() {
        enable_colors();
        let result = render_error("Something went wrong");
        assert!(result.contains("Something went wrong"));
        // Should contain ANSI escape codes
        assert!(result.contains("\x1b["));
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn test_render_tool_result_success() {
        enable_colors();
        let result = render_tool_result(false, "", None);
        assert!(result.contains("✓"));
        // Should contain ANSI escape codes
        assert!(result.contains("\x1b["));
    }

    #[test]
    fn test_render_tool_result_error() {
        enable_colors();
        let result = render_tool_result(true, "File not found", None);
        assert!(result.contains("✗"));
        // Should contain ANSI escape codes
        assert!(result.contains("\x1b["));
        // Error preview is no longer displayed inline
        assert!(!result.contains("File not found"));
    }

    #[test]
    fn test_style_tool_output_line_strips_carriage_returns() {
        enable_colors();
        let styled = style_tool_output_line("hello\r");
        assert!(!styled.contains('\r'));
    }

    #[test]
    fn test_code_block_highlighting() {
        enable_colors();
        let text = r#"Here is some code:
```rust
let x = 42;
```
And more text."#;

        let result = render_assistant_text(text, false, 80);

        // Should contain the code block markers
        assert!(result.contains("```rust"));
        assert!(result.contains("```\n"));

        // Should contain ANSI color codes from syntax highlighting
        assert!(result.contains("\x1b["));

        // Check for highlighted code tokens (may be split by ANSI codes)
        assert!(result.contains("let"));
        assert!(result.contains("42"));

        // Should contain the surrounding text
        assert!(result.contains("Here is some code:"));
        assert!(result.contains("And more text."));
    }

    #[test]
    fn test_multiple_code_blocks() {
        let text = r#"First block:
```python
def hello():
    pass
```
Second block:
```javascript
const x = 42;
```"#;

        enable_colors();
        let result = render_assistant_text(text, false, 80);

        // Both code blocks should be present with fence markers
        assert!(result.contains("```python"));
        assert!(result.contains("```javascript"));

        // Check for highlighted code tokens
        assert!(result.contains("def"));
        assert!(result.contains("hello"));
        assert!(result.contains("const"));

        // Should have syntax highlighting
        assert!(result.contains("\x1b["));
    }

    #[test]
    fn test_no_code_blocks() {
        let text = "Just plain text without any code blocks.";
        let result = render_assistant_text(text, false, 80);

        assert!(result.contains("Just plain text"));
        // No ANSI escape codes for syntax highlighting
        assert!(!result.contains("\x1b[38;2;"));
    }

    #[test]
    fn test_render_assistant_text_strips_carriage_returns() {
        let text = "## Summary\r\n\r\n**Display Elements:**\r\n-   Markdown table with features\r\n-   Rust function with proper formatting\r\n-   Go function with proper formatting\r\n";

        let result = render_assistant_text(text, false, 80);

        assert!(!result.contains('\r'));
        assert!(result.contains("-   Markdown table with features"));
        assert!(result.contains("-   Rust function with proper formatting"));
        assert!(result.contains("-   Go function with proper formatting"));
    }

    #[test]
    fn test_render_all_suppresses_trailing_checkmark() {
        enable_colors();
        let events = vec![
            HistoryEvent::FileDiff {
                diff: "some diff".to_string(),
                language: None,
                summary: None,
            },
            HistoryEvent::ToolResult {
                output: "".to_string(),
                is_error: false,
                summary: None,
            },
        ];

        let result = render_all(&events, 80);

        // Should have one checkmark (from FileDiff)
        assert!(result.contains("✓"));
        // Should NOT have two checkmarks
        assert_eq!(result.matches("✓").count(), 1);
    }

    #[test]
    fn test_render_file_diff_collapsed_shows_tail_indicator() {
        enable_colors();
        let mut diff = String::from("@@ -0,0 +1,8 @@\n");
        for idx in 1..=8 {
            diff.push_str(&format!("+line {}\n", idx));
        }

        let rendered = render_file_diff(&diff, None, None);
        assert!(rendered.contains("Showing last"));
        assert!(rendered.contains("of 8 lines"));
    }

    #[test]
    fn test_tail_lines_fast_small_input() {
        // When input is small, returns all lines with no hidden
        let input = "line1\nline2\nline3\n";
        let (lines, hidden) = tail_lines_fast(input, 3, 10, None);
        assert_eq!(lines, vec!["line1", "line2", "line3"]);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn test_tail_lines_fast_truncates_to_max_lines() {
        // When input exceeds max_lines, returns only the tail
        let input = "a\nb\nc\nd\ne\n";
        let (lines, hidden) = tail_lines_fast(input, 5, 3, None);
        assert_eq!(lines, vec!["c", "d", "e"]);
        assert_eq!(hidden, 2);
    }

    #[test]
    fn test_tail_lines_fast_large_input() {
        // For large input, should return correct tail
        let mut input = String::new();
        for i in 0..100 {
            input.push_str(&format!("line{}\n", i));
        }
        let (lines, hidden) = tail_lines_fast(&input, 100, 5, None);
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[4], "line99");
        assert_eq!(hidden, 95);
    }

    #[test]
    fn test_tail_lines_fast_empty_input() {
        let (lines, hidden) = tail_lines_fast("", 0, 10, None);
        assert!(lines.is_empty());
        assert_eq!(hidden, 0);
    }

    #[test]
    fn test_tail_lines_fast_with_wrapping() {
        // Long line should be wrapped, hidden count reflects wrapped lines
        let input = "short\nthis_is_a_very_long_line_that_exceeds_width\n";
        let (lines, hidden) = tail_lines_fast(input, 2, 10, Some(20));
        // First line "short" fits, second line wraps to 3 chunks (45 chars / 20 = 3)
        assert!(lines.len() >= 2);
        assert_eq!(lines[0], "short");
        assert_eq!(hidden, 0); // All lines fit in viewport
    }

    #[test]
    fn test_tail_lines_fast_wrapping_hidden_count() {
        // Test that hidden count is correct when wrapping causes overflow
        let input = "a\nb\nc\n"; // 3 short lines
        let (lines, hidden) = tail_lines_fast(input, 3, 2, None);
        assert_eq!(lines.len(), 2);
        assert_eq!(hidden, 1);
    }

    #[test]
    fn test_tail_lines_fast_ansi_not_wrapped() {
        // Lines with ANSI escapes should not be wrapped (would break sequences)
        let input = "normal\n\x1b[31mthis_is_colored_and_very_long_line\x1b[0m\n";
        let (lines, _) = tail_lines_fast(input, 2, 10, Some(10));
        // The colored line should remain intact, not split
        assert!(lines.iter().any(|l| l.contains("\x1b[31m")));
    }

    #[test]
    fn test_style_tool_output_line_fast_path() {
        // Lines without special characters should use fast path
        let styled = style_tool_output_line("simple line");
        assert!(styled.contains("simple line"));
        assert!(styled.contains("\x1b[48;5;233m")); // Has background color
    }

    #[test]
    fn test_style_tool_output_line_handles_reset_sequences() {
        // Lines with reset sequences should restore background
        let styled = style_tool_output_line("before\x1b[0mafter");
        assert!(styled.contains("before"));
        assert!(styled.contains("after"));
        // Should restore background after reset
        assert!(styled.contains("\x1b[0m\x1b[48;5;233m"));
    }
}
