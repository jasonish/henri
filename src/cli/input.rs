// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Input state and key event handling for CLI mode.
//!
//! This module extracts input state management from prompt.rs and provides
//! an action-based key event handler that returns what action should be taken
//! rather than directly modifying terminal state.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use super::slash_menu::{SlashMenuState, extract_query};
use crate::cli::PastedImage;
use crate::commands::Command;
use crate::completion::FileCompleter;

pub(super) const PROMPT: &str = "";

/// Get display width of a character
fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Get display width of a string
pub(super) fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Image marker prefix - markers look like "Image#1", "Image#2", etc.
pub(super) const IMAGE_MARKER_PREFIX: &str = "Image#";

fn max_image_marker_id_in_text(content: &str) -> usize {
    let mut max_id = 0usize;
    let mut rest = content;

    while let Some(pos) = rest.find(IMAGE_MARKER_PREFIX) {
        rest = &rest[pos + IMAGE_MARKER_PREFIX.len()..];

        let digits_len = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits_len == 0 {
            continue;
        }

        // Safe because we only count ASCII digits.
        let id_str = &rest[..digits_len];
        if let Ok(id) = id_str.parse::<usize>() {
            max_id = max_id.max(id);
        }

        rest = &rest[digits_len..];
    }

    max_id
}

fn next_image_id_for_content(content: &str, images: &[PastedImage]) -> usize {
    let mut max_id = max_image_marker_id_in_text(content);
    for image in images {
        if let Some(id_str) = image.marker.strip_prefix(IMAGE_MARKER_PREFIX)
            && let Ok(id) = id_str.parse::<usize>()
        {
            max_id = max_id.max(id);
        }
    }

    if max_id == 0 { 1 } else { max_id + 1 }
}

/// Check if we're at the start of an image marker at the given position.
/// Returns the length of the marker if found, or None.
fn image_marker_at(chars: &[char], pos: usize) -> Option<usize> {
    let prefix_chars: Vec<char> = IMAGE_MARKER_PREFIX.chars().collect();
    let prefix_len = prefix_chars.len();

    // Need room for prefix + at least one digit
    if pos + prefix_len >= chars.len() {
        return None;
    }

    for (i, &pc) in prefix_chars.iter().enumerate() {
        if chars[pos + i] != pc {
            return None;
        }
    }

    // Check that there's at least one digit after the prefix
    if !chars[pos + prefix_len].is_ascii_digit() {
        return None;
    }

    // Count all digits after the prefix
    let mut end = pos + prefix_len;
    while end < chars.len() && chars[end].is_ascii_digit() {
        end += 1;
    }

    Some(end - pos)
}

/// Check if position is inside an image marker.
/// Returns (marker_start, marker_end) if inside a marker, or None.
fn inside_image_marker(chars: &[char], pos: usize) -> Option<(usize, usize)> {
    // Search backwards for a potential marker start. We look back far enough
    // to cover the prefix plus a reasonable number of digits (e.g., "Image#999").
    let max_marker_len = IMAGE_MARKER_PREFIX.len() + 4;
    let search_start = pos.saturating_sub(max_marker_len);

    for start in search_start..=pos {
        if let Some(len) = image_marker_at(chars, start) {
            let end = start + len;
            if pos >= start && pos <= end {
                return Some((start, end));
            }
        }
    }
    None
}

/// Find the previous word boundary, treating image markers as single tokens.
/// Returns the new column position.
fn find_word_start_backward(chars: &[char], mut col: usize) -> usize {
    if col == 0 {
        return 0;
    }

    // Check if we're at the end of or inside an image marker
    if let Some((marker_start, _)) = inside_image_marker(chars, col.saturating_sub(1)) {
        return marker_start;
    }

    // 1. Skip non-alphanumeric backwards
    while col > 0 && !chars[col - 1].is_alphanumeric() {
        col -= 1;
    }

    // Check if we landed at the end of an image marker
    if let Some((marker_start, _)) = inside_image_marker(chars, col.saturating_sub(1)) {
        return marker_start;
    }

    // 2. Skip alphanumeric backwards
    while col > 0 && chars[col - 1].is_alphanumeric() {
        col -= 1;
    }

    col
}

/// Find the next word boundary, treating image markers as single tokens.
/// Returns the new column position.
fn find_word_end_forward(chars: &[char], mut col: usize) -> usize {
    let len = chars.len();
    if col >= len {
        return len;
    }

    // Check if we're at the start of an image marker
    if let Some(marker_len) = image_marker_at(chars, col) {
        return col + marker_len;
    }

    // Check if we're inside an image marker
    if let Some((_, marker_end)) = inside_image_marker(chars, col) {
        return marker_end;
    }

    // 1. Skip non-alphanumeric forwards
    while col < len && !chars[col].is_alphanumeric() {
        col += 1;
    }

    // Check if we landed at an image marker
    if let Some(marker_len) = image_marker_at(chars, col) {
        return col + marker_len;
    }

    // 2. Skip alphanumeric forwards
    while col < len && chars[col].is_alphanumeric() {
        col += 1;
    }

    col
}

/// A single wrapped display row
#[derive(Debug, Clone)]
pub(super) struct WrappedRow {
    /// The text content of this row
    pub text: String,
}

/// Information about cursor position in wrapped display
#[derive(Debug, Clone)]
pub(super) struct CursorPosition {
    /// The row index in the wrapped display (0-based)
    pub row: usize,
    /// The column within that row (after the prefix)
    pub col: usize,
}

/// Compute wrapped display rows for a list of lines.
/// Returns the wrapped rows and the cursor position within them.
pub(super) fn compute_wrapped_display_lines(
    lines: &[String],
    cursor_line_idx: usize,
    cursor_col_idx: usize,
    term_width: usize,
) -> (Vec<WrappedRow>, CursorPosition) {
    let mut rows = Vec::new();
    let mut cursor_pos = CursorPosition { row: 0, col: 0 };

    let prefix_width = display_width(PROMPT);
    let content_width = term_width.saturating_sub(prefix_width);

    if content_width == 0 {
        // Terminal too narrow, just output lines as-is
        for (line_idx, line) in lines.iter().enumerate() {
            rows.push(WrappedRow { text: line.clone() });
            if line_idx == cursor_line_idx {
                // Convert char index to byte index for cursor display
                let cursor_byte_idx = line
                    .char_indices()
                    .nth(cursor_col_idx)
                    .map(|(i, _)| i)
                    .unwrap_or(line.len());
                cursor_pos = CursorPosition {
                    row: rows.len() - 1,
                    col: cursor_byte_idx,
                };
            }
        }
        return (rows, cursor_pos);
    }

    for (line_idx, line) in lines.iter().enumerate() {
        let is_cursor_line = line_idx == cursor_line_idx;
        let cursor_byte_idx = if is_cursor_line {
            // Convert char index to byte index
            Some(
                line.char_indices()
                    .nth(cursor_col_idx)
                    .map(|(i, _)| i)
                    .unwrap_or(line.len()),
            )
        } else {
            None
        };

        let wrapped = wrap_line_with_cursor(line, content_width, cursor_byte_idx);

        for (text, cursor_col) in wrapped {
            let row_idx = rows.len();
            rows.push(WrappedRow { text });

            if let Some(col) = cursor_col {
                cursor_pos = CursorPosition { row: row_idx, col };
            }
        }
    }

    // Ensure at least one row exists
    if rows.is_empty() {
        rows.push(WrappedRow {
            text: String::new(),
        });
        cursor_pos = CursorPosition { row: 0, col: 0 };
    }

    (rows, cursor_pos)
}

/// Wrap a single line with word wrapping, tracking cursor position.
/// Returns a vector of (text, Option<cursor_col>) tuples.
/// cursor_col is Some if the cursor is in that wrapped row.
fn wrap_line_with_cursor(
    line: &str,
    width: usize,
    cursor_byte_idx: Option<usize>,
) -> Vec<(String, Option<usize>)> {
    if width == 0 {
        let cursor_col = cursor_byte_idx.map(|_| line.len().min(cursor_byte_idx.unwrap_or(0)));
        return vec![(line.to_string(), cursor_col)];
    }

    let mut result: Vec<(String, Option<usize>)> = Vec::new();
    let mut current_row = String::new();
    let mut current_width = 0usize;
    let mut cursor_in_current_row: Option<usize> = None;

    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0;

    while i < chars.len() {
        let (byte_idx, ch) = chars[i];

        // Check if cursor is at this position
        if let Some(cursor_idx) = cursor_byte_idx
            && byte_idx == cursor_idx
        {
            cursor_in_current_row = Some(current_width);
        }

        if ch.is_whitespace() {
            // Output whitespace directly
            let ch_width = char_width(ch);

            if current_width + ch_width > width && current_width > 0 {
                // Wrap before whitespace
                result.push((
                    std::mem::take(&mut current_row),
                    cursor_in_current_row.take(),
                ));
                current_width = 0;
                // Update cursor position if it was at wrap point
                if let Some(cursor_idx) = cursor_byte_idx
                    && byte_idx == cursor_idx
                {
                    cursor_in_current_row = Some(0);
                }

                // Avoid leading spaces on wrapped lines when whitespace caused the wrap.
                i += 1;
                continue;
            }

            current_row.push(ch);
            current_width += ch_width;
            i += 1;
        } else {
            // Non-whitespace: calculate word width for word-wrapping
            let mut word_width = 0usize;
            let mut j = i;
            while j < chars.len() && !chars[j].1.is_whitespace() {
                word_width += char_width(chars[j].1);
                j += 1;
            }

            // Check if word fits on current line
            let should_wrap =
                current_width + word_width > width && current_width > 0 && word_width <= width;

            if should_wrap {
                // Wrap before the word
                result.push((
                    std::mem::take(&mut current_row),
                    cursor_in_current_row.take(),
                ));
                current_width = 0;

                // Update cursor position if it's at current character
                if let Some(cursor_idx) = cursor_byte_idx
                    && byte_idx == cursor_idx
                {
                    cursor_in_current_row = Some(0);
                }
            }

            // Output the character
            let ch_width = char_width(ch);

            // Handle very long words that don't fit on a line
            if current_width + ch_width > width && current_width > 0 {
                result.push((
                    std::mem::take(&mut current_row),
                    cursor_in_current_row.take(),
                ));
                current_width = 0;

                // Update cursor position
                if let Some(cursor_idx) = cursor_byte_idx
                    && byte_idx == cursor_idx
                {
                    cursor_in_current_row = Some(0);
                }
            }

            current_row.push(ch);
            current_width += ch_width;
            i += 1;
        }
    }

    // Handle cursor at end of line
    if let Some(cursor_idx) = cursor_byte_idx
        && cursor_idx >= line.len()
    {
        cursor_in_current_row = Some(current_width);
    }

    // Push final row
    result.push((current_row, cursor_in_current_row));

    // Ensure at least one row
    if result.is_empty() {
        let cursor_col = if cursor_byte_idx.is_some() {
            Some(0)
        } else {
            None
        };
        result.push((String::new(), cursor_col));
    }

    result
}

/// Actions that can result from handling a key event
#[derive(Debug)]
pub(super) enum InputAction {
    /// No action needed (key was handled internally)
    None,
    /// Redraw just the current line
    RedrawLine,
    /// Full redraw of the input box needed
    Redraw,
    /// Submit the current input
    Submit,
    /// Cancel the agent loop (ESC)
    CancelAgentLoop,
    /// Clear prompt or exit (Ctrl+C behavior)
    ClearOrExit,
    /// Quit the application (Ctrl+D on empty input)
    Quit,
    /// Move cursor (simple cursor movement, no redraw)
    MoveCursor,
    /// Open the model selection menu (from slash menu or shortcut)
    OpenModelMenu,
    /// Open the settings menu
    OpenSettingsMenu,
    /// Open the tools menu
    OpenToolsMenu,
    /// Open the MCP menu
    OpenMcpMenu,
    /// Open the LSP menu
    OpenLspMenu,
    /// Trigger the clear command
    TriggerClear,
    /// Trigger read-only mode
    TriggerReadOnly,
    /// Trigger read-write mode
    TriggerReadWrite,
    /// Navigate up in history (older entries)
    HistoryUp,
    /// Navigate down in history (newer entries)
    HistoryDown,
    /// Handle clipboard paste (Ctrl+V)
    ClipboardPaste,
    /// Open history search with fzf (Ctrl+R)
    HistorySearch,
    /// Edit the current prompt in $VISUAL/$EDITOR (Ctrl+G)
    EditInEditor,
    /// Cycle forward through favorite models (Ctrl+Y)
    CycleFavoritesForward,
    /// Cycle backward through favorite models (Shift+Ctrl+Y)
    CycleFavoritesBackward,
    /// Toggle hide tool output (Ctrl+H)
    ToggleHideToolOutput,
    /// Toggle expanded tool output view (Ctrl+O)
    ToggleToolOutputExpanded,
    /// Force full UI redraw (Ctrl+L)
    RedrawAll,
}

/// State for multi-line input
pub(super) struct InputState {
    pub lines: Vec<String>,
    pub line_idx: usize,
    pub col_idx: usize,
    /// Slash menu state (None if menu is not active)
    pub slash_menu: Option<SlashMenuState>,
    /// History navigation index (None = not browsing history)
    history_index: Option<usize>,
    /// Draft content saved when browsing history
    draft: Vec<String>,
    /// Pasted images for the current input
    pub pasted_images: Vec<PastedImage>,
    /// Counter for generating unique image markers
    next_image_id: usize,
    /// Timestamp of last Ctrl+C press for double-Ctrl+C exit detection
    last_ctrl_c_time: Option<Instant>,
    /// File completion state
    pub file_completer: FileCompleter,
    /// Whether the current provider is Claude/Anthropic
    is_claude: bool,
}

impl InputState {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            lines: vec![String::new()],
            line_idx: 0,
            col_idx: 0,
            slash_menu: None,
            history_index: None,
            draft: vec![String::new()],
            pasted_images: Vec::new(),
            next_image_id: 1,
            last_ctrl_c_time: None,
            file_completer: FileCompleter::new(working_dir),
            is_claude: false,
        }
    }

    /// Update whether the current provider is Claude/Anthropic
    pub fn set_is_claude(&mut self, is_claude: bool) {
        self.is_claude = is_claude;
        // Also update any active slash menu
        if let Some(ref mut menu) = self.slash_menu {
            menu.set_is_claude(is_claude);
        }
    }

    pub fn current_line(&self) -> &str {
        &self.lines[self.line_idx]
    }

    pub fn total_lines(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn content(&self) -> String {
        self.lines.join("\n")
    }

    /// Convert character index to byte index in the current line
    fn char_to_byte_idx(&self, char_idx: usize) -> usize {
        self.current_line()
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.current_line().len())
    }

    /// Get the number of characters in the current line
    fn current_line_char_len(&self) -> usize {
        self.current_line().chars().count()
    }

    fn delete_forward(&mut self) -> InputAction {
        if self.col_idx < self.current_line_char_len() {
            // Get byte range for the character at cursor
            let curr_char_start = self.char_to_byte_idx(self.col_idx);
            let next_char_start = self.char_to_byte_idx(self.col_idx + 1);
            self.lines[self.line_idx].replace_range(curr_char_start..next_char_start, "");
            self.update_slash_menu();
            InputAction::Redraw
        } else if self.line_idx < self.total_lines() - 1 {
            let next_line = self.lines.remove(self.line_idx + 1);
            self.lines[self.line_idx].push_str(&next_line);
            self.update_slash_menu();
            InputAction::Redraw
        } else {
            InputAction::None
        }
    }

    /// Get the number of characters in a specific line
    fn line_char_len(&self, line_idx: usize) -> usize {
        self.lines[line_idx].chars().count()
    }

    pub fn display_lines(&self) -> Vec<String> {
        self.lines.clone()
    }

    pub(super) fn display_prefix_width(&self) -> usize {
        0
    }

    pub fn display_lines_and_cursor(&self, term_width: usize) -> (Vec<WrappedRow>, CursorPosition) {
        let display = self.display_lines();
        let cursor_line_idx = self.line_idx;
        let cursor_col_idx = self.col_idx;
        compute_wrapped_display_lines(&display, cursor_line_idx, cursor_col_idx, term_width)
    }

    /// Check if the current line would require word wrapping at the given terminal width.
    #[cfg(test)]
    pub fn current_line_needs_wrapping(&self, term_width: usize) -> bool {
        let prefix_width = display_width(PROMPT);
        let content_width = term_width.saturating_sub(prefix_width);
        if content_width == 0 {
            return false;
        }
        let line_width = display_width(self.current_line());
        line_width > content_width
    }

    /// Update the slash menu based on current input content
    pub fn update_slash_menu(&mut self) {
        // Clear file completion when input changes
        self.file_completer.clear();

        let content = self.content();
        if let Some(query) = extract_query(&content) {
            // Initialize menu if not already active
            if self.slash_menu.is_none() {
                self.slash_menu = Some(SlashMenuState::new(self.is_claude));
            }
            if let Some(ref mut menu) = self.slash_menu {
                menu.update(query);
            }
        } else {
            // Close menu if input doesn't match
            self.slash_menu = None;
        }
    }

    /// Check if slash menu is active
    pub fn slash_menu_active(&self) -> bool {
        self.slash_menu
            .as_ref()
            .is_some_and(|m| !m.items.is_empty())
    }

    /// Check if file completion menu is active
    pub fn completion_active(&self) -> bool {
        self.file_completer.is_active()
    }

    /// Initialize file completion based on word at cursor
    pub fn init_completion(&mut self) {
        if let Some((_start, _end, word)) =
            crate::completion::get_word_at_cursor(&self.content(), self.cursor_byte_offset())
        {
            if FileCompleter::should_complete(&word) {
                self.file_completer.init(&word);
            } else {
                self.file_completer.clear();
            }
        } else {
            self.file_completer.clear();
        }
    }

    /// Apply the selected completion to input
    pub fn apply_completion(&mut self) -> bool {
        if !self.completion_active() {
            return false;
        }

        let content = self.content();
        let cursor_offset = self.cursor_byte_offset();
        if let Some(selected) = self.file_completer.current()
            && let Some((word_start, word_end, _word)) =
                crate::completion::get_word_at_cursor(&content, cursor_offset)
        {
            let selected = selected.to_string();
            // Replace the word with the completion
            self.replace_range(word_start, word_end, &selected);

            // Clear completion after applying
            self.file_completer.clear();
            return true;
        }

        self.file_completer.clear();
        false
    }

    /// Move through completion options and apply immediately (like bash)
    pub fn move_completion(&mut self, delta: isize) -> bool {
        if !self.completion_active() {
            return false;
        }

        self.file_completer.move_selection(delta);

        let content = self.content();
        let cursor_offset = self.cursor_byte_offset();
        if let Some(selected) = self.file_completer.current()
            && let Some((word_start, word_end, _word)) =
                crate::completion::get_word_at_cursor(&content, cursor_offset)
        {
            let selected = selected.to_string();
            self.replace_range(word_start, word_end, &selected);
            return true;
        }

        false
    }

    /// Get the cursor position as a byte offset into the full content string
    fn cursor_byte_offset(&self) -> usize {
        // Add up bytes from previous lines (including newlines)
        let mut offset = 0;
        for i in 0..self.line_idx {
            offset += self.lines[i].len() + 1; // +1 for newline
        }
        // Add cursor position in current line (convert from char to byte index)
        offset += self.char_to_byte_idx(self.col_idx);
        offset
    }

    /// Replace a byte range in the content with new text
    fn replace_range(&mut self, start: usize, end: usize, replacement: &str) {
        // Reconstruct full content
        let mut content = self.content();

        // Do the replacement
        content.replace_range(start..end, replacement);

        // Re-parse into lines
        self.lines = content.lines().map(String::from).collect();
        if self.lines.is_empty() {
            self.lines = vec![String::new()];
        }

        // Position cursor at end of replacement
        let new_cursor_offset = start + replacement.len();

        // Find which line and column the new cursor is in
        let mut offset = 0;
        for (i, line) in self.lines.iter().enumerate() {
            if offset + line.len() >= new_cursor_offset {
                self.line_idx = i;
                // Convert byte offset within line to char index
                let byte_in_line = new_cursor_offset - offset;
                self.col_idx = line[..byte_in_line].chars().count();
                return;
            }
            offset += line.len() + 1; // +1 for newline
        }

        // Fallback to end of last line
        self.line_idx = self.lines.len() - 1;
        self.col_idx = self.lines[self.line_idx].chars().count();
    }

    /// Handle a key event and return the action to take.
    /// This method modifies internal state but does NOT touch the terminal.
    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        let menu_active = self.slash_menu_active();

        match (key.code, key.modifiers) {
            // Alt+Enter, Shift+Enter, Ctrl+J - insert newline
            (KeyCode::Enter, KeyModifiers::ALT)
            | (KeyCode::Enter, KeyModifiers::SHIFT)
            | (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                let byte_idx = self.char_to_byte_idx(self.col_idx);
                let remainder = self.lines[self.line_idx][byte_idx..].to_string();
                self.lines[self.line_idx].truncate(byte_idx);

                self.line_idx += 1;
                self.lines.insert(self.line_idx, remainder);
                self.col_idx = 0;

                // Close slash menu on newline
                self.slash_menu = None;
                InputAction::Redraw
            }

            // Enter - submit or execute menu selection
            (KeyCode::Enter, _) => {
                if menu_active {
                    // Execute the selected slash command
                    if let Some(selected) =
                        self.slash_menu.as_ref().and_then(|m| m.current().cloned())
                    {
                        // For /model command, return special action that works during streaming
                        if matches!(selected.command, Command::Model) {
                            self.clear(); // Clear input so the menu doesn't show /model
                            return InputAction::OpenModelMenu;
                        }
                        if matches!(selected.command, Command::Settings) {
                            self.clear();
                            return InputAction::OpenSettingsMenu;
                        }
                        if matches!(selected.command, Command::Tools) {
                            self.clear();
                            return InputAction::OpenToolsMenu;
                        }
                        if matches!(selected.command, Command::Mcp) {
                            self.clear();
                            return InputAction::OpenMcpMenu;
                        }
                        if matches!(selected.command, Command::Lsp) {
                            self.clear();
                            return InputAction::OpenLspMenu;
                        }
                        // For custom commands, insert with trailing space
                        if matches!(selected.command, Command::Custom { .. }) {
                            self.lines[0] = format!("/{} ", selected.name);
                            self.col_idx = self.lines[0].len();
                            self.slash_menu = None;
                            return InputAction::Redraw;
                        }
                        // For built-in commands, insert and submit
                        self.lines[0] = format!("/{}", selected.name);
                        self.col_idx = self.lines[0].len();
                        self.slash_menu = None;
                    }
                }
                InputAction::Submit
            }

            // Ctrl+C - clear prompt or exit
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if !self.is_empty() {
                    // Text in prompt: clear it
                    self.clear();
                    self.last_ctrl_c_time = None;
                    InputAction::Redraw
                } else {
                    // No text: return ClearOrExit to let mod.rs handle double-tap logic
                    InputAction::ClearOrExit
                }
            }

            // Ctrl+D - quit if empty, otherwise act like Delete
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                if self.is_empty() {
                    InputAction::Quit
                } else {
                    self.delete_forward()
                }
            }

            // Escape - cancel agent loop or close menu
            (KeyCode::Esc, _) => {
                if menu_active {
                    self.slash_menu = None;
                    self.clear(); // Clear the input buffer completely (remove the '/')
                    InputAction::Redraw
                } else if self.completion_active() {
                    // Close file completion
                    self.file_completer.clear();
                    InputAction::Redraw
                } else {
                    InputAction::CancelAgentLoop
                }
            }

            // Tab - apply selected command name, file completion, or regular tab
            (KeyCode::Tab, _) => {
                if menu_active
                    && let Some(selected) =
                        self.slash_menu.as_ref().and_then(|m| m.current().cloned())
                {
                    // Auto-trigger certain commands
                    match selected.command {
                        Command::Model => {
                            self.clear();
                            return InputAction::OpenModelMenu;
                        }
                        Command::Settings => {
                            self.clear();
                            return InputAction::OpenSettingsMenu;
                        }
                        Command::Tools => {
                            self.clear();
                            return InputAction::OpenToolsMenu;
                        }
                        Command::Mcp => {
                            self.clear();
                            return InputAction::OpenMcpMenu;
                        }
                        Command::Lsp => {
                            self.clear();
                            return InputAction::OpenLspMenu;
                        }
                        Command::ReadOnly => {
                            self.clear();
                            return InputAction::TriggerReadOnly;
                        }
                        Command::ReadWrite => {
                            self.clear();
                            return InputAction::TriggerReadWrite;
                        }
                        Command::Clear => {
                            self.clear();
                            return InputAction::TriggerClear;
                        }
                        _ => {
                            // Replace the current input with the selected command
                            //
                            // Match Enter behavior for custom commands: insert a trailing space so
                            // the user can immediately type args.
                            if matches!(selected.command, Command::Custom { .. }) {
                                self.lines[0] = format!("/{} ", selected.name);
                            } else {
                                self.lines[0] = format!("/{}", selected.name);
                            }
                            self.col_idx = self.lines[0].len();
                            self.slash_menu = None;
                        }
                    }
                    InputAction::Redraw
                } else if self.completion_active() {
                    // File completion active - cycle to next match
                    self.move_completion(1);
                    InputAction::Redraw
                } else {
                    // Try to initiate file completion
                    self.init_completion();
                    if self.completion_active() {
                        // If only one match, apply it directly
                        if self.file_completer.matches.len() == 1 {
                            self.apply_completion();
                        }
                        InputAction::Redraw
                    } else {
                        // No completion possible - insert 2 spaces
                        let byte_idx = self.char_to_byte_idx(self.col_idx);
                        self.lines[self.line_idx].insert_str(byte_idx, "  ");
                        self.col_idx += 2;
                        InputAction::RedrawLine
                    }
                }
            }

            // Shift+Tab or BackTab - cycle backwards through file completion
            (KeyCode::BackTab, _) => {
                if self.completion_active() {
                    self.move_completion(-1);
                    InputAction::Redraw
                } else {
                    InputAction::None
                }
            }

            // Up - menu navigation, line navigation, or history
            (KeyCode::Up, _) => {
                if menu_active {
                    if let Some(ref mut menu) = self.slash_menu {
                        menu.move_up();
                    }
                    InputAction::Redraw
                } else if self.line_idx > 0 {
                    self.line_idx -= 1;
                    self.col_idx = self.col_idx.min(self.current_line_char_len());
                    InputAction::MoveCursor
                } else {
                    // At first line - navigate history
                    InputAction::HistoryUp
                }
            }

            // Down - menu navigation, line navigation, or history
            (KeyCode::Down, _) => {
                if menu_active {
                    if let Some(ref mut menu) = self.slash_menu {
                        menu.move_down();
                    }
                    InputAction::Redraw
                } else if self.line_idx < self.total_lines() - 1 {
                    self.line_idx += 1;
                    self.col_idx = self.col_idx.min(self.current_line_char_len());
                    InputAction::MoveCursor
                } else {
                    // At last line - navigate history (if browsing)
                    InputAction::HistoryDown
                }
            }

            // Alt+Backspace, Ctrl+W, Ctrl+Backspace - Delete word backwards (treats image markers as single tokens)
            (KeyCode::Backspace, KeyModifiers::ALT)
            | (KeyCode::Char('w'), KeyModifiers::CONTROL)
            | (KeyCode::Backspace, KeyModifiers::CONTROL) => {
                if self.col_idx > 0 {
                    let line = &self.lines[self.line_idx];
                    let chars: Vec<char> = line.chars().collect();
                    let new_col = find_word_start_backward(&chars, self.col_idx);

                    // Delete the range using byte indices
                    let start_byte = self.char_to_byte_idx(new_col);
                    let end_byte = self.char_to_byte_idx(self.col_idx);
                    self.lines[self.line_idx].replace_range(start_byte..end_byte, "");
                    self.col_idx = new_col;
                    self.update_slash_menu();
                    InputAction::Redraw
                } else if self.line_idx > 0 {
                    // Join with previous line
                    let current_line = self.lines.remove(self.line_idx);
                    self.line_idx -= 1;
                    self.col_idx = self.current_line_char_len();
                    self.lines[self.line_idx].push_str(&current_line);
                    self.update_slash_menu();
                    InputAction::Redraw
                } else {
                    InputAction::None
                }
            }

            // Backspace
            (KeyCode::Backspace, _) => {
                if self.col_idx > 0 {
                    // Get byte range for the character before cursor
                    let prev_char_start = self.char_to_byte_idx(self.col_idx - 1);
                    let curr_char_start = self.char_to_byte_idx(self.col_idx);
                    self.lines[self.line_idx].replace_range(prev_char_start..curr_char_start, "");
                    self.col_idx -= 1;
                    self.update_slash_menu();
                    InputAction::Redraw
                } else if self.line_idx > 0 {
                    let current_line = self.lines.remove(self.line_idx);
                    self.line_idx -= 1;
                    self.col_idx = self.current_line_char_len();
                    self.lines[self.line_idx].push_str(&current_line);
                    self.update_slash_menu();
                    InputAction::Redraw
                } else {
                    InputAction::None
                }
            }

            // Delete
            (KeyCode::Delete, _) => self.delete_forward(),

            // Left arrow
            (KeyCode::Left, _) => {
                if self.col_idx > 0 {
                    self.col_idx -= 1;
                    InputAction::MoveCursor
                } else if self.line_idx > 0 {
                    self.line_idx -= 1;
                    self.col_idx = self.current_line_char_len();
                    InputAction::MoveCursor
                } else {
                    InputAction::None
                }
            }

            // Right arrow
            (KeyCode::Right, _) => {
                if self.col_idx < self.current_line_char_len() {
                    self.col_idx += 1;
                    InputAction::MoveCursor
                } else if self.line_idx < self.total_lines() - 1 {
                    self.line_idx += 1;
                    self.col_idx = 0;
                    InputAction::MoveCursor
                } else {
                    InputAction::None
                }
            }

            // Home / Ctrl+A - context-dependent beginning navigation
            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                if self.col_idx == 0 {
                    // Already at beginning of line - go to beginning of prompt
                    self.line_idx = 0;
                }
                self.col_idx = 0;
                InputAction::MoveCursor
            }

            // End / Ctrl+E - context-dependent end navigation
            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                let line_len = self.current_line_char_len();
                if self.col_idx == line_len {
                    // Already at end of line - go to end of prompt
                    self.line_idx = self.lines.len() - 1;
                    self.col_idx = self.line_char_len(self.line_idx);
                } else {
                    self.col_idx = line_len;
                }
                InputAction::MoveCursor
            }

            // Ctrl+K - kill to end of line (or delete empty line)
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                let byte_idx = self.char_to_byte_idx(self.col_idx);
                if byte_idx == 0 && self.lines[self.line_idx].is_empty() && self.lines.len() > 1 {
                    // Line is empty and there are other lines - delete this line entirely
                    self.lines.remove(self.line_idx);
                    // Adjust line_idx if we were at the last line
                    if self.line_idx >= self.lines.len() {
                        self.line_idx = self.lines.len() - 1;
                    }
                    // col_idx stays at 0
                    self.update_slash_menu();
                    InputAction::Redraw
                } else {
                    // Normal behavior: truncate from cursor to end of line
                    self.lines[self.line_idx].truncate(byte_idx);
                    self.update_slash_menu();
                    InputAction::Redraw
                }
            }

            // Ctrl+U - kill to start of line
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                let byte_idx = self.char_to_byte_idx(self.col_idx);
                let remaining = self.lines[self.line_idx][byte_idx..].to_string();
                self.lines[self.line_idx] = remaining;
                self.col_idx = 0;
                self.update_slash_menu();
                InputAction::Redraw
            }

            // Ctrl+I (Tab with enhancement)
            (KeyCode::Char('i'), KeyModifiers::CONTROL) => {
                // Delegate to Tab logic
                self.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            }

            // Ctrl+V - paste from clipboard
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => InputAction::ClipboardPaste,

            // Ctrl+R - history search with fzf
            (KeyCode::Char('r'), KeyModifiers::CONTROL) => InputAction::HistorySearch,

            // Ctrl+G - edit prompt in editor
            (KeyCode::Char('g'), KeyModifiers::CONTROL) => InputAction::EditInEditor,

            // Ctrl+P - open model menu (fallback for Ctrl+M on older terminals)
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => InputAction::OpenModelMenu,

            // Ctrl+Y - Cycle forward through favorite models
            (KeyCode::Char('y'), mods) if mods == KeyModifiers::CONTROL => {
                InputAction::CycleFavoritesForward
            }

            // Shift+Ctrl+Y - Cycle backward through favorite models
            (KeyCode::Char('y'), mods)
                if mods.contains(KeyModifiers::CONTROL) && mods.contains(KeyModifiers::SHIFT) =>
            {
                InputAction::CycleFavoritesBackward
            }
            (KeyCode::Char('Y'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                // Shift+Ctrl+Y may come as capital 'Y' with CONTROL modifier
                InputAction::CycleFavoritesBackward
            }

            // Ctrl+H - Toggle hide tool output
            (KeyCode::Char('h'), KeyModifiers::CONTROL) => InputAction::ToggleHideToolOutput,

            // Ctrl+O - Toggle expanded tool output view (10 lines <-> full output)
            (KeyCode::Char('o'), KeyModifiers::CONTROL) => InputAction::ToggleToolOutputExpanded,

            // Ctrl+L - force a full UI redraw
            (KeyCode::Char('l'), KeyModifiers::CONTROL) => InputAction::RedrawAll,
            (KeyCode::Char('L'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                // Shift+Ctrl+L may come as capital 'L' with CONTROL modifier
                InputAction::RedrawAll
            }

            // Alt+B - Backward word (treats image markers as single tokens)
            (KeyCode::Char('b'), KeyModifiers::ALT) => {
                if self.col_idx > 0 {
                    let line = &self.lines[self.line_idx];
                    let chars: Vec<char> = line.chars().collect();
                    self.col_idx = find_word_start_backward(&chars, self.col_idx);
                    InputAction::MoveCursor
                } else if self.line_idx > 0 {
                    // Wrap to end of previous line
                    self.line_idx -= 1;
                    self.col_idx = self.current_line_char_len();
                    InputAction::MoveCursor
                } else {
                    InputAction::None
                }
            }

            // Alt+F - Forward word (treats image markers as single tokens)
            (KeyCode::Char('f'), KeyModifiers::ALT) => {
                let line_char_len = self.current_line_char_len();
                if self.col_idx < line_char_len {
                    let chars: Vec<char> = self.lines[self.line_idx].chars().collect();
                    self.col_idx = find_word_end_forward(&chars, self.col_idx);
                    InputAction::MoveCursor
                } else if self.line_idx < self.lines.len() - 1 {
                    // Wrap to beginning of next line
                    self.line_idx += 1;
                    self.col_idx = 0;
                    InputAction::MoveCursor
                } else {
                    InputAction::None
                }
            }

            // Alt+D - Delete word forward (treats image markers as single tokens)
            (KeyCode::Char('d'), KeyModifiers::ALT) => {
                let line_char_len = self.current_line_char_len();
                if self.col_idx < line_char_len {
                    let chars: Vec<char> = self.lines[self.line_idx].chars().collect();
                    let end_col = find_word_end_forward(&chars, self.col_idx);

                    // Delete the range using byte indices
                    let start_byte = self.char_to_byte_idx(self.col_idx);
                    let end_byte = self.char_to_byte_idx(end_col);
                    self.lines[self.line_idx].replace_range(start_byte..end_byte, "");
                    self.update_slash_menu();
                    InputAction::Redraw
                } else if self.line_idx < self.lines.len() - 1 {
                    // Join with next line
                    let next_line = self.lines.remove(self.line_idx + 1);
                    self.lines[self.line_idx].push_str(&next_line);
                    self.update_slash_menu();
                    InputAction::Redraw
                } else {
                    InputAction::None
                }
            }

            // Character input
            (KeyCode::Char(c), modifier)
                if !modifier.contains(KeyModifiers::CONTROL)
                    && !modifier.contains(KeyModifiers::ALT) =>
            {
                let byte_idx = self.char_to_byte_idx(self.col_idx);
                self.lines[self.line_idx].insert(byte_idx, c);
                self.col_idx += 1;
                self.update_slash_menu();

                // Need full redraw if menu is active, otherwise just line
                if self.slash_menu.is_some() {
                    InputAction::Redraw
                } else if self.col_idx == self.current_line_char_len() {
                    // Character appended at end - can optimize with just printing
                    InputAction::RedrawLine
                } else {
                    InputAction::RedrawLine
                }
            }

            _ => InputAction::None,
        }
    }

    /// Clear the input state for a fresh prompt
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.line_idx = 0;
        self.col_idx = 0;
        self.slash_menu = None;
        self.history_index = None;
        self.draft = vec![String::new()];
        self.pasted_images.clear();
        self.next_image_id = 1;
        self.file_completer.clear();
    }

    pub fn can_edit_pending_prompt(&self) -> bool {
        !self.slash_menu_active() && !self.completion_active() && self.is_empty()
    }

    pub fn can_delete_pending_prompt(&self) -> bool {
        !self.slash_menu_active() && !self.completion_active() && self.is_empty()
    }

    /// Set the content of the input from a string (e.g., from history search)
    pub fn set_content(&mut self, content: &str) {
        self.lines = content.split('\n').map(|s| s.to_string()).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.line_idx = self.lines.len() - 1;
        self.col_idx = self.current_line_char_len();
        self.history_index = None;
        self.update_slash_menu();
    }

    pub fn set_content_with_images(&mut self, content: &str, images: Vec<PastedImage>) {
        self.set_content(content);
        self.pasted_images = images;
        self.next_image_id = next_image_id_for_content(&self.content(), &self.pasted_images);
    }

    pub fn prune_unused_images(&mut self) {
        let content = self.content();
        self.pasted_images
            .retain(|img| content.contains(&img.marker));
    }

    /// Navigate up through history (older entries)
    pub fn apply_history_up(&mut self, history: &crate::history::FileHistory) {
        if history.is_empty() {
            return;
        }

        // Save current input as draft if entering history mode
        if self.history_index.is_none() {
            self.draft = self.lines.clone();
        }

        let mut next_idx = match self.history_index {
            None => Some(history.len().saturating_sub(1)),
            Some(0) => Some(0),
            Some(i) => Some(i.saturating_sub(1)),
        };

        // Skip entries that are built-in slash commands
        while let Some(idx) = next_idx {
            if let Some(entry) = history.get(idx) {
                if !should_include_in_history(entry) {
                    next_idx = if idx == 0 {
                        None
                    } else {
                        Some(idx.saturating_sub(1))
                    };
                } else {
                    break;
                }
            } else {
                next_idx = None;
            }
        }

        if let Some(idx) = next_idx
            && let Some(entry) = history.get(idx)
        {
            self.lines = entry.lines().map(String::from).collect();
            if self.lines.is_empty() {
                self.lines = vec![String::new()];
            }
            self.line_idx = self.lines.len() - 1;
            self.col_idx = self.line_char_len(self.line_idx);
            self.history_index = Some(idx);
        }
    }

    /// Navigate down through history (newer entries)
    pub fn apply_history_down(&mut self, history: &crate::history::FileHistory) {
        if history.is_empty() {
            return;
        }

        if let Some(idx) = self.history_index {
            let mut next = idx + 1;

            // Skip entries that are built-in slash commands
            while next < history.len() {
                if let Some(entry) = history.get(next) {
                    if !should_include_in_history(entry) {
                        next += 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }

            if next >= history.len() {
                // Reached end of history, restore draft
                self.lines = std::mem::take(&mut self.draft);
                if self.lines.is_empty() {
                    self.lines = vec![String::new()];
                }
                self.line_idx = self.lines.len() - 1;
                self.col_idx = self.line_char_len(self.line_idx);
                self.history_index = None;
            } else if let Some(entry) = history.get(next) {
                self.lines = entry.lines().map(String::from).collect();
                if self.lines.is_empty() {
                    self.lines = vec![String::new()];
                }
                self.line_idx = self.lines.len() - 1;
                self.col_idx = self.line_char_len(self.line_idx);
                self.history_index = Some(next);
            }
        }
    }

    /// Insert a string at the current cursor position.
    /// Handles newlines by splitting into multiple lines.
    pub fn insert_str(&mut self, s: &str) {
        // Split the input string by newlines
        let parts: Vec<&str> = s.split('\n').collect();

        if parts.is_empty() {
            return;
        }

        // Insert the first part at the current cursor position
        let byte_idx = self.char_to_byte_idx(self.col_idx);
        let first_part = parts[0];

        if parts.len() == 1 {
            // No newlines - simple insert
            self.lines[self.line_idx].insert_str(byte_idx, first_part);
            self.col_idx += first_part.chars().count();
        } else {
            // Has newlines - need to split and create new lines
            // Get the text after the cursor on the current line
            let after_cursor = self.lines[self.line_idx][byte_idx..].to_string();

            // Truncate current line and append first part
            self.lines[self.line_idx].truncate(byte_idx);
            self.lines[self.line_idx].push_str(first_part);

            // Insert middle parts as new lines
            for (i, part) in parts.iter().enumerate().skip(1) {
                if i == parts.len() - 1 {
                    // Last part - append the text that was after the cursor
                    let mut new_line = part.to_string();
                    new_line.push_str(&after_cursor);
                    self.lines.insert(self.line_idx + i, new_line);
                } else {
                    // Middle part - just a new line
                    self.lines.insert(self.line_idx + i, part.to_string());
                }
            }

            // Update cursor position to end of last inserted part
            self.line_idx += parts.len() - 1;
            self.col_idx = parts[parts.len() - 1].chars().count();
        }

        self.update_slash_menu();
    }

    /// Add a pasted image to the input state.
    /// Inserts a marker into the text and stores the image data.
    /// The marker format is "Image#N" (e.g., "Image#1") and word-movement
    /// commands like Alt+B, Alt+F, Ctrl+W treat it as a single token.
    pub fn add_pasted_image(&mut self, mime_type: String, data: Vec<u8>) {
        let marker_id = self.next_image_id;
        self.next_image_id += 1;

        let marker = format!("{}{}", IMAGE_MARKER_PREFIX, marker_id);

        self.insert_str(&marker);
        self.pasted_images.push(PastedImage {
            marker,
            mime_type,
            data,
        });
    }

    /// Get images that are still referenced in the current input.
    /// Images whose markers have been deleted are not included.
    pub fn active_images(&mut self) -> Vec<PastedImage> {
        let content = self.content();
        let mut images = std::mem::take(&mut self.pasted_images);
        images.retain(|img| content.contains(&img.marker));
        images
    }
}

/// Check if an entry should be included in history navigation.
/// Returns true for regular input and custom slash commands, false for built-in commands.
fn should_include_in_history(content: &str) -> bool {
    if let Some(cmd_str) = content.trim_start().strip_prefix('/') {
        // It's a slash command - check if it's a built-in command
        let cmd_name = cmd_str.split_whitespace().next().unwrap_or(cmd_str);
        // Built-in commands that should NOT be in history
        !matches!(
            cmd_name,
            "quit"
                | "q"
                | "clear"
                | "help"
                | "model"
                | "undo"
                | "forget"
                | "truncate"
                | "read-only"
                | "read-write"
                | "yolo"
                | "tools"
                | "mcp"
                | "compact"
                | "settings"
        )
    } else {
        // Not a slash command, include in history
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::DynamicSlashCommand;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// Create a test InputState with a temporary directory
    fn test_state() -> InputState {
        InputState::new(PathBuf::from("/tmp"))
    }

    #[test]
    fn test_tab_completes_custom_slash_command_with_space() {
        let mut state = test_state();
        state.lines[0] = "/rev".to_string();
        state.col_idx = state.lines[0].len();
        state.update_slash_menu();

        let mut menu = SlashMenuState::new(false);
        menu.items = vec![DynamicSlashCommand {
            command: Command::Custom {
                name: "review".to_string(),
                args: String::new(),
            },
            name: "review".to_string(),
            description: "Review".to_string(),
        }];
        menu.selected = 0;
        state.slash_menu = Some(menu);

        state.handle_key(key(KeyCode::Tab, KeyModifiers::NONE));

        assert_eq!(state.lines[0], "/review ");
        assert_eq!(state.col_idx, "/review ".len());
        assert!(state.slash_menu.is_none());
    }

    #[test]
    fn test_shift_enter() {
        let mut state = test_state();
        state.lines[0] = "line1".to_string();
        state.col_idx = 5;

        // Shift+Enter
        state.handle_key(key(KeyCode::Enter, KeyModifiers::SHIFT));

        assert_eq!(state.lines.len(), 2);
        assert_eq!(state.lines[0], "line1");
        assert_eq!(state.lines[1], "");
        assert_eq!(state.line_idx, 1);
        assert_eq!(state.col_idx, 0);
    }

    #[test]
    fn test_alt_b_basic() {
        let mut state = test_state();
        state.lines[0] = "hello world".to_string();
        state.col_idx = 11; // End

        // Alt+B -> "hello |world"
        state.handle_key(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(state.col_idx, 6); // start of "world"

        // Alt+B -> "|hello world"
        state.handle_key(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(state.col_idx, 0); // start of "hello"
    }

    #[test]
    fn test_alt_b_punctuation() {
        let mut state = test_state();
        state.lines[0] = "a.b".to_string();
        state.col_idx = 3; // End

        // Alt+B -> "a.|b" (skip 'b')
        state.handle_key(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(state.col_idx, 2);

        // Alt+B -> "|a.b"
        state.handle_key(key(KeyCode::Char('b'), KeyModifiers::ALT));
        assert_eq!(state.col_idx, 0);
    }

    #[test]
    fn test_alt_d_basic() {
        let mut state = test_state();
        state.lines[0] = "hello world".to_string();
        state.col_idx = 0;

        // Alt+D -> "| world"
        state.handle_key(key(KeyCode::Char('d'), KeyModifiers::ALT));
        assert_eq!(state.lines[0], " world");
        assert_eq!(state.col_idx, 0);

        // Alt+D -> "|"
        state.handle_key(key(KeyCode::Char('d'), KeyModifiers::ALT));
        assert_eq!(state.lines[0], "");
    }

    #[test]
    fn test_multibyte_char_insert() {
        let mut state = test_state();
        state.lines[0] = "héllo".to_string(); // 'é' is 2 bytes
        state.col_idx = 1; // After 'h', before 'é'

        // Insert a character at position 1
        state.handle_key(key(KeyCode::Char('X'), KeyModifiers::NONE));
        assert_eq!(state.lines[0], "hXéllo");
        assert_eq!(state.col_idx, 2);
    }

    #[test]
    fn test_multibyte_char_backspace() {
        let mut state = test_state();
        state.lines[0] = "héllo".to_string(); // 'é' is 2 bytes
        state.col_idx = 2; // After 'h' and 'é'

        // Backspace should remove the 'é' (2-byte char)
        state.handle_key(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(state.lines[0], "hllo");
        assert_eq!(state.col_idx, 1);
    }

    #[test]
    fn test_multibyte_char_delete() {
        let mut state = test_state();
        state.lines[0] = "héllo".to_string(); // 'é' is 2 bytes
        state.col_idx = 1; // After 'h', cursor on 'é'

        // Delete should remove the 'é' (2-byte char)
        state.handle_key(key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(state.lines[0], "hllo");
        assert_eq!(state.col_idx, 1);
    }

    #[test]
    fn test_ctrl_d_quits_if_empty() {
        let mut state = test_state();

        let action = state.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(matches!(action, InputAction::Quit));
    }

    #[test]
    fn test_ctrl_d_deletes_char_at_cursor() {
        let mut state = test_state();
        state.lines[0] = "abc".to_string();
        state.col_idx = 1;

        let action = state.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(state.lines[0], "ac");
        assert_eq!(state.col_idx, 1);
        assert!(matches!(action, InputAction::Redraw));
    }

    #[test]
    fn test_ctrl_d_joins_with_next_line_at_eol() {
        let mut state = test_state();
        state.lines = vec!["abc".to_string(), "def".to_string()];
        state.line_idx = 0;
        state.col_idx = 3;

        let action = state.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.lines[0], "abcdef");
        assert_eq!(state.line_idx, 0);
        assert_eq!(state.col_idx, 3);
        assert!(matches!(action, InputAction::Redraw));
    }

    #[test]
    fn test_ctrl_d_at_end_of_last_line_is_noop() {
        let mut state = test_state();
        state.lines[0] = "abc".to_string();
        state.col_idx = 3;

        let action = state.handle_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(state.lines[0], "abc");
        assert_eq!(state.col_idx, 3);
        assert!(matches!(action, InputAction::None));
    }

    #[test]
    fn test_multibyte_char_navigation() {
        let mut state = test_state();
        state.lines[0] = "aé中b".to_string(); // 'é' is 2 bytes, '中' is 3 bytes
        state.col_idx = 0;

        // Move right through each character
        state.handle_key(key(KeyCode::Right, KeyModifiers::NONE)); // past 'a'
        assert_eq!(state.col_idx, 1);
        state.handle_key(key(KeyCode::Right, KeyModifiers::NONE)); // past 'é'
        assert_eq!(state.col_idx, 2);
        state.handle_key(key(KeyCode::Right, KeyModifiers::NONE)); // past '中'
        assert_eq!(state.col_idx, 3);
        state.handle_key(key(KeyCode::Right, KeyModifiers::NONE)); // past 'b'
        assert_eq!(state.col_idx, 4);

        // Move left back
        state.handle_key(key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(state.col_idx, 3);
        state.handle_key(key(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(state.col_idx, 2);
    }

    #[test]
    fn test_multibyte_insert_str() {
        let mut state = test_state();
        state.lines[0] = "aéb".to_string();
        state.col_idx = 2; // After 'é', before 'b'

        state.insert_str("中文");
        assert_eq!(state.lines[0], "aé中文b");
        assert_eq!(state.col_idx, 4); // advanced by 2 characters
    }

    #[test]
    fn test_multiline_paste() {
        let mut state = test_state();
        state.lines[0] = "start".to_string();
        state.col_idx = 5; // At end

        // Paste multiline text
        state.insert_str("line1\nline2\nline3");

        assert_eq!(state.lines.len(), 3);
        assert_eq!(state.lines[0], "startline1");
        assert_eq!(state.lines[1], "line2");
        assert_eq!(state.lines[2], "line3");
        assert_eq!(state.line_idx, 2);
        assert_eq!(state.col_idx, 5); // "line3".len()
    }

    #[test]
    fn test_multiline_paste_mid_line() {
        let mut state = test_state();
        state.lines[0] = "hello world".to_string();
        state.col_idx = 5; // After "hello", before " world"

        // Paste multiline text in the middle
        state.insert_str("A\nB");

        assert_eq!(state.lines.len(), 2);
        assert_eq!(state.lines[0], "helloA");
        assert_eq!(state.lines[1], "B world"); // " world" should follow "B"
        assert_eq!(state.line_idx, 1);
        assert_eq!(state.col_idx, 1); // After "B"
    }
}

#[cfg(test)]
mod wrap_tests {
    use super::*;

    fn test_state() -> InputState {
        InputState::new(PathBuf::from("/tmp"))
    }

    #[test]
    fn test_wrap_line_basic() {
        // 20 char width, "hello there beautiful" should wrap before "beautiful"
        let result = wrap_line_with_cursor("hello there beautiful", 20, None);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "hello there ");
        assert_eq!(result[1].0, "beautiful");
    }

    #[test]
    fn test_wrap_line_at_boundary() {
        // Width = 10, text exactly 10 chars should not wrap
        let result = wrap_line_with_cursor("0123456789", 10, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "0123456789");
    }

    #[test]
    fn test_wrap_line_long_word() {
        // Width = 5, word longer than width should be split
        let result = wrap_line_with_cursor("abcdefghij", 5, None);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "abcde");
        assert_eq!(result[1].0, "fghij");
    }

    #[test]
    fn test_wrap_line_exceeds_edge() {
        // Simulate typing past the right edge
        let content_width = 78;

        // Build a string that goes PAST the edge (79 chars)
        let text =
            "This is a test of word wrapping functionality at the rightmost column boundary!";

        let result = wrap_line_with_cursor(text, content_width, None);

        // "boundary!" (9 chars) should wrap to next line
        assert_eq!(result.len(), 2);
        // First row should fit within content_width
        assert!(
            display_width(&result[0].0) <= content_width,
            "First row display width {} exceeds {}",
            display_width(&result[0].0),
            content_width
        );

        // Continuation row should not start with whitespace.
        assert!(!result[1].0.starts_with(' '));
    }

    #[test]
    fn test_current_line_needs_wrapping() {
        let mut state = test_state();

        // Short line should not need wrapping (80 char terminal, 0 char prefix = 80 content)
        state.lines[0] = "short".to_string();
        assert!(!state.current_line_needs_wrapping(80));

        // Long line should need wrapping
        state.lines[0] = "a".repeat(160);
        assert!(state.current_line_needs_wrapping(80));

        // Exactly at boundary should not need wrapping
        state.lines[0] = "a".repeat(80);
        assert!(!state.current_line_needs_wrapping(80));

        // One over boundary should need wrapping
        state.lines[0] = "a".repeat(81);
        assert!(state.current_line_needs_wrapping(80));
    }
}
