// SPDX-License-Identifier: MIT
// Rendering functions for the TUI

use ratatui::{prelude::*, widgets::Paragraph};

use super::layout::{TAB_WIDTH, char_display_width, word_display_width};
use super::markdown::{
    MarkdownStyle, align_markdown_tables, find_markdown_spans, get_markdown_style,
};
use super::messages::{
    DiffMessage, TextMessage, ThinkingMessage, TodoListDisplay, ToolCallsMessage, UsageDisplay,
};
use super::models::ModelChoice;
use super::selection::{ContentPosition, InputSelection, PositionMap, Selection};
use super::syntax::HighlightLookup;
use crate::tools::TodoStatus;

// Re-export menu rendering functions from menus module
pub(crate) use super::menus::{
    render_completion_menu, render_history_search, render_model_menu, render_settings_menu,
    render_slash_menu,
};

pub(crate) const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Render a character to the buffer, expanding tabs to spaces.
/// Returns the number of cells consumed (1 for normal chars, TAB_WIDTH for tabs).
#[inline]
fn render_char_to_cell(
    buffer: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    ch: char,
    style: Style,
    max_x: u16,
) -> usize {
    if ch == '\t' {
        // Render tab as TAB_WIDTH spaces
        for i in 0..TAB_WIDTH {
            let cell_x = x + i as u16;
            if cell_x < max_x
                && let Some(cell) = buffer.cell_mut((cell_x, y))
            {
                cell.set_char(' ');
                cell.set_style(style);
            }
        }
        TAB_WIDTH
    } else {
        if let Some(cell) = buffer.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
        char_display_width(ch)
    }
}

/// Context for rendering messages with selection support
pub(crate) struct RenderContext<'a, 'b> {
    pub frame: &'a mut Frame<'b>,
    pub area: Rect,
    pub message_idx: usize,
    pub skip_top: u16,
    pub selection: &'a Selection,
    pub position_map: &'a mut PositionMap,
    /// Base offset added to byte indices to distinguish subframes within a message
    pub byte_offset_base: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitKey {
    CtrlC,
    CtrlD,
}

#[derive(Clone, Copy)]
pub(crate) struct ExitPrompt {
    pub key: ExitKey,
    pub expires_at: std::time::Instant,
}

/// Renders input text with selection highlighting and scroll offset
pub(crate) fn render_input_with_selection(
    frame: &mut Frame,
    area: Rect,
    text: &str,
    selection: &InputSelection,
    bg: Color,
    fg: Color,
    scroll: u16,
) {
    let width = area.width as usize;
    if width == 0 || area.height == 0 {
        return;
    }

    // Use effective width that leaves 1 column margin at the edge
    let effective_width = width.saturating_sub(1).max(1);

    let (sel_start, sel_end) = selection.ordered().unwrap_or((usize::MAX, usize::MAX));

    let normal_style = Style::default().fg(fg).bg(bg);
    let selected_style = Style::default().fg(Color::Black).bg(Color::White);

    let mut logical_row: u16 = 0; // row in the full input (before scrolling)
    let mut screen_col: usize = 0;
    let mut prev_was_whitespace = true; // Start as true to handle first word
    let mut wrapped = false; // Track if current line is from wrapping

    for (byte_idx, ch) in text.char_indices() {
        if ch == '\n' {
            logical_row += 1;
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
            let word_width = super::layout::word_display_width(text, byte_idx);
            // Only wrap if word is longer than remaining space but fits on a line
            if word_width <= effective_width && screen_col + word_width > effective_width {
                logical_row += 1;
                screen_col = 0;
                wrapped = true;
            }
        }

        // Handle line wrapping for characters that don't fit
        if screen_col + ch_width > effective_width {
            logical_row += 1;
            screen_col = 0;
            wrapped = true;
        }

        // Stop if we're past the visible area
        if logical_row >= scroll + area.height {
            break;
        }

        // Only render if we're in the visible area (logical_row >= scroll)
        if logical_row >= scroll {
            let screen_row = logical_row - scroll;
            let y = area.y + screen_row;
            let x = area.x + screen_col as u16;

            let is_selected = byte_idx >= sel_start && byte_idx < sel_end;
            let style = if is_selected {
                selected_style
            } else {
                normal_style
            };

            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }

        screen_col += ch_width;

        if screen_col == effective_width {
            logical_row += 1;
            screen_col = 0;
            wrapped = true;
        }

        prev_was_whitespace = is_whitespace;
    }
}

/// Renders text with selection highlighting while building the position map
pub(crate) fn render_text_with_selection(
    ctx: &mut RenderContext<'_, '_>,
    text: &str,
    bg_color: Option<Color>,
    fg_color: Option<Color>,
) {
    let width = ctx.area.width as usize;
    if width == 0 || ctx.area.height == 0 {
        return;
    }

    // Use effective width that leaves 1 column margin at the edge
    let effective_width = width.saturating_sub(1).max(1);

    // Fill background if specified
    if let Some(bg) = bg_color {
        for y in ctx.area.y..ctx.area.y + ctx.area.height {
            for x in ctx.area.x..ctx.area.x + ctx.area.width {
                if let Some(cell) = ctx.frame.buffer_mut().cell_mut((x, y)) {
                    cell.set_bg(bg);
                }
            }
        }
    }

    // Only apply selection highlighting for actual selections (not single clicks)
    let (sel_start, sel_end) = ctx
        .selection
        .ordered()
        .filter(|(s, e)| s != e)
        .map_or((None, None), |(s, e)| (Some(s), Some(e)));

    // Check if this message is in the selection range
    let in_selection_range = |msg_idx: usize, byte_offset: usize| -> bool {
        let Some(start) = sel_start else {
            return false;
        };
        let Some(end) = sel_end else { return false };
        let pos = ContentPosition::new(msg_idx, byte_offset);
        pos >= start && pos <= end
    };

    let fg = fg_color.unwrap_or(Color::White);
    let normal_style = if let Some(bg) = bg_color {
        Style::default().fg(fg).bg(bg)
    } else {
        Style::default().fg(fg)
    };
    let selected_style = Style::default().fg(Color::Black).bg(Color::White);

    // Track current screen position
    let mut screen_row: i32 = -(ctx.skip_top as i32); // Negative means above visible area
    let mut screen_col: usize = 0;
    let mut prev_was_whitespace = true; // Start as true to handle first word
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
            // Newline: move to next row
            screen_row += 1;
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
        // Don't word-wrap inside code blocks
        if !in_code_block && !is_whitespace && prev_was_whitespace && screen_col > 0 {
            let word_width = word_display_width(text, byte_idx);
            // Only wrap if word is longer than remaining space but fits on a line
            if word_width <= effective_width && screen_col + word_width > effective_width {
                screen_row += 1;
                screen_col = 0;
            }
        }

        // Handle line wrapping for characters that don't fit
        if screen_col + ch_width > effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        // Check if this character is visible
        if screen_row >= 0 && screen_row < ctx.area.height as i32 {
            let y = ctx.area.y + screen_row as u16;
            let x = ctx.area.x + screen_col as u16;

            // Update position map (add base offset for subframe distinction)
            ctx.position_map.set(
                x,
                y,
                ContentPosition::new(ctx.message_idx, ctx.byte_offset_base + byte_idx),
            );

            // Determine style
            let is_selected = in_selection_range(ctx.message_idx, ctx.byte_offset_base + byte_idx);
            let style = if is_selected {
                selected_style
            } else {
                normal_style
            };

            // Render the character (tabs expand to spaces)
            let max_x = ctx.area.x + ctx.area.width;
            render_char_to_cell(ctx.frame.buffer_mut(), x, y, ch, style, max_x);
        }

        screen_col += ch_width;

        // Handle exact width boundary
        if screen_col == effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        // Stop if we've gone past the visible area
        if screen_row >= ctx.area.height as i32 {
            break;
        }

        prev_was_whitespace = is_whitespace;
    }
}

/// Renders text with markdown formatting and selection highlighting
pub(crate) fn render_markdown_with_selection(
    ctx: &mut RenderContext<'_, '_>,
    text: &str,
    bg_color: Option<Color>,
    base_fg_color: Option<Color>,
) {
    let width = ctx.area.width as usize;
    if width == 0 || ctx.area.height == 0 {
        return;
    }

    let effective_width = width.saturating_sub(1).max(1);

    if let Some(bg) = bg_color {
        for y in ctx.area.y..ctx.area.y + ctx.area.height {
            for x in ctx.area.x..ctx.area.x + ctx.area.width {
                if let Some(cell) = ctx.frame.buffer_mut().cell_mut((x, y)) {
                    cell.set_bg(bg);
                }
            }
        }
    }

    let markdown_spans = find_markdown_spans(text);
    let syntax_lookup = HighlightLookup::new(text);

    let (sel_start, sel_end) = ctx
        .selection
        .ordered()
        .filter(|(s, e)| s != e)
        .map_or((None, None), |(s, e)| (Some(s), Some(e)));

    let in_selection_range = |msg_idx: usize, byte_offset: usize| -> bool {
        let Some(start) = sel_start else {
            return false;
        };
        let Some(end) = sel_end else { return false };
        let pos = ContentPosition::new(msg_idx, byte_offset);
        pos >= start && pos <= end
    };

    let base_fg = base_fg_color.unwrap_or(Color::White);
    let selected_style = Style::default().fg(Color::Black).bg(Color::White);

    let mut screen_row: i32 = -(ctx.skip_top as i32);
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
        let (md_style, is_marker) = get_markdown_style(byte_idx, &markdown_spans);
        if is_marker {
            continue;
        }

        if ch == '\n' {
            // Check if this line was a code fence
            let line = &text[line_start_byte..byte_idx];
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                in_code_block = !in_code_block;
            }
            screen_row += 1;
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

        // Skip leading whitespace at start of line - but NOT in code blocks
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

        // Word wrap - but NOT in code blocks
        if !in_code_block && !is_whitespace && prev_was_whitespace && screen_col > 0 {
            let word_width = word_display_width(text, byte_idx);
            if word_width <= effective_width && screen_col + word_width > effective_width {
                screen_row += 1;
                screen_col = 0;
            }
        }

        if screen_col + ch_width > effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        if screen_row >= 0 && screen_row < ctx.area.height as i32 {
            let y = ctx.area.y + screen_row as u16;
            let x = ctx.area.x + screen_col as u16;

            ctx.position_map.set(
                x,
                y,
                ContentPosition::new(ctx.message_idx, ctx.byte_offset_base + byte_idx),
            );

            let is_selected = in_selection_range(ctx.message_idx, ctx.byte_offset_base + byte_idx);
            let style = if is_selected {
                selected_style
            } else {
                // Check for syntax highlighting first (only applies inside code blocks)
                let syntax_color = syntax_lookup.color_at(byte_idx);

                let fg = if let Some(color) = syntax_color {
                    color
                } else if ch == '✓' {
                    Color::Green
                } else if ch == '✗' {
                    Color::Red
                } else {
                    base_fg
                };

                let mut style = Style::default().fg(fg);
                if let Some(bg) = bg_color {
                    style = style.bg(bg);
                }

                // Apply markdown styles (but not for syntax-highlighted code)
                if syntax_color.is_none() {
                    match md_style {
                        MarkdownStyle::Bold => style = style.add_modifier(Modifier::BOLD),
                        MarkdownStyle::InlineCode => {
                            style = style.fg(Color::Green);
                        }
                        MarkdownStyle::Normal => {}
                    }
                }
                style
            };

            // Render the character (tabs expand to spaces)
            let max_x = ctx.area.x + ctx.area.width;
            render_char_to_cell(ctx.frame.buffer_mut(), x, y, ch, style, max_x);
        }

        screen_col += ch_width;

        if screen_col == effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        if screen_row >= ctx.area.height as i32 {
            break;
        }

        prev_was_whitespace = is_whitespace;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_status_line(
    frame: &mut Frame,
    area: Rect,
    current_model: &Option<ModelChoice>,
    exit_prompt: Option<ExitPrompt>,
    thinking_enabled: bool,
    thinking_available: bool,
    thinking_mode: super::thinking_mode::ThinkingMode,
    show_network_stats: bool,
    lsp_server_count: usize,
) {
    // If there's an exit prompt, show that instead of the normal status
    if let Some(prompt) = exit_prompt {
        let key_label = match prompt.key {
            ExitKey::CtrlC => "Ctrl+C",
            ExitKey::CtrlD => "Ctrl+D",
        };
        let text = format!("Press {key_label} again within 2s to exit");
        let widget = Paragraph::new(text).style(Style::default().fg(Color::Yellow));
        frame.render_widget(widget, area);
        return;
    }

    // Build the status line with directory and model
    let cwd = std::env::current_dir()
        .map(|p| {
            // Try to shorten the path using ~ for home directory
            if let Some(home) = dirs::home_dir()
                && let Ok(relative) = p.strip_prefix(&home)
            {
                return format!("~/{}", relative.display());
            }
            p.display().to_string()
        })
        .unwrap_or_else(|_| "?".to_string());

    // Get provider and model separately for colored display
    let (provider_display, model_display) = current_model
        .as_ref()
        .map(|m| {
            let provider = m
                .custom_provider
                .as_deref()
                .unwrap_or_else(|| m.provider.id());
            (provider.to_string(), m.model_id.clone())
        })
        .unwrap_or_else(|| ("none".to_string(), "no model".to_string()));

    let thinking_status = if thinking_available {
        // For Gemini models, show the thinking mode; for others, show on/off
        let is_gemini = current_model
            .as_ref()
            .map(|m| m.model_id.starts_with("gemini-"))
            .unwrap_or(false);

        let (thinking_text, thinking_color) = if is_gemini {
            // Show the thinking mode for Gemini
            let mode_display = format!("thinking {}", thinking_mode.display());
            let color = if thinking_mode == super::thinking_mode::ThinkingMode::Off {
                Color::Yellow
            } else {
                Color::Green
            };
            (mode_display, color)
        } else if thinking_enabled {
            ("thinking on".to_string(), Color::Green)
        } else {
            ("thinking off".to_string(), Color::Yellow)
        };

        Some(vec![
            Span::styled(" [", Style::default().fg(Color::DarkGray)),
            Span::styled(thinking_text, Style::default().fg(thinking_color)),
            Span::styled("]", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        None
    };

    // Match CLI color scheme: provider=Magenta, /=DarkGray, model=Cyan, path=Blue
    let mut status_spans = vec![
        Span::styled(provider_display, Style::default().fg(Color::Magenta)),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled(model_display, Style::default().fg(Color::Cyan)),
        Span::styled(" ", Style::default()),
        Span::styled(cwd, Style::default().fg(Color::Blue)),
    ];
    if let Some(mut thinking_spans) = thinking_status {
        status_spans.append(&mut thinking_spans);
    }

    // Add LSP status if servers are connected
    if lsp_server_count > 0 {
        status_spans.push(Span::styled(" [", Style::default().fg(Color::DarkGray)));
        status_spans.push(Span::styled(
            format!("LSP: {}", lsp_server_count),
            Style::default().fg(Color::Green),
        ));
        status_spans.push(Span::styled("]", Style::default().fg(Color::DarkGray)));
    }

    let status_text = Line::from(status_spans);

    let widget = Paragraph::new(status_text);
    frame.render_widget(widget, area);

    // Render network stats on the right
    if show_network_stats {
        let stats = crate::usage::network_stats();
        let tx = stats.tx_bytes();
        let rx = stats.rx_bytes();

        let format_bytes = |b: u64| -> String {
            if b < 1024 {
                format!("{} B", b)
            } else if b < 1024 * 1024 {
                format!("{:.1} KB", b as f64 / 1024.0)
            } else {
                format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
            }
        };

        let net_text = Line::from(Span::styled(
            format!("▼{} ▲{}", format_bytes(rx), format_bytes(tx)),
            Style::default().fg(Color::DarkGray),
        ));
        let net_widget = Paragraph::new(net_text).alignment(Alignment::Right);
        frame.render_widget(net_widget, area);
    }
}
/// Render usage display with colored progress bars and mouse selection support
pub(crate) fn render_usage_with_selection(ctx: &mut RenderContext<'_, '_>, usage: &UsageDisplay) {
    let width = ctx.area.width as usize;
    if width == 0 || ctx.area.height == 0 {
        return;
    }

    fn utilization_color(util: f64) -> Color {
        if util < 0.50 {
            Color::Green
        } else if util < 0.75 {
            Color::Yellow
        } else if util < 0.90 {
            Color::Rgb(255, 165, 0)
        } else {
            Color::Red
        }
    }

    // Build styled segments: Vec<(text, fg_color, is_bold)>
    // Each segment corresponds to a portion of display_text
    struct StyledSegment {
        len: usize,
        fg: Option<Color>,
        bold: bool,
    }

    let mut segments: Vec<StyledSegment> = Vec::new();
    let limits = &usage.limits;

    // Header: "Anthropic Rate Limits" (cyan, bold)
    segments.push(StyledSegment {
        len: "Anthropic Rate Limits".len(),
        fg: Some(Color::Cyan),
        bold: true,
    });
    // Newline after header
    segments.push(StyledSegment {
        len: 1,
        fg: None,
        bold: false,
    });

    // Helper to add a limit line's segments
    fn add_limit_segments(
        segments: &mut Vec<StyledSegment>,
        label: &str,
        util: f64,
        color: Color,
        reset_text_len: usize,
    ) {
        // "  <label>:  " prefix
        segments.push(StyledSegment {
            len: label.len(),
            fg: None,
            bold: false,
        });
        // Progress bar filled (20 chars max, each █ is 3 bytes)
        let filled = ((util * 20.0).round() as usize).min(20);
        let empty = 20 - filled;
        segments.push(StyledSegment {
            len: filled * 3, // █ is 3 bytes
            fg: Some(color),
            bold: false,
        });
        // Progress bar empty (░ is 3 bytes)
        segments.push(StyledSegment {
            len: empty * 3,
            fg: None,
            bold: false,
        });
        // Space + percentage (colored)
        let pct_str = format!(" {:5.1}%", util * 100.0);
        segments.push(StyledSegment {
            len: pct_str.len(),
            fg: Some(color),
            bold: false,
        });
        // "  resets in <time>\n"
        segments.push(StyledSegment {
            len: reset_text_len,
            fg: None,
            bold: false,
        });
    }

    // Calculate reset text lengths from display_text by parsing it
    // This is a bit fragile but necessary to match the pre-computed display_text
    let lines: Vec<&str> = usage.display_text.lines().collect();
    let mut line_idx = 2; // Skip header and blank line

    if let Some(util) = limits.unified_5h_utilization {
        let color = utilization_color(util);
        let reset_text_len = if line_idx < lines.len() {
            let line = lines[line_idx];
            // Find "  resets in " and get the rest
            if let Some(pos) = line.find("  resets in ") {
                line.len() - pos + 1 // +1 for newline
            } else {
                1
            }
        } else {
            1
        };
        add_limit_segments(
            &mut segments,
            "  5-hour limit:  ",
            util,
            color,
            reset_text_len,
        );
        line_idx += 1;
    }

    if let Some(util) = limits.unified_7d_utilization {
        let color = utilization_color(util);
        let reset_text_len = if line_idx < lines.len() {
            let line = lines[line_idx];
            if let Some(pos) = line.find("  resets in ") {
                line.len() - pos + 1
            } else {
                1
            }
        } else {
            1
        };
        add_limit_segments(
            &mut segments,
            "  7-day limit:   ",
            util,
            color,
            reset_text_len,
        );
        line_idx += 1;
    }

    if let Some(util) = limits.unified_7d_sonnet_utilization {
        let color = utilization_color(util);
        let reset_text_len = if line_idx < lines.len() {
            let line = lines[line_idx];
            if let Some(pos) = line.find("  resets in ") {
                line.len() - pos + 1
            } else {
                1
            }
        } else {
            1
        };
        add_limit_segments(
            &mut segments,
            "  7d Sonnet:     ",
            util,
            color,
            reset_text_len,
        );
    }

    // Now render character by character using segments for styling
    let text = &usage.display_text;
    let effective_width = width;

    // Selection helper
    let in_selection_range = |msg_idx: usize, byte_idx: usize| -> bool {
        if let Some((start, end)) = ctx.selection.ordered() {
            if msg_idx < start.message_idx || msg_idx > end.message_idx {
                return false;
            }
            if msg_idx == start.message_idx && msg_idx == end.message_idx {
                return byte_idx >= start.byte_offset && byte_idx < end.byte_offset;
            }
            if msg_idx == start.message_idx {
                return byte_idx >= start.byte_offset;
            }
            if msg_idx == end.message_idx {
                return byte_idx < end.byte_offset;
            }
            true
        } else {
            false
        }
    };

    // Find style for a byte offset
    let style_for_offset = |byte_offset: usize| -> Style {
        let mut offset = 0;
        for seg in &segments {
            if byte_offset < offset + seg.len {
                let mut style = Style::default();
                if let Some(fg) = seg.fg {
                    style = style.fg(fg);
                }
                if seg.bold {
                    style = style.bold();
                }
                return style;
            }
            offset += seg.len;
        }
        Style::default()
    };

    let mut screen_row: i32 = -(ctx.skip_top as i32);
    let mut screen_col: usize = 0;

    for (byte_idx, ch) in text.char_indices() {
        let ch_width = char_display_width(ch);

        // Handle newlines
        if ch == '\n' {
            // Register position for the newline character
            if screen_row >= 0 && screen_row < ctx.area.height as i32 {
                let y = ctx.area.y + screen_row as u16;
                let x = ctx.area.x + screen_col as u16;
                if x < ctx.area.x + ctx.area.width {
                    ctx.position_map.set(
                        x,
                        y,
                        ContentPosition::new(ctx.message_idx, ctx.byte_offset_base + byte_idx),
                    );
                }
            }
            screen_row += 1;
            screen_col = 0;
            continue;
        }

        // Handle line wrapping
        if screen_col + ch_width > effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        // Check if visible
        if screen_row >= 0 && screen_row < ctx.area.height as i32 {
            let y = ctx.area.y + screen_row as u16;
            let x = ctx.area.x + screen_col as u16;

            // Register position
            ctx.position_map.set(
                x,
                y,
                ContentPosition::new(ctx.message_idx, ctx.byte_offset_base + byte_idx),
            );

            // Get style
            let base_style = style_for_offset(byte_idx);
            let is_selected = in_selection_range(ctx.message_idx, ctx.byte_offset_base + byte_idx);
            let style = if is_selected {
                Style::default().bg(Color::Rgb(60, 60, 120))
            } else {
                base_style
            };

            // Render character
            if let Some(cell) = ctx.frame.buffer_mut().cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }

        screen_col += ch_width;

        // Handle exact width boundary
        if screen_col == effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        // Stop if past visible area
        if screen_row >= ctx.area.height as i32 {
            break;
        }
    }
}

/// Render todo list display with colored status indicators and mouse selection support
pub(crate) fn render_todo_list_with_selection(
    ctx: &mut RenderContext<'_, '_>,
    todo: &TodoListDisplay,
) {
    let width = ctx.area.width as usize;
    if width == 0 || ctx.area.height == 0 {
        return;
    }

    // Build styled segments for the todo list
    // Each segment has: len (in bytes), optional foreground color, bold flag
    struct StyledSegment {
        len: usize,
        fg: Option<Color>,
        bold: bool,
    }

    let mut segments: Vec<StyledSegment> = Vec::new();

    if todo.todos.is_empty() {
        // "Todo list cleared." - dim style
        segments.push(StyledSegment {
            len: "Todo list cleared.".len(),
            fg: Some(Color::DarkGray),
            bold: false,
        });
    } else {
        // Header: "Todo List:" - cyan, bold
        segments.push(StyledSegment {
            len: "Todo List:".len(),
            fg: Some(Color::Cyan),
            bold: true,
        });
        // Newline
        segments.push(StyledSegment {
            len: 1,
            fg: None,
            bold: false,
        });

        // Each todo item
        for item in &todo.todos {
            let (indicator, text, color) = match item.status {
                TodoStatus::Pending => ("○", &item.content, Color::DarkGray),
                TodoStatus::InProgress => ("◐", &item.active_form, Color::Cyan),
                TodoStatus::Completed => ("●", &item.content, Color::Green),
            };

            // "  " indent (2 spaces)
            segments.push(StyledSegment {
                len: 2,
                fg: None,
                bold: false,
            });
            // Status indicator (○/◐/● - 3 bytes each)
            segments.push(StyledSegment {
                len: indicator.len(),
                fg: Some(color),
                bold: false,
            });
            // Space after indicator
            segments.push(StyledSegment {
                len: 1,
                fg: None,
                bold: false,
            });
            // Todo text with same color
            segments.push(StyledSegment {
                len: text.len(),
                fg: Some(color),
                bold: false,
            });
            // Newline (if not last item)
            segments.push(StyledSegment {
                len: 1,
                fg: None,
                bold: false,
            });
        }
    }

    // Selection helper
    let in_selection_range = |msg_idx: usize, byte_idx: usize| -> bool {
        if let Some((start, end)) = ctx.selection.ordered() {
            if msg_idx < start.message_idx || msg_idx > end.message_idx {
                return false;
            }
            if msg_idx == start.message_idx && msg_idx == end.message_idx {
                return byte_idx >= start.byte_offset && byte_idx < end.byte_offset;
            }
            if msg_idx == start.message_idx {
                return byte_idx >= start.byte_offset;
            }
            if msg_idx == end.message_idx {
                return byte_idx < end.byte_offset;
            }
            true
        } else {
            false
        }
    };

    // Find style for a byte offset
    let style_for_offset = |byte_offset: usize| -> Style {
        let mut offset = 0;
        for seg in &segments {
            if byte_offset < offset + seg.len {
                let mut style = Style::default();
                if let Some(fg) = seg.fg {
                    style = style.fg(fg);
                }
                if seg.bold {
                    style = style.bold();
                }
                return style;
            }
            offset += seg.len;
        }
        Style::default()
    };

    let text = &todo.display_text;
    let effective_width = width;
    let mut screen_row: i32 = -(ctx.skip_top as i32);
    let mut screen_col: usize = 0;

    for (byte_idx, ch) in text.char_indices() {
        let ch_width = char_display_width(ch);

        // Handle newlines
        if ch == '\n' {
            // Register position for the newline character
            if screen_row >= 0 && screen_row < ctx.area.height as i32 {
                let y = ctx.area.y + screen_row as u16;
                let x = ctx.area.x + screen_col as u16;
                if x < ctx.area.x + ctx.area.width {
                    ctx.position_map.set(
                        x,
                        y,
                        ContentPosition::new(ctx.message_idx, ctx.byte_offset_base + byte_idx),
                    );
                }
            }
            screen_row += 1;
            screen_col = 0;
            continue;
        }

        // Handle line wrapping
        if screen_col + ch_width > effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        // Check if visible
        if screen_row >= 0 && screen_row < ctx.area.height as i32 {
            let y = ctx.area.y + screen_row as u16;
            let x = ctx.area.x + screen_col as u16;

            // Register position
            ctx.position_map.set(
                x,
                y,
                ContentPosition::new(ctx.message_idx, ctx.byte_offset_base + byte_idx),
            );

            // Get style
            let base_style = style_for_offset(byte_idx);
            let is_selected = in_selection_range(ctx.message_idx, ctx.byte_offset_base + byte_idx);
            let style = if is_selected {
                Style::default().bg(Color::Rgb(60, 60, 120))
            } else {
                base_style
            };

            // Render character
            if let Some(cell) = ctx.frame.buffer_mut().cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }

        screen_col += ch_width;

        // Handle exact width boundary
        if screen_col == effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        // Stop if past visible area
        if screen_row >= ctx.area.height as i32 {
            break;
        }
    }
}

/// Render a thinking message with indentation
pub(crate) fn render_thinking_message(
    ctx: &mut RenderContext<'_, '_>,
    msg: &ThinkingMessage,
    bg_color: Option<Color>,
) {
    if ctx.area.width == 0 || ctx.area.height == 0 {
        return;
    }

    let trimmed = msg.text.trim();
    if trimmed.is_empty() {
        return;
    }

    // Indent thinking text
    let mut indented = String::new();
    for line in trimmed.lines() {
        indented.push_str("  ");
        indented.push_str(line.trim_end());
        indented.push('\n');
    }
    if indented.ends_with('\n') {
        indented.pop();
    }

    render_markdown_with_selection(ctx, &indented, bg_color, Some(Color::DarkGray));
}

/// Render a tool calls message with indentation
pub(crate) fn render_tool_calls_message(
    ctx: &mut RenderContext<'_, '_>,
    msg: &ToolCallsMessage,
    bg_color: Option<Color>,
) {
    if ctx.area.width == 0 || ctx.area.height == 0 {
        return;
    }

    if msg.calls.is_empty() {
        return;
    }

    // Indent tool calls
    let mut text = String::new();
    for (i, call) in msg.calls.iter().enumerate() {
        text.push_str("  ");
        text.push_str(call.trim());
        if i < msg.calls.len() - 1 {
            text.push('\n');
        }
    }

    // Use markdown renderer for ✓/✗ coloring, with gray base text
    render_markdown_with_selection(ctx, &text, bg_color, Some(Color::Gray));
}

/// Render an assistant text message
pub(crate) fn render_text_message(
    ctx: &mut RenderContext<'_, '_>,
    msg: &TextMessage,
    bg_color: Option<Color>,
) {
    if ctx.area.width == 0 || ctx.area.height == 0 {
        return;
    }

    let trimmed = msg.text.trim();
    if trimmed.is_empty() {
        return;
    }

    let aligned_text = align_markdown_tables(trimmed);
    render_markdown_with_selection(ctx, &aligned_text, bg_color, None);
}

pub(crate) fn render_diff_with_selection(ctx: &mut RenderContext<'_, '_>, diff: &DiffMessage) {
    if ctx.area.width == 0 || ctx.area.height == 0 {
        return;
    }

    let width = ctx.area.width as usize;
    let effective_width = width.saturating_sub(1).max(1);

    let in_selection_range = |msg_idx: usize, byte_idx: usize| -> bool {
        if let Some((start, end)) = ctx.selection.ordered() {
            if msg_idx < start.message_idx || msg_idx > end.message_idx {
                return false;
            }
            if msg_idx == start.message_idx && msg_idx == end.message_idx {
                return byte_idx >= start.byte_offset && byte_idx < end.byte_offset;
            }
            if msg_idx == start.message_idx {
                return byte_idx >= start.byte_offset;
            }
            if msg_idx == end.message_idx {
                return byte_idx < end.byte_offset;
            }
            true
        } else {
            false
        }
    };

    let header_text = format!(
        "{} +{} -{}",
        diff.path, diff.lines_added, diff.lines_removed
    );

    let header_len = header_text.len();
    let diff_text = &diff.diff;

    let mut screen_row: i32 = -(ctx.skip_top as i32);
    let mut screen_col: usize = 0;

    let is_diff_line = |line: &str| -> (bool, Color) {
        if line.starts_with('+') && !line.starts_with("+++") {
            (true, Color::Green)
        } else if line.starts_with('-') && !line.starts_with("---") {
            (true, Color::Red)
        } else if line.starts_with("@@") {
            (true, Color::Cyan)
        } else {
            (false, Color::White)
        }
    };

    let text = &header_text;
    for (byte_idx, ch) in text.char_indices() {
        let ch_width = char_display_width(ch);

        if ch == '\n' {
            screen_row += 1;
            screen_col = 0;
            continue;
        }

        if ch_width == 0 {
            continue;
        }

        if screen_col + ch_width > effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        if screen_row >= 0 && screen_row < ctx.area.height as i32 {
            let y = ctx.area.y + screen_row as u16;
            let x = ctx.area.x + screen_col as u16;

            ctx.position_map
                .set(x, y, ContentPosition::new(ctx.message_idx, byte_idx));

            let is_selected = in_selection_range(ctx.message_idx, byte_idx);
            let style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::White)
            } else {
                Style::default().fg(Color::Cyan).bold()
            };

            if let Some(cell) = ctx.frame.buffer_mut().cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }

        screen_col += ch_width;

        if screen_col == effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        if screen_row >= ctx.area.height as i32 {
            break;
        }
    }

    screen_row += 1;
    screen_col = 0;

    let diff_byte_offset = header_len;
    let mut current_line_start = 0;
    for (byte_idx, ch) in diff_text.char_indices() {
        let ch_width = char_display_width(ch);

        if ch == '\n' {
            screen_row += 1;
            screen_col = 0;
            current_line_start = byte_idx + 1;
            continue;
        }

        if ch_width == 0 {
            continue;
        }

        if screen_col + ch_width > effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        if screen_row >= 0 && screen_row < ctx.area.height as i32 {
            let y = ctx.area.y + screen_row as u16;
            let x = ctx.area.x + screen_col as u16;

            ctx.position_map.set(
                x,
                y,
                ContentPosition::new(ctx.message_idx, diff_byte_offset + byte_idx),
            );

            let is_selected = in_selection_range(ctx.message_idx, diff_byte_offset + byte_idx);
            let base_style = Style::default().fg(Color::White);
            let line = &diff_text[current_line_start..byte_idx];
            let (_, diff_color) = is_diff_line(line);

            let style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::White)
            } else {
                base_style.fg(diff_color)
            };

            if let Some(cell) = ctx.frame.buffer_mut().cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_style(style);
            }
        }

        screen_col += ch_width;

        if screen_col == effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        if screen_row >= ctx.area.height as i32 {
            break;
        }
    }
}
