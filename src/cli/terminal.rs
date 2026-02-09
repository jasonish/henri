// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Terminal management for CLI mode.
//!
//! Provides output functions that coordinate with a persistent prompt area
//! at the bottom of the terminal.

use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crossterm::cursor::{Hide, MoveTo, MoveToNextLine, Show};
use crossterm::execute;
use crossterm::terminal::{self, Clear, ClearType, ScrollDown, ScrollUp, SetTitle};

/// Global state: whether prompt box is visible and where it is
static PROMPT_STATE: Mutex<PromptState> = Mutex::new(PromptState::new());

const STREAMING_STATUS_LINE_ROWS: u16 = 2;

struct PromptState {
    visible: bool,
    height: u16,
    /// Row where the prompt starts (0-indexed)
    start_row: u16,
    status_row_offset: u16,
    /// Cursor position relative to prompt (row_offset, col)
    cursor_pos: Option<(u16, u16)>,
    /// Whether the streaming status line area (above prompt) is active/reserved
    status_line_active: bool,
}

pub(super) fn prompt_position() -> Option<(u16, u16)> {
    let state = PROMPT_STATE.lock().ok()?;
    if !state.visible {
        return None;
    }
    Some((state.start_row, state.height))
}

impl PromptState {
    const fn new() -> Self {
        Self {
            visible: false,
            height: 3,
            start_row: 0,
            status_row_offset: 2,
            cursor_pos: None,
            status_line_active: false,
        }
    }
}

struct OutputCursor {
    col: u16,
    /// Number of consecutive trailing `\n` characters in the *visible* output stream.
    ///
    /// This ignores ANSI escape sequences, and is used for spacing decisions (e.g., ensuring a
    /// single blank row between logical blocks without adding extra blank lines).
    trailing_newlines: u8,
    /// Whether we've ever printed output above the prompt.
    /// Used to scroll the terminal on first output to avoid overwriting previous content.
    has_output: bool,
}

impl OutputCursor {
    const fn new() -> Self {
        Self {
            col: 0,
            trailing_newlines: 0,
            has_output: false,
        }
    }
}

static OUTPUT_CURSOR: Mutex<OutputCursor> = Mutex::new(OutputCursor::new());
static OUTPUT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static OUTPUT_BUFFERING: AtomicBool = AtomicBool::new(false);

fn output_lock() -> &'static Mutex<()> {
    OUTPUT_LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) fn lock_output() -> MutexGuard<'static, ()> {
    output_lock().lock().unwrap()
}

pub(crate) struct OutputBufferGuard;

pub(crate) fn buffer_output() -> OutputBufferGuard {
    OUTPUT_BUFFERING.store(true, Ordering::SeqCst);
    OutputBufferGuard
}

impl Drop for OutputBufferGuard {
    fn drop(&mut self) {
        OUTPUT_BUFFERING.store(false, Ordering::SeqCst);
    }
}

pub(crate) fn is_output_buffering() -> bool {
    OUTPUT_BUFFERING.load(Ordering::SeqCst)
}

fn reset_output_cursor() {
    if let Ok(mut state) = OUTPUT_CURSOR.lock() {
        state.col = 0;
        state.trailing_newlines = 0;
        state.has_output = false;
    }
}

fn set_output_cursor(col: u16, trailing_newlines: u8) {
    if let Ok(mut state) = OUTPUT_CURSOR.lock() {
        state.col = col;
        state.trailing_newlines = trailing_newlines;
        state.has_output = true;
    }
}

pub(crate) fn output_cursor_col() -> u16 {
    OUTPUT_CURSOR.lock().map(|state| state.col).unwrap_or(0)
}

pub(crate) fn output_trailing_newlines() -> u8 {
    OUTPUT_CURSOR
        .lock()
        .map(|state| state.trailing_newlines)
        .unwrap_or(0)
}

pub(crate) fn output_has_output() -> bool {
    OUTPUT_CURSOR
        .lock()
        .map(|state| state.has_output)
        .unwrap_or(false)
}

fn trailing_newlines_with_implicit_wrap(col: u16, trailing_newlines: u8, has_output: bool) -> u8 {
    if trailing_newlines == 0 && col == 0 && has_output {
        // If the cursor is already at the start of a line but the last chunk did not end with an
        // explicit newline, we may have landed here due to terminal auto-wrapping (exact-width
        // output). Treat that as an implicit line break so we don't add an extra blank line.
        1
    } else {
        trailing_newlines
    }
}

fn newlines_needed(min: u8, col: u16, trailing_newlines: u8, has_output: bool) -> u8 {
    let current = trailing_newlines_with_implicit_wrap(col, trailing_newlines, has_output);
    min.saturating_sub(current)
}

/// Ensure output ends with at least `min` trailing newlines.
///
/// This is used to insert spacing *reactively* when starting a new logical block.
pub(crate) fn ensure_trailing_newlines(min: u8) {
    let _guard = lock_output();

    let needed = newlines_needed(
        min,
        output_cursor_col(),
        output_trailing_newlines(),
        output_has_output(),
    );
    match needed {
        0 => {}
        1 => print_above_locked("\n"),
        2 => print_above_locked("\n\n"),
        _ => {
            let s = "\n".repeat(needed as usize);
            print_above_locked(&s);
        }
    }
}

/// Notify the terminal manager that the prompt box is now visible.
/// `start_row` is the 0-indexed row where the prompt begins.
pub(super) fn set_prompt_visible(height: u16, start_row: u16, status_row_offset: u16) {
    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.visible = true;
        state.height = height;
        state.start_row = start_row;
        state.status_row_offset = if height == 0 {
            0
        } else {
            status_row_offset.min(height.saturating_sub(1))
        };
    }
}

/// Update the cursor position relative to the prompt start.
pub(super) fn set_prompt_cursor(row_offset: u16, col: u16) {
    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.cursor_pos = Some((row_offset, col));
    }
}

/// Clear the tracked cursor position (e.g. when cursor should be hidden).
pub(super) fn clear_prompt_cursor() {
    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.cursor_pos = None;
    }
}

/// Notify the terminal manager that the prompt box is hidden.
pub(super) fn set_prompt_hidden() {
    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.visible = false;
    }
}

/// Reset the tracked output cursor state.
///
/// Use this when external UI clears or otherwise disrupts the terminal output area
/// (e.g. `hide_and_clear` / `hide_and_exit`).
pub(super) fn reset_output_cursor_tracking() {
    reset_output_cursor();
}

/// Set whether the streaming status line area (above prompt) is active/reserved.
///
/// This area is drawn on the rows immediately above the prompt:
/// - (top) a blank spacer row
/// - (bottom) the actual status line
///
/// If enabling the status line while the prompt is visible, we try to make room
/// for it without overwriting existing output by shifting the prompt down when
/// possible.
pub(super) fn set_streaming_status_line_active(active: bool) {
    let _guard = lock_output();

    let (was_active, visible, start_row, height, cursor_pos) = {
        let state = PROMPT_STATE.lock().unwrap();
        (
            state.status_line_active,
            state.visible,
            state.start_row,
            state.height,
            state.cursor_pos,
        )
    };

    if active && !was_active && visible {
        let reserve_rows = STREAMING_STATUS_LINE_ROWS;

        let mut stdout = io::stdout();
        let _ = execute!(stdout, crossterm::cursor::SavePosition, Hide);

        let term_height = term_height();

        // Prefer to shift the prompt DOWN by `reserve_rows` lines (if there is room) to make space
        // for the status lines. This avoids wiping out existing terminal output when the
        // prompt is near the top.
        // Layout: <output> / <spacer> / <status line> / <prompt>
        let can_shift_down = start_row
            .saturating_add(height)
            .saturating_add(reserve_rows)
            <= term_height;
        if can_shift_down {
            let _ = execute!(stdout, MoveTo(0, start_row));
            // Insert blank lines at the prompt start, pushing the prompt down.
            print!("\x1b[{}L", reserve_rows);
            let _ = stdout.flush();

            if let Ok(mut state) = PROMPT_STATE.lock() {
                state.start_row = state.start_row.saturating_add(reserve_rows);
            }
        } else if start_row > 0 {
            // Fallback: scroll the output area up to clear the status line rows.
            // This is used when the prompt is already at the bottom and cannot move down.
            print!("\x1b[{};{}r", 1, start_row);
            let _ = execute!(stdout, MoveTo(0, start_row.saturating_sub(1)));
            let _ = execute!(stdout, ScrollUp(reserve_rows));
            print!("\x1b[r");
            let _ = stdout.flush();
        }

        // Restore cursor to prompt.
        let new_start_row = PROMPT_STATE
            .lock()
            .map(|s| s.start_row)
            .unwrap_or(start_row);
        if let Some((off, col)) = cursor_pos {
            let _ = execute!(stdout, MoveTo(col, new_start_row + off), Show);
        } else {
            let _ = execute!(stdout, Show);
        }
    }

    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.status_line_active = active;
    }
}

pub(super) fn streaming_status_line_reserved_rows() -> u16 {
    PROMPT_STATE
        .lock()
        .map(|s| {
            if s.status_line_active {
                STREAMING_STATUS_LINE_ROWS
            } else {
                0
            }
        })
        .unwrap_or(0)
}

/// Ensure the cursor is on a fresh line before drawing the initial prompt.
///
/// Some shells/terminal configurations may leave the cursor at the end of the
/// command line that launched henri. Moving to the next line avoids overwriting
/// that content.
pub(crate) fn ensure_cursor_on_new_line() {
    let _guard = lock_output();

    let Ok((col, _row)) = crossterm::cursor::position() else {
        return;
    };

    if col == 0 {
        return;
    }

    let mut stdout = io::stdout();
    let _ = execute!(stdout, MoveToNextLine(1));
    let _ = stdout.flush();
}

pub(crate) fn update_terminal_title(title: &str) {
    if !io::stdout().is_terminal() {
        return;
    }

    let _guard = lock_output();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, SetTitle(title));
    let _ = stdout.flush();
}

/// Write text to the status line (if active and visible).
///
/// This function uses `try_lock` to avoid blocking on the output lock, allowing
/// the spinner to update even while tool output is being rendered. If the lock
/// is held, the status line update is skipped (it will be refreshed on the next
/// spinner tick).
pub(crate) fn write_status_line(text: &str) {
    use crossterm::SynchronizedUpdate;

    // Use try_lock to avoid blocking the spinner when output is being rendered.
    // The spinner ticks every 100ms, so missing one update is acceptable.
    let Ok(_guard) = output_lock().try_lock() else {
        return;
    };
    let state = PROMPT_STATE.lock().unwrap();

    let reserved_rows = if state.status_line_active {
        STREAMING_STATUS_LINE_ROWS
    } else {
        0
    };

    if !state.visible || reserved_rows == 0 || state.start_row < reserved_rows {
        return;
    }

    // Layout: <output> / <spacer> / <status line> / <prompt>
    let spacer_row = state.start_row - 2;
    let status_row = state.start_row - 1;

    let cursor_info = state
        .cursor_pos
        .map(|(off, col)| (state.start_row + off, col));
    drop(state); // Release lock before I/O

    let mut stdout = io::stdout();

    // Use synchronized update to prevent flicker during status line writes
    let _ = stdout.sync_update(|stdout| {
        use crossterm::queue;
        use std::io::Write;

        queue!(stdout, Hide)?;

        // Keep the spacer row truly blank.
        queue!(stdout, MoveTo(0, spacer_row), Clear(ClearType::CurrentLine))?;

        queue!(stdout, MoveTo(0, status_row), Clear(ClearType::CurrentLine))?;
        write!(stdout, "{}", text)?;

        // Restore cursor only if we have a saved position (menu modes hide the cursor)
        if let Some((cursor_row, cursor_col)) = cursor_info {
            queue!(stdout, MoveTo(cursor_col, cursor_row))?;
        }

        io::Result::Ok(())
    });
}

/// Get whether the prompt is visible.
fn is_prompt_visible() -> bool {
    PROMPT_STATE.lock().map(|s| s.visible).unwrap_or(false)
}

pub(crate) fn prompt_visible() -> bool {
    is_prompt_visible()
}

fn calculate_output_size(mut col: u16, text: &str, width: u16) -> (u16, u16) {
    let mut lines: u16 = 0;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek().copied() {
                // CSI: ESC [ ... <final byte>
                Some('[') => {
                    chars.next();
                    for code in chars.by_ref() {
                        if ('@'..='~').contains(&code) {
                            break;
                        }
                    }
                }
                // OSC / APC / DCS: ESC ] / ESC _ / ESC P ... terminated by BEL or ST (ESC \\)
                Some(']') | Some('_') | Some('P') | Some('^') => {
                    let introducer = chars.next().unwrap_or('\0');
                    let mut prev_esc = false;
                    for code in chars.by_ref() {
                        if introducer == ']' && code == '\x07' {
                            break;
                        }
                        if prev_esc && code == '\\' {
                            break;
                        }
                        prev_esc = code == '\x1b';
                    }
                }
                _ => {}
            }
            continue;
        }

        match ch {
            '\n' => {
                lines = lines.saturating_add(1);
                col = 0;
            }
            '\r' => {
                col = 0;
            }
            _ => {
                if col >= width {
                    lines = lines.saturating_add(1);
                    col = 0;
                }
                col = col.saturating_add(1);
            }
        }
    }
    (lines, col)
}

/// Ensure output is terminated by a line break.
///
/// This intentionally does *not* add extra blank lines; it only inserts a `\n`
/// if the output cursor is currently mid-line.
pub(crate) fn ensure_line_break() {
    let _guard = lock_output();

    if output_cursor_col() != 0 {
        print_above_locked("\n");
    }
}

fn visible_trailing_newlines(text: &str) -> Option<u8> {
    let mut chars = text.chars().peekable();
    let mut saw_visible = false;
    let mut trailing: u8 = 0;

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek().copied() {
                // CSI: ESC [ ... <final byte>
                Some('[') => {
                    chars.next();
                    for code in chars.by_ref() {
                        if ('@'..='~').contains(&code) {
                            break;
                        }
                    }
                }
                // OSC / APC / DCS: ESC ] / ESC _ / ESC P ... terminated by BEL or ST (ESC \\)
                Some(']') | Some('_') | Some('P') | Some('^') => {
                    let introducer = chars.next().unwrap_or('\0');
                    let mut prev_esc = false;
                    for code in chars.by_ref() {
                        if introducer == ']' && code == '\x07' {
                            break;
                        }
                        if prev_esc && code == '\\' {
                            break;
                        }
                        prev_esc = code == '\x1b';
                    }
                }
                _ => {}
            }
            continue;
        }

        saw_visible = true;
        if ch == '\n' {
            trailing = trailing.saturating_add(1);
        } else {
            trailing = 0;
        }
    }

    if saw_visible { Some(trailing) } else { None }
}

fn update_output_trailing_newlines(text: &str) {
    let Some(trailing) = visible_trailing_newlines(text) else {
        return;
    };

    if let Ok(mut state) = OUTPUT_CURSOR.lock() {
        state.trailing_newlines = trailing;
    }
}

fn shift_prompt_down(lines: u16) {
    if lines == 0 {
        return;
    }

    let (start_row, height, status_active) = {
        let state = PROMPT_STATE.lock();
        if let Ok(state) = state {
            if !state.visible {
                return;
            }
            (state.start_row, state.height, state.status_line_active)
        } else {
            return;
        }
    };

    let term_height = term_height();
    // If status active, reserve the status line rows above the prompt.
    let reserved_rows = if status_active {
        STREAMING_STATUS_LINE_ROWS
    } else {
        0u16
    };
    let effective_start_row = start_row.saturating_sub(reserved_rows);
    let effective_height = height.saturating_add(reserved_rows);

    let max_shift = term_height.saturating_sub(effective_start_row + effective_height);
    let shift = lines.min(max_shift);

    if shift == 0 {
        return;
    }

    // Scroll region starts at effective_start_row (which corresponds to 1-based index effective_start_row + 1)
    // Wait, scroll region top is 1-based.
    // If effective_start_row = 19 (line 20). Top = 20.
    // So top = effective_start_row + 1.

    let mut stdout = io::stdout();
    let _ = execute!(stdout, crossterm::cursor::SavePosition, Hide);
    print!("\x1b[{};{}r", effective_start_row + 1, term_height);
    let _ = execute!(stdout, MoveTo(0, effective_start_row));
    let _ = execute!(stdout, ScrollDown(shift));
    print!("\x1b[r");

    // Restore cursor. If we have a tracked prompt position, use that instead of RestorePosition
    // because the prompt may have moved due to scrolling.
    let restored = if let Ok(state) = PROMPT_STATE.lock()
        && let Some((off, col)) = state.cursor_pos
    {
        let new_start = state.start_row.saturating_add(shift);
        let _ = execute!(stdout, MoveTo(col, new_start + off), Show);
        true
    } else {
        false
    };

    if !restored {
        let _ = execute!(stdout, crossterm::cursor::RestorePosition, Show);
    }

    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.start_row = state.start_row.saturating_add(shift);
    }
}

/// Reserve blank lines above the prompt for a tool output viewport.
/// Returns true if all requested lines were reserved.
pub(crate) fn reserve_output_lines(lines: u16) -> bool {
    if lines == 0 {
        return true;
    }

    let _guard = lock_output();

    if !is_prompt_visible() {
        return false;
    }

    let (start_row, height, status_active) = {
        let state = PROMPT_STATE.lock().unwrap();
        (state.start_row, state.height, state.status_line_active)
    };

    let reserved_rows = if status_active {
        STREAMING_STATUS_LINE_ROWS
    } else {
        0
    };

    let insert_row = start_row.saturating_sub(reserved_rows);
    let term_height = term_height();

    let max_insert = term_height.saturating_sub(start_row.saturating_add(height));
    let shift = lines.min(max_insert);
    let mut reserved = 0u16;

    let mut stdout = io::stdout();
    let _ = execute!(stdout, crossterm::cursor::SavePosition, Hide);

    if shift > 0 {
        let _ = execute!(stdout, MoveTo(0, insert_row));
        for _ in 0..shift {
            print!("\x1b[L");
        }
        let _ = stdout.flush();
        reserved = reserved.saturating_add(shift);

        if let Ok(mut state) = PROMPT_STATE.lock() {
            state.start_row = state.start_row.saturating_add(shift);
        }
    }

    if shift < lines {
        let remaining = lines.saturating_sub(shift);
        let output_bottom = insert_row.saturating_sub(1);
        if output_bottom > 0 {
            print!("\x1b[{};{}r", 1, output_bottom + 1);
            let _ = execute!(stdout, MoveTo(0, output_bottom));
            let _ = execute!(stdout, ScrollUp(remaining));
            print!("\x1b[r");
            let _ = stdout.flush();
            reserved = reserved.saturating_add(remaining);
        } else {
            // No output area to scroll; cannot reserve remaining lines.
        }
    }

    // Restore cursor to prompt
    let restored = if let Ok(state) = PROMPT_STATE.lock()
        && let Some((off, col)) = state.cursor_pos
    {
        let _ = execute!(stdout, MoveTo(col, state.start_row + off), Show);
        true
    } else {
        false
    };

    if !restored {
        let _ = execute!(stdout, crossterm::cursor::RestorePosition, Show);
    }

    let success = reserved == lines;
    if success {
        // Treat the reserved spacer line as the current output cursor location.
        set_output_cursor(0, 1);
    }

    success
}

/// Clear lines at the top of the old viewport when collapsing to a smaller size.
/// `lines_to_clear` is how many lines to clear, `old_reserved` is the old total reserved lines.
pub(crate) fn clear_viewport_lines(lines_to_clear: u16, old_reserved: u16) {
    use crossterm::SynchronizedUpdate;

    let _guard = lock_output();

    let (start_row, status_active, cursor_pos, visible) = {
        let state = PROMPT_STATE.lock().unwrap();
        (
            state.start_row,
            state.status_line_active,
            state.cursor_pos,
            state.visible,
        )
    };

    if !visible || lines_to_clear == 0 {
        return;
    }

    let reserved_rows = if status_active {
        STREAMING_STATUS_LINE_ROWS
    } else {
        0
    };

    // Calculate where the old viewport top was
    let spacer_bottom = start_row.saturating_sub(reserved_rows).saturating_sub(1);
    let spacer = crate::cli::TOOL_OUTPUT_VIEWPORT_SPACER_LINES;
    let old_viewport_bottom = spacer_bottom.saturating_sub(spacer);
    let old_viewport_height = old_reserved.saturating_sub(spacer);

    if old_viewport_bottom + 1 < old_viewport_height {
        return;
    }

    let old_viewport_top =
        old_viewport_bottom.saturating_sub(old_viewport_height.saturating_sub(1));

    let mut stdout = io::stdout();
    let _ = stdout.sync_update(|stdout| {
        use crossterm::queue;
        use crossterm::terminal::{Clear, ClearType};

        queue!(stdout, Hide)?;

        // Clear the lines at the top of the old viewport that are no longer needed
        for idx in 0..lines_to_clear {
            let row = old_viewport_top.saturating_add(idx);
            queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        }

        // Restore cursor
        if let Some((off, col)) = cursor_pos {
            queue!(stdout, MoveTo(col, start_row + off))?;
        }

        io::Result::Ok(())
    });
}

/// Render a viewport above the prompt.
pub(crate) fn render_tool_viewport(lines: &[String], height: u16, spacer_lines: u16) {
    use crossterm::SynchronizedUpdate;

    let _guard = lock_output();

    let (start_row, status_active, cursor_pos, visible) = {
        let state = PROMPT_STATE.lock().unwrap();
        (
            state.start_row,
            state.status_line_active,
            state.cursor_pos,
            state.visible,
        )
    };

    if !visible || height == 0 {
        return;
    }

    let reserved_rows = if status_active {
        STREAMING_STATUS_LINE_ROWS
    } else {
        0
    };

    let spacer_bottom = start_row.saturating_sub(reserved_rows).saturating_sub(1);
    let viewport_bottom = spacer_bottom.saturating_sub(spacer_lines);

    if viewport_bottom + 1 < height {
        return;
    }

    let viewport_top = viewport_bottom.saturating_sub(height.saturating_sub(1));

    let mut stdout = io::stdout();
    let _ = stdout.sync_update(|stdout| {
        use crossterm::queue;
        use crossterm::terminal::{Clear, ClearType};
        use std::io::Write;

        queue!(stdout, Hide)?;

        for idx in 0..height {
            let row = viewport_top.saturating_add(idx);
            let line = lines.get(idx as usize).map(String::as_str).unwrap_or("");
            queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
            write!(stdout, "{}", line)?;
        }

        // Restore cursor
        if let Some((off, col)) = cursor_pos {
            queue!(stdout, MoveTo(col, start_row + off))?;
        }

        io::Result::Ok(())
    });
}

/// Write a complete line, handling prompt visibility.
/// If prompt is visible, writes above it. Otherwise writes normally.
pub(crate) fn println_above(text: &str) {
    let _guard = lock_output();
    println_above_locked(text);
}

fn println_above_locked(text: &str) {
    if !is_prompt_visible() {
        print_above_locked(text);
        if !text.ends_with('\n') {
            print_above_locked("\n");
        }
        return;
    }

    print_above_locked(text);
    if text.ends_with('\n') {
        return;
    }
    print_above_locked("\n");
}

/// Write text (possibly partial line), handling prompt visibility.
pub(crate) fn print_above(text: &str) {
    let _guard = lock_output();
    print_above_locked(text);
}

fn print_above_locked(text: &str) {
    let mut stdout = io::stdout();

    if text.is_empty() {
        return;
    }

    // When the terminal is in raw mode, `\n` does not reliably return to column 0.
    // Normalize to CRLF so multi-line output renders correctly and our cursor tracking
    // matches what the terminal actually does.
    let normalized = text.contains('\n').then(|| normalize_newlines(text));
    let text = normalized.as_deref().unwrap_or(text);

    if !is_prompt_visible() {
        print!("{}", text);
        let _ = stdout.flush();

        let term_width = term_width();
        let output_col = OUTPUT_CURSOR.lock().map(|state| state.col).unwrap_or(0);
        let (_lines_needed, new_col) = calculate_output_size(output_col, text, term_width);
        if let Ok(mut state) = OUTPUT_CURSOR.lock() {
            state.col = new_col;
            state.has_output = true;
        }
        update_output_trailing_newlines(text);
        return;
    }

    let term_width = term_width();
    let output_col = OUTPUT_CURSOR.lock().map(|state| state.col).unwrap_or(0);

    let (lines_needed, new_col) = calculate_output_size(output_col, text, term_width);

    // Check if this is the first output
    let is_first_output = OUTPUT_CURSOR.lock().map(|s| !s.has_output).unwrap_or(false);

    // Mark that we've started outputting
    if let Ok(mut cursor_state) = OUTPUT_CURSOR.lock() {
        cursor_state.has_output = true;
    }

    // For the first output, we need to create space for the output area.
    // The prompt was drawn directly at the cursor when henri started,
    // with previous terminal content above it. We handle this by:
    // 1. Moving to just above the prompt
    // 2. Using InsertLine to push the prompt down (preserving content above)
    // 3. Printing our text in the newly created blank lines
    if is_first_output {
        let (start_row, height, status_active) = {
            let state = PROMPT_STATE.lock().unwrap();
            (state.start_row, state.height, state.status_line_active)
        };

        // The row just above the prompt (accounting for streaming status line rows)
        let reserved_rows = if status_active {
            STREAMING_STATUS_LINE_ROWS
        } else {
            0u16
        };

        let insert_row = start_row.saturating_sub(reserved_rows);

        // Calculate how many lines we want to insert
        let desired_insert_lines = lines_needed.saturating_add(1);

        // Calculate maximum lines we can insert without pushing prompt off-screen
        // The prompt can move down until start_row + height == term_height
        let term_height = term_height();
        let max_insert = term_height.saturating_sub(start_row.saturating_add(height));

        // Use the smaller of desired and maximum
        let insert_lines = desired_insert_lines.min(max_insert);

        // If we can't insert any lines, the prompt is already at the bottom of the terminal.
        // In that case, scroll the OUTPUT region up to create blank lines just above the prompt,
        // preserving the user's existing terminal output.
        if insert_lines == 0 {
            let output_bottom = insert_row.saturating_sub(1);

            // If there's no output area at all (prompt at top), fall through to normal output.
            if output_bottom == 0 {
                if let Ok(mut cursor_state) = OUTPUT_CURSOR.lock() {
                    cursor_state.has_output = false;
                }
            } else {
                let _ = execute!(stdout, crossterm::cursor::SavePosition, Hide);

                // Scroll just the output area (top of screen .. row above prompt/status).
                // This creates `desired_insert_lines` blank lines at the bottom of that region.
                print!("\x1b[{};{}r", 1, output_bottom + 1);
                let _ = execute!(stdout, MoveTo(0, output_bottom));
                let _ = execute!(stdout, ScrollUp(desired_insert_lines));
                print!("\x1b[r");
                let _ = stdout.flush();

                // Print the text into the newly freed lines above the prompt.
                let print_row = output_bottom.saturating_sub(lines_needed);
                let _ = execute!(stdout, MoveTo(output_col, print_row));
                print!("{}", text);
                let _ = stdout.flush();

                // Restore cursor to prompt
                if let Ok(state) = PROMPT_STATE.lock() {
                    if let Some((off, col)) = state.cursor_pos {
                        let _ = execute!(stdout, MoveTo(col, state.start_row + off), Show);
                    } else {
                        let _ = execute!(stdout, crossterm::cursor::RestorePosition, Show);
                    }
                }

                if let Ok(mut state) = OUTPUT_CURSOR.lock() {
                    state.col = new_col;
                }
                update_output_trailing_newlines(text);
                return;
            }
        } else {
            let _ = execute!(stdout, crossterm::cursor::SavePosition, Hide);

            // Move to the prompt position and insert blank lines
            // This pushes the prompt down without affecting content above
            let _ = execute!(stdout, MoveTo(0, insert_row));
            for _ in 0..insert_lines {
                // Insert a blank line, pushing everything below down
                print!("\x1b[L");
            }
            let _ = stdout.flush();

            // Update prompt position - it moved down
            if let Ok(mut state) = PROMPT_STATE.lock() {
                state.start_row = state.start_row.saturating_add(insert_lines);
            }

            // Now print the text at insert_row
            let _ = execute!(stdout, MoveTo(output_col, insert_row));
            print!("{}", text);
            let _ = stdout.flush();

            // Restore cursor to prompt
            if let Ok(state) = PROMPT_STATE.lock() {
                if let Some((off, col)) = state.cursor_pos {
                    let _ = execute!(stdout, MoveTo(col, state.start_row + off), Show);
                } else {
                    let _ = execute!(stdout, crossterm::cursor::RestorePosition, Show);
                }
            }

            if let Ok(mut state) = OUTPUT_CURSOR.lock() {
                state.col = new_col;
            }
            update_output_trailing_newlines(text);
            return;
        }
    }

    // Normal output path (not first output)
    let (old_start_row, status_active) = {
        let state = PROMPT_STATE.lock().unwrap();
        (state.start_row, state.status_line_active)
    };

    shift_prompt_down(lines_needed);

    let start_row = {
        let state = PROMPT_STATE.lock().unwrap();
        state.start_row
    };

    if start_row == 0 {
        print!("{}", text);
        let _ = stdout.flush();
        if let Ok(mut state) = OUTPUT_CURSOR.lock() {
            state.col = new_col;
        }
        update_output_trailing_newlines(text);
        return;
    }

    let reserved_rows = if status_active {
        STREAMING_STATUS_LINE_ROWS
    } else {
        0u16
    };

    // Continue printing where we left off
    let output_row = old_start_row
        .saturating_sub(1)
        .saturating_sub(reserved_rows);

    // Reserve the streaming status line rows (immediately above the prompt) when active.
    //
    // `start_row` is 0-indexed, but the terminal scroll-region escape uses 1-indexed rows.
    let scroll_region_bottom_row = start_row.saturating_sub(reserved_rows.saturating_add(1));
    let scroll_region_bottom_line = scroll_region_bottom_row.saturating_add(1);

    let _ = execute!(stdout, crossterm::cursor::SavePosition, Hide);
    print!("\x1b[{};{}r", 1, scroll_region_bottom_line);
    let _ = execute!(stdout, MoveTo(output_col, output_row));
    print!("{}", text);
    let _ = stdout.flush();
    print!("\x1b[r");

    // Restore cursor
    let restored = if let Ok(state) = PROMPT_STATE.lock()
        && state.visible
        && let Some((off, col)) = state.cursor_pos
    {
        let _ = execute!(stdout, MoveTo(col, state.start_row + off), Show);
        true
    } else {
        false
    };

    if !restored {
        let _ = execute!(stdout, crossterm::cursor::RestorePosition, Show);
    }

    if let Ok(mut state) = OUTPUT_CURSOR.lock() {
        state.col = new_col;
    }
    update_output_trailing_newlines(text);
}

/// Get terminal height.
pub(super) fn term_height() -> u16 {
    terminal::size().map(|(_, h)| h).unwrap_or(24)
}

/// Get terminal width.
pub(crate) fn term_width() -> u16 {
    terminal::size().map(|(w, _)| w).unwrap_or(80)
}

/// Normalize newlines for terminals running in raw mode.
fn normalize_newlines(text: &str) -> String {
    if !text.contains('\n') {
        return text.to_string();
    }

    let mut output = String::with_capacity(text.len());
    let mut prev_cr = false;

    for ch in text.chars() {
        if ch == '\n' {
            if !prev_cr {
                output.push('\r');
            }
            output.push('\n');
        } else {
            output.push(ch);
        }
        prev_cr = ch == '\r';
    }

    output
}

fn render_history_with_gap(width: usize, status_active: bool) -> Option<String> {
    use super::history;
    use super::render;

    let events = history::snapshot();
    if events.is_empty() {
        return None;
    }

    let rendered = render::render_all(&events, width);
    let normalized = normalize_newlines(&rendered);

    if normalized.is_empty() {
        return None;
    }

    // Add a blank line for visual separation between output and prompt when the
    // streaming status line is inactive. When active, the spacer row already
    // provides that separation.
    if status_active {
        Some(normalized)
    } else {
        Some(format!("{}\n", normalized))
    }
}

/// Redraw all output from history after terminal resize.
/// Clears screen and reprints everything at the new width.
///
/// If width/height are provided, uses those dimensions instead of querying the terminal.
/// This is important during resize events where the terminal may not have fully settled.
fn redraw_from_history_with_size_inner(
    prompt_height: u16,
    width: Option<u16>,
    height: Option<u16>,
) {
    use std::io::Write;

    let width = width.unwrap_or_else(term_width) as usize;
    let term_height = height.unwrap_or_else(term_height);

    let status_active = PROMPT_STATE
        .lock()
        .map(|s| s.status_line_active)
        .unwrap_or(false);

    let Some(with_gap) = render_history_with_gap(width, status_active) else {
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            terminal::Clear(ClearType::All),
            terminal::Clear(ClearType::Purge),
            MoveTo(0, 0)
        );
        let _ = stdout.flush();
        reset_output_cursor();
        return;
    };

    let (lines_used, end_col) = calculate_output_size(0, &with_gap, width as u16);
    // `calculate_output_size` returns the number of line breaks/wraps encountered;
    // the actual number of terminal rows occupied is that count + 1 for non-empty output.
    let rows_used = lines_used.saturating_add(1);

    let mut stdout = io::stdout();

    // Calculate where output should start so it ends just above the prompt.
    // Reserve space for the streaming status line row when it is active.
    let reserved_above_prompt = if status_active {
        STREAMING_STATUS_LINE_ROWS
    } else {
        0
    };
    let available_height = term_height
        .saturating_sub(prompt_height)
        .saturating_sub(reserved_above_prompt);

    // If output fits, position it just above where prompt will be (with gap)
    // If output is larger than available space, it will scroll
    let output_start_row = if rows_used <= available_height {
        available_height.saturating_sub(rows_used)
    } else {
        0
    };

    // Clear screen properly using crossterm
    let _ = execute!(
        stdout,
        terminal::Clear(ClearType::All),
        terminal::Clear(ClearType::Purge),
        MoveTo(0, 0)
    );

    // Move to output start position
    let _ = execute!(stdout, MoveTo(0, output_start_row));

    // Print the rendered output (with trailing blank line for visual separation)
    print!("{}", with_gap);
    let _ = stdout.flush();

    let mut final_col = end_col;

    // If output overflowed the available height (encroaching on prompt/reserved area),
    // scroll it up to clear the space.
    if rows_used > available_height {
        let overshoot = if rows_used >= term_height {
            // We are at the bottom of the terminal.
            // Text ends at term_height - 1. We want it at available_height - 1.
            term_height.saturating_sub(available_height)
        } else {
            // We are not at the bottom.
            // Text ends at rows_used - 1. We want it at available_height - 1.
            rows_used.saturating_sub(available_height)
        };

        if overshoot > 0 {
            if rows_used >= term_height {
                // At bottom: printing newlines forces scroll
                let padding = "\n".repeat(overshoot as usize);
                print!("{}", padding);
                final_col = 0;
            } else {
                // Not at bottom: explicit scroll required
                let _ = execute!(stdout, terminal::ScrollUp(overshoot));
            }
            let _ = stdout.flush();
        }
    }

    // Reset output cursor state
    let trailing = visible_trailing_newlines(&with_gap).unwrap_or(0);
    set_output_cursor(final_col, trailing);
}

/// Redraw all output from history after terminal resize.
/// Clears screen and reprints everything at the new width.
///
/// If width/height are provided, uses those dimensions instead of querying the terminal.
/// This is important during resize events where the terminal may not have fully settled.
pub(crate) fn redraw_from_history_with_size(
    prompt_height: u16,
    width: Option<u16>,
    height: Option<u16>,
) {
    let _guard = lock_output();
    redraw_from_history_with_size_inner(prompt_height, width, height);
}

pub(crate) fn redraw_from_history_with_size_locked(
    prompt_height: u16,
    width: Option<u16>,
    height: Option<u16>,
) {
    redraw_from_history_with_size_inner(prompt_height, width, height);
}

/// Redraw all output from history after terminal resize.
/// Uses current terminal dimensions (queries the terminal).
pub(crate) fn redraw_from_history(prompt_height: u16) {
    redraw_from_history_with_size(prompt_height, None, None);
}

/// Print history as scrolling output without clearing the screen.
///
/// This is used when restoring a session on startup (`henri -c`) to preserve
/// prior terminal output in the scrollback buffer. Unlike `redraw_from_history`,
/// this function prints the history content as normal terminal output that
/// scrolls naturally.
pub(crate) fn print_history_scrolled() {
    use std::io::Write;

    let _guard = lock_output();

    let status_active = PROMPT_STATE
        .lock()
        .map(|s| s.status_line_active)
        .unwrap_or(false);

    let width = term_width() as usize;
    let Some(with_gap) = render_history_with_gap(width, status_active) else {
        return;
    };

    // Print the rendered output as normal scrolling text.
    print!("{}", with_gap);

    // Preserve the extra separator row used during session restore so the
    // prompt doesn't appear glued to the last restored line.
    if !status_active {
        print!("\r\n");
    }

    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    use super::newlines_needed;

    #[test]
    fn ensure_trailing_newlines_counts_implicit_wrap() {
        // Cursor at col 0 without explicit newline (implicit wrap) should behave as if we already
        // have 1 trailing newline, so requesting 2 only prints 1.
        assert_eq!(newlines_needed(2, 0, 0, true), 1);
    }

    #[test]
    fn ensure_trailing_newlines_mid_line_needs_two() {
        assert_eq!(newlines_needed(2, 5, 0, true), 2);
    }

    #[test]
    fn ensure_trailing_newlines_one_newline_needs_one() {
        assert_eq!(newlines_needed(2, 0, 1, true), 1);
        assert_eq!(newlines_needed(2, 10, 1, true), 1);
    }

    #[test]
    fn ensure_trailing_newlines_already_blank_needs_none() {
        assert_eq!(newlines_needed(2, 0, 2, true), 0);
        assert_eq!(newlines_needed(2, 10, 3, true), 0);
    }

    #[test]
    fn ensure_trailing_newlines_no_output_does_not_assume_wrap() {
        // Startup: col 0, no output yet should not be treated as an implicit newline.
        assert_eq!(newlines_needed(2, 0, 0, false), 2);
    }
}
