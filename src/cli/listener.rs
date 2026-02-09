// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Output listener for CLI mode - prints output above the prompt.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use colored::{Color, Colorize};
use tokio::sync::watch;
use unicode_width::UnicodeWidthChar;

use super::history::{self, HistoryEvent};
use super::markdown::{
    render_markdown_inlines, render_markdown_inlines_with_style, render_markdown_line,
};
use super::render::{BG_DARK_GREEN, BG_DARK_RED, format_summary_suffix, style_file_read_line};
use super::spacing::{LastBlock, needs_blank_line_before};
use super::terminal;
use crate::output::{OutputEvent, OutputListener};
use crate::syntax;

static ACTIVE_LISTENER: OnceLock<&'static CliListener> = OnceLock::new();

// Global spinner state - completely decoupled from mutex-protected state
static SPINNER_STATE: AtomicU8 = AtomicU8::new(0); // 0=Ready, 1=Working, 2=Thinking
static SPINNER_TX: OnceLock<watch::Sender<u8>> = OnceLock::new();

// Whether to show image previews (loaded from config)
static SHOW_IMAGE_PREVIEWS: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum ToolOutputDisplayMode {
    Hidden = 0,
    Viewport = 1,
    Expanded = 2,
}

impl ToolOutputDisplayMode {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Hidden,
            1 => Self::Viewport,
            2 => Self::Expanded,
            _ => Self::Viewport,
        }
    }
}

// Tool output display mode: hidden, viewport tail, or expanded full output.
static TOOL_OUTPUT_DISPLAY_MODE: AtomicU8 = AtomicU8::new(ToolOutputDisplayMode::Viewport as u8);
// Last non-hidden display mode for hidden-mode expanded toggles and visibility restores
// driven by config reload.
static LAST_VISIBLE_TOOL_OUTPUT_MODE: AtomicU8 =
    AtomicU8::new(ToolOutputDisplayMode::Viewport as u8);

// Lock to prevent racing between viewport toggle and streaming output rendering
static VIEWPORT_TRANSITION_LOCK: Mutex<()> = Mutex::new(());

// Context/token tracking state for status line display
static CONTEXT_TOKENS: AtomicU64 = AtomicU64::new(0);
static CONTEXT_LIMIT: AtomicU64 = AtomicU64::new(0); // 0 = unknown
static TOTAL_TOKENS: AtomicU64 = AtomicU64::new(0);
static TOTAL_TOKENS_DISPLAY: AtomicU64 = AtomicU64::new(0);
static INPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
static OUTPUT_TOKENS: AtomicU64 = AtomicU64::new(0);
static CACHE_READ_TOKENS: AtomicU64 = AtomicU64::new(0);
static STREAMING_START: Mutex<Option<std::time::Instant>> = Mutex::new(None);
// Accumulated duration from previous API calls in this turn (in milliseconds)
static ACCUMULATED_DURATION_MS: AtomicU64 = AtomicU64::new(0);
// Stores the final duration in milliseconds when streaming completes (0 = still streaming or no data)
static FINAL_DURATION_MS: AtomicU64 = AtomicU64::new(0);

/// Reload the show_image_previews setting from config
pub(crate) fn reload_show_image_previews() {
    let enabled = crate::config::ConfigFile::load()
        .map(|c| c.show_image_previews)
        .unwrap_or(true);
    SHOW_IMAGE_PREVIEWS.store(enabled, Ordering::Relaxed);
}

/// Reload the hide_tool_output setting from config
pub(crate) fn reload_hide_tool_output() {
    let enabled = crate::config::ConfigFile::load()
        .map(|c| c.hide_tool_output)
        .unwrap_or(false);
    if enabled {
        hide_tool_output();
    } else {
        show_tool_output();
    }
}

fn tool_output_display_mode() -> ToolOutputDisplayMode {
    ToolOutputDisplayMode::from_u8(TOOL_OUTPUT_DISPLAY_MODE.load(Ordering::Relaxed))
}

fn set_tool_output_display_mode(mode: ToolOutputDisplayMode) {
    TOOL_OUTPUT_DISPLAY_MODE.store(mode as u8, Ordering::Relaxed);
    if mode != ToolOutputDisplayMode::Hidden {
        LAST_VISIBLE_TOOL_OUTPUT_MODE.store(mode as u8, Ordering::Relaxed);
    }
}

fn last_visible_tool_output_mode() -> ToolOutputDisplayMode {
    match ToolOutputDisplayMode::from_u8(LAST_VISIBLE_TOOL_OUTPUT_MODE.load(Ordering::Relaxed)) {
        ToolOutputDisplayMode::Hidden => ToolOutputDisplayMode::Viewport,
        mode => mode,
    }
}

fn hide_tool_output() {
    let mode = tool_output_display_mode();
    if mode != ToolOutputDisplayMode::Hidden {
        LAST_VISIBLE_TOOL_OUTPUT_MODE.store(mode as u8, Ordering::Relaxed);
        TOOL_OUTPUT_DISPLAY_MODE.store(ToolOutputDisplayMode::Hidden as u8, Ordering::Relaxed);
    }
}

fn show_tool_output() {
    if tool_output_display_mode() == ToolOutputDisplayMode::Hidden {
        TOOL_OUTPUT_DISPLAY_MODE.store(last_visible_tool_output_mode() as u8, Ordering::Relaxed);
    }
}

/// Check if tool output should be hidden
pub(crate) fn is_tool_output_hidden() -> bool {
    tool_output_display_mode() == ToolOutputDisplayMode::Hidden
}

/// Toggle hide tool output state at runtime.
/// Returns the new hidden state (true = hidden, false = visible).
pub(crate) fn toggle_hide_tool_output() -> bool {
    if is_tool_output_hidden() {
        set_tool_output_display_mode(ToolOutputDisplayMode::Viewport);
        false
    } else {
        hide_tool_output();
        true
    }
}

pub(crate) fn is_tool_output_expanded() -> bool {
    tool_output_display_mode() == ToolOutputDisplayMode::Expanded
}

/// Toggle tool output viewport expanded state.
/// Returns the new expanded state (true = expanded, false = collapsed).
pub(crate) fn toggle_tool_output_expanded() -> bool {
    let mode = tool_output_display_mode();
    let next_mode = match mode {
        ToolOutputDisplayMode::Viewport => ToolOutputDisplayMode::Expanded,
        ToolOutputDisplayMode::Expanded => ToolOutputDisplayMode::Viewport,
        // If hidden, Ctrl+O should unhide directly into full/expanded mode.
        ToolOutputDisplayMode::Hidden => ToolOutputDisplayMode::Expanded,
    };

    // Note: We don't reset reserved_lines here because force_tool_output_rerender
    // needs the old value to know how many lines to clear when collapsing.
    set_tool_output_display_mode(next_mode);

    next_mode == ToolOutputDisplayMode::Expanded
}

/// Acquire the viewport transition lock.
/// Returns a guard that must be held during viewport state changes.
pub(crate) fn lock_viewport_transition() -> std::sync::MutexGuard<'static, ()> {
    VIEWPORT_TRANSITION_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Reset the tool output viewport reserved lines.
/// Called after redraw_history to avoid stale reserved_lines causing
/// unwanted scrolling.
pub(crate) fn reset_tool_output_viewport() {
    if let Some(listener) = ACTIVE_LISTENER.get()
        && let Ok(mut state) = listener.state.lock()
    {
        state.tool_output.reset_reserved();
    }
}

/// Set the tool output reserved lines to a specific value.
/// Used after resize to sync with the space already reserved by history rendering.
pub(crate) fn set_tool_output_reserved(reserved: u16) {
    if let Some(listener) = ACTIVE_LISTENER.get()
        && let Ok(mut state) = listener.state.lock()
    {
        state.tool_output.reserved_lines = reserved;
    }
}

/// Check if tool output is actively streaming.
/// Uses try_lock to avoid deadlock when called while holding the output lock.
/// Returns true if the lock cannot be acquired (safe default for callers that
/// skip rendering when active).
pub(crate) fn is_tool_output_active() -> bool {
    let Some(listener) = ACTIVE_LISTENER.get() else {
        return false;
    };
    // Use try_lock to avoid deadlock - if streaming task holds this lock
    // while waiting for output lock, and we hold output lock, we'd deadlock.
    match listener.state.try_lock() {
        Ok(state) => state.tool_output.active,
        Err(_) => true, // Can't get lock, assume active (safe for skip-rendering callers)
    }
}

/// Check if tool output is actively streaming and being rendered in the live viewport.
///
/// When expanded, tool output is printed as normal scrolling output and is not
/// rendered in the viewport.
pub(crate) fn is_tool_output_viewport_active() -> bool {
    if is_tool_output_hidden() {
        return false;
    }
    if is_tool_output_expanded() {
        return false;
    }

    let Some(listener) = ACTIVE_LISTENER.get() else {
        return false;
    };

    match listener.state.try_lock() {
        Ok(state) => state.tool_output.active,
        Err(_) => true, // Can't get lock, assume active (safe for skip-rendering callers)
    }
}

/// Get the current tool output viewport height (including spacer) if active.
/// Returns 0 if tool output is not active.
pub(crate) fn active_tool_output_height() -> u16 {
    if is_tool_output_hidden() {
        return 0;
    }
    if is_tool_output_expanded() {
        return 0;
    }

    let Some(listener) = ACTIVE_LISTENER.get() else {
        return 0;
    };
    let Ok(state) = listener.state.lock() else {
        return 0;
    };
    if !state.tool_output.active {
        return 0;
    }

    let max_lines = tool_output_viewport_lines();
    let line_count = state.tool_output.line_count;
    let visible_lines = line_count.min(max_lines);

    // Add 1 for scroll indicator if there are hidden lines
    let has_scroll_indicator = line_count > max_lines || state.tool_output.total_lines > line_count;
    let viewport_height = if has_scroll_indicator {
        visible_lines + 1
    } else {
        visible_lines
    };

    let spacer = crate::cli::TOOL_OUTPUT_VIEWPORT_SPACER_LINES as usize;
    (viewport_height + spacer) as u16
}

/// Force a re-render of the tool output viewport with the current size settings.
/// Called when toggling viewport size during active streaming.
pub(crate) fn force_tool_output_rerender() {
    if is_tool_output_hidden() {
        return;
    }
    if is_tool_output_expanded() {
        return;
    }

    let Some(listener) = ACTIVE_LISTENER.get() else {
        return;
    };

    let (buffer, line_count, total_lines, old_reserved_lines) = {
        let Ok(state) = listener.state.lock() else {
            return;
        };
        if !state.tool_output.active {
            return;
        }
        (
            state.tool_output.buffer.clone(),
            state.tool_output.line_count,
            state.tool_output.total_lines,
            state.tool_output.reserved_lines,
        )
    };

    let max_lines = tool_output_viewport_lines();
    let width = CliListener::terminal_width();

    // Use fast tail extraction with hard wrapping for long lines
    let (tail_lines, hidden) =
        crate::cli::render::tail_lines_fast(&buffer, line_count, max_lines, Some(width));

    // Build visible lines with scrolled indicator at bottom
    let mut visible: Vec<String> = Vec::with_capacity(tail_lines.len() + 1);
    visible.extend(
        tail_lines
            .iter()
            .map(|line| crate::cli::render::style_tool_output_line(line)),
    );
    if hidden > 0 || total_lines > line_count {
        visible.push(crate::cli::render::style_tool_output_line(
            &crate::cli::render::format_scrolled_indicator(
                hidden,
                tail_lines.len(),
                Some(total_lines),
            ),
        ));
    }
    let viewport_height = visible.len() as u16;

    // Keep a spacer row between the tool viewport and where subsequent
    // output (✓/✗, messages, etc.) is printed.
    let spacer = crate::cli::TOOL_OUTPUT_VIEWPORT_SPACER_LINES;
    let new_reserved = viewport_height.saturating_add(spacer);

    // If collapsing (new size < old size), clear the extra lines first
    if new_reserved < old_reserved_lines {
        let lines_to_clear = old_reserved_lines - new_reserved;
        super::terminal::clear_viewport_lines(lines_to_clear, old_reserved_lines);
    }

    // Update reserved_lines in state
    if let Ok(mut state) = listener.state.lock() {
        state.tool_output.reserved_lines = new_reserved;
    }

    // If expanding, reserve additional space
    if new_reserved > old_reserved_lines {
        let reserve_delta = new_reserved - old_reserved_lines;
        let reserved = super::terminal::reserve_output_lines(reserve_delta);
        if !reserved {
            // Can't reserve space, fall back to simple output
            return;
        }
    }

    super::terminal::render_tool_viewport(&visible, viewport_height, spacer);
}

/// Get the current tool output viewport line count
pub(crate) fn tool_output_viewport_lines() -> usize {
    // Cap to available terminal space (leave room for prompt + status + spacer)
    // Prompt: ~4 lines, status: 1 line, spacer: 1 line, scroll indicator: 1 line
    let term_height = super::terminal::term_height() as usize;
    let reserved_for_ui = 8; // prompt + status + spacer + some buffer
    let available = term_height.saturating_sub(reserved_for_ui).max(1);

    crate::cli::TOOL_OUTPUT_VIEWPORT_LINES.min(available)
}

/// Render a single diff line with syntax highlighting.
/// Applies diff colors (green for +, red for -, cyan for @@) and overlays syntax highlighting
/// on the code content portion of the line.
fn render_diff_line(
    line: &str,
    language: Option<&str>,
    old_line_num: &mut usize,
    new_line_num: &mut usize,
) -> Option<String> {
    // Skip --- and +++ header lines
    if line.starts_with("+++") || line.starts_with("---") {
        return None;
    }

    // Parse hunk headers to update line numbers, but don't render them
    if line.starts_with("@@") {
        if let Some((old_start, new_start)) = parse_hunk_header(line) {
            *old_line_num = old_start;
            *new_line_num = new_start;
        }
        return None;
    }

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
        return Some(line.to_string());
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

    Some(result)
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

/// Initialize the global spinner task. Call once at startup.
pub(crate) fn init_spinner() {
    if SPINNER_TX.get().is_some() {
        return;
    }
    let (tx, rx) = watch::channel(0u8);
    if SPINNER_TX.set(tx).is_ok() {
        tokio::spawn(spinner_task(rx));
    }
}

/// Set spinner to "Ready" state
pub(crate) fn spinner_ready() {
    SPINNER_STATE.store(0, Ordering::Release);
    // Write the ready line immediately to avoid a brief period where the reserved
    // status-line rows are blank (which looks like an extra empty line).
    write_ready_status_line();
    if let Some(tx) = SPINNER_TX.get() {
        let _ = tx.send(0);
    }
}

fn write_status_line_for_spinner_state(state: u8, frame: Option<&str>) {
    update_total_tokens_display();
    let stats = build_stats_string();

    match state {
        0 => {
            let line = format_status_line(&"✓ Done".green().to_string(), stats.as_deref());
            terminal::write_status_line(&line);
        }
        1 => {
            let spinner = frame.unwrap_or("⠿");
            let left = format!("{} Working...", spinner.cyan());
            let line = format_status_line(&left, stats.as_deref());
            terminal::write_status_line(&line);
        }
        2 => {
            let spinner = frame.unwrap_or("⠿");
            let left = format!("{} {}", spinner.cyan(), "Thinking...".bright_black());
            let line = format_status_line(&left, stats.as_deref());
            terminal::write_status_line(&line);
        }
        _ => {}
    }
}

fn write_ready_status_line() {
    write_status_line_for_spinner_state(0, None);
}

/// Repaint the status line after a prompt/history redraw (e.g. resize).
pub(crate) fn redraw_status_line() {
    if terminal::streaming_status_line_reserved_rows() == 0 {
        return;
    }

    let state = SPINNER_STATE.load(Ordering::Acquire);
    write_status_line_for_spinner_state(state, None);
}

/// Set spinner to "Working" state
pub(crate) fn spinner_working() {
    let prev = SPINNER_STATE.swap(1, Ordering::AcqRel);
    if prev != 1
        && let Some(tx) = SPINNER_TX.get()
    {
        // Activate the status line row lazily so we don't reserve a blank line at startup.
        terminal::set_streaming_status_line_active(true);

        let _ = tx.send(1);
    }
}

/// Set spinner to "Thinking" state
pub(crate) fn spinner_thinking() {
    let prev = SPINNER_STATE.swap(2, Ordering::AcqRel);
    if prev != 2
        && let Some(tx) = SPINNER_TX.get()
    {
        // Activate the status line row lazily so we don't reserve a blank line at startup.
        terminal::set_streaming_status_line_active(true);

        let _ = tx.send(2);
    }
}

/// Start streaming - record start time and reset counters
fn start_streaming() {
    if let Ok(mut start) = STREAMING_START.lock()
        && start.is_none()
    {
        *start = Some(std::time::Instant::now());
    }
}

/// Latch current streaming stats into accumulated values (called on Waiting during agent loop)
fn latch_streaming_stats() {
    // Latch the current duration into accumulated
    if let Ok(mut start) = STREAMING_START.lock()
        && let Some(s) = start.take()
    {
        let elapsed_ms = s.elapsed().as_millis() as u64;
        let accumulated = ACCUMULATED_DURATION_MS.load(Ordering::Relaxed);
        ACCUMULATED_DURATION_MS.store(accumulated + elapsed_ms, Ordering::Relaxed);
    }
    // Clear the final duration marker since we're starting a new API call
    FINAL_DURATION_MS.store(0, Ordering::Relaxed);
}

/// Update context information during streaming
fn update_context(input_tokens: u64, context_limit: Option<u64>) {
    CONTEXT_TOKENS.store(input_tokens, Ordering::Relaxed);
    if let Some(limit) = context_limit {
        CONTEXT_LIMIT.store(limit, Ordering::Relaxed);
    }
}

/// Update total tokens during streaming
fn update_total_tokens(total: u64) {
    TOTAL_TOKENS.store(total, Ordering::Relaxed);
}

/// Update usage stats during streaming
fn update_usage_stats(input_tokens: u64, output_tokens: u64, cache_read_tokens: u64) {
    if input_tokens > 0 {
        INPUT_TOKENS.fetch_add(input_tokens, Ordering::Relaxed);
    }
    if output_tokens > 0 {
        OUTPUT_TOKENS.fetch_add(output_tokens, Ordering::Relaxed);
    }
    if cache_read_tokens > 0 {
        CACHE_READ_TOKENS.fetch_add(cache_read_tokens, Ordering::Relaxed);
    }

    let input_total = INPUT_TOKENS.load(Ordering::Relaxed);
    let output_total = OUTPUT_TOKENS.load(Ordering::Relaxed);
    TOTAL_TOKENS.store(input_total + output_total, Ordering::Relaxed);
}

/// Reset streaming state completely - call at the start of a new turn
pub(crate) fn reset_turn_stats() {
    if let Ok(mut start) = STREAMING_START.lock() {
        *start = None;
    }
    CONTEXT_TOKENS.store(0, Ordering::Relaxed);
    CONTEXT_LIMIT.store(0, Ordering::Relaxed);
    TOTAL_TOKENS.store(0, Ordering::Relaxed);
    TOTAL_TOKENS_DISPLAY.store(0, Ordering::Relaxed);
    INPUT_TOKENS.store(0, Ordering::Relaxed);
    OUTPUT_TOKENS.store(0, Ordering::Relaxed);
    CACHE_READ_TOKENS.store(0, Ordering::Relaxed);
    ACCUMULATED_DURATION_MS.store(0, Ordering::Relaxed);
    FINAL_DURATION_MS.store(0, Ordering::Relaxed);
}

/// Finalize streaming - capture the final duration and clear the start time
fn finalize_streaming() {
    if let Ok(mut start) = STREAMING_START.lock()
        && let Some(s) = start.take()
    {
        let elapsed_ms = s.elapsed().as_millis() as u64;
        let accumulated = ACCUMULATED_DURATION_MS.load(Ordering::Relaxed);
        let total_ms = accumulated + elapsed_ms;
        FINAL_DURATION_MS.store(total_ms, Ordering::Relaxed);
    } else {
        // No active streaming, use accumulated duration as final
        let accumulated = ACCUMULATED_DURATION_MS.load(Ordering::Relaxed);
        if accumulated > 0 {
            FINAL_DURATION_MS.store(accumulated, Ordering::Relaxed);
        }
    }
    // Force total tokens display to catch up to actual value
    let total = TOTAL_TOKENS.load(Ordering::Relaxed);
    TOTAL_TOKENS_DISPLAY.store(total, Ordering::Relaxed);
}

/// Get the current streaming duration in seconds (includes accumulated time from previous API calls)
fn get_streaming_duration() -> Option<f64> {
    // First check if we have a finalized duration
    let final_ms = FINAL_DURATION_MS.load(Ordering::Relaxed);
    if final_ms > 0 {
        return Some(final_ms as f64 / 1000.0);
    }
    // Otherwise compute accumulated + current elapsed
    let accumulated_ms = ACCUMULATED_DURATION_MS.load(Ordering::Relaxed);
    if let Ok(start) = STREAMING_START.lock()
        && let Some(s) = *start
    {
        let elapsed_ms = s.elapsed().as_millis() as u64;
        return Some((accumulated_ms + elapsed_ms) as f64 / 1000.0);
    }
    // No active streaming, but we might have accumulated time
    if accumulated_ms > 0 {
        Some(accumulated_ms as f64 / 1000.0)
    } else {
        None
    }
}

/// Build the stats string for the status line (right side)
fn build_stats_string() -> Option<String> {
    let duration = get_streaming_duration()?;

    let mut parts = vec![format!("{:.1}s", duration)];

    let ctx_tokens = CONTEXT_TOKENS.load(Ordering::Relaxed);
    let ctx_limit = CONTEXT_LIMIT.load(Ordering::Relaxed);

    if ctx_tokens > 0 && ctx_limit > 0 {
        let ctx_k = (ctx_tokens as f64 / 1000.0).round() as u64;
        let limit_k = ctx_limit / 1000;
        let pct = (ctx_tokens as f64 / ctx_limit as f64) * 100.0;
        parts.push(format!("ctx:{}k/{}k ({:.0}%)", ctx_k, limit_k, pct));
    } else if ctx_tokens > 0 {
        let ctx_k = (ctx_tokens as f64 / 1000.0).round() as u64;
        parts.push(format!("ctx:{}k", ctx_k));
    }

    let input_tokens = INPUT_TOKENS.load(Ordering::Relaxed);
    let output_tokens = OUTPUT_TOKENS.load(Ordering::Relaxed);
    let cache_read_tokens = CACHE_READ_TOKENS.load(Ordering::Relaxed);
    let total_display = TOTAL_TOKENS_DISPLAY.load(Ordering::Relaxed);
    let mut usage_parts = Vec::new();
    if input_tokens > 0 {
        usage_parts.push(format!("i:{}", format_tokens(input_tokens)));
    }
    if output_tokens > 0 {
        usage_parts.push(format!("o:{}", format_tokens(output_tokens)));
    }
    if cache_read_tokens > 0 {
        usage_parts.push(format!("c:{}", format_tokens(cache_read_tokens)));
    }
    if total_display > 0 {
        usage_parts.push(format!("t:{}", format_tokens(total_display)));
    }
    if !usage_parts.is_empty() {
        parts.push(usage_parts.join(" "));
    }

    Some(format!("[{}]", parts.join(" | ")))
}

/// Format token counts as compact strings using whole-number suffixes (e.g. 102k).
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{}m", tokens / 1_000_000)
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// Smoothly animate a display value towards a target value.
/// Returns the new display value.
fn animate_value(current_display: u64, target: u64) -> u64 {
    if current_display >= target {
        return target;
    }

    let diff = target - current_display;
    // For small gaps, increment by 1 or by a fraction of the gap
    // For larger gaps, increment faster to catch up within ~3-5 ticks
    let increment = if diff < 100 {
        // Small: increment by at least 1, up to 10% of diff
        (diff / 10).max(1)
    } else if diff < 1024 {
        // Medium: catch up in ~5 ticks
        diff / 5
    } else {
        // Large: catch up in ~3 ticks
        diff / 3
    }
    .max(1);

    (current_display + increment).min(target)
}

/// Animate total tokens display towards target value
fn update_total_tokens_display() {
    let target = TOTAL_TOKENS.load(Ordering::Relaxed);
    let current = TOTAL_TOKENS_DISPLAY.load(Ordering::Relaxed);
    let new_val = animate_value(current, target);
    TOTAL_TOKENS_DISPLAY.store(new_val, Ordering::Relaxed);
}

/// Build a status line with left text and optional right-aligned stats
fn format_status_line(left: &str, stats: Option<&str>) -> String {
    if let Some(stats_text) = stats {
        let width = terminal::term_width() as usize;
        // Calculate visible lengths (excluding ANSI escape codes)
        let left_visible = visible_length(left);
        let stats_visible = stats_text.len(); // stats has no ANSI codes

        let padding = width.saturating_sub(left_visible + stats_visible);
        if padding > 0 {
            format!(
                "{}{:>width$}",
                left,
                stats_text,
                width = padding + stats_visible
            )
        } else {
            // Not enough room for stats, just show left text
            left.to_string()
        }
    } else {
        left.to_string()
    }
}

/// Calculate the visible length of a string (excluding ANSI escape codes)
fn visible_length(s: &str) -> usize {
    let mut len = 0;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip ANSI escape sequence
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                for code in chars.by_ref() {
                    if ('@'..='~').contains(&code) {
                        break;
                    }
                }
            }
        } else {
            len += 1;
        }
    }
    len
}

/// Async spinner task that runs independently
async fn spinner_task(mut rx: watch::Receiver<u8>) {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut frame_idx = 0usize;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));

    loop {
        tokio::select! {
            result = rx.changed() => {
                if result.is_err() {
                    break;
                }
                let state = *rx.borrow_and_update();
                write_status_line_for_spinner_state(state, Some(FRAMES[frame_idx % FRAMES.len()]));
            }
            _ = interval.tick() => {
                let state = SPINNER_STATE.load(Ordering::Acquire);
                if state != 0 {
                    frame_idx = frame_idx.wrapping_add(1);
                    write_status_line_for_spinner_state(state, Some(FRAMES[frame_idx % FRAMES.len()]));
                }
            }
        }
    }
}

/// Word wrapper for streaming text output
struct WordWrapper {
    /// Current column position (0-indexed)
    column: usize,
    /// Buffer for current word being accumulated
    word_buffer: String,
    /// Whether we've output any content yet
    has_content: bool,
    /// Optional style to apply (e.g., dim gray for thinking)
    style: Option<&'static str>,
    /// Indentation for wrapped lines (0 for none, 2 for response text)
    indent: usize,
    /// Current line buffer for detecting code fences
    line_buffer: String,
    /// Whether we're inside a code block
    in_code_block: bool,
    /// Language for current code block (for syntax highlighting)
    code_language: Option<String>,
    /// Whether we're inside an inline code span
    in_inline_code: bool,
    /// Current bold delimiter if inside a bold span ('*' or '_')
    in_bold_span: Option<char>,
    /// Pending bold marker to detect ** or __
    pending_bold_marker: Option<char>,
    /// Buffered table rows for formatting
    table_buffer: Vec<String>,
    /// Whether we're currently buffering a table
    in_table: bool,
    /// Terminal width for table formatting
    table_width: usize,
    /// Whether the current line might be a table row (starts with |)
    /// We buffer the line and don't print until we know for sure
    maybe_table_row: bool,
    /// Whether the current line might be a markdown heading
    maybe_heading: bool,
    /// Optional spacing to apply right before the first rendered output.
    ///
    /// `Some(1)` means ensure a line break, `Some(2)` means ensure a blank line.
    pending_spacing_before_output: Option<usize>,
    /// Whether output was emitted since the last `take_emitted_output()`.
    emitted_output_since_check: bool,
}

impl WordWrapper {
    fn new(style: Option<&'static str>, indent: usize) -> Self {
        Self {
            column: 0,
            word_buffer: String::new(),
            has_content: false,
            style,
            indent,
            line_buffer: String::new(),
            in_code_block: false,
            code_language: None,
            in_inline_code: false,
            in_bold_span: None,
            pending_bold_marker: None,
            table_buffer: Vec::new(),
            in_table: false,
            table_width: 0,
            maybe_table_row: false,
            maybe_heading: false,
            pending_spacing_before_output: None,
            emitted_output_since_check: false,
        }
    }

    fn reset(&mut self) {
        self.column = 0;
        self.word_buffer.clear();
        self.has_content = false;
        self.line_buffer.clear();
        self.in_code_block = false;
        self.code_language = None;
        self.in_inline_code = false;
        self.in_bold_span = None;
        self.pending_bold_marker = None;
        self.flush_table(); // Flush any pending table
        self.table_buffer.clear();
        self.in_table = false;
        self.maybe_table_row = false;
        self.maybe_heading = false;
        self.pending_spacing_before_output = None;
        self.emitted_output_since_check = false;
    }

    fn set_pending_spacing_before_output(&mut self, min_newlines: usize) {
        let min_newlines = min_newlines.max(1);
        self.pending_spacing_before_output = Some(
            self.pending_spacing_before_output
                .map_or(min_newlines, |existing| existing.max(min_newlines)),
        );
    }

    fn take_emitted_output(&mut self) -> bool {
        std::mem::replace(&mut self.emitted_output_since_check, false)
    }

    fn apply_pending_spacing_before_output(&mut self) {
        let Some(min_newlines) = self.pending_spacing_before_output.take() else {
            return;
        };

        if min_newlines >= 2 {
            terminal::ensure_trailing_newlines(min_newlines.min(u8::MAX as usize) as u8);
        } else {
            terminal::ensure_line_break();
        }
    }

    fn emit_print(&mut self, text: &str) {
        self.apply_pending_spacing_before_output();
        terminal::print_above(text);
        self.emitted_output_since_check = true;
    }

    fn emit_println(&mut self, text: &str) {
        self.apply_pending_spacing_before_output();
        terminal::println_above(text);
        self.emitted_output_since_check = true;
    }

    fn needs_line_break(&self) -> bool {
        self.column > 0
            || !self.word_buffer.is_empty()
            || !self.line_buffer.is_empty()
            || self.in_table
    }

    fn finish_line(&mut self, width: usize) {
        if self.in_table {
            self.flush_table();
        }

        if self.in_code_block && !self.line_buffer.is_empty() {
            let highlighted = self.highlight_line(&self.line_buffer.clone());
            self.emit_println(&highlighted);
            self.line_buffer.clear();
            self.word_buffer.clear();
            self.column = 0;
            self.maybe_table_row = false;
            self.maybe_heading = false;
            self.in_inline_code = false;
            self.in_bold_span = None;
            self.pending_bold_marker = None;
            return;
        }

        self.flush_word(width);
        if self.column > 0 || !self.line_buffer.is_empty() {
            self.emit_print("\n");
        }
        self.column = 0;
        self.word_buffer.clear();
        self.line_buffer.clear();
        self.maybe_table_row = false;
        self.maybe_heading = false;
        self.in_inline_code = false;
        self.in_bold_span = None;
        self.pending_bold_marker = None;
    }

    /// Flush the word buffer, wrapping to new line if needed
    fn flush_word(&mut self, width: usize) {
        if self.word_buffer.is_empty() {
            return;
        }

        let word_width = display_width(&self.word_buffer);

        // If word doesn't fit on current line and we're not at the start, wrap first
        if self.column + word_width > width && self.column > self.indent {
            self.emit_print("\n");
            // Start new line with indent (with style if set)
            if self.indent > 0 {
                let indent_str = " ".repeat(self.indent);
                if let Some(style) = self.style {
                    self.emit_print(&format!("{}{}", style, indent_str));
                } else {
                    self.emit_print(&indent_str);
                }
            } else if let Some(style) = self.style {
                self.emit_print(style);
            }
            self.column = self.indent;
        }

        // Print the word (with style if set)
        if let Some(style) = self.style {
            let rendered = render_markdown_inlines_with_style(&self.word_buffer, Some(style));
            self.emit_print(&format!("{}{}\x1b[0m", style, rendered));
        } else {
            self.emit_print(&render_markdown_inlines(&self.word_buffer));
        }
        self.column += word_width;
        self.word_buffer.clear();
    }

    fn flush_table_inner(&mut self, trailing_newline: bool) {
        if self.table_buffer.is_empty() {
            return;
        }

        use super::markdown::align_markdown_tables;

        // Join the buffered lines and align the table
        let table_text = self.table_buffer.join("\n");
        let aligned = align_markdown_tables(&table_text, Some(self.table_width));

        // Output each line. When flushing at end-of-response, avoid printing a trailing newline on
        // the final row so we don't leave a visually blank row above the spacer/status area.
        let mut lines = aligned.lines().peekable();
        while let Some(line) = lines.next() {
            let is_last = lines.peek().is_none();
            if !trailing_newline && is_last {
                self.emit_print(line);
                self.column = display_width(line);
            } else {
                self.emit_println(line);
                self.column = 0;
            }
        }

        self.table_buffer.clear();
        self.in_table = false;
    }

    /// Flush the table buffer, formatting and outputting the table.
    fn flush_table(&mut self) {
        self.flush_table_inner(true);
    }

    fn flush_table_no_trailing_newline(&mut self) {
        self.flush_table_inner(false);
    }

    /// Check if a line looks like a table row (starts and ends with |)
    fn is_table_row(line: &str) -> bool {
        let trimmed = line.trim();
        trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() >= 3
    }

    /// Process a chunk of text with word wrapping.
    /// Code blocks are printed with syntax highlighting as each line completes.
    /// Table rows are buffered and only printed when the table is complete.
    fn process_text(&mut self, text: &str, width: usize) {
        for ch in text.chars() {
            if ch == '\r' {
                continue;
            }

            // Accumulate into line buffer to detect code fences and table rows
            if ch != '\n' {
                self.line_buffer.push(ch);
            }

            if !self.in_code_block {
                if ch == '`' {
                    self.in_inline_code = !self.in_inline_code;
                    self.pending_bold_marker = None;
                } else if !self.in_inline_code && (ch == '*' || ch == '_') {
                    if self.pending_bold_marker == Some(ch) {
                        if self.in_bold_span == Some(ch) {
                            self.in_bold_span = None;
                        } else {
                            self.in_bold_span = Some(ch);
                        }
                        self.pending_bold_marker = None;
                    } else {
                        self.pending_bold_marker = Some(ch);
                    }
                } else if !self.in_inline_code {
                    self.pending_bold_marker = None;
                }
            }

            // Check if this line might be a table row (starts with |)
            // We need to detect this early to avoid printing content prematurely
            if !self.in_code_block
                && !self.maybe_table_row
                && self.column == self.indent
                && self.line_buffer.trim_start().starts_with('|')
            {
                self.maybe_table_row = true;
            }
            if !self.in_code_block
                && self.style.is_none()
                && !self.maybe_table_row
                && !self.maybe_heading
                && self.column == self.indent
                && self.line_buffer.trim_start().starts_with('#')
            {
                self.maybe_heading = true;
            }

            if ch == '\n' {
                // Check if the line is a code fence
                let is_fence = self.line_buffer.trim().starts_with("```");

                if is_fence {
                    if self.in_code_block {
                        // Closing fence - just print it and reset state
                        self.in_code_block = false;
                        self.code_language = None;
                        self.emit_println("```");
                        self.column = 0;
                    } else {
                        // Opening fence - extract language and enter code block mode
                        self.word_buffer.clear();
                        self.in_code_block = true;
                        let lang = self
                            .line_buffer
                            .trim()
                            .strip_prefix("```")
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        self.code_language = if lang.is_empty() { None } else { Some(lang) };
                        let fence_line = self.line_buffer.clone();
                        self.emit_println(&fence_line);
                        self.column = 0;
                    }
                    self.line_buffer.clear();
                    self.maybe_table_row = false;
                    self.maybe_heading = false;
                    self.in_inline_code = false;
                    self.in_bold_span = None;
                    self.pending_bold_marker = None;
                    continue;
                }

                if self.in_code_block {
                    // Inside code block - print line with syntax highlighting
                    let highlighted = self.highlight_line(&self.line_buffer.clone());
                    self.emit_println(&highlighted);
                    self.line_buffer.clear();
                } else if self.maybe_heading {
                    if self.in_table {
                        self.flush_table();
                    }
                    let line = self.line_buffer.clone();
                    if super::markdown::is_heading_line(&line) {
                        self.flush_word(width);
                        if self.column > self.indent {
                            self.emit_print("\n");
                        }
                        self.emit_print(&render_markdown_line(&line));
                        self.emit_print("\n");
                        if self.indent > 0 {
                            let indent_str = " ".repeat(self.indent);
                            self.emit_print(&indent_str);
                        }
                        self.column = self.indent;
                    } else {
                        self.output_line_content(&line, width);
                        self.emit_print("\n");
                        if self.indent > 0 {
                            let indent_str = " ".repeat(self.indent);
                            self.emit_print(&indent_str);
                        }
                        self.column = self.indent;
                    }
                    self.line_buffer.clear();
                    self.word_buffer.clear();
                } else {
                    // Check if this line is a table row
                    let is_table_row = Self::is_table_row(&self.line_buffer);

                    if is_table_row {
                        // Confirmed table row - add to buffer
                        if !self.in_table {
                            // First table row - flush any pending content
                            self.flush_word(width);
                            if self.column > self.indent {
                                self.emit_print("\n");
                            }
                            self.table_width = width;
                        }
                        self.in_table = true;
                        self.table_buffer.push(self.line_buffer.clone());
                        self.line_buffer.clear();
                        self.word_buffer.clear();
                        self.column = 0;
                    } else if self.maybe_table_row {
                        // Line started with | but doesn't end with | - not a table row
                        // Need to output the buffered line content normally
                        if self.in_table {
                            // We were in a table, flush it first
                            self.flush_table();
                        }
                        // Print the line that turned out not to be a table row
                        self.output_line_content(&self.line_buffer.clone(), width);
                        self.emit_print("\n");
                        self.line_buffer.clear();
                        self.word_buffer.clear();
                        // Start new line with indent
                        if self.indent > 0 {
                            let indent_str = " ".repeat(self.indent);
                            if let Some(style) = self.style {
                                self.emit_print(&format!("{}{}", style, indent_str));
                            } else {
                                self.emit_print(&indent_str);
                            }
                        } else if let Some(style) = self.style {
                            self.emit_print(style);
                        }
                        self.column = self.indent;
                    } else if self.in_table {
                        // Non-table line while in table mode - flush table first
                        self.flush_table();
                        // Then process this line normally
                        self.flush_word(width);
                        self.emit_print("\n");
                        // Start new line with indent (with style if set)
                        if self.indent > 0 {
                            let indent_str = " ".repeat(self.indent);
                            if let Some(style) = self.style {
                                self.emit_print(&format!("{}{}", style, indent_str));
                            } else {
                                self.emit_print(&indent_str);
                            }
                        } else if let Some(style) = self.style {
                            self.emit_print(style);
                        }
                        self.column = self.indent;
                        self.line_buffer.clear();
                    } else {
                        // Normal text - flush word and handle newline
                        self.flush_word(width);

                        // Output newline and start next line with indent
                        self.emit_print("\n");
                        if self.indent > 0 {
                            let indent_str = " ".repeat(self.indent);
                            if let Some(style) = self.style {
                                self.emit_print(&format!("{}{}", style, indent_str));
                            } else {
                                self.emit_print(&indent_str);
                            }
                        } else if let Some(style) = self.style {
                            self.emit_print(style);
                        }
                        self.column = self.indent;
                        self.line_buffer.clear();
                    }
                }
                self.maybe_table_row = false;
                self.maybe_heading = false;
                self.in_inline_code = false;
                self.in_bold_span = None;
                self.pending_bold_marker = None;
            } else if self.in_code_block {
                // Inside code block - just accumulate in line_buffer (already done above)
            } else if self.maybe_table_row {
                // Potentially a table row - just accumulate in line_buffer, don't print yet
                // Content is already in line_buffer from above
            } else if self.maybe_heading {
                // Potential heading line - buffer until newline
            } else if ch == ' ' || ch == '\t' {
                // Normal whitespace handling
                if self.in_inline_code || self.in_bold_span.is_some() {
                    self.word_buffer.push(ch);
                } else {
                    self.flush_word(width);
                    if self.column > self.indent {
                        if let Some(style) = self.style {
                            self.emit_print(&format!("{} \x1b[0m", style));
                        } else {
                            self.emit_print(" ");
                        }
                        self.column += 1;
                    }
                }
            } else {
                // Regular character - accumulate in word buffer
                self.word_buffer.push(ch);
            }
        }
    }

    /// Output a line's content with word wrapping (used when a maybe-table-row turns out not to be)
    fn output_line_content(&mut self, line: &str, width: usize) {
        for ch in line.chars() {
            if ch == ' ' || ch == '\t' {
                self.flush_word(width);
                if self.column > self.indent {
                    if let Some(style) = self.style {
                        self.emit_print(&format!("{} \x1b[0m", style));
                    } else {
                        self.emit_print(" ");
                    }
                    self.column += 1;
                }
            } else {
                self.word_buffer.push(ch);
            }
        }
        self.flush_word(width);
    }

    /// Highlight a single line of code using the current code block language
    fn highlight_line(&self, line: &str) -> String {
        let lang = self.code_language.as_deref();
        let spans = syntax::highlight_code(line, lang);

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputState {
    Idle,
    Thinking { has_output: bool },
    Text { has_output: bool },
    ToolBlock,
}

impl OutputState {
    fn start_thinking(&mut self) {
        if matches!(self, OutputState::Idle | OutputState::Thinking { .. }) {
            *self = OutputState::Thinking { has_output: false };
        }
    }

    fn mark_thinking_output(&mut self) {
        match self {
            OutputState::Thinking { has_output } => *has_output = true,
            OutputState::Idle => *self = OutputState::Thinking { has_output: true },
            _ => {}
        }
    }

    fn end_thinking(&mut self) -> bool {
        match *self {
            OutputState::Thinking { has_output } => {
                *self = OutputState::Idle;
                has_output
            }
            _ => false,
        }
    }

    fn start_text(&mut self) {
        if matches!(
            self,
            OutputState::Idle | OutputState::Thinking { .. } | OutputState::Text { .. }
        ) {
            *self = OutputState::Text { has_output: false };
        }
    }

    fn mark_text_output(&mut self) {
        match self {
            OutputState::Text { has_output } => *has_output = true,
            OutputState::Idle => *self = OutputState::Text { has_output: true },
            OutputState::Thinking { .. } => *self = OutputState::Text { has_output: true },
            _ => {}
        }
    }

    fn end_text(&mut self) -> bool {
        match *self {
            OutputState::Text { has_output } => {
                *self = OutputState::Idle;
                has_output
            }
            _ => false,
        }
    }

    fn start_tool_block(&mut self) -> bool {
        let needs_spacing = matches!(
            *self,
            OutputState::Thinking { has_output: true } | OutputState::Text { has_output: true }
        );
        *self = OutputState::ToolBlock;
        needs_spacing
    }

    fn end_tool_block(&mut self) {
        if matches!(*self, OutputState::ToolBlock) {
            *self = OutputState::Idle;
        }
    }
}

/// State for streaming output
struct StreamState {
    /// Word wrapper for thinking text (dim style)
    thinking: WordWrapper,
    /// Word wrapper for response text (no style)
    response: WordWrapper,
    /// Last logical block that emitted visible output.
    last_block: Option<LastBlock>,
    /// Trailing newlines seen in thinking deltas that we haven't rendered yet.
    ///
    /// Some providers/models (notably Gemini via antigravity) tend to emit one or more `\n`
    /// at the end of thinking. If we render those eagerly, we end up with a dangling empty line
    /// right above the streaming status line.
    ///
    /// We buffer these newlines and only render them once we see subsequent non-newline thinking
    /// content. When thinking ends, we discard the buffered newlines.
    thinking_pending_newlines: usize,
    /// Whether any assistant text output was printed since the last reset.
    text_output_written: bool,
    /// Current output state for spacing/blocks
    output_state: OutputState,
    /// Buffered output events during resize redraws
    buffered_events: Vec<OutputEvent>,
    /// Whether a diff was just shown (to skip redundant checkmark in ToolResult)
    diff_shown: bool,
    /// Whether the last output line is a tool call awaiting its checkmark
    last_tool_call_open: bool,
    /// Whether we're inside a <Tool>...</Tool> block
    in_tool_block: bool,
    /// Tool output viewport state
    tool_output: ToolOutputState,
    /// Whether an info message was printed inside a tool block since the last tool call banner.
    ///
    /// Some info lines (e.g. "[LSP activated...]") can be emitted by tools and end up visually
    /// adjacent to a subsequent tool call once the previous tool result checkmark is rendered.
    /// Track this explicitly so we can insert an extra blank line for readability.
    info_since_last_tool_call: bool,
}

impl StreamState {
    fn new() -> Self {
        Self {
            thinking: WordWrapper::new(Some("\x1b[3;90m"), 0),
            response: WordWrapper::new(None, 0),
            last_block: None,
            thinking_pending_newlines: 0,
            text_output_written: false,
            output_state: OutputState::Idle,
            buffered_events: Vec::new(),
            diff_shown: false,
            last_tool_call_open: false,
            in_tool_block: false,
            tool_output: ToolOutputState::new(),
            info_since_last_tool_call: false,
        }
    }

    fn reset(&mut self) {
        self.thinking.reset();
        self.response.reset();
        self.thinking_pending_newlines = 0;
        self.text_output_written = false;
        self.output_state = OutputState::Idle;
        self.buffered_events.clear();
        self.diff_shown = false;
        self.last_tool_call_open = false;
        self.in_tool_block = false;
        self.tool_output.reset();
        self.info_since_last_tool_call = false;
    }
}

#[derive(Debug, Default)]
struct ToolOutputState {
    active: bool,
    /// Set when ToolResult is received - prevents late-arriving output from rendering
    completed: bool,
    buffer: String,
    reserved_lines: u16,
    /// Running count of complete lines (newlines seen) - includes truncated lines
    total_lines: usize,
    /// Lines currently in buffer (may be less than total_lines after truncation)
    line_count: usize,
}

impl ToolOutputState {
    fn new() -> Self {
        Self {
            active: false,
            completed: false,
            buffer: String::new(),
            reserved_lines: 0,
            total_lines: 0,
            line_count: 0,
        }
    }

    fn reset(&mut self) {
        self.active = false;
        self.completed = false;
        self.buffer.clear();
        self.reserved_lines = 0;
        self.total_lines = 0;
        self.line_count = 0;
    }

    /// Mark the tool as completed - late-arriving output will be ignored
    fn mark_completed(&mut self) {
        self.completed = true;
        self.active = false;
    }

    /// Reset only the reserved lines count without clearing the buffer.
    /// Used when toggling viewport size to force re-reservation.
    fn reset_reserved(&mut self) {
        self.reserved_lines = 0;
    }

    /// Append text to buffer and update line count.
    fn append(&mut self, text: &str) {
        // Count newlines in the new text
        let new_lines = text.bytes().filter(|&b| b == b'\n').count();
        self.total_lines += new_lines;
        self.line_count += new_lines;
        self.buffer.push_str(text);
    }

    /// Truncate buffer to keep only the last `max_lines` lines.
    /// Updates line_count to reflect the truncated state (total_lines preserved).
    fn truncate_to_last_lines(&mut self, max_lines: usize) {
        if self.line_count <= max_lines {
            return;
        }

        // Find the byte offset where we should start keeping content
        let lines_to_skip = self.line_count - max_lines;
        let mut skipped = 0;
        let mut start_offset = 0;

        for (i, c) in self.buffer.char_indices() {
            if c == '\n' {
                skipped += 1;
                if skipped >= lines_to_skip {
                    start_offset = i + 1;
                    break;
                }
            }
        }

        if start_offset > 0 && start_offset < self.buffer.len() {
            self.buffer = self.buffer[start_offset..].to_string();
            self.line_count = max_lines;
            // total_lines is preserved to show how many lines were seen overall
        }
    }
}

/// Get display width of a string (accounting for Unicode width)
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Output listener for CLI mode
pub(crate) struct CliListener {
    state: Mutex<StreamState>,
}

impl CliListener {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(StreamState::new()),
        }
    }

    pub(crate) fn register_active(&'static self) {
        let _ = ACTIVE_LISTENER.set(self);
    }

    pub(crate) fn note_user_prompt_printed() {
        if let Some(listener) = ACTIVE_LISTENER.get()
            && let Ok(mut state) = listener.state.lock()
        {
            state.last_block = Some(LastBlock::UserPrompt);
        }
    }

    pub(crate) fn buffer_event(event: &OutputEvent) {
        if let Some(listener) = ACTIVE_LISTENER.get()
            && let Ok(mut state) = listener.state.lock()
        {
            state.buffered_events.push(event.clone());
        }
    }

    pub(crate) fn flush_buffered() {
        if let Some(listener) = ACTIVE_LISTENER.get() {
            listener.flush_buffered_events();
        }
    }

    fn flush_buffered_events(&self) {
        let events = if let Ok(mut state) = self.state.lock() {
            if state.buffered_events.is_empty() {
                return;
            }
            std::mem::take(&mut state.buffered_events)
        } else {
            return;
        };

        for event in events {
            self.handle_event(&event);
        }
    }

    /// Terminal width for content rendering, reduced by 2 to prevent wrapping artifacts.
    fn terminal_width() -> usize {
        terminal::term_width().saturating_sub(2).max(1) as usize
    }

    /// Close an open tool block if one exists.
    ///
    /// If `ensure_line_break` is true, a line break is inserted if output is currently mid-line.
    /// This is useful when we're about to print more output and want it to start on a fresh line.
    ///
    /// For end-of-turn cleanup (e.g. `Done`), we intentionally avoid forcing a line break so we
    /// don't leave a trailing empty line above the prompt.
    fn close_tool_block_inner(&self, ensure_line_break: bool) {
        let was_in_block = self
            .state
            .lock()
            .map(|mut s| {
                let was = s.in_tool_block;
                if was {
                    s.in_tool_block = false;
                    s.output_state.end_tool_block();
                }
                was
            })
            .unwrap_or(false);
        if was_in_block {
            if ensure_line_break {
                terminal::ensure_line_break();
            }
            history::push(HistoryEvent::ToolEnd);
        }
    }

    fn close_tool_block(&self) {
        self.close_tool_block_inner(true);
    }

    fn close_tool_block_no_line_break(&self) {
        self.close_tool_block_inner(false);
    }

    fn handle_event(&self, event: &OutputEvent) {
        match event {
            OutputEvent::ThinkingStart => {
                self.close_tool_block();
                let Ok(mut state) = self.state.lock() else {
                    return;
                };
                if matches!(state.output_state, OutputState::Idle) {
                    state.output_state.start_thinking();
                }
                spinner_thinking();
                // Spaced mode: no tag printed
            }

            OutputEvent::Thinking(text) => {
                // Skip empty thinking events
                if text.is_empty() {
                    return;
                }

                let width = Self::terminal_width();
                let Ok(mut state) = self.state.lock() else {
                    // Fallback: print without word wrap
                    terminal::print_above(&text.bright_black().to_string());
                    return;
                };

                // Accumulate for history
                history::append_thinking(text);

                // Buffer trailing newlines in thinking. Some providers/models emit one or more
                // `\n` at the end of thinking; rendering those eagerly leaves an empty line above
                // the streaming status line.
                let bytes = text.as_bytes();
                let mut cut = bytes.len();
                let mut trailing_newlines = 0usize;
                while cut > 0 {
                    match bytes[cut - 1] {
                        b'\n' => {
                            trailing_newlines += 1;
                            cut -= 1;
                        }
                        b'\r' => {
                            cut -= 1;
                        }
                        _ => break,
                    }
                }
                let main = &text[..cut];

                if !main.is_empty() {
                    // Set up lazy spacing before the first thinking output. The
                    // spacing only materializes when `emit_print`/`emit_println`
                    // actually renders content, avoiding phantom blank lines.
                    if !state.thinking.has_content {
                        let min_newlines =
                            if needs_blank_line_before(state.last_block, LastBlock::Thinking) {
                                2
                            } else {
                                1
                            };
                        state
                            .thinking
                            .set_pending_spacing_before_output(min_newlines);
                    }

                    // Flush any previously buffered trailing newlines now that we have more content.
                    if state.thinking_pending_newlines > 0 {
                        let pending = "\n".repeat(state.thinking_pending_newlines);
                        state.thinking.process_text(&pending, width);
                        state.thinking_pending_newlines = 0;
                    }

                    state.thinking.process_text(main, width);
                }

                state.thinking_pending_newlines = state
                    .thinking_pending_newlines
                    .saturating_add(trailing_newlines);

                let did_output = state.thinking.take_emitted_output();
                if did_output {
                    state.thinking.has_content = true;
                    state.last_block = Some(LastBlock::Thinking);
                }

                if matches!(
                    state.output_state,
                    OutputState::Idle | OutputState::Thinking { .. }
                ) {
                    state.output_state.start_thinking();
                    if did_output {
                        state.output_state.mark_thinking_output();
                    }
                }
            }

            OutputEvent::ThinkingEnd => {
                let Ok(mut state) = self.state.lock() else {
                    return;
                };

                // Leave any buffered trailing newlines pending. If response text arrives next,
                // we'll flush them there so the text starts on the expected line. If the turn
                // ends without text, they'll be discarded on `Done`.

                let width = Self::terminal_width();
                state.thinking.flush_word(width);

                // flush_word may have emitted content that was buffered in the
                // word wrapper (e.g. an entire bold span like "**some text**"
                // where spaces don't trigger flushes). Account for that now so
                // the placeholder check below sees the correct state.
                if state.thinking.take_emitted_output() {
                    state.thinking.has_content = true;
                    state.output_state.mark_thinking_output();
                    state.last_block = Some(LastBlock::Thinking);
                }

                // If a thinking block started but never received any text, render a
                // visible placeholder so the user can see it existed.
                if matches!(
                    state.output_state,
                    OutputState::Thinking { has_output: false }
                ) && !state.thinking.has_content
                {
                    // Ensure the placeholder becomes its own visual block instead of
                    // appearing adjacent to the user prompt/tool line.
                    if needs_blank_line_before(state.last_block, LastBlock::Thinking) {
                        terminal::ensure_trailing_newlines(2);
                    } else {
                        terminal::ensure_line_break();
                    }

                    terminal::println_above("\x1b[3;90mThinking...\x1b[0m");
                    history::append_thinking("Thinking...");
                    state.thinking_pending_newlines = 0;
                    state.output_state.mark_thinking_output();
                    state.last_block = Some(LastBlock::Thinking);
                }

                history::finish_thinking();

                // End style reset (if content was printed)
                if state.thinking.has_content {
                    terminal::print_above("\x1b[0m");
                }

                if state.output_state.end_thinking() {
                    history::push(HistoryEvent::ThinkingEnd);
                }

                state.thinking.reset();
                // Switch to working state after thinking ends
                spinner_working();
            }

            OutputEvent::Text(text) => {
                // Skip empty text events
                if text.is_empty() {
                    return;
                }

                self.close_tool_block();

                let width = Self::terminal_width();
                let Ok(mut state) = self.state.lock() else {
                    // Fallback: print directly
                    print!("{}", text);
                    let _ = io::stdout().flush();
                    return;
                };

                // Flush any buffered trailing newlines from thinking now that response text is
                // arriving. These are real model-emitted newlines; we just delayed rendering them
                // to avoid leaving a dangling blank row above the status line when nothing follows.
                if state.thinking_pending_newlines > 0 {
                    terminal::print_above(&"\n".repeat(state.thinking_pending_newlines));
                    state.thinking_pending_newlines = 0;
                }

                // Ensure response text starts on the correct line.
                if !state.text_output_written {
                    let min_newlines = if needs_blank_line_before(state.last_block, LastBlock::Text)
                    {
                        2
                    } else {
                        1
                    };
                    state
                        .response
                        .set_pending_spacing_before_output(min_newlines);
                }

                state.response.process_text(text, width);
                let did_output = state.response.take_emitted_output();
                if did_output {
                    state.text_output_written = true;
                    state.last_block = Some(LastBlock::Text);
                }
                history::append_assistant_text(text);
                state.output_state.start_text();
                if did_output {
                    state.output_state.mark_text_output();
                }
            }

            OutputEvent::TextEnd => {
                // If the model ended its response without emitting any text events,
                // we still want to treat this as a transition away from any open
                // tool output so the placeholder renders as its own block.
                self.close_tool_block();

                let Ok(mut state) = self.state.lock() else {
                    return;
                };

                // If a text block ended without writing any output, render a visible
                // placeholder so we can see it existed.
                if !state.text_output_written {
                    // Ensure the placeholder becomes its own visual block instead of
                    // appearing adjacent to the user prompt/tool line.
                    if needs_blank_line_before(state.last_block, LastBlock::Text) {
                        terminal::ensure_trailing_newlines(2);
                    } else {
                        terminal::ensure_line_break();
                    }

                    terminal::println_above("[text with no data]");
                    history::append_assistant_text("[text with no data]");
                    state.last_block = Some(LastBlock::Text);
                }

                // Flush any remaining word buffer
                let width = Self::terminal_width();
                state.response.flush_word(width);

                // If we were buffering a markdown table, flush it. When the model ends mid-table
                // (no trailing newline), avoid printing an additional trailing newline on the final
                // table row so we don't leave an extra blank row above the spacer/status area.
                if state.response.in_table {
                    state.response.flush_table_no_trailing_newline();
                }

                // Handle incomplete code block: just print closing fence
                if state.response.in_code_block {
                    terminal::println_above("```");
                    state.response.in_code_block = false;
                }

                if state.output_state.end_text() {
                    history::push(HistoryEvent::ResponseEnd);
                }
                history::finish_assistant_text();
                state.reset();
            }

            OutputEvent::ToolCall { description } => {
                spinner_working();
                let starting_block = if let Ok(mut state) = self.state.lock() {
                    let width = Self::terminal_width();

                    if state.thinking.needs_line_break() {
                        state.thinking.finish_line(width);
                    }
                    if state.response.needs_line_break() {
                        state.response.finish_line(width);
                    }
                    if state.thinking_pending_newlines > 0 {
                        terminal::print_above(&"\n".repeat(state.thinking_pending_newlines));
                        state.thinking_pending_newlines = 0;
                    }

                    let starting = !state.in_tool_block;

                    if starting {
                        // Spacing rules: insert a blank line between blocks where it
                        // improves readability.
                        if needs_blank_line_before(state.last_block, LastBlock::ToolCall) {
                            terminal::ensure_trailing_newlines(2);
                        } else {
                            terminal::ensure_line_break();
                        }

                        state.output_state.start_tool_block();
                        state.in_tool_block = true;
                    } else {
                        // Subsequent tool call within the same tool block. Use the
                        // same spacing rules as all other block transitions.
                        if needs_blank_line_before(state.last_block, LastBlock::ToolCall) {
                            terminal::ensure_trailing_newlines(2);
                        } else {
                            terminal::ensure_line_break();
                        }
                    }

                    // Reset tool output state for the new tool
                    state.tool_output.reset();

                    state.info_since_last_tool_call = false;

                    let text = format!("{}{}", super::TOOL_USE_PREFIX, description);
                    terminal::print_above(&text);
                    state.last_tool_call_open = true;
                    state.last_block = Some(LastBlock::ToolCall);
                    starting
                } else {
                    false
                };
                if starting_block {
                    history::push(HistoryEvent::ToolStart);
                }
                history::push(HistoryEvent::ToolUse {
                    description: description.clone(),
                });
            }

            OutputEvent::ToolResult {
                tool_name,
                is_error,
                error_preview,
                exit_code,
                summary,
            } => {
                // Acquire viewport lock to prevent racing with streaming output
                let _viewport_guard = VIEWPORT_TRANSITION_LOCK
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());

                let pending_tool_line = self
                    .state
                    .lock()
                    .map(|mut s| {
                        let pending = s.last_tool_call_open;
                        s.last_tool_call_open = false;
                        pending
                    })
                    .unwrap_or(false);

                if !pending_tool_line
                    && let Ok(mut state) = self.state.lock()
                    && !state.in_tool_block
                {
                    let width = Self::terminal_width();
                    if state.thinking.needs_line_break() {
                        state.thinking.finish_line(width);
                    }
                    if state.response.needs_line_break() {
                        state.response.finish_line(width);
                    }
                    if terminal::output_cursor_col() != 0 {
                        terminal::print_above("\n");
                    }
                }

                let diff_shown = self
                    .state
                    .lock()
                    .map(|mut s| {
                        let shown = s.diff_shown;
                        s.diff_shown = false;
                        shown
                    })
                    .unwrap_or(false);

                let tool_output_active = self
                    .state
                    .lock()
                    .map(|s| s.tool_output.active)
                    .unwrap_or(false);

                let exit_code_suffix = if *is_error && tool_name == "bash" {
                    match (exit_code.as_ref(), summary.as_deref()) {
                        (Some(code), Some(summary))
                            if !summary.to_ascii_lowercase().contains("exit code") =>
                        {
                            format!(" {}", format!("(exit code {})", code).bright_black())
                        }
                        (Some(code), None) => {
                            format!(" {}", format!("(exit code {})", code).bright_black())
                        }
                        _ => String::new(),
                    }
                } else {
                    String::new()
                };

                let summary_suffix = format_summary_suffix(summary.as_deref());
                // Leaving the cursor at end-of-line prevents a "dangling" empty line
                // above the prompt when a turn ends with just a tool call.
                // If there's a summary and we're still on the tool header line,
                // force a newline so the summary appears on its own line.
                // Also force newline when the tool output viewport is active.
                let force_newline =
                    pending_tool_line && (!summary_suffix.is_empty() || tool_output_active);

                let text = if *is_error {
                    if pending_tool_line && !force_newline {
                        format!(" {}{}{}", "✗".red(), exit_code_suffix, summary_suffix)
                    } else {
                        format!("{}{}{}", "✗".red(), exit_code_suffix, summary_suffix)
                    }
                } else if diff_shown {
                    // Diff already showed checkmark.
                    String::new()
                } else if pending_tool_line && !force_newline {
                    format!(" {}{}", "✓".green(), summary_suffix)
                } else {
                    format!("{}{}", "✓".green(), summary_suffix)
                };

                if !text.is_empty() {
                    if !pending_tool_line || force_newline {
                        if terminal::prompt_visible() {
                            if tool_output_active {
                                terminal::ensure_line_break();
                            } else if force_newline {
                                terminal::print_above("\n");
                            }
                        } else {
                            // In batch mode, tool output is printed directly. Make sure the
                            // checkmark starts on its own line even if the tool output did not
                            // end with a newline.
                            println!();
                            let _ = io::stdout().flush();
                        }
                    }
                    terminal::print_above(&text);
                }

                if let Ok(mut state) = self.state.lock() {
                    state.last_block = Some(LastBlock::ToolContent);
                }

                history::push(HistoryEvent::ToolResult {
                    output: error_preview.clone().unwrap_or_default(),
                    is_error: *is_error,
                    summary: summary.clone(),
                });

                // Mark as completed to prevent late-arriving output from rendering
                if let Ok(mut state) = self.state.lock() {
                    state.tool_output.mark_completed();
                }
            }

            OutputEvent::ToolOutput { text } => {
                if text.is_empty() {
                    return;
                }

                // Check if tool has already completed (late-arriving output after interrupt)
                let is_completed = self
                    .state
                    .lock()
                    .map(|s| s.tool_output.completed)
                    .unwrap_or(false);
                if is_completed {
                    // Still record to history but don't render
                    history::append_tool_output(text);
                    return;
                }

                // If hiding tool output, skip rendering but still record to history
                if is_tool_output_hidden() {
                    // Still mark tool output as active so ToolResult rendering works correctly
                    if let Ok(mut state) = self.state.lock() {
                        state.last_tool_call_open = false;
                        if !state.tool_output.active {
                            state.tool_output.active = true;
                        }
                        // Keep the last N lines so toggling visibility can show recent output
                        state.tool_output.append(text);
                        state
                            .tool_output
                            .truncate_to_last_lines(crate::cli::TOOL_OUTPUT_MAX_BUFFER_LINES);
                    }
                    // Record to history so toggling the setting can show it later
                    history::append_tool_output(text);
                    return;
                }

                // Acquire viewport transition lock to prevent racing with toggle
                let _viewport_guard = VIEWPORT_TRANSITION_LOCK
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());

                let expanded = is_tool_output_expanded();

                // EXPANDED MODE: Print tool output as normal scrolling text.
                // This allows the user to scroll back through up to the last 2000 buffered lines
                // (and more if their terminal scrollback retains it).
                if expanded {
                    {
                        let Ok(mut state) = self.state.lock() else {
                            return;
                        };

                        state.last_tool_call_open = false;

                        if !state.tool_output.active {
                            state.tool_output.active = true;
                        }

                        state.tool_output.append(text);
                        state
                            .tool_output
                            .truncate_to_last_lines(crate::cli::TOOL_OUTPUT_MAX_BUFFER_LINES);
                    }

                    // Render complete lines only (leave partial trailing line buffered).
                    let mut rendered = String::new();
                    for chunk in text.split_inclusive('\n') {
                        let Some(line) = chunk.strip_suffix('\n') else {
                            continue;
                        };
                        rendered.push_str(&crate::cli::render::style_tool_output_line(line));
                        rendered.push('\n');
                    }

                    if !rendered.is_empty() {
                        terminal::ensure_line_break();
                        terminal::print_above(&rendered);
                    }

                    history::append_tool_output(text);
                    if let Ok(mut state) = self.state.lock() {
                        state.last_block = Some(LastBlock::ToolContent);
                    }
                    return;
                }

                // VIEWPORT MODE: Show only the last N lines in a fixed viewport.
                let has_trailing_newline = text.ends_with('\n');

                {
                    let Ok(mut state) = self.state.lock() else {
                        return;
                    };

                    state.last_tool_call_open = false;

                    if !state.tool_output.active {
                        state.tool_output.active = true;
                    }

                    state.tool_output.append(text);

                    // Truncate buffer to save memory.
                    state
                        .tool_output
                        .truncate_to_last_lines(crate::cli::TOOL_OUTPUT_MAX_BUFFER_LINES);
                }

                // Only render viewport on complete lines
                if has_trailing_newline {
                    let (reserve_delta, visible_lines, viewport_height, spacer_lines) = {
                        let Ok(mut state) = self.state.lock() else {
                            return;
                        };

                        let max_lines = tool_output_viewport_lines();
                        let buffer_lines = state.tool_output.line_count;
                        let total_seen = state.tool_output.total_lines;
                        let width = Self::terminal_width();

                        let (tail_lines, hidden) = crate::cli::render::tail_lines_fast(
                            &state.tool_output.buffer,
                            buffer_lines,
                            max_lines,
                            Some(width),
                        );

                        let mut visible: Vec<String> = Vec::with_capacity(tail_lines.len() + 1);
                        visible.extend(
                            tail_lines
                                .iter()
                                .map(|line| crate::cli::render::style_tool_output_line(line)),
                        );
                        if hidden > 0 || total_seen > buffer_lines {
                            visible.push(crate::cli::render::style_tool_output_line(
                                &crate::cli::render::format_scrolled_indicator(
                                    hidden,
                                    tail_lines.len(),
                                    Some(total_seen),
                                ),
                            ));
                        }
                        let viewport_height = visible.len() as u16;

                        let spacer = crate::cli::TOOL_OUTPUT_VIEWPORT_SPACER_LINES;
                        let desired_total = viewport_height.saturating_add(spacer);
                        let reserve_delta =
                            desired_total.saturating_sub(state.tool_output.reserved_lines);
                        state.tool_output.reserved_lines = desired_total;

                        (reserve_delta, visible, viewport_height, spacer)
                    };

                    if reserve_delta > 0 {
                        terminal::reserve_output_lines(reserve_delta);
                    }

                    terminal::render_tool_viewport(&visible_lines, viewport_height, spacer_lines);
                }

                history::append_tool_output(text);

                if let Ok(mut state) = self.state.lock() {
                    state.last_block = Some(LastBlock::ToolContent);
                }
            }

            OutputEvent::FileReadOutput { filename, text } => {
                if text.is_empty() {
                    return;
                }

                // Check if tool has already completed (late-arriving output after interrupt)
                let is_completed = self
                    .state
                    .lock()
                    .map(|s| s.tool_output.completed)
                    .unwrap_or(false);
                if is_completed {
                    // Still record to history but don't render
                    history::append_file_read_output(filename, text);
                    return;
                }

                // If hiding tool output, skip rendering but still record to history
                if is_tool_output_hidden() {
                    // Still mark tool output as active so ToolResult rendering works correctly
                    if let Ok(mut state) = self.state.lock() {
                        state.last_tool_call_open = false;
                        if !state.tool_output.active {
                            state.tool_output.active = true;
                        }
                        // Keep the last N lines so toggling visibility can show recent output
                        state.tool_output.append(text);
                        state
                            .tool_output
                            .truncate_to_last_lines(crate::cli::TOOL_OUTPUT_MAX_BUFFER_LINES);
                    }
                    // Record to history so toggling the setting can show it later
                    history::append_file_read_output(filename, text);
                    return;
                }

                // Acquire viewport transition lock to prevent racing with toggle
                let _viewport_guard = VIEWPORT_TRANSITION_LOCK
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());

                let language = syntax::language_from_path(filename);

                let expanded = is_tool_output_expanded();

                // EXPANDED MODE: Print output as normal scrolling text.
                if expanded {
                    {
                        let Ok(mut state) = self.state.lock() else {
                            return;
                        };

                        state.last_tool_call_open = false;

                        if !state.tool_output.active {
                            state.tool_output.active = true;
                        }

                        state.tool_output.append(text);
                        state
                            .tool_output
                            .truncate_to_last_lines(crate::cli::TOOL_OUTPUT_MAX_BUFFER_LINES);
                    }

                    // Render complete lines only (leave partial trailing line buffered).
                    let mut rendered = String::new();
                    for chunk in text.split_inclusive('\n') {
                        let Some(line) = chunk.strip_suffix('\n') else {
                            continue;
                        };
                        rendered.push_str(&style_file_read_line(line, language.as_deref()));
                        rendered.push('\n');
                    }

                    if !rendered.is_empty() {
                        terminal::ensure_line_break();
                        terminal::print_above(&rendered);
                    }

                    history::append_file_read_output(filename, text);
                    if let Ok(mut state) = self.state.lock() {
                        state.last_block = Some(LastBlock::ToolContent);
                    }
                    return;
                }

                // VIEWPORT MODE: Show only the last N lines in a fixed viewport.
                let has_trailing_newline = text.ends_with('\n');

                {
                    let Ok(mut state) = self.state.lock() else {
                        return;
                    };

                    state.last_tool_call_open = false;

                    if !state.tool_output.active {
                        state.tool_output.active = true;
                    }

                    state.tool_output.append(text);

                    // Truncate buffer to save memory.
                    state
                        .tool_output
                        .truncate_to_last_lines(crate::cli::TOOL_OUTPUT_MAX_BUFFER_LINES);
                }

                if has_trailing_newline {
                    let (reserve_delta, visible_lines, viewport_height, spacer_lines) = {
                        let Ok(mut state) = self.state.lock() else {
                            return;
                        };

                        let max_lines = tool_output_viewport_lines();
                        let buffer_lines = state.tool_output.line_count;
                        let total_seen = state.tool_output.total_lines;
                        let width = Self::terminal_width();

                        let (tail_lines, hidden) = crate::cli::render::tail_lines_fast(
                            &state.tool_output.buffer,
                            buffer_lines,
                            max_lines,
                            Some(width),
                        );

                        let mut visible: Vec<String> = Vec::with_capacity(tail_lines.len() + 1);
                        visible.extend(
                            tail_lines
                                .iter()
                                .map(|line| style_file_read_line(line, language.as_deref())),
                        );
                        if hidden > 0 || total_seen > buffer_lines {
                            visible.push(crate::cli::render::style_tool_output_line(
                                &crate::cli::render::format_scrolled_indicator(
                                    hidden,
                                    tail_lines.len(),
                                    Some(total_seen),
                                ),
                            ));
                        }
                        let viewport_height = visible.len() as u16;

                        let spacer = crate::cli::TOOL_OUTPUT_VIEWPORT_SPACER_LINES;
                        let desired_total = viewport_height.saturating_add(spacer);
                        let reserve_delta =
                            desired_total.saturating_sub(state.tool_output.reserved_lines);
                        state.tool_output.reserved_lines = desired_total;

                        (reserve_delta, visible, viewport_height, spacer)
                    };

                    if reserve_delta > 0 {
                        terminal::reserve_output_lines(reserve_delta);
                    }

                    terminal::render_tool_viewport(&visible_lines, viewport_height, spacer_lines);
                }

                history::append_file_read_output(filename, text);

                if let Ok(mut state) = self.state.lock() {
                    state.last_block = Some(LastBlock::ToolContent);
                }
            }

            OutputEvent::Info(msg) => {
                let (last_block, in_tool_block, pending_tool_line) = self
                    .state
                    .lock()
                    .map(|s| (s.last_block, s.in_tool_block, s.last_tool_call_open))
                    .unwrap_or((None, false, false));

                if needs_blank_line_before(last_block, LastBlock::Info) {
                    terminal::ensure_trailing_newlines(2);
                } else {
                    terminal::ensure_line_break();
                }

                let styled = msg.cyan().to_string();

                // In prompt mode, avoid leaving the output cursor on a blank trailing line.
                // That blank line stacks with the status-line spacer row and looks like
                // "two blank lines" before the status line.
                //
                // Exception: if we're in the middle of a tool call banner awaiting its
                // checkmark, we *do* terminate the line so the ✓/✗ doesn't get appended to
                // the info message.
                if terminal::prompt_visible() && !pending_tool_line {
                    terminal::print_above(&styled);
                } else {
                    terminal::println_above(&styled);
                }

                history::push(HistoryEvent::Info(msg.clone()));
                if let Ok(mut state) = self.state.lock() {
                    state.last_block = Some(LastBlock::Info);
                    if in_tool_block {
                        state.info_since_last_tool_call = true;
                    }
                    if pending_tool_line {
                        state.last_tool_call_open = false;
                    }
                }
            }

            OutputEvent::Error(msg) => {
                self.close_tool_block();
                // Finalize current duration so the stats are displayed
                finalize_streaming();
                spinner_ready();
                // Reset turn-level state for next turn
                if let Ok(mut state) = self.state.lock() {
                    state.output_state = OutputState::Idle;
                }

                let last_block = self.state.lock().map(|s| s.last_block).unwrap_or(None);

                if needs_blank_line_before(last_block, LastBlock::Info) {
                    terminal::ensure_trailing_newlines(2);
                } else {
                    terminal::ensure_line_break();
                }

                terminal::println_above(&msg.red().to_string());
                history::push(HistoryEvent::Error(msg.clone()));
                if let Ok(mut state) = self.state.lock() {
                    state.last_block = Some(LastBlock::Info);
                }
            }

            OutputEvent::Warning(msg) => {
                let last_block = self.state.lock().map(|s| s.last_block).unwrap_or(None);

                if needs_blank_line_before(last_block, LastBlock::Info) {
                    terminal::ensure_trailing_newlines(2);
                } else {
                    terminal::ensure_line_break();
                }

                terminal::println_above(&msg.yellow().to_string());
                history::push(HistoryEvent::Warning(msg.clone()));
                if let Ok(mut state) = self.state.lock() {
                    state.last_block = Some(LastBlock::Info);
                }
            }

            OutputEvent::FileDiff {
                diff,
                language,
                summary,
            } => {
                // Acquire viewport transition lock to prevent racing with toggle
                let _viewport_guard = VIEWPORT_TRANSITION_LOCK
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());

                let pending_tool_line = self
                    .state
                    .lock()
                    .map(|mut s| {
                        let pending = s.last_tool_call_open;
                        s.last_tool_call_open = false;
                        pending
                    })
                    .unwrap_or(false);

                let expanded = is_tool_output_expanded();

                // In expanded mode we print diff lines inline, so we must close the banner line first.
                // In collapsed viewport mode we defer this until the checkmark, avoiding an extra
                // blank line between the tool banner and the viewport content.
                if pending_tool_line && expanded {
                    if terminal::prompt_visible() {
                        terminal::ensure_line_break();
                    } else {
                        println!();
                        let _ = io::stdout().flush();
                    }
                }

                // Only render the diff if tool output is not hidden
                if !is_tool_output_hidden() {
                    if expanded {
                        // Track line numbers across the diff
                        let mut old_line_num = 0usize;
                        let mut new_line_num = 0usize;
                        for line in diff.lines() {
                            if let Some(styled) = render_diff_line(
                                line,
                                language.as_deref(),
                                &mut old_line_num,
                                &mut new_line_num,
                            ) {
                                terminal::println_above(&styled);
                            }
                        }
                    } else {
                        use std::collections::VecDeque;

                        let max_lines = tool_output_viewport_lines();
                        let mut old_line_num = 0usize;
                        let mut new_line_num = 0usize;
                        let mut tail: VecDeque<String> = VecDeque::with_capacity(max_lines);
                        let mut total_lines = 0usize;

                        for line in diff.lines() {
                            if let Some(styled) = render_diff_line(
                                line,
                                language.as_deref(),
                                &mut old_line_num,
                                &mut new_line_num,
                            ) {
                                total_lines += 1;
                                if tail.len() == max_lines {
                                    tail.pop_front();
                                }
                                tail.push_back(styled);
                            }
                        }

                        if !tail.is_empty() {
                            let visible_count = tail.len();
                            let hidden = total_lines.saturating_sub(visible_count);
                            let mut visible: Vec<String> = tail.into_iter().collect();
                            if hidden > 0 {
                                visible.push(crate::cli::render::style_tool_output_line(
                                    &crate::cli::render::format_scrolled_indicator(
                                        hidden,
                                        visible_count,
                                        Some(total_lines),
                                    ),
                                ));
                            }

                            let viewport_height = visible.len() as u16;
                            let spacer = crate::cli::TOOL_OUTPUT_VIEWPORT_SPACER_LINES;
                            let desired_total = viewport_height.saturating_add(spacer);
                            let reserve_delta = self
                                .state
                                .lock()
                                .map(|mut state| {
                                    let delta = desired_total
                                        .saturating_sub(state.tool_output.reserved_lines);
                                    state.tool_output.reserved_lines = desired_total;
                                    delta
                                })
                                .unwrap_or(desired_total);

                            if reserve_delta > 0 {
                                terminal::reserve_output_lines(reserve_delta);
                            }

                            terminal::render_tool_viewport(&visible, viewport_height, spacer);
                        }
                    }
                }

                if pending_tool_line && !expanded {
                    if terminal::prompt_visible() {
                        terminal::ensure_line_break();
                    } else {
                        println!();
                        let _ = io::stdout().flush();
                    }
                }

                let summary_suffix = format_summary_suffix(summary.as_deref());
                let checkmark = format!("{}{}", "✓".green(), summary_suffix);
                if terminal::prompt_visible() {
                    terminal::println_above(&checkmark);
                } else {
                    println!("{}", checkmark);
                    let _ = io::stdout().flush();
                }
                history::push(HistoryEvent::FileDiff {
                    diff: diff.clone(),
                    language: language.clone(),
                    summary: summary.clone(),
                });
                // Mark that diff was shown so ToolResult skips its checkmark
                if let Ok(mut state) = self.state.lock() {
                    state.diff_shown = true;
                }
            }

            OutputEvent::ImagePreview { data, mime_type } => {
                // Display image preview for terminals that support it (e.g., Kitty)
                if SHOW_IMAGE_PREVIEWS.load(Ordering::Relaxed)
                    && let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(data)
                    && let Some(preview) =
                        super::image_preview::get_image_preview(&decoded, mime_type)
                {
                    // Ensure the image preview starts on its own line below the tool header.
                    terminal::ensure_line_break();

                    // Transmit the image data and create a virtual placement for placeholders.
                    terminal::print_above(&preview.escape_sequence);

                    // Print Unicode placeholders so the image persists in scrollback.
                    for line in &preview.placeholder_lines {
                        terminal::println_above(line);
                    }

                    history::push(HistoryEvent::ImagePreview {
                        data: decoded,
                        mime_type: preview.mime_type.clone(),
                    });

                    if let Ok(mut state) = self.state.lock() {
                        state.last_tool_call_open = false;
                    }
                }
            }

            OutputEvent::AutoCompactStarting {
                current_usage,
                limit,
            } => {
                let pct = (*current_usage as f64 / *limit as f64) * 100.0;
                let msg = format!(
                    "Context at {:.0}% ({}/{}) - auto-compacting...",
                    pct, current_usage, limit
                );
                terminal::ensure_line_break();
                terminal::println_above(&msg.yellow().to_string());
                history::push(HistoryEvent::AutoCompact { message: msg });
            }

            OutputEvent::AutoCompactCompleted { messages_compacted } => {
                let msg = format!("Compacted {} messages into summary.", messages_compacted);
                terminal::ensure_line_break();
                terminal::println_above(&msg.green().to_string());
                history::push(HistoryEvent::AutoCompact { message: msg });
            }

            OutputEvent::Waiting => {
                // Latch current stats into accumulated values (for multi-API-call turns)
                latch_streaming_stats();
                start_streaming();
                spinner_working();
            }

            OutputEvent::WorkingProgress { total_tokens } => {
                update_total_tokens(*total_tokens);
                spinner_working();
            }

            OutputEvent::UsageUpdate {
                input_tokens,
                output_tokens,
                cache_read_tokens,
            } => {
                update_usage_stats(*input_tokens, *output_tokens, *cache_read_tokens);
            }

            OutputEvent::Done => {
                // End-of-turn: close the tool block without forcing a trailing newline.
                self.close_tool_block_no_line_break();
                // Capture final duration and update display
                finalize_streaming();
                spinner_ready();
                // Reset turn-level state for next turn
                if let Ok(mut state) = self.state.lock() {
                    state.output_state = OutputState::Idle;
                    state.thinking_pending_newlines = 0;
                }
            }

            OutputEvent::Interrupted => {
                // End-of-turn: close the tool block without forcing a trailing newline.
                self.close_tool_block_no_line_break();
                // Finalize current duration so the stats are displayed on "Cancelled"
                finalize_streaming();
                spinner_ready();
                // Reset turn-level state for next turn
                if let Ok(mut state) = self.state.lock() {
                    state.output_state = OutputState::Idle;
                    state.thinking_pending_newlines = 0;
                }
            }

            OutputEvent::ContextUpdate {
                input_tokens,
                context_limit,
            } => {
                update_context(*input_tokens, *context_limit);
            }
        }
    }
}

impl OutputListener for CliListener {
    fn on_event(&self, event: &OutputEvent) {
        if terminal::is_output_buffering() {
            Self::buffer_event(event);
            return;
        }
        self.handle_event(event);
    }
}

pub(crate) struct CliListenerProxy {
    inner: &'static CliListener,
}

impl CliListenerProxy {
    pub(crate) fn new(inner: &'static CliListener) -> Self {
        Self { inner }
    }
}

impl OutputListener for CliListenerProxy {
    fn on_event(&self, event: &OutputEvent) {
        self.inner.on_event(event);
    }
}

/// A quiet listener that only prints errors
pub(crate) struct QuietListener;

impl QuietListener {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl OutputListener for QuietListener {
    fn on_event(&self, event: &OutputEvent) {
        if let OutputEvent::Error(e) = event {
            let _ = writeln!(io::stderr(), "Error: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OutputState;

    #[test]
    fn output_state_thinking_spacing() {
        let mut state = OutputState::Idle;
        state.start_thinking();
        assert!(!state.end_thinking());

        state.start_thinking();
        state.mark_thinking_output();
        assert!(state.end_thinking());
    }

    #[test]
    fn output_state_text_spacing() {
        let mut state = OutputState::Idle;
        state.start_text();
        assert!(!state.end_text());

        state.start_text();
        state.mark_text_output();
        assert!(state.end_text());
    }

    #[test]
    fn output_state_tool_spacing_only_after_content() {
        let mut state = OutputState::Idle;
        assert!(!state.start_tool_block());
        state.end_tool_block();

        state.start_text();
        state.mark_text_output();
        assert!(state.start_tool_block());
        state.end_tool_block();

        assert!(!state.start_tool_block());
    }

    #[test]
    fn format_tokens_uses_whole_number_suffixes() {
        assert_eq!(super::format_tokens(999), "999");
        assert_eq!(super::format_tokens(1_000), "1k");
        assert_eq!(super::format_tokens(3_300), "3k");
        assert_eq!(super::format_tokens(102_100), "102k");
        assert_eq!(super::format_tokens(16_800), "16k");
        assert_eq!(super::format_tokens(1_200_000), "1m");
    }
}
