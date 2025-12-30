// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
};

use crate::completion::COMPLETION_MENU_MAX_VISIBLE;

use super::app::App;
use super::commands::SLASH_MENU_MAX_VISIBLE;
use super::layout::{
    INPUT_PROMPT_GAP, INPUT_PROMPT_WIDTH, MIN_MESSAGE_HEIGHT, USER_MESSAGE_PADDING,
    compute_visible_segments, current_prompt, cursor_position, input_display_lines,
    text_message_height,
};
use super::messages::Message;
use super::models::HISTORY_SEARCH_MAX_VISIBLE;
use super::render::{
    RenderContext, SPINNER_FRAMES, render_completion_menu, render_diff_with_selection,
    render_history_search, render_input_with_selection, render_model_menu, render_settings_menu,
    render_slash_menu, render_status_line, render_text_with_selection,
    render_todo_list_with_selection, render_usage_with_selection,
};

pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    let size = frame.area();
    let input_width = size
        .width
        .saturating_sub(INPUT_PROMPT_WIDTH)
        .saturating_sub(INPUT_PROMPT_GAP)
        .max(1);
    let (display_input, cursor_offset) = match app.input.strip_prefix('!') {
        Some(rest) => (rest.to_string(), 1),
        None => (app.input.clone(), 0),
    };
    let needed_input_lines = input_display_lines(&display_input, input_width);

    let desired_input_height = (needed_input_lines as u16).saturating_add(2); // +2 for border lines
    let max_input_height = size.height.saturating_sub(MIN_MESSAGE_HEIGHT).max(3); // ensure room for borders + 1 line
    let input_height = desired_input_height
        .min(max_input_height)
        .max(3)
        .min(size.height);

    let slash_items = app.slash_menu_items();
    let slash_menu_height = if !app.slash_menu_active() || slash_items.is_empty() {
        0
    } else {
        slash_items
            .len()
            .min(SLASH_MENU_MAX_VISIBLE)
            .min(u16::MAX as usize) as u16
    };

    // Completion menu height (only show if no slash menu is active)
    let completion_menu_height = if slash_menu_height == 0
        && app.completion_active()
        && !app.file_completer.matches.is_empty()
    {
        app.file_completer
            .matches
            .len()
            .min(COMPLETION_MENU_MAX_VISIBLE)
            .min(u16::MAX as usize) as u16
    } else {
        0
    };

    let history_menu_height = if let Some(ref search) = app.history_search {
        // Calculate total lines needed for visible entries + 1 for search prompt
        let max_entries = HISTORY_SEARCH_MAX_VISIBLE;
        let mut total_lines = 1usize; // Start with 1 for the search prompt line
        let mut entries_shown = 0usize;
        for &history_idx in &search.filtered_indices {
            if entries_shown >= max_entries {
                break;
            }
            if let Some(entry) = app.input_history.entries().get(history_idx) {
                total_lines += entry.lines().count().max(1);
                entries_shown += 1;
            }
        }
        if entries_shown == 0 {
            total_lines += 1; // Add 1 for "(no matches)" line
        }
        total_lines.min(21).min(u16::MAX as usize) as u16
    } else {
        0
    };

    // Working indicator: spinner line + empty line = 2 lines
    let working_indicator_height = if app.is_chatting
        || app.is_compacting
        || app.streaming_tokens.is_some()
        || app.streaming_duration.is_some()
    {
        2u16
    } else {
        0u16
    };

    // Calculate height for pending prompts (queued output)
    // Use effective width accounting for prompt character and gap
    let pending_text_width = size
        .width
        .saturating_sub(INPUT_PROMPT_WIDTH + INPUT_PROMPT_GAP);
    let pending_prompts_height: u16 = app
        .pending_prompts
        .iter()
        .map(|p| text_message_height(&p.display_text, pending_text_width))
        .sum();

    let mut constraints = vec![Constraint::Min(MIN_MESSAGE_HEIGHT)];

    // Only add separator if there will be something below chat
    if working_indicator_height > 0
        || pending_prompts_height > 0
        || slash_menu_height > 0
        || completion_menu_height > 0
        || history_menu_height > 0
        || input_height > 0
    {
        constraints.push(Constraint::Length(1)); // Blank line separator
    }

    if working_indicator_height > 0 {
        constraints.push(Constraint::Length(working_indicator_height));
    }
    if pending_prompts_height > 0 {
        constraints.push(Constraint::Length(pending_prompts_height));
        constraints.push(Constraint::Length(1)); // Blank line after pending prompts
    }
    if slash_menu_height > 0 {
        constraints.push(Constraint::Length(slash_menu_height));
    }
    if completion_menu_height > 0 {
        constraints.push(Constraint::Length(completion_menu_height));
    }
    if history_menu_height > 0 {
        constraints.push(Constraint::Length(history_menu_height));
    }
    constraints.push(Constraint::Length(input_height));
    constraints.push(Constraint::Length(1)); // Status line always present

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(size);

    let mut idx = 0usize;
    let chat_area = layout[idx];
    idx += 1;

    // Skip separator if it exists
    if working_indicator_height > 0
        || pending_prompts_height > 0
        || slash_menu_height > 0
        || completion_menu_height > 0
        || history_menu_height > 0
        || input_height > 0
    {
        idx += 1;
    }

    let working_area = if working_indicator_height > 0 && idx < layout.len() {
        let area = layout[idx];
        idx += 1;
        Some(area)
    } else {
        None
    };
    let pending_prompts_area = if pending_prompts_height > 0 && idx < layout.len() {
        let area = layout[idx];
        idx += 1;
        idx += 1; // Skip blank line separator after pending prompts
        Some(area)
    } else {
        None
    };
    let slash_area = if slash_menu_height > 0 && idx < layout.len() {
        let area = layout[idx];
        idx += 1;
        Some(area)
    } else {
        None
    };
    let completion_area = if completion_menu_height > 0 && idx < layout.len() {
        let area = layout[idx];
        idx += 1;
        Some(area)
    } else {
        None
    };
    let history_area = if history_menu_height > 0 && idx < layout.len() {
        let area = layout[idx];
        idx += 1;
        Some(area)
    } else {
        None
    };
    let input_area = layout[idx];
    idx += 1;
    let status_area = layout[idx];

    let message_width = chat_area.width.max(1);
    let message_heights: Vec<u16> = app.message_heights(message_width).to_vec();

    let total_height: usize = app.layout_cache.total_height;
    let max_scroll = total_height.saturating_sub(chat_area.height as usize);

    // If in position-locked mode, recalculate scroll_lines from absolute position
    // using current dimensions (not the width that was used during content change)
    if let Some(abs_pos) = app.absolute_scroll_position {
        let viewport = chat_area.height as usize;
        app.scroll_lines = total_height.saturating_sub(viewport + abs_pos);
    }

    // Clamp scroll_lines to valid range
    app.scroll_lines = app.scroll_lines.min(max_scroll);

    app.last_viewport_height = chat_area.height;
    app.last_total_height = total_height;

    // Initialize position map for this frame
    app.position_map.init(chat_area);

    let segments = compute_visible_segments(
        &message_heights,
        chat_area.height,
        total_height,
        app.scroll_lines,
    );

    app.visible_message_count = segments.len().max(1);

    // Put filler space at the beginning so messages appear at the bottom
    let mut constraints: Vec<Constraint> = vec![Constraint::Min(0)];
    for seg in &segments {
        constraints.push(Constraint::Length(seg.height));
    }
    let rows = Layout::vertical(constraints).split(chat_area);

    // Clone selection to avoid borrow issues
    let selection = app.selection.clone();

    // Render messages
    let mut row_idx = 1; // Skip index 0 (the filler space)
    for segment in &segments {
        let area = rows[row_idx];
        row_idx += 1;

        render_message(
            frame,
            area,
            app,
            segment.index,
            segment.skip_top,
            &selection,
        );
    }

    // Render scrollbar only when scrolled away from bottom
    if app.scroll_lines > 0 {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▴"))
            .end_symbol(Some("▾"))
            .track_symbol(Some(" "))
            .thumb_symbol("│")
            .style(Style::default().fg(Color::Rgb(60, 60, 60)))
            .thumb_style(Style::default().fg(Color::Rgb(100, 100, 100)));

        // Invert scroll position: app.scroll_lines is "up from bottom", scrollbar needs "down from top"
        let scrollbar_position = max_scroll.saturating_sub(app.scroll_lines);

        let mut scrollbar_state = ScrollbarState::new(max_scroll)
            .position(scrollbar_position)
            .viewport_content_length(chat_area.height as usize);

        frame.render_stateful_widget(scrollbar, chat_area, &mut scrollbar_state);
    }

    // Render working indicator when the model is processing
    if let Some(area) = working_area {
        render_working_indicator(frame, area, app);
    }

    // Render pending prompts (queued output) between working indicator and input
    if let Some(area) = pending_prompts_area {
        render_pending_prompts(frame, area, app);
    }

    if let Some(area) = slash_area {
        let total = slash_items.len();
        if total > 0 {
            let selected = app.slash_menu_index.min(total.saturating_sub(1));
            let visible = slash_menu_height as usize;
            let max_start = total.saturating_sub(visible);
            let start = selected
                .saturating_sub(visible.saturating_sub(1))
                .min(max_start);
            let end = (start + visible).min(total);
            let view = &slash_items[start..end];
            let selected_in_view = selected.saturating_sub(start);
            render_slash_menu(frame, area, view, selected_in_view);
        }
    }

    if let Some(area) = completion_area {
        let total = app.file_completer.matches.len();
        if total > 0 {
            let selected = app.file_completer.index.min(total.saturating_sub(1));
            let visible = completion_menu_height as usize;
            let max_start = total.saturating_sub(visible);
            let start = selected
                .saturating_sub(visible.saturating_sub(1))
                .min(max_start);
            let end = (start + visible).min(total);
            let view = &app.file_completer.matches[start..end];
            let selected_in_view = selected.saturating_sub(start);
            render_completion_menu(frame, area, view, selected_in_view);
        }
    }

    if let Some(area) = history_area
        && let Some(ref search) = app.history_search
    {
        render_history_search(
            frame,
            area,
            search,
            app.input_history.entries(),
            search.selected_index,
        );
    }

    render_input_area(
        frame,
        input_area,
        app,
        &display_input,
        cursor_offset,
        needed_input_lines,
    );

    render_status_line(
        frame,
        status_area,
        &app.current_model,
        app.exit_prompt,
        app.thinking_enabled,
        app.thinking_available,
        app.thinking_mode.clone(),
        app.show_network_stats,
        app.lsp_server_count,
    );

    // Render model menu as overlay if active
    if let Some(ref menu) = app.model_menu {
        render_model_menu(frame, size, menu);
    }

    // Render settings menu as overlay if active
    if let Some(ref menu) = app.settings_menu {
        render_settings_menu(frame, size, menu);
    }

    // Render MCP menu as overlay if active
    if let Some(ref menu) = app.mcp_menu {
        super::menus::render_mcp_menu(frame, size, menu);
    }
}

/// Render pending prompts (queued output) in a fixed section
fn render_pending_prompts(frame: &mut Frame, area: Rect, app: &App) {
    if app.pending_prompts.is_empty() {
        return;
    }

    let user_bg = Color::Rgb(32, 32, 32);
    let prompt_fg = Color::Rgb(100, 100, 100); // Darker grey for pending
    let text_fg = Color::Rgb(150, 150, 150); // Dimmer text for pending

    // Fill background
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_bg(user_bg);
            }
        }
    }

    // Render each pending prompt
    let mut current_y = area.y;
    let text_width = area
        .width
        .saturating_sub(INPUT_PROMPT_WIDTH + INPUT_PROMPT_GAP);
    for prompt in &app.pending_prompts {
        if current_y >= area.y + area.height {
            break;
        }

        // Calculate height for this prompt using the actual text width
        let prompt_height = text_message_height(&prompt.display_text, text_width);

        // Render hourglass prompt
        if let Some(cell) = frame.buffer_mut().cell_mut((area.x, current_y)) {
            cell.set_char('⧖');
            cell.set_fg(prompt_fg);
            cell.set_bg(user_bg);
        }

        // Render prompt text with word-wrapping
        let text_area = Rect {
            x: area.x + INPUT_PROMPT_WIDTH + INPUT_PROMPT_GAP,
            y: current_y,
            width: text_width,
            height: prompt_height.min(area.y + area.height - current_y),
        };

        render_wrapped_text(
            frame,
            text_area,
            &prompt.display_text,
            Style::default().fg(text_fg).bg(user_bg),
        );

        current_y += prompt_height;
    }
}

/// Render text with word-wrapping matching the input editor's behavior
fn render_wrapped_text(frame: &mut Frame, area: Rect, text: &str, style: Style) {
    use super::layout::{char_display_width, word_display_width};

    let width = area.width as usize;
    if width == 0 || area.height == 0 {
        return;
    }

    let effective_width = width.saturating_sub(1).max(1);

    let mut screen_row: usize = 0;
    let mut screen_col: usize = 0;
    let mut prev_was_whitespace = true;

    for (byte_idx, ch) in text.char_indices() {
        if ch == '\n' {
            screen_row += 1;
            screen_col = 0;
            prev_was_whitespace = true;
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

        // Stop if we're past the visible area
        if screen_row >= area.height as usize {
            break;
        }

        let y = area.y + screen_row as u16;
        let x = area.x + screen_col as u16;

        if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }

        screen_col += ch_width;

        if screen_col == effective_width {
            screen_row += 1;
            screen_col = 0;
        }

        prev_was_whitespace = is_whitespace;
    }
}

/// Render a single message by type
fn render_message(
    frame: &mut Frame,
    content_area: Rect,
    app: &mut App,
    index: usize,
    adjusted_skip: u16,
    selection: &super::selection::Selection,
) {
    use super::messages::{bulletify, format_error_message};

    match &mut app.messages[index] {
        Message::Text(text) => {
            let content = bulletify(text);
            let mut ctx = RenderContext {
                frame,
                area: content_area,
                message_idx: index,
                skip_top: adjusted_skip,
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            super::render::render_markdown_with_selection(&mut ctx, &content, None, None);
        }
        Message::Error(err) => {
            let content = format_error_message(err);
            let mut ctx = RenderContext {
                frame,
                area: content_area,
                message_idx: index,
                skip_top: adjusted_skip,
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            render_text_with_selection(&mut ctx, &content, None, Some(Color::Red));
        }
        Message::AssistantThinking(msg) => {
            let mut ctx = RenderContext {
                frame,
                area: content_area,
                message_idx: index,
                skip_top: adjusted_skip,
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            super::render::render_thinking_message(&mut ctx, msg, None);
        }
        Message::AssistantToolCalls(msg) => {
            let mut ctx = RenderContext {
                frame,
                area: content_area,
                message_idx: index,
                skip_top: adjusted_skip,
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            super::render::render_tool_calls_message(&mut ctx, msg, None);
        }
        Message::AssistantText(msg) => {
            let mut ctx = RenderContext {
                frame,
                area: content_area,
                message_idx: index,
                skip_top: adjusted_skip,
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            super::render::render_text_message(&mut ctx, msg, None);
        }
        Message::Shell(shell) => {
            let mut ctx = RenderContext {
                frame,
                area: content_area,
                message_idx: index,
                skip_top: adjusted_skip,
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            render_text_with_selection(&mut ctx, &shell.display, None, None);
        }
        Message::User(user_msg) => {
            let display_text = user_msg.display_text.clone();
            render_user_message(
                frame,
                content_area,
                app,
                index,
                adjusted_skip,
                selection,
                &display_text,
            );
        }
        Message::Usage(usage_display) => {
            let mut ctx = RenderContext {
                frame,
                area: content_area,
                message_idx: index,
                skip_top: adjusted_skip,
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            render_usage_with_selection(&mut ctx, usage_display);
        }
        Message::TodoList(todo_display) => {
            let mut ctx = RenderContext {
                frame,
                area: content_area,
                message_idx: index,
                skip_top: adjusted_skip,
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            render_todo_list_with_selection(&mut ctx, todo_display);
        }
        Message::FileDiff(diff_display) => {
            let mut ctx = RenderContext {
                frame,
                area: content_area,
                message_idx: index,
                skip_top: adjusted_skip,
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            render_diff_with_selection(&mut ctx, diff_display);
        }
    }
}

/// Render a user message with background and prompt
fn render_user_message(
    frame: &mut Frame,
    content_area: Rect,
    app: &mut App,
    index: usize,
    adjusted_skip: u16,
    selection: &super::selection::Selection,
    display_text: &str,
) {
    // User messages use the same grey background as the input prompt
    let user_bg = Color::Rgb(32, 32, 32);
    let prompt_fg = Color::Rgb(200, 200, 200);

    // Fill the content area with the grey background
    for y in content_area.y..content_area.y + content_area.height {
        for x in content_area.x..content_area.x + content_area.width {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_bg(user_bg);
            }
        }
    }

    // Calculate padding: 1 empty line at top, 1 at bottom
    let top_padding = USER_MESSAGE_PADDING / 2;

    // Adjust area and skip_top for the padding
    // If adjusted_skip is less than top_padding, we're in the padding area
    if adjusted_skip < top_padding {
        // Some padding is visible, offset the text area
        let padding_visible = top_padding - adjusted_skip;
        if content_area.height > padding_visible {
            let inner_area = Rect {
                x: content_area.x,
                y: content_area.y + padding_visible,
                width: content_area.width,
                height: content_area.height - padding_visible,
            };

            // Render the ">" prompt at the start
            if let Some(cell) = frame.buffer_mut().cell_mut((inner_area.x, inner_area.y)) {
                cell.set_char('>');
                cell.set_fg(prompt_fg);
                cell.set_bg(user_bg);
            }

            // Text area starts after "> "
            let text_area = Rect {
                x: inner_area.x + INPUT_PROMPT_WIDTH + INPUT_PROMPT_GAP,
                y: inner_area.y,
                width: inner_area
                    .width
                    .saturating_sub(INPUT_PROMPT_WIDTH + INPUT_PROMPT_GAP),
                height: inner_area.height,
            };
            let mut ctx = RenderContext {
                frame,
                area: text_area,
                message_idx: index,
                skip_top: 0, // No skip since we're starting fresh
                selection,
                position_map: &mut app.position_map,
                byte_offset_base: 0,
            };
            render_text_with_selection(&mut ctx, display_text, Some(user_bg), None);
        }
    } else {
        // Top padding is scrolled past, calculate text skip
        let text_skip = adjusted_skip - top_padding;

        // Only render ">" if we're on the first line of text
        if text_skip == 0
            && let Some(cell) = frame
                .buffer_mut()
                .cell_mut((content_area.x, content_area.y))
        {
            cell.set_char('>');
            cell.set_fg(prompt_fg);
            cell.set_bg(user_bg);
        }

        // Text area starts after "> "
        let text_area = Rect {
            x: content_area.x + INPUT_PROMPT_WIDTH + INPUT_PROMPT_GAP,
            y: content_area.y,
            width: content_area
                .width
                .saturating_sub(INPUT_PROMPT_WIDTH + INPUT_PROMPT_GAP),
            height: content_area.height,
        };
        let mut ctx = RenderContext {
            frame,
            area: text_area,
            message_idx: index,
            skip_top: text_skip,
            selection,
            position_map: &mut app.position_map,
            byte_offset_base: 0,
        };
        render_text_with_selection(&mut ctx, display_text, Some(user_bg), None);
    }
}

/// Render the working indicator (spinner + stats)
fn render_working_indicator(frame: &mut Frame, area: Rect, app: &App) {
    // Area is 2 lines: spinner + text, then empty line below
    let spinner = if app.is_chatting || app.is_compacting {
        SPINNER_FRAMES[app.spinner_frame % SPINNER_FRAMES.len()]
    } else {
        "✓" // Checkmark when done
    };
    let spinner_color = if app.is_chatting || app.is_compacting {
        Color::Cyan
    } else {
        Color::Green
    };
    let spinner_span = Span::styled(spinner, Style::default().fg(spinner_color));

    let (left_text, right_text) = if app.is_compacting {
        (" Compacting...".to_string(), None)
    } else if let Some(duration) = app.streaming_duration {
        let status = if app.is_chatting {
            if app.is_thinking {
                " Thinking..."
            } else {
                " Working..."
            }
        } else {
            " Done"
        };
        let mut stats = if app.streaming_tokens.is_some() && app.streaming_tokens_display > 0 {
            format!(
                "({} tokens • {:.1}s)",
                app.streaming_tokens_display, duration
            )
        } else {
            format!("({:.1}s)", duration)
        };

        // Add context info when done and available
        if !app.is_chatting {
            if let (Some(ctx_tokens), Some(ctx_limit)) =
                (app.last_context_tokens, app.context_limit)
            {
                let ctx_k = (ctx_tokens as f64 / 1000.0).round() as u64;
                let limit_k = ctx_limit / 1000;
                let pct = (ctx_tokens as f64 / ctx_limit as f64) * 100.0;
                stats.push_str(&format!(" • ctx: {}k/{}k ({:.0}%)", ctx_k, limit_k, pct));
            } else if let Some(ctx_tokens) = app.last_context_tokens {
                let ctx_k = (ctx_tokens as f64 / 1000.0).round() as u64;
                stats.push_str(&format!(" • ctx: {}k", ctx_k));
            }
        }

        (status.to_string(), Some(stats))
    } else if app.is_chatting {
        if app.is_thinking {
            (" Thinking...".to_string(), None)
        } else {
            (" Working...".to_string(), None)
        }
    } else {
        (" Done".to_string(), None)
    };

    let left_span = Span::styled(left_text, Style::default().fg(Color::Rgb(150, 150, 150)));
    let left_line = Line::from(vec![spinner_span, left_span]);

    let spinner_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };

    frame.render_widget(
        Paragraph::new(left_line).alignment(Alignment::Left),
        spinner_area,
    );

    if let Some(stat_text) = right_text {
        let right_span = Span::styled(stat_text, Style::default().fg(Color::Rgb(150, 150, 150)));

        // Reserve space for "more" indicator if needed
        let more_width = if app.has_new_content_below && app.scroll_lines > 0 {
            "↓ more ".len() as u16
        } else {
            0
        };

        let stats_area = Rect {
            width: area.width.saturating_sub(more_width),
            ..spinner_area
        };

        frame.render_widget(
            Paragraph::new(Line::from(right_span)).alignment(Alignment::Right),
            stats_area,
        );
    }

    // Render "more" indicator on the same line as working indicator
    if app.has_new_content_below && app.scroll_lines > 0 {
        let indicator_text = "↓ more ";
        let indicator_width = indicator_text.len() as u16;
        let x = area
            .x
            .saturating_add(area.width)
            .saturating_sub(indicator_width);
        let indicator_area = Rect {
            x,
            y: area.y,
            width: indicator_width,
            height: 1,
        };
        let indicator =
            Paragraph::new(indicator_text).style(Style::default().fg(Color::Rgb(100, 100, 100)));
        frame.render_widget(indicator, indicator_area);
    }
}

/// Render the input area with prompt and text
fn render_input_area(
    frame: &mut Frame,
    input_area: Rect,
    app: &mut App,
    display_input: &str,
    cursor_offset: usize,
    needed_input_lines: usize,
) {
    let prompt_fg = Color::Rgb(200, 200, 200);
    let text_fg = Color::Rgb(220, 220, 220);
    let border_fg = Color::Rgb(60, 60, 60);

    let input_block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(border_fg));
    let text_row = input_block.inner(input_area);
    frame.render_widget(input_block, input_area);

    let text_sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(INPUT_PROMPT_WIDTH),
            Constraint::Length(INPUT_PROMPT_GAP),
            Constraint::Min(1),
        ])
        .split(text_row);

    let prompt_symbol = current_prompt(&app.input);
    let prompt = Paragraph::new(prompt_symbol).style(Style::default().fg(prompt_fg));
    frame.render_widget(prompt, text_sections[0]);

    // Store input text area for mouse hit testing
    app.input_text_area = text_sections[2];

    // Calculate cursor position and auto-scroll to keep cursor visible
    let text_width = text_sections[2].width.max(1);
    let cursor_col_offset = app.cursor.saturating_sub(cursor_offset);
    let (cursor_line, col_idx) = cursor_position(display_input, cursor_col_offset, text_width);

    // Auto-scroll the input to keep the cursor visible
    let visible_lines = text_row.height;
    if cursor_line < app.input_scroll {
        // Cursor is above visible area - scroll up
        app.input_scroll = cursor_line;
    } else if cursor_line >= app.input_scroll + visible_lines {
        // Cursor is below visible area - scroll down
        app.input_scroll = cursor_line.saturating_sub(visible_lines.saturating_sub(1));
    }

    // Clamp input scroll to valid range
    let total_input_lines = needed_input_lines as u16;
    let max_input_scroll = total_input_lines.saturating_sub(visible_lines);
    app.input_scroll = app.input_scroll.min(max_input_scroll);

    // Render input with selection highlighting and scroll offset
    render_input_with_selection(
        frame,
        text_sections[2],
        display_input,
        &app.input_selection,
        Color::Reset,
        text_fg,
        app.input_scroll,
    );

    // Cursor inside the input box, accounting for scroll offset
    if app.show_cursor {
        let screen_line = cursor_line.saturating_sub(app.input_scroll);

        let max_cursor_x = text_sections[2]
            .x
            .saturating_add(text_sections[2].width.saturating_sub(1));
        let max_cursor_y = text_row.y.saturating_add(text_row.height.saturating_sub(1));

        let cursor_x = (text_sections[2].x + col_idx).min(max_cursor_x);
        let cursor_y = (text_row.y + screen_line).min(max_cursor_y);
        frame.set_cursor_position((cursor_x, cursor_y));
    }
    // Note: cursor is automatically hidden by ratatui when set_cursor_position is not called
}
