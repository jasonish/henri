// SPDX-License-Identifier: MIT
// Layout caching and message segment computation for the TUI

use super::messages::{
    DiffMessage, Message, TextMessage, ThinkingMessage, TodoListDisplay, ToolCallsMessage,
    bulletify, format_error_message,
};
use unicode_width::UnicodeWidthChar;

pub(crate) const MIN_MESSAGE_HEIGHT: u16 = 1;
pub(crate) const INPUT_PROMPT_WIDTH: u16 = 1;
pub(crate) const INPUT_PROMPT_GAP: u16 = 1;
pub(crate) const INPUT_PROMPT: &str = ">";
pub(crate) const SHELL_PROMPT: &str = "!";

/// Number of spaces to display for a tab character.
pub(crate) const TAB_WIDTH: usize = 4;

/// Get the display width of a character, handling tabs specially.
/// Tabs are treated as a fixed width (TAB_WIDTH spaces) for rendering in code blocks.
#[inline]
pub(crate) fn char_display_width(ch: char) -> usize {
    if ch == '\t' {
        TAB_WIDTH
    } else {
        UnicodeWidthChar::width(ch).unwrap_or(0)
    }
}

pub(crate) struct LayoutCache {
    pub width: u16,
    pub heights: Vec<u16>,
    pub total_height: usize,
}

impl LayoutCache {
    pub(crate) fn new() -> Self {
        Self {
            width: 0,
            heights: Vec::new(),
            total_height: 0,
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.width = 0;
        self.heights.clear();
        self.total_height = 0;
    }
}

impl Default for LayoutCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Get the display width of a list item prefix (e.g., "1. ", "- ", "10. ").
/// Returns the width if the line starts with a list marker, otherwise None.
pub(crate) fn list_prefix_width(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let leading_spaces = line.len() - trimmed.len();

    // Check for bullet list: "- " or "* "
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        return Some(leading_spaces + 2);
    }

    // Check for numbered list: "N. " where N is one or more digits
    let bytes = trimmed.as_bytes();
    if !bytes.is_empty() && bytes[0].is_ascii_digit() {
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        // Check for ". " after digits
        if i < bytes.len() && bytes[i] == b'.' && i + 1 < bytes.len() && bytes[i + 1] == b' ' {
            return Some(leading_spaces + i + 2);
        }
    }

    None
}

/// Wrap a single line with a hanging indent for list items.
/// Returns a vector of wrapped lines (as String).
/// If the line is not a list item or doesn't need wrapping, returns a single-element vec.
pub(crate) fn wrap_line_with_hanging_indent(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![line.to_string()];
    }

    let effective_width = width.saturating_sub(1).max(1);
    let line_width: usize = line.chars().map(char_display_width).sum();

    // If line fits, no wrapping needed
    if line_width <= effective_width {
        return vec![line.to_string()];
    }

    let indent_width = list_prefix_width(line).unwrap_or(0);
    let indent_str: String = " ".repeat(indent_width);

    let mut result = Vec::new();
    let mut current_line = String::new();
    let mut current_width = 0;

    // Split into words for word wrapping
    let mut chars = line.char_indices().peekable();
    let mut word_start = 0;

    while let Some((idx, ch)) = chars.next() {
        let is_last = chars.peek().is_none();

        // Detect word boundaries
        if ch.is_whitespace() || is_last {
            let word_end = if is_last { line.len() } else { idx };
            let word = &line[word_start..word_end];
            let word_width: usize = word.chars().map(char_display_width).sum();

            // Check if word fits on current line
            let space_needed = if current_line.is_empty() { 0 } else { 1 };
            if current_width + space_needed + word_width <= effective_width {
                if !current_line.is_empty() {
                    current_line.push(' ');
                    current_width += 1;
                }
                current_line.push_str(word);
                current_width += word_width;
            } else {
                // Word doesn't fit, start new line
                if !current_line.is_empty() {
                    result.push(current_line);
                }
                // Continuation lines get the hanging indent
                current_line = format!("{}{}", indent_str, word);
                current_width = indent_width + word_width;
            }

            // Skip whitespace between words
            word_start = idx + ch.len_utf8();
            while let Some((_, c)) = chars.peek() {
                if c.is_whitespace() {
                    word_start += c.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
        }
    }

    if !current_line.is_empty() {
        result.push(current_line);
    }

    if result.is_empty() {
        vec![line.to_string()]
    } else {
        result
    }
}

#[derive(Clone, Copy)]
pub(crate) struct MessageSegment {
    pub index: usize,
    pub skip_top: u16,
    pub height: u16,
}

pub(crate) fn compute_visible_segments(
    heights: &[u16],
    viewport_height: u16,
    total_height: usize,
    scroll_lines: usize,
) -> Vec<MessageSegment> {
    if heights.is_empty() || viewport_height == 0 || total_height == 0 {
        return Vec::new();
    }

    let viewport = viewport_height as usize;
    let max_scroll = total_height.saturating_sub(viewport);
    let clamped_scroll = scroll_lines.min(max_scroll);

    let window_start = total_height.saturating_sub(viewport + clamped_scroll);
    let window_end = total_height.saturating_sub(clamped_scroll);

    let mut segments = Vec::new();
    let mut acc = 0usize;

    for (idx, h) in heights.iter().enumerate() {
        let msg_height = *h as usize;
        let msg_start = acc;
        let msg_end = acc + msg_height;

        if msg_end <= window_start {
            acc = msg_end;
            continue;
        }
        if msg_start >= window_end {
            break;
        }

        let msg_overlap_start = window_start.max(msg_start);
        let msg_overlap_end = window_end.min(msg_end);
        if msg_overlap_end > msg_overlap_start {
            let skip_top = (msg_overlap_start - msg_start).min(u16::MAX as usize) as u16;
            let height = (msg_overlap_end - msg_overlap_start)
                .max(1)
                .min(u16::MAX as usize) as u16;

            segments.push(MessageSegment {
                index: idx,
                skip_top,
                height,
            });
        }

        acc = msg_end;
    }

    segments
}

pub(crate) const USER_MESSAGE_PADDING: u16 = 0;

/// Determines if a blank line spacer is needed when transitioning between message types.
/// This centralizes the spacing logic for visual separation between different content blocks.
pub(crate) fn needs_spacer_above(prev: &Message, current: &Message) -> bool {
    use Message::*;
    match (prev, current) {
        // Always separate todo list and usage displays from surrounding content
        (TodoList(_), _) | (_, TodoList(_)) => true,
        (_, Usage(_)) => true,

        // Add space before assistant text when following tool calls or thinking
        (AssistantToolCalls(_), AssistantText(_)) => true,
        (AssistantThinking(_), AssistantText(_)) => true,

        // Add space before tool calls when following text or thinking
        (AssistantText(_), AssistantToolCalls(_)) => true,
        (AssistantThinking(_), AssistantToolCalls(_)) => true,

        // Add space before thinking when following text or tool calls
        (AssistantText(_), AssistantThinking(_)) => true,
        (AssistantToolCalls(_), AssistantThinking(_)) => true,

        // No spacer needed for other transitions
        _ => false,
    }
}

pub(crate) fn message_display_height(message: &Message, width: u16) -> u16 {
    match message {
        Message::AssistantThinking(msg) => {
            return thinking_message_display_height(msg, width);
        }
        Message::AssistantToolCalls(msg) => {
            return tool_calls_message_display_height(msg, width);
        }
        Message::AssistantText(msg) => {
            return text_message_display_height(msg, width);
        }
        _ => {}
    }

    match message {
        Message::Text(text) => text_message_height(&bulletify(text), width),
        Message::Error(err) => text_message_height(&format_error_message(err), width),
        Message::Warning(warn) => text_message_height(&bulletify(warn), width),
        Message::AssistantThinking(_)
        | Message::AssistantToolCalls(_)
        | Message::AssistantText(_) => unreachable!(),
        Message::Shell(shell) => text_message_height(&shell.display, width),
        Message::User(user_msg) => {
            text_message_height(&user_msg.display_text, width).saturating_add(USER_MESSAGE_PADDING)
        }
        Message::Usage(usage) => text_message_height(&usage.display_text, width),
        Message::TodoList(todo) => todo_list_display_height(todo, width),
        Message::FileDiff(diff) => diff_display_height(diff, width),
    }
}

fn todo_list_display_height(todo: &TodoListDisplay, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    // One line per todo item (status indicator + text)
    // Plus header line
    (todo.todos.len() + 1).max(1).min(u16::MAX as usize) as u16
}

/// Width of the line number gutter in diff display: "  3 + " = 6 chars
pub(crate) const DIFF_GUTTER_WIDTH: u16 = 6;

fn diff_display_height(diff: &DiffMessage, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    // Account for gutter width
    let effective_width = width
        .saturating_sub(1)
        .saturating_sub(DIFF_GUTTER_WIDTH)
        .max(1);

    // Filter out hunk header lines (@@) since they're not rendered
    // and sum the wrapped lines of each content line
    let diff_lines: usize = diff
        .diff
        .lines()
        .filter(|line| !line.starts_with("@@"))
        .map(|line| count_wrapped_lines(line, effective_width as usize))
        .sum();

    (diff_lines as u16).max(1)
}

pub(crate) fn text_message_height(display: &str, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let lines = count_wrapped_lines(display, width as usize);
    lines.max(1).min(u16::MAX as usize) as u16
}

pub(crate) fn thinking_message_display_height(msg: &ThinkingMessage, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }

    let trimmed = msg.text.trim();
    if trimmed.is_empty() {
        return 0;
    }

    let effective_width = (width as usize).saturating_sub(1).max(1);

    // Pre-wrap lines with hanging indent for list items (matching render logic)
    let mut total_lines = 0usize;
    for line in trimmed.lines() {
        let line = line.trim_end();
        let wrapped = wrap_line_with_hanging_indent(line, effective_width);
        total_lines += wrapped.len();
    }

    total_lines.max(1).min(u16::MAX as usize) as u16
}

pub(crate) fn tool_calls_message_display_height(msg: &ToolCallsMessage, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }

    if msg.calls.is_empty() {
        return 0;
    }

    // Build the indented tool calls display
    let mut tools_display = String::new();
    for (i, tool) in msg.calls.iter().enumerate() {
        tools_display.push_str("  ");
        tools_display.push_str(tool.trim());
        if i < msg.calls.len() - 1 {
            tools_display.push('\n');
        }
    }

    let lines = count_wrapped_lines(&tools_display, width as usize);
    lines.min(u16::MAX as usize) as u16
}

pub(crate) fn text_message_display_height(msg: &TextMessage, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }

    let trimmed = msg.text.trim();
    if trimmed.is_empty() {
        return 0;
    }

    let lines = count_wrapped_lines(trimmed, width as usize);
    lines.min(u16::MAX as usize) as u16
}

/// Count wrapped lines for text at a given width with word wrapping
fn count_wrapped_lines(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }

    // Use effective width that leaves 1 column margin at edge
    let effective_width = width.saturating_sub(1).max(1);

    let mut lines: usize = 0;
    let mut screen_col: usize = 0;
    let mut prev_was_whitespace = true;
    let mut in_code_block = false;
    let mut line_start_byte: usize = 0;

    // Helper to check if a line is a code fence
    let is_code_fence_line = |start: usize, end: usize| -> bool {
        if end <= start {
            return false;
        }
        let line = &text[start..end];
        line.trim().starts_with("```")
    };

    for (byte_idx, ch) in text.char_indices() {
        if ch == '\n' {
            // Check if this line was a code fence
            let line = &text[line_start_byte..byte_idx];
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                in_code_block = !in_code_block;
            }
            lines += 1;
            screen_col = 0;
            prev_was_whitespace = true;
            line_start_byte = byte_idx + 1;
            continue;
        }

        let ch_width = char_display_width(ch);
        if ch_width == 0 {
            continue;
        }

        let is_whitespace = ch.is_whitespace();

        // Skip leading whitespace at start of line (after wrap) - but NOT in code blocks
        // Exception: also skip leading whitespace on code fence lines for consistency
        if is_whitespace && screen_col == 0 {
            // Check if this is a code fence line by looking ahead
            let line_end = text[byte_idx..]
                .find('\n')
                .map(|pos| byte_idx + pos)
                .unwrap_or(text.len());
            let is_fence = is_code_fence_line(line_start_byte, line_end);

            if !in_code_block || is_fence {
                prev_was_whitespace = true;
                continue;
            }
        }

        // Word wrap: if starting a new word and it won't fit, wrap first
        // Don't word-wrap inside code blocks - just character wrap
        if !in_code_block && !is_whitespace && prev_was_whitespace && screen_col > 0 {
            let word_width = word_display_width(text, byte_idx);
            if word_width <= effective_width && screen_col + word_width > effective_width {
                lines += 1;
                screen_col = 0;
            }
        }

        // Handle line wrapping for characters that don't fit
        if screen_col + ch_width > effective_width {
            lines += 1;
            screen_col = 0;
        }

        screen_col += ch_width;
        prev_was_whitespace = is_whitespace;
    }

    // Account for final line if there's content
    if screen_col > 0 {
        lines += 1;
    }

    lines.max(1)
}

/// Get the display width of the word starting at a byte offset
pub(crate) fn word_display_width(text: &str, start_byte: usize) -> usize {
    let mut width = 0;
    for ch in text[start_byte..].chars() {
        if ch.is_whitespace() {
            break;
        }
        width += char_display_width(ch);
    }
    width
}

/// Calculate how many lines the input will take at a given width
pub(crate) fn input_display_lines(input: &str, width: u16) -> usize {
    if width == 0 {
        return 1;
    }
    let width = width as usize;

    // Use effective width that leaves 1 column margin at edge
    let effective_width = width.saturating_sub(1).max(1);

    let mut lines: usize = 1;
    let mut screen_col: usize = 0;
    let mut prev_was_whitespace = true; // Start as true to handle first word
    let mut wrapped = false; // Track if current line is from wrapping

    for (byte_idx, ch) in input.char_indices() {
        if ch == '\n' {
            lines += 1;
            screen_col = 0;
            prev_was_whitespace = true;
            wrapped = false;
            continue;
        }

        let ch_width = char_display_width(ch);
        if ch_width == 0 {
            continue;
        }

        let is_whitespace = ch.is_whitespace();

        // Skip leading whitespace only on wrapped lines (not on explicit newlines or first line)
        if is_whitespace && screen_col == 0 && wrapped {
            prev_was_whitespace = true;
            continue;
        }

        // Word wrap: if starting a new word and it won't fit, wrap first
        if !is_whitespace && prev_was_whitespace && screen_col > 0 {
            let word_width = word_display_width(input, byte_idx);
            // Only wrap if word is longer than remaining space but fits on a line
            if word_width <= effective_width && screen_col + word_width > effective_width {
                lines += 1;
                screen_col = 0;
                wrapped = true;
            }
        }

        // Handle line wrapping for characters that don't fit
        if screen_col + ch_width > effective_width {
            lines += 1;
            screen_col = 0;
            wrapped = true;
        }

        screen_col += ch_width;

        if screen_col == effective_width {
            lines += 1;
            screen_col = 0;
            wrapped = true;
        }

        prev_was_whitespace = is_whitespace;
    }

    lines.max(1)
}

/// Get the cursor position (line, column) in the input
pub(crate) fn cursor_position(input: &str, cursor: usize, width: u16) -> (u16, u16) {
    if width == 0 {
        return (0, 0);
    }
    let width = width as usize;

    // Use effective width that leaves 1 column margin at edge
    let effective_width = width.saturating_sub(1).max(1);

    let mut line: usize = 0;
    let mut col: usize = 0;
    let mut prev_was_whitespace = true; // Start as true to handle first word
    let mut wrapped = false; // Track if current line is from wrapping

    for (byte_idx, ch) in input.char_indices() {
        if byte_idx >= cursor {
            break;
        }

        if ch == '\n' {
            line += 1;
            col = 0;
            prev_was_whitespace = true;
            wrapped = false;
            continue;
        }

        let ch_width = char_display_width(ch);
        if ch_width == 0 {
            continue;
        }

        let is_whitespace = ch.is_whitespace();

        // Skip leading whitespace only on wrapped lines (not on explicit newlines or first line)
        if is_whitespace && col == 0 && wrapped {
            prev_was_whitespace = true;
            continue;
        }

        // Word wrap: if starting a new word and it won't fit, wrap first
        if !is_whitespace && prev_was_whitespace && col > 0 {
            let word_width = word_display_width(input, byte_idx);
            // Only wrap if word is longer than remaining space but fits on a line
            if word_width <= effective_width && col + word_width > effective_width {
                line += 1;
                col = 0;
                wrapped = true;
            }
        }

        // Handle line wrapping for characters that don't fit
        if col + ch_width > effective_width {
            line += 1;
            col = 0;
            wrapped = true;
        }

        col += ch_width;

        if col == effective_width {
            line += 1;
            col = 0;
            wrapped = true;
        }

        prev_was_whitespace = is_whitespace;
    }

    (
        line.min(u16::MAX as usize) as u16,
        col.min(u16::MAX as usize) as u16,
    )
}

pub(crate) fn current_prompt(input: &str) -> &'static str {
    if input.trim_start().starts_with('!') {
        SHELL_PROMPT
    } else {
        INPUT_PROMPT
    }
}
