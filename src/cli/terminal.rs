// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Terminal management for CLI mode.
//!
//! Provides output functions that coordinate with a persistent prompt area
//! at the bottom of the terminal.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crossterm::cursor::{Hide, MoveTo, MoveToNextLine, Show};
use crossterm::execute;
use crossterm::terminal::{self, Clear, ClearType, ScrollDown, ScrollUp};

/// Global state: whether prompt box is visible and where it is
static PROMPT_STATE: Mutex<PromptState> = Mutex::new(PromptState::new());

struct PromptState {
    visible: bool,
    height: u16,
    /// Row where the prompt starts (0-indexed)
    start_row: u16,
    status_row_offset: u16,
    /// Cursor position relative to prompt (row_offset, col)
    cursor_pos: Option<(u16, u16)>,
    /// Whether the streaming status line (above prompt) is active/reserved
    status_line_active: bool,
    /// Whether bandwidth stats are currently allowed to render
    bandwidth_allowed: bool,
    /// Minimum column where bandwidth stats may start (prevents overlap)
    bandwidth_min_col: u16,
    /// Last bandwidth display column for clearing artifacts
    bandwidth_col: Option<u16>,
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
            bandwidth_allowed: false,
            bandwidth_min_col: 0,
            bandwidth_col: None,
        }
    }
}

struct OutputCursor {
    col: u16,
    /// Whether we've ever printed output above the prompt.
    /// Used to scroll the terminal on first output to avoid overwriting previous content.
    has_output: bool,
    /// Number of trailing newlines emitted by output functions.
    ///
    /// This lets the CLI renderer normalize spacing between blocks:
    /// - If previous output ended without a newline, next block can force `\n\n`.
    /// - If previous output ended with `\n\n`, nothing to do.
    /// - If more than 2 newlines were emitted, trim to 2.
    trailing_newlines: u8,
}

impl OutputCursor {
    const fn new() -> Self {
        Self {
            col: 0,
            has_output: false,
            trailing_newlines: 0,
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
        state.has_output = false;
        state.trailing_newlines = 0;
    }
}

fn set_output_cursor(col: u16) {
    if let Ok(mut state) = OUTPUT_CURSOR.lock() {
        state.col = col;
    }
}

pub(crate) fn output_cursor_col() -> u16 {
    OUTPUT_CURSOR.lock().map(|state| state.col).unwrap_or(0)
}

/// Notify the terminal manager that the prompt box is now visible.
/// `start_row` is the 0-indexed row where the prompt begins.
pub(super) fn set_prompt_visible(height: u16, start_row: u16, status_row_offset: u16) {
    let was_visible = if let Ok(mut state) = PROMPT_STATE.lock() {
        let was = state.visible;
        state.visible = true;
        state.height = height;
        state.start_row = start_row;
        state.status_row_offset = if height == 0 {
            0
        } else {
            status_row_offset.min(height.saturating_sub(1))
        };
        state.bandwidth_allowed = false;
        state.bandwidth_min_col = 0;
        state.bandwidth_col = None;
        was
    } else {
        false
    };
    // Only reset output cursor when prompt transitions from hidden to visible,
    // not on every redraw. Resetting during redraw causes streaming output to
    // overwrite itself when user is typing.
    if !was_visible {
        reset_output_cursor();
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
        state.bandwidth_allowed = false;
        state.bandwidth_min_col = 0;
        state.bandwidth_col = None;
    }
    reset_output_cursor();
}

/// Set whether the status line (row above prompt) is active/reserved.
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

    if active && !was_active && visible && start_row > 0 {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, crossterm::cursor::SavePosition, Hide);

        let term_height = term_height();

        // Prefer to shift the prompt DOWN by two lines (if there is room) to make space
        // for the status line and a blank spacer row above it. This avoids wiping out
        // existing terminal output when the prompt is near the top.
        // Layout: <output> / <blank spacer> / <status line> / <prompt>
        let can_shift_down = start_row.saturating_add(height).saturating_add(1) < term_height;
        if can_shift_down {
            let _ = execute!(stdout, MoveTo(0, start_row));
            // Insert two blank lines at the prompt start, pushing the prompt down.
            print!("\x1b[2L");
            let _ = stdout.flush();

            if let Ok(mut state) = PROMPT_STATE.lock() {
                state.start_row = state.start_row.saturating_add(2);
            }
        } else {
            // Fallback: scroll the output area up by two lines to clear the status + spacer rows.
            // This is used when the prompt is already at the bottom and cannot move down.
            print!("\x1b[{};{}r", 1, start_row);
            let _ = execute!(stdout, MoveTo(0, start_row.saturating_sub(1)));
            let _ = execute!(stdout, ScrollUp(2));
            print!("\x1b[r");
            let _ = stdout.flush();
        }

        // Restore cursor to prompt
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

pub(super) fn is_streaming_status_line_active() -> bool {
    PROMPT_STATE
        .lock()
        .map(|s| s.status_line_active)
        .unwrap_or(false)
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

/// Write text to the status line (if active and visible).
pub(crate) fn write_status_line(text: &str) {
    use crossterm::SynchronizedUpdate;

    let _guard = lock_output();
    let state = PROMPT_STATE.lock().unwrap();

    if !state.visible || !state.status_line_active || state.start_row == 0 {
        return;
    }

    let row = state.start_row - 1;
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

        // Always clear the spacer row above the status line. Resize redraws intentionally
        // leave this row untouched, so stale content can otherwise remain visible.
        if row > 0 {
            queue!(stdout, MoveTo(0, row - 1), Clear(ClearType::CurrentLine))?;
        }

        queue!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))?;
        write!(stdout, "{}", text)?;

        // Restore cursor
        if let Some((cursor_row, cursor_col)) = cursor_info {
            queue!(stdout, MoveTo(cursor_col, cursor_row), Show)?;
        } else {
            queue!(stdout, Show)?;
        }

        io::Result::Ok(())
    });
}

/// Update the bandwidth display on the provider/model status line (bottom of prompt).
pub(crate) fn update_bandwidth_display(text: &str) {
    use crossterm::SynchronizedUpdate;
    use crossterm::style::{Color, ResetColor, SetForegroundColor};

    let _guard = lock_output();
    let (row, cursor_info, prev_col, min_col, allowed) = {
        let state = PROMPT_STATE.lock().unwrap();
        if !state.visible || state.height == 0 {
            return;
        }
        let row = state.start_row + state.status_row_offset;
        let cursor_info = state
            .cursor_pos
            .map(|(off, col)| (state.start_row + off, col));
        (
            row,
            cursor_info,
            state.bandwidth_col,
            state.bandwidth_min_col,
            state.bandwidth_allowed,
        )
    };

    if !allowed && !text.is_empty() {
        return;
    }

    let width = term_width();
    let text_len = text.chars().count() as u16;

    if text.is_empty() {
        // Clear any previous bandwidth display.
        let mut stdout = io::stdout();
        let _ = stdout.sync_update(|stdout| {
            use crossterm::queue;
            use crossterm::terminal::{Clear, ClearType};

            queue!(stdout, Hide)?;
            if let Some(prev_col) = prev_col {
                queue!(
                    stdout,
                    MoveTo(prev_col, row),
                    Clear(ClearType::UntilNewLine)
                )?;
            }

            // Restore cursor
            if let Some((cursor_row, cursor_col)) = cursor_info {
                queue!(stdout, MoveTo(cursor_col, cursor_row), Show)?;
            } else {
                queue!(stdout, Show)?;
            }

            io::Result::Ok(())
        });

        if let Ok(mut state) = PROMPT_STATE.lock() {
            state.bandwidth_col = None;
        }
        return;
    }

    let col = width.saturating_sub(text_len).max(min_col);

    // If we still cannot fit without overlapping, hide.
    if col.saturating_add(text_len) > width {
        update_bandwidth_display("");
        return;
    }

    let mut stdout = io::stdout();

    let _ = stdout.sync_update(|stdout| {
        use crossterm::queue;
        use crossterm::terminal::{Clear, ClearType};
        use std::io::Write;

        queue!(stdout, Hide)?;

        if let Some(prev_col) = prev_col
            && prev_col != col
        {
            queue!(
                stdout,
                MoveTo(prev_col, row),
                Clear(ClearType::UntilNewLine)
            )?;
        }

        queue!(
            stdout,
            MoveTo(col, row),
            Clear(ClearType::UntilNewLine),
            SetForegroundColor(Color::DarkGrey)
        )?;
        write!(stdout, "{}", text)?;
        queue!(stdout, ResetColor)?;

        // Restore cursor
        if let Some((cursor_row, cursor_col)) = cursor_info {
            queue!(stdout, MoveTo(cursor_col, cursor_row), Show)?;
        } else {
            queue!(stdout, Show)?;
        }

        io::Result::Ok(())
    });

    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.bandwidth_col = Some(col);
    }
}

/// Get whether the prompt is visible.
fn is_prompt_visible() -> bool {
    PROMPT_STATE.lock().map(|s| s.visible).unwrap_or(false)
}

fn calculate_output_size(mut col: u16, text: &str, width: u16) -> (u16, u16) {
    let mut lines = 0;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                for code in chars.by_ref() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
            continue;
        }

        match ch {
            '\n' => {
                lines += 1;
                col = 0;
            }
            '\r' => {
                col = 0;
            }
            _ => {
                if col >= width {
                    lines += 1;
                    col = 0;
                }
                col += 1;
            }
        }
    }
    (lines, col)
}

fn count_trailing_newlines(text: &str) -> u8 {
    let mut count = 0u8;
    for ch in text.chars().rev() {
        match ch {
            '\n' => {
                count = count.saturating_add(1);
            }
            '\r' => {
                // ignore \r in CRLF; keep scanning
            }
            _ => break,
        }
    }
    count
}

fn clamp_trailing_newlines() {
    if let Ok(mut state) = OUTPUT_CURSOR.lock() {
        state.trailing_newlines = state.trailing_newlines.min(2);
    }
}

/// Ensure there are at least `min_trailing` newlines separating output blocks.
///
/// This is a best-effort normalization based on tracking what this process has
/// printed; it cannot remove extra newlines that were already emitted.
///
/// - If the previous output ended with fewer than `min_trailing` trailing
///   newlines, prints the difference.
/// - If it ended with `min_trailing` or more trailing newlines, does nothing.
///
/// Note: the output cursor only tracks up to 2 trailing newlines.
pub(crate) fn normalize_block_spacing_min(min_trailing: u8) {
    let _guard = lock_output();

    let min_trailing = min_trailing.clamp(0, 2);

    let trailing = OUTPUT_CURSOR
        .lock()
        .map(|s| s.trailing_newlines)
        .unwrap_or(0);

    match (trailing, min_trailing) {
        (_, 0) => {}
        (0, 1) => print_above_locked("\n"),
        (0, 2) => print_above_locked("\n\n"),
        (1, 2) => print_above_locked("\n"),
        _ => {}
    }

    clamp_trailing_newlines();
}

/// Ensure there are at least two newlines separating output blocks.
pub(crate) fn normalize_block_spacing() {
    normalize_block_spacing_min(2);
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
    // If status active, reserve a spacer row above it so output stays
    // one line away from the status line.
    let reserved_rows = if status_active { 2u16 } else { 0u16 };
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

/// Write a complete line, handling prompt visibility.
/// If prompt is visible, writes above it. Otherwise writes normally.
pub(crate) fn println_above(text: &str) {
    let _guard = lock_output();
    println_above_locked(text);
}

fn println_above_locked(text: &str) {
    if !is_prompt_visible() {
        println!("{}", text);
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

    // Track trailing newlines so callers can normalize spacing between blocks.
    if let Ok(mut state) = OUTPUT_CURSOR.lock() {
        let trailing = count_trailing_newlines(text);
        state.trailing_newlines = if trailing == 0 {
            0
        } else {
            state.trailing_newlines.saturating_add(trailing)
        };
    }

    if !is_prompt_visible() {
        print!("{}", text);
        let _ = stdout.flush();
        clamp_trailing_newlines();
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

        // The row just above the prompt (accounting for streaming status line + spacer above)
        let insert_row = if status_active {
            start_row.saturating_sub(2)
        } else {
            start_row
        };

        // Calculate how many lines we want to insert
        let desired_insert_lines = lines_needed.saturating_add(1);

        // Calculate effective height (prompt height + status line + spacer if active)
        let effective_height = if status_active { height + 2 } else { height };

        // Calculate maximum lines we can insert without pushing prompt off-screen
        // The prompt can move down until start_row + height == term_height
        let term_height = term_height();
        let max_insert = term_height.saturating_sub(start_row + effective_height);

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
                clamp_trailing_newlines();
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
            clamp_trailing_newlines();
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
        clamp_trailing_newlines();
        return;
    }

    // Continue printing where we left off
    let output_row = old_start_row
        .saturating_sub(1)
        .saturating_sub(if status_active { 2 } else { 0 });

    let scroll_region_bottom = start_row.saturating_sub(if status_active { 2 } else { 0 });

    // If scrolling region is invalid (e.g. at top of screen), fallback to normal print
    if scroll_region_bottom < 1 {
        print!("{}", text);
        let _ = stdout.flush();
        if let Ok(mut state) = OUTPUT_CURSOR.lock() {
            state.col = new_col;
        }
        clamp_trailing_newlines();
        return;
    }

    let _ = execute!(stdout, crossterm::cursor::SavePosition, Hide);
    print!("\x1b[{};{}r", 1, scroll_region_bottom);
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

    clamp_trailing_newlines();
}

/// Get terminal height.
pub(super) fn term_height() -> u16 {
    terminal::size().map(|(_, h)| h).unwrap_or(24)
}

/// Get terminal width.
pub(crate) fn term_width() -> u16 {
    terminal::size().map(|(w, _)| w).unwrap_or(80)
}

pub(super) fn set_bandwidth_allowed(allowed: bool) {
    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.bandwidth_allowed = allowed;
    }
}

pub(super) fn set_bandwidth_min_col(min_col: u16) {
    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.bandwidth_min_col = min_col;
    }
}

pub(super) fn set_bandwidth_col(col: Option<u16>) {
    if let Ok(mut state) = PROMPT_STATE.lock() {
        state.bandwidth_col = col;
    }
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

    use super::history;
    use super::render;

    let events = history::snapshot();

    if events.is_empty() {
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
    }

    let width = width.unwrap_or_else(term_width) as usize;
    let term_height = height.unwrap_or_else(term_height);

    let rendered = render::render_all(&events, width);
    let normalized = normalize_newlines(&rendered);
    let (lines_used, end_col) = calculate_output_size(0, &normalized, width as u16);
    // `calculate_output_size` returns the number of line breaks/wraps encountered;
    // the actual number of terminal rows occupied is that count + 1 for non-empty output.
    let rows_used = if normalized.is_empty() {
        0
    } else {
        lines_used.saturating_add(1)
    };

    let mut stdout = io::stdout();

    // Calculate where output should start so it ends just above the prompt.
    // Reserve space for: (if status active) blank spacer + status line + prompt gap, else just a prompt gap.
    let status_active = PROMPT_STATE
        .lock()
        .map(|s| s.status_line_active)
        .unwrap_or(false);
    let reserved_above_prompt = if status_active { 2 } else { 1 };
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

    // Print the rendered output
    print!("{}", normalized);
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
    set_output_cursor(final_col);
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
