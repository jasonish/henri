// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Rendering functions for CLI history events.
//!
//! Converts semantic HistoryEvents into styled, wrapped text lines
//! suitable for terminal display.

use colored::{Color, ColoredString, Colorize};
use unicode_width::UnicodeWidthChar;

use super::history::{HistoryEvent, ImageMeta, TodoStatus};
use super::markdown::{align_markdown_tables, render_markdown_line};
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
        HistoryEvent::ToolOutput { text } => render_tool_output(text, width),
        HistoryEvent::FileReadOutput { filename, text } => {
            render_file_read_output(filename, text, width)
        }
        HistoryEvent::Error(msg) => render_error(msg),
        HistoryEvent::Warning(msg) => render_warning(msg),
        HistoryEvent::Info(msg) => render_info(msg),
        HistoryEvent::FileDiff { diff, language } => render_file_diff(diff, language.as_deref()),
        HistoryEvent::TodoList { items } => render_todo_list(items),
        HistoryEvent::AutoCompact { message } => render_auto_compact(message),
    }
}

/// Determine if a blank line should be inserted before this event based on the previous event.
///
/// This mirrors the spacing logic in `listener.rs` for streaming output.
fn needs_blank_line_before(prev: Option<&HistoryEvent>, current: &HistoryEvent) -> bool {
    let Some(prev) = prev else {
        return false;
    };

    match (prev, current) {
        // Any -> Info: blank line (live mode prints a blank line before info)
        (_, HistoryEvent::Info(_)) => true,
        // UserPrompt -> Thinking: blank line
        (HistoryEvent::UserPrompt { .. }, HistoryEvent::Thinking { .. }) => true,
        // UserPrompt -> Text: blank line (prompt ends with newline in render)
        (HistoryEvent::UserPrompt { .. }, HistoryEvent::AssistantText { .. }) => true,
        // Thinking -> Text: blank line
        (HistoryEvent::Thinking { .. }, HistoryEvent::AssistantText { .. }) => true,
        (HistoryEvent::ThinkingEnd, HistoryEvent::AssistantText { .. }) => true,
        // Tool -> Text: blank line
        (HistoryEvent::ToolResult { .. }, HistoryEvent::AssistantText { .. }) => true,
        (HistoryEvent::ToolOutput { .. }, HistoryEvent::AssistantText { .. }) => true,
        (HistoryEvent::FileReadOutput { .. }, HistoryEvent::AssistantText { .. }) => true,
        (HistoryEvent::ToolEnd, HistoryEvent::AssistantText { .. }) => true,
        // Tool -> Thinking: blank line
        (HistoryEvent::ToolResult { .. }, HistoryEvent::Thinking { .. }) => true,
        (HistoryEvent::ToolOutput { .. }, HistoryEvent::Thinking { .. }) => true,
        (HistoryEvent::FileReadOutput { .. }, HistoryEvent::Thinking { .. }) => true,
        (HistoryEvent::ToolEnd, HistoryEvent::Thinking { .. }) => true,
        // Thinking/Text -> ToolStart: blank line
        (HistoryEvent::Thinking { .. }, HistoryEvent::ToolStart) => true,
        (HistoryEvent::ThinkingEnd, HistoryEvent::ToolStart) => true,
        (HistoryEvent::AssistantText { .. }, HistoryEvent::ToolStart) => true,
        (HistoryEvent::ResponseEnd, HistoryEvent::ToolStart) => true,
        // Info -> Tool: blank line
        (HistoryEvent::Info(_), HistoryEvent::ToolStart) => true,
        (HistoryEvent::Info(_), HistoryEvent::ToolUse { .. }) => true,
        // ResponseEnd -> Text/Thinking: blank line (for session replay across message boundaries)
        (HistoryEvent::ResponseEnd, HistoryEvent::AssistantText { .. }) => true,
        (HistoryEvent::ResponseEnd, HistoryEvent::Thinking { .. }) => true,
        // ResponseEnd -> UserPrompt: blank line (new turn from user)
        (HistoryEvent::ResponseEnd, HistoryEvent::UserPrompt { .. }) => true,
        _ => false,
    }
}

/// Render all history events.
pub(crate) fn render_all(events: &[HistoryEvent], width: usize) -> String {
    let mut output = String::new();

    for (idx, event) in events.iter().enumerate() {
        let prev = if idx > 0 {
            Some(&events[idx - 1])
        } else {
            None
        };

        // Insert blank line between blocks where appropriate
        if needs_blank_line_before(prev, event) {
            output.push('\n');
        }

        let next = events.get(idx + 1);

        let rendered = match event {
            HistoryEvent::ToolUse { description } => {
                let mut s = render_tool_use(description);
                // If next event is not a ToolResult, add newline to terminate the line.
                // This handles batched tool calls where results come later.
                if !matches!(next, Some(HistoryEvent::ToolResult { .. })) {
                    s.push('\n');
                }
                s
            }
            HistoryEvent::ToolResult {
                is_error,
                output,
                summary,
            } => {
                // If the previous event was a FileDiff, it already displayed a checkmark,
                // so we don't need another one for success.
                if !*is_error && matches!(prev, Some(HistoryEvent::FileDiff { .. })) {
                    String::new()
                } else {
                    // Only add leading space if previous was ToolUse (inline display).
                    let inline = matches!(prev, Some(HistoryEvent::ToolUse { .. }));
                    render_tool_result_with_context(*is_error, output, summary.as_deref(), inline)
                }
            }
            _ => render_event(event, width),
        };

        output.push_str(&rendered);
    }

    // Avoid leaving the output cursor on an empty trailing line above the prompt/status line.
    while output.ends_with(['\n', '\r']) {
        output.pop();
    }

    output
}

/// Render user prompt - with arrow prefix and grey background spanning full width
///
/// The text may contain inline image markers like `Image#1` which are colorized.
fn render_user_prompt(text: &str, _images: &[ImageMeta], width: usize) -> String {
    let arrow = if text.starts_with('!') {
        super::input::SHELL_PROMPT
    } else {
        super::input::PROMPT
    };
    let text = text.strip_prefix('!').unwrap_or(text);
    let continuation = super::input::CONTINUATION;

    // Use a slightly reduced width to avoid terminals auto-wrapping when the last column
    // is filled. We still paint the rest of the line using `\x1b[K`.
    let safe_width = width.saturating_sub(1).max(1);
    let content_width = safe_width.saturating_sub(2); // arrow/continuation is 2 chars

    let mut output = String::new();

    // Padding row above the prompt block. Include a single space so the output cursor is mid-line
    // (mirrors the interactive prompt rendering path).
    output.push_str(BG_GREY_ANSI);
    output.push_str(" \x1b[K\x1b[0m\n");

    for (i, line) in text.lines().enumerate() {
        let prefix = if i == 0 { arrow } else { continuation };
        let wrapped = wrap_text(line, content_width);
        for (j, wrapped_line) in wrapped.iter().enumerate() {
            let p = if j == 0 { prefix } else { continuation };

            // Colorize image markers in the line.
            let styled_line = colorize_image_markers(wrapped_line);

            // Full-width grey background without printing trailing spaces.
            //
            // Important: `colorize_image_markers()` may emit ANSI reset codes; re-assert
            // the prompt background before `\x1b[K` so the erase uses the grey background.
            output.push_str(BG_GREY_ANSI);
            output.push_str(p);
            output.push_str(&styled_line);
            output.push_str(BG_GREY_ANSI);
            output.push_str("\x1b[K\x1b[0m\n");
        }
    }

    // Padding row below the prompt block.
    output.push_str(BG_GREY_ANSI);
    output.push_str(" \x1b[K\x1b[0m\n");

    output
}

// Background color for image markers - dark cyan/teal
const BG_IMAGE_MARKER: Color = Color::TrueColor { r: 0, g: 40, b: 50 };

use super::input::IMAGE_MARKER_PREFIX;

/// Colorize image markers like `Image#1` with cyan text on dark background.
/// Matches "Image#" followed by one or more digits.
pub(super) fn colorize_image_markers(text: &str) -> String {
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
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// Render assistant text with syntax highlighting for code blocks
fn render_assistant_text(text: &str, _is_streaming: bool, width: usize) -> String {
    // Align markdown tables if they fit within the width
    let aligned_text = align_markdown_tables(text, Some(width));

    let mut output = String::new();
    let mut pos = 0;

    // Find and process code blocks
    while let Some((block_start, block_end, highlighted)) = find_next_code_block(&aligned_text, pos)
    {
        // Render text before the code block
        let before = &aligned_text[pos..block_start];
        if !before.is_empty() {
            for line in wrap_text(before, width) {
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
        for line in wrap_text(remaining, width) {
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

/// Render thinking text - dimmed
fn render_thinking(text: &str, _is_streaming: bool, width: usize) -> String {
    let wrapped = wrap_text(text, width);
    let mut output = String::new();

    for line in &wrapped {
        output.push_str(&format!("{}\n", line.bright_black()));
    }

    output
}

/// Render tool use - "▶ {description}"
fn render_tool_use(description: &str) -> String {
    format!("▶ {}", description)
}

/// Render tool result - checkmark or X
fn render_tool_result(is_error: bool, _output: &str, summary: Option<&str>) -> String {
    let summary_suffix = summary
        .map(|text| format!(" {}", text.bright_black()))
        .unwrap_or_default();
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
    let summary_suffix = summary
        .map(|text| format!(" {}", text.bright_black()))
        .unwrap_or_default();
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

    let line = line.replace("\x1b[0m", "\x1b[0m\x1b[48;5;233m");
    let line = line.replace("\x1b[m", "\x1b[m\x1b[48;5;233m");

    // Paint full width by clearing the entire line with the background active, then
    // render the content. Finally, `EL` ensures any remaining portion after the last
    // printed character uses our background (in case the content changed background
    // mid-line).
    format!("{}\x1b[2K{}{}\x1b[K{}", BG, line, BG, RESET)
}

/// Format the "scrolled lines" indicator shown when output is truncated.
pub(crate) fn format_scrolled_indicator(hidden_lines: usize) -> String {
    format!("(... {} previous lines)", hidden_lines)
        .bright_black()
        .to_string()
}

/// Render tool output tail (up to the configured viewport height).
fn render_tool_output(text: &str, width: usize) -> String {
    let wrapped = wrap_text(text, width);
    if wrapped.is_empty() {
        return String::new();
    }

    let max_lines = crate::cli::TOOL_OUTPUT_VIEWPORT_LINES;
    let start = wrapped.len().saturating_sub(max_lines);
    let mut output = String::new();

    // Show indicator if lines were scrolled out of view
    if start > 0 {
        output.push_str(&style_tool_output_line(&format_scrolled_indicator(start)));
        output.push('\n');
    }

    for line in &wrapped[start..] {
        output.push_str(&style_tool_output_line(line));
        output.push('\n');
    }

    output
}

/// Render file read output with syntax highlighting based on file extension.
fn render_file_read_output(filename: &str, text: &str, width: usize) -> String {
    let wrapped = wrap_text(text, width);
    if wrapped.is_empty() {
        return String::new();
    }

    let max_lines = crate::cli::TOOL_OUTPUT_VIEWPORT_LINES;
    let start = wrapped.len().saturating_sub(max_lines);
    let language = syntax::language_from_path(filename);
    let mut output = String::new();

    // Show indicator if lines were scrolled out of view
    if start > 0 {
        output.push_str(&style_tool_output_line(&format_scrolled_indicator(start)));
        output.push('\n');
    }

    for line in &wrapped[start..] {
        let styled = style_file_read_line(line, language.as_deref());
        output.push_str(&styled);
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

    // Split at the first tab to separate line number prefix from content
    let (prefix, content) = if let Some(tab_pos) = line.find('\t') {
        (&line[..=tab_pos], &line[tab_pos + 1..])
    } else {
        // No tab found - treat entire line as content
        ("", line)
    };

    // Apply syntax highlighting only to the content portion
    let highlighted_content = if let Some(lang) = language {
        highlight_line_content(content, lang)
    } else {
        content.to_string()
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
    let spans = syntax::highlight_code(line, Some(language));

    if spans.is_empty() {
        return line.to_string();
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
fn render_file_diff(diff: &str, language: Option<&str>) -> String {
    let mut output = format!(" {}\n", "✓".green());

    // Track line numbers by parsing hunk headers
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

        let styled =
            render_diff_line_with_gutter(line, language, &mut old_line_num, &mut new_line_num);
        output.push_str(&styled);
        output.push('\n');
    }

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

/// Render todo list
fn render_todo_list(items: &[super::history::TodoItem]) -> String {
    let mut output = format!("{}\n", "Todo:".cyan().bold());

    if items.is_empty() {
        output.push_str(&format!("{}\n", "  (empty)".bright_black()));
    } else {
        for item in items {
            let (indicator, content_styled): (&str, ColoredString) = match item.status {
                TodoStatus::Pending => ("[ ]", item.content.white()),
                TodoStatus::InProgress => ("[-]", item.content.cyan().bold()),
                TodoStatus::Completed => ("[✓]", item.content.bright_black()),
            };
            output.push_str(&format!("  {} {}\n", indicator, content_styled));
        }
    }

    output
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
                }
                current_line.push(ch);
                current_width += ch_width;
            }
            _ => {
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
    fn test_render_all_suppresses_trailing_checkmark() {
        enable_colors();
        let events = vec![
            HistoryEvent::FileDiff {
                diff: "some diff".to_string(),
                language: None,
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
}
