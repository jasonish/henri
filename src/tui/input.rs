// SPDX-License-Identifier: MIT
// Input editing functions for the TUI

use ratatui::prelude::*;

use super::layout::char_display_width;

/// Find the previous character boundary at or before the given byte offset
pub(crate) fn prev_char_boundary(text: &str, byte_offset: usize) -> usize {
    if byte_offset == 0 || text.is_empty() {
        return 0;
    }
    let offset = byte_offset.min(text.len());
    // Find the last char that starts at or before offset
    text[..offset]
        .char_indices()
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Find the next character boundary after the given byte offset
/// Returns end of string if at or past the last character
pub(crate) fn next_char_boundary(text: &str, byte_offset: usize) -> usize {
    if text.is_empty() || byte_offset >= text.len() {
        return text.len();
    }
    // Find the first char that starts after byte_offset
    text.char_indices()
        .find(|(i, _)| *i > byte_offset)
        .map(|(i, _)| i)
        .unwrap_or(text.len())
}

/// Snap a byte offset to a valid character boundary for cursor positioning
/// If offset is already at a boundary, returns it unchanged
/// Otherwise snaps to the previous boundary
pub(crate) fn snap_to_char_boundary(text: &str, offset: usize) -> usize {
    let offset = offset.min(text.len());
    if text.is_char_boundary(offset) {
        offset
    } else {
        prev_char_boundary(text, offset)
    }
}

/// Find line boundaries around a byte offset in text
/// Returns (start, end) byte offsets of the line (excluding newline)
pub(crate) fn find_line_boundaries(text: &str, byte_offset: usize) -> (usize, usize) {
    if text.is_empty() {
        return (0, 0);
    }

    let offset = byte_offset.min(text.len().saturating_sub(1));
    let bytes = text.as_bytes();

    // Find start of line
    let mut start = offset;
    while start > 0 && bytes[start - 1] != b'\n' {
        start -= 1;
    }

    // Find end of line
    let mut end = offset;
    while end < bytes.len() && bytes[end] != b'\n' {
        end += 1;
    }

    (start, end)
}

/// Find word boundaries around a byte offset in text
/// Returns (start, end) byte offsets of the word
pub(crate) fn find_word_boundaries(text: &str, byte_offset: usize) -> (usize, usize) {
    if text.is_empty() || byte_offset > text.len() {
        return (0, 0);
    }

    let bytes = text.as_bytes();
    let offset = byte_offset.min(text.len().saturating_sub(1));

    // Helper to check if a byte is part of a word
    let is_word_char = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

    // If we're on whitespace/punctuation, just select that character
    if offset < bytes.len() && !is_word_char(bytes[offset]) {
        // Find the end of this non-word sequence
        let mut end = offset;
        while end < bytes.len() && !is_word_char(bytes[end]) && bytes[end] != b'\n' {
            end += 1;
        }
        let mut start = offset;
        while start > 0 && !is_word_char(bytes[start - 1]) && bytes[start - 1] != b'\n' {
            start -= 1;
        }
        return (start, end);
    }

    // Find start of word
    let mut start = offset;
    while start > 0 && is_word_char(bytes[start - 1]) {
        start -= 1;
    }

    // Find end of word
    let mut end = offset;
    while end < bytes.len() && is_word_char(bytes[end]) {
        end += 1;
    }

    (start, end)
}

/// Convert screen coordinates to byte offset in input text.
/// Clamps coordinates to the area bounds to allow selection to start from margins.
pub(crate) fn screen_to_input_offset(
    screen_x: u16,
    screen_y: u16,
    area: Rect,
    text: &str,
) -> Option<usize> {
    if area.width == 0 || area.height == 0 {
        return Some(0);
    }

    // Clamp coordinates to area bounds instead of rejecting out-of-bounds clicks.
    // This allows selection to start from margins.
    let clamped_x = screen_x.clamp(area.x, area.x + area.width - 1);
    let clamped_y = screen_y.clamp(area.y, area.y + area.height - 1);

    let target_row = (clamped_y - area.y) as usize;
    let target_col = (clamped_x - area.x) as usize;

    // Use effective width that leaves 1 column margin at the edge
    let width = area.width as usize;
    let effective_width = width.saturating_sub(1).max(1);

    let mut screen_row: usize = 0;
    let mut screen_col: usize = 0;
    let mut last_byte_idx: usize = 0;
    let mut prev_was_whitespace = true; // Start as true to handle first word

    for (byte_idx, ch) in text.char_indices() {
        if ch == '\n' {
            // Check if click is on this row after the text
            if screen_row == target_row && target_col >= screen_col {
                return Some(byte_idx);
            }
            screen_row += 1;
            screen_col = 0;
            prev_was_whitespace = true;
            last_byte_idx = byte_idx + ch.len_utf8();
            continue;
        }

        let ch_width = char_display_width(ch);
        if ch_width == 0 {
            continue;
        }

        let is_whitespace = ch.is_whitespace();

        // Skip leading whitespace at start of line (after wrap)
        if is_whitespace && screen_col == 0 {
            prev_was_whitespace = true;
            continue;
        }

        // Word wrap: if starting a new word and it won't fit, wrap first
        if !is_whitespace && prev_was_whitespace && screen_col > 0 {
            let word_width = super::layout::word_display_width(text, byte_idx);
            // Only wrap if word is longer than remaining space but fits on a line
            if word_width <= effective_width && screen_col + word_width > effective_width {
                if screen_row == target_row && target_col >= screen_col {
                    return Some(byte_idx);
                }
                screen_row += 1;
                screen_col = 0;
            }
        }

        if screen_col + ch_width > effective_width {
            if screen_row == target_row && target_col >= screen_col {
                return Some(byte_idx);
            }
            screen_row += 1;
            screen_col = 0;
        }

        // Check if this is the target position
        if screen_row == target_row
            && screen_col <= target_col
            && target_col < screen_col + ch_width
        {
            return Some(byte_idx);
        }

        // Past target row
        if screen_row > target_row {
            return Some(last_byte_idx);
        }

        screen_col += ch_width;
        last_byte_idx = byte_idx;

        if screen_col == effective_width {
            if screen_row == target_row {
                return Some(byte_idx + ch.len_utf8());
            }
            screen_row += 1;
            screen_col = 0;
        }

        prev_was_whitespace = is_whitespace;
    }

    // Click is past the end of text
    Some(text.len())
}

/// Input editing trait - implemented by App
pub(crate) trait InputEditor {
    fn input(&self) -> &str;
    fn input_mut(&mut self) -> &mut String;
    fn cursor(&self) -> usize;
    fn set_cursor(&mut self, pos: usize);

    fn insert_str_at_cursor(&mut self, s: &str) {
        let cursor = self.cursor();
        // Ensure cursor is at a valid UTF-8 character boundary
        let cursor = if self.input().is_char_boundary(cursor) {
            cursor
        } else {
            snap_to_char_boundary(self.input(), cursor)
        };
        self.input_mut().insert_str(cursor, s);
        self.set_cursor(cursor + s.len());
    }

    fn backspace(&mut self) {
        let cursor = self.cursor();
        let input = self.input_mut();
        if let Some((idx, _ch)) = input[..cursor].char_indices().last() {
            input.drain(idx..cursor);
            self.set_cursor(idx);
        }
    }

    fn delete_forward(&mut self) {
        let cursor = self.cursor();
        let input = self.input_mut();
        if cursor >= input.len() {
            return;
        }
        let mut iter = input[cursor..].char_indices();
        if let Some((_, ch)) = iter.next() {
            let next = cursor + ch.len_utf8();
            input.drain(cursor..next);
        }
    }

    fn delete_word_forward(&mut self) {
        let cursor = self.cursor();
        let text = self.input();
        let mut pos = cursor;
        let mut skipping_whitespace = true;

        for (byte_idx, ch) in text[cursor..].char_indices() {
            if skipping_whitespace {
                if !ch.is_whitespace() {
                    skipping_whitespace = false;
                }
                pos = cursor + byte_idx + ch.len_utf8();
            } else {
                if ch.is_whitespace() {
                    pos = cursor + byte_idx;
                    break;
                }
                pos = cursor + byte_idx + ch.len_utf8();
            }
        }

        if pos > cursor {
            self.input_mut().drain(cursor..pos);
        }
    }

    fn move_left(&mut self) {
        let cursor = self.cursor();
        if let Some((idx, _)) = self.input()[..cursor].char_indices().last() {
            self.set_cursor(idx);
        }
    }

    fn move_right(&mut self) {
        let cursor = self.cursor();
        let mut iter = self.input()[cursor..].char_indices();
        if let Some((_, ch)) = iter.next() {
            self.set_cursor(cursor + ch.len_utf8());
        } else {
            self.set_cursor(self.input().len());
        }
    }

    fn move_to_start(&mut self) {
        self.set_cursor(0);
    }

    fn move_to_end(&mut self) {
        self.set_cursor(self.input().len());
    }

    fn move_to_line_start(&mut self) {
        let cursor = self.cursor();
        let (start, _) = find_line_boundaries(self.input(), cursor);
        self.set_cursor(start);
    }

    fn move_to_line_end(&mut self) {
        let cursor = self.cursor();
        let (_, end) = find_line_boundaries(self.input(), cursor);
        self.set_cursor(end);
    }

    fn move_word_left(&mut self) {
        let cursor = self.cursor();
        let text = self.input();
        let mut pos = cursor;
        let mut skipping_whitespace = true;

        for (byte_idx, ch) in text[..cursor].char_indices().rev() {
            if skipping_whitespace {
                if !ch.is_whitespace() {
                    skipping_whitespace = false;
                }
                pos = byte_idx;
            } else {
                if ch.is_whitespace() {
                    pos = byte_idx + ch.len_utf8();
                    break;
                }
                pos = byte_idx;
            }
        }

        self.set_cursor(pos);
    }

    fn move_word_right(&mut self) {
        let cursor = self.cursor();
        let text = self.input();
        let mut pos = cursor;
        let mut skipping_whitespace = true;

        for (byte_idx, ch) in text[cursor..].char_indices() {
            if skipping_whitespace {
                if !ch.is_whitespace() {
                    skipping_whitespace = false;
                }
                pos = cursor + byte_idx + ch.len_utf8();
            } else {
                if ch.is_whitespace() {
                    pos = cursor + byte_idx;
                    break;
                }
                pos = cursor + byte_idx + ch.len_utf8();
            }
        }

        self.set_cursor(pos);
    }

    fn delete_word_back(&mut self) {
        let original = self.cursor();
        self.move_word_left();
        let new_cursor = self.cursor();
        if new_cursor < original {
            self.input_mut().drain(new_cursor..original);
            self.set_cursor(new_cursor);
        }
    }

    fn kill_to_start(&mut self) {
        let cursor = self.cursor();
        if cursor == 0 {
            return;
        }
        self.input_mut().drain(0..cursor);
        self.set_cursor(0);
    }

    fn kill_to_end(&mut self) {
        let cursor = self.cursor();
        self.input_mut().truncate(cursor);
    }

    fn clear_input(&mut self) {
        self.input_mut().clear();
        self.set_cursor(0);
    }
}

/// Convert logical line and column to byte offset in input text.
/// Returns the byte offset, clamped to valid positions.
pub(crate) fn line_col_to_offset(
    text: &str,
    target_line: u16,
    target_col: u16,
    width: u16,
) -> usize {
    if width == 0 || text.is_empty() {
        return 0;
    }

    let effective_width = (width as usize).saturating_sub(1).max(1);
    let target_line = target_line as usize;
    let target_col = target_col as usize;

    let mut line: usize = 0;
    let mut col: usize = 0;
    let mut last_byte_idx: usize = 0;
    let mut prev_was_whitespace = true;

    for (byte_idx, ch) in text.char_indices() {
        if ch == '\n' {
            if line == target_line && target_col >= col {
                return byte_idx;
            }
            line += 1;
            col = 0;
            prev_was_whitespace = true;
            last_byte_idx = byte_idx + ch.len_utf8();
            continue;
        }

        let ch_width = char_display_width(ch);
        if ch_width == 0 {
            continue;
        }

        let is_whitespace = ch.is_whitespace();

        if is_whitespace && col == 0 {
            prev_was_whitespace = true;
            continue;
        }

        if !is_whitespace && prev_was_whitespace && col > 0 {
            let word_width = super::layout::word_display_width(text, byte_idx);
            if word_width <= effective_width && col + word_width > effective_width {
                if line == target_line && target_col >= col {
                    return byte_idx;
                }
                line += 1;
                col = 0;
            }
        }

        if col + ch_width > effective_width {
            if line == target_line && target_col >= col {
                return byte_idx;
            }
            line += 1;
            col = 0;
        }

        if line == target_line && col <= target_col && target_col < col + ch_width {
            return byte_idx;
        }

        if line > target_line {
            return last_byte_idx;
        }

        col += ch_width;
        last_byte_idx = byte_idx;

        if col == effective_width {
            if line == target_line {
                return byte_idx + ch.len_utf8();
            }
            line += 1;
            col = 0;
        }

        prev_was_whitespace = is_whitespace;
    }

    if line == target_line && target_col >= col {
        return text.len();
    }

    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestEditor {
        input: String,
        cursor: usize,
    }

    impl InputEditor for TestEditor {
        fn input(&self) -> &str {
            &self.input
        }
        fn input_mut(&mut self) -> &mut String {
            &mut self.input
        }
        fn cursor(&self) -> usize {
            self.cursor
        }
        fn set_cursor(&mut self, pos: usize) {
            self.cursor = pos;
        }
    }

    #[test]
    fn test_line_movements() {
        let mut editor = TestEditor {
            input: "Line 1\nLine 2\nLine 3".to_string(),
            cursor: 0,
        };

        // Start at beginning
        assert_eq!(editor.cursor(), 0);

        // Move to end of first line
        editor.move_to_line_end();
        assert_eq!(editor.cursor(), 6); // "Line 1" is 6 chars

        // Move to start of first line
        editor.move_to_line_start();
        assert_eq!(editor.cursor(), 0);

        // Move to middle of second line
        editor.set_cursor(10); // "Line 1\nLi"

        // Move to start of second line
        editor.move_to_line_start();
        assert_eq!(editor.cursor(), 7); // "Line 1\n" is 7 chars

        // Move to end of second line
        editor.move_to_line_end();
        assert_eq!(editor.cursor(), 13); // "Line 1\nLine 2" is 13 chars
    }

    #[test]
    fn test_word_movements_with_multibyte_chars() {
        let mut editor = TestEditor {
            input: "hello 😀 world".to_string(),
            cursor: 0,
        };

        // "hello" = 5 bytes, " " = 1 byte, "😀" = 4 bytes, " " = 1 byte, "world" = 5 bytes
        // Byte positions: hello=0-4, space=5, emoji=6-9, space=10, world=11-15
        // Total: 16 bytes

        // Move right from start should skip "hello", stop at space
        editor.move_word_right();
        assert_eq!(editor.cursor(), 5); // At the space after "hello"
        assert!(editor.input.is_char_boundary(editor.cursor()));

        // Move right again should skip space and emoji, stop at next space
        editor.move_word_right();
        assert_eq!(editor.cursor(), 10); // At the space after emoji
        assert!(editor.input.is_char_boundary(editor.cursor()));

        // Move right again should skip to end
        editor.move_word_right();
        assert_eq!(editor.cursor(), 16); // End of string
        assert!(editor.input.is_char_boundary(editor.cursor()));

        // Now test moving left
        editor.move_word_left();
        assert_eq!(editor.cursor(), 11); // Start of "world"
        assert!(editor.input.is_char_boundary(editor.cursor()));

        editor.move_word_left();
        assert_eq!(editor.cursor(), 6); // Start of emoji
        assert!(editor.input.is_char_boundary(editor.cursor()));

        editor.move_word_left();
        assert_eq!(editor.cursor(), 0); // Start of "hello"
        assert!(editor.input.is_char_boundary(editor.cursor()));
    }

    #[test]
    fn test_delete_word_forward_with_multibyte_chars() {
        let mut editor = TestEditor {
            input: "test 😀 data".to_string(),
            cursor: 0,
        };

        // Delete "test" (stops at space)
        editor.delete_word_forward();
        assert_eq!(editor.input(), " 😀 data");
        assert_eq!(editor.cursor(), 0);
        assert!(editor.input.is_char_boundary(editor.cursor()));

        // Delete " 😀" (stops at space)
        editor.delete_word_forward();
        assert_eq!(editor.input(), " data");
        assert_eq!(editor.cursor(), 0);
        assert!(editor.input.is_char_boundary(editor.cursor()));
    }

    #[test]
    fn test_insert_at_cursor_with_multibyte_chars() {
        let mut editor = TestEditor {
            input: "hello world".to_string(),
            cursor: 6, // After "hello "
        };

        // Insert emoji
        editor.insert_str_at_cursor("😀 ");
        assert_eq!(editor.input(), "hello 😀 world");
        assert_eq!(editor.cursor(), 11); // 6 + 4 bytes (emoji) + 1 byte (space)
        assert!(editor.input.is_char_boundary(editor.cursor()));
    }
}
