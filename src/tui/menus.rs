// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::commands::DynamicSlashCommand;
use super::models::{HistorySearchState, MODEL_MENU_MAX_VISIBLE, ModelChoice, ModelMenuState};
use super::settings::SettingsMenuState;

pub(crate) fn render_slash_menu(
    frame: &mut Frame,
    area: Rect,
    items: &[DynamicSlashCommand],
    selected: usize,
) {
    let bg = Color::Rgb(24, 24, 24);

    // Calculate max command name length for alignment
    let max_name_len = items.iter().map(|cmd| cmd.name.len()).max().unwrap_or(0);

    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, cmd)| {
            let is_selected = i == selected;
            let prefix = if is_selected { ">" } else { " " };
            let text = format!(
                "{prefix} /{:<width$}  {}",
                cmd.name,
                cmd.description,
                width = max_name_len
            );
            let style = if is_selected {
                Style::default().fg(Color::Cyan).bg(bg)
            } else {
                Style::default().fg(Color::Gray).bg(bg)
            };
            Line::raw(text).style(style)
        })
        .collect();

    let block = Block::default().style(Style::default().bg(bg));
    let widget = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(widget, area);
}

pub(crate) fn render_model_menu(frame: &mut Frame, screen: Rect, menu: &ModelMenuState) {
    let bg = Color::Rgb(32, 32, 32);
    let border_color = Color::Rgb(80, 80, 80);

    // Get filtered choices
    let filtered: Vec<&ModelChoice> = menu.filtered_choices();

    // Calculate popup dimensions
    // Use 2/3 of screen width to accommodate longer model names
    let popup_width = (screen.width * 2 / 3).min(screen.width.saturating_sub(4));
    let visible_items = MODEL_MENU_MAX_VISIBLE.min(filtered.len().max(1));
    let popup_height = (visible_items as u16 + 3).min(screen.height.saturating_sub(4)); // +3 for borders + search

    // Center the popup
    let popup_x = (screen.width.saturating_sub(popup_width)) / 2;
    let popup_y = (screen.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    // Clear the area behind the popup
    frame.render_widget(Clear, popup_area);

    // Build the search line first
    let search_text = if menu.search_query.is_empty() {
        " Search: (type to filter)".to_string()
    } else {
        format!(" Search: {}_", menu.search_query)
    };
    let search_style = if menu.search_query.is_empty() {
        Style::default().fg(Color::DarkGray).bg(bg)
    } else {
        Style::default().fg(Color::Yellow).bg(bg)
    };
    let search_line = Line::raw(search_text).style(search_style);

    // Build the list items
    let mut lines: Vec<Line> = vec![search_line];

    if filtered.is_empty() {
        lines.push(Line::raw(" No matches").style(Style::default().fg(Color::DarkGray).bg(bg)));
    } else {
        // Calculate which items to show (scroll if needed)
        let total = filtered.len();
        let visible = visible_items;
        let max_start = total.saturating_sub(visible);
        let start = menu
            .selected_index
            .saturating_sub(visible.saturating_sub(1))
            .min(max_start);
        let end = (start + visible).min(total);

        for (i, choice) in filtered[start..end].iter().enumerate() {
            let actual_idx = start + i;
            let is_selected = actual_idx == menu.selected_index;

            let prefix = if is_selected { ">" } else { " " };
            let star = if choice.is_favorite { "*" } else { " " };

            let main_style = if is_selected {
                Style::default().fg(Color::Cyan).bg(Color::Rgb(48, 48, 48))
            } else {
                Style::default().fg(Color::White).bg(bg)
            };

            let star_style = if is_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .bg(Color::Rgb(48, 48, 48))
            } else {
                Style::default().fg(Color::Yellow).bg(bg)
            };

            let muted_style = if is_selected {
                Style::default()
                    .fg(Color::Rgb(128, 128, 128))
                    .bg(Color::Rgb(48, 48, 48))
            } else {
                Style::default().fg(Color::Rgb(128, 128, 128)).bg(bg)
            };

            // Build the line with styled spans
            let mut spans = vec![
                Span::styled(prefix, main_style),
                Span::styled(star, star_style),
                Span::styled(choice.display(), main_style),
            ];

            // Add the suffix (provider type) in muted color if present
            if let Some(suffix) = choice.display_suffix() {
                spans.push(Span::styled(suffix, muted_style));
            }

            lines.push(Line::from(spans));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(" Select Model (↑↓ Enter Esc, ^f=fav) ")
        .title_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(bg));

    let widget = Paragraph::new(lines).block(block);
    frame.render_widget(widget, popup_area);
}

pub(crate) fn render_settings_menu(frame: &mut Frame, screen: Rect, menu: &SettingsMenuState) {
    let bg = Color::Rgb(32, 32, 32);
    let border_color = Color::Rgb(80, 80, 80);

    // Calculate popup dimensions - at least 60 characters wide
    let popup_width = 60.min(screen.width.saturating_sub(4));
    let popup_height = (menu.options.len() as u16 + 2).min(screen.height.saturating_sub(4));

    // Center the popup
    let popup_x = (screen.width.saturating_sub(popup_width)) / 2;
    let popup_y = (screen.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    // Clear the area behind the popup
    frame.render_widget(Clear, popup_area);

    let mut lines: Vec<Line> = Vec::new();

    for (i, option) in menu.options.iter().enumerate() {
        let is_selected = i == menu.selected_index;
        let prefix = if is_selected { ">" } else { " " };
        let text = format!("{} {}", prefix, option.display());

        let style = if is_selected {
            Style::default().fg(Color::Cyan).bg(Color::Rgb(48, 48, 48))
        } else {
            Style::default().fg(Color::White).bg(bg)
        };

        lines.push(Line::raw(text).style(style));
    }

    let title = if menu.is_submenu_open() {
        " Settings "
    } else {
        " Settings (Space/Enter to select) "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title)
        .title_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(bg));

    let widget = Paragraph::new(lines).block(block);
    frame.render_widget(widget, popup_area);

    // Render submenu if open
    if let Some(submenu) = &menu.default_model_submenu {
        render_default_model_submenu(frame, screen, submenu);
    }
}

/// Render the default model selection submenu
fn render_default_model_submenu(
    frame: &mut Frame,
    screen: Rect,
    submenu: &super::settings::DefaultModelMenuState,
) {
    let bg = Color::Rgb(28, 28, 28);
    let border_color = Color::Rgb(100, 100, 100);

    let filtered = submenu.filtered_choices();
    let max_visible = 12;
    let visible_count = filtered.len().min(max_visible);

    // Calculate submenu dimensions
    // At least 60 characters wide, or 2/3 of screen width if larger
    let submenu_width = 60
        .max(screen.width * 2 / 3)
        .min(screen.width.saturating_sub(4));
    let submenu_height = (visible_count as u16 + 3).min(screen.height.saturating_sub(4)); // +3 for borders and search

    // Center the submenu in the middle of the screen
    let submenu_x = (screen.width.saturating_sub(submenu_width)) / 2;
    let submenu_y = (screen.height.saturating_sub(submenu_height)) / 2;

    let submenu_area = Rect::new(submenu_x, submenu_y, submenu_width, submenu_height);

    // Clear the area behind the submenu
    frame.render_widget(Clear, submenu_area);

    // Build lines
    let mut lines: Vec<Line> = Vec::new();

    // Search line
    let search_text = if submenu.search_query.is_empty() {
        "Type to search...".to_string()
    } else {
        format!("Search: {}", submenu.search_query)
    };
    let search_style = if submenu.search_query.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Yellow)
    };
    lines.push(Line::raw(search_text).style(search_style));

    // Calculate scroll offset for long lists
    let scroll_offset = if submenu.selected_index >= max_visible {
        submenu.selected_index - max_visible + 1
    } else {
        0
    };

    // Model choices
    for (display_idx, (original_idx, choice)) in filtered.iter().enumerate() {
        if display_idx < scroll_offset {
            continue;
        }
        if display_idx >= scroll_offset + max_visible {
            break;
        }

        let is_selected = display_idx == submenu.selected_index;
        let prefix = if is_selected { ">" } else { " " };

        // Check if this is the currently selected default
        let is_current = match choice {
            super::settings::DefaultModelChoice::LastUsed => {
                submenu.choices.first().is_some_and(|first| {
                    matches!(first, super::settings::DefaultModelChoice::LastUsed)
                        && *original_idx == 0
                })
            }
            _ => false,
        };

        let main_style = if is_selected {
            Style::default().fg(Color::Cyan).bg(Color::Rgb(48, 48, 48))
        } else {
            Style::default().fg(Color::White).bg(bg)
        };

        let muted_style = if is_selected {
            Style::default()
                .fg(Color::Rgb(128, 128, 128))
                .bg(Color::Rgb(48, 48, 48))
        } else {
            Style::default().fg(Color::Rgb(128, 128, 128)).bg(bg)
        };

        // Build the line with styled spans
        let mut spans = vec![Span::styled(
            format!("{} {}", prefix, choice.display()),
            main_style,
        )];

        // Add the suffix (provider type) in muted color if present
        if let Some(suffix) = choice.display_suffix() {
            spans.push(Span::styled(suffix, muted_style));
        }

        if is_current && *original_idx == 0 {
            spans.push(Span::styled(" (current)", muted_style));
        }

        lines.push(Line::from(spans));
    }

    let title = format!(" Select Default Model ({}) ", filtered.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title)
        .title_style(Style::default().fg(Color::Green))
        .style(Style::default().bg(bg));

    let widget = Paragraph::new(lines).block(block);
    frame.render_widget(widget, submenu_area);
}

pub(crate) fn render_history_search(
    frame: &mut Frame,
    area: Rect,
    search: &HistorySearchState,
    history_entries: &[String],
    selected: usize,
) {
    let bg = Color::Rgb(24, 24, 24);
    let selected_bg = Color::Rgb(40, 40, 40);

    // Fill background
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_bg(bg);
            }
        }
    }

    // Draw search prompt on first line
    let search_text = if search.search_query.is_empty() {
        "bck-search: _".to_string()
    } else {
        format!("bck-search: {}_", search.search_query)
    };
    let search_style = if search.search_query.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Yellow)
    };
    for (i, ch) in search_text.chars().enumerate() {
        let x = area.x + i as u16;
        if x < area.x + area.width
            && let Some(cell) = frame.buffer_mut().cell_mut((x, area.y))
        {
            cell.set_char(ch);
            cell.set_style(search_style);
        }
    }

    // Adjust area for entries (skip search line)
    let entries_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    if search.filtered_indices.is_empty() {
        let text = "  (no matches)";
        for (i, ch) in text.chars().enumerate() {
            let x = entries_area.x + i as u16;
            if x < entries_area.x + entries_area.width
                && let Some(cell) = frame.buffer_mut().cell_mut((x, entries_area.y))
            {
                cell.set_char(ch);
                cell.set_fg(Color::DarkGray);
            }
        }
        return;
    }

    let content_width = entries_area.width.saturating_sub(2) as usize;
    let max_lines = entries_area.height as usize;

    // Calculate which entries to show and how many lines each takes
    let mut entries_to_render: Vec<(usize, bool, &str)> = Vec::new();
    let mut total_lines = 0usize;

    // Find the starting entry that keeps selected visible
    let mut start_idx = 0usize;
    for (i, &history_idx) in search.filtered_indices.iter().enumerate() {
        if let Some(entry) = history_entries.get(history_idx) {
            let entry_lines = entry.lines().count().max(1);
            if i < selected {
                // Check if we need to scroll to keep selected visible
                let lines_before_selected: usize = search.filtered_indices[start_idx..=i]
                    .iter()
                    .filter_map(|&idx| history_entries.get(idx))
                    .map(|e| e.lines().count().max(1))
                    .sum();
                let selected_entry_lines = search
                    .filtered_indices
                    .get(selected)
                    .and_then(|&idx| history_entries.get(idx))
                    .map(|e| e.lines().count().max(1))
                    .unwrap_or(1);

                if lines_before_selected + selected_entry_lines > max_lines {
                    start_idx = i + 1;
                }
            }
            let _ = entry_lines; // suppress warning
        }
    }

    // Collect entries to render
    for (i, &history_idx) in search.filtered_indices.iter().enumerate().skip(start_idx) {
        if let Some(entry) = history_entries.get(history_idx) {
            let entry_lines = entry.lines().count().max(1);
            if total_lines + entry_lines > max_lines {
                break;
            }
            entries_to_render.push((i, i == selected, entry));
            total_lines += entry_lines;
        }
    }

    // Render entries
    let mut current_row = 0u16;
    for (_, is_selected, entry) in entries_to_render {
        let style = if is_selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };
        let entry_bg = if is_selected { selected_bg } else { bg };

        let lines: Vec<&str> = entry.lines().collect();
        let lines = if lines.is_empty() { vec![""] } else { lines };

        for (line_idx, line) in lines.iter().enumerate() {
            if current_row >= entries_area.height {
                break;
            }

            let y = entries_area.y + current_row;

            // Fill line background for selected entry
            if is_selected {
                for x in entries_area.x..entries_area.x + entries_area.width {
                    if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                        cell.set_bg(entry_bg);
                    }
                }
            }

            // Draw prefix on first line
            let prefix = if line_idx == 0 {
                if is_selected { "> " } else { "  " }
            } else {
                "  "
            };

            let mut col = 0usize;
            for ch in prefix.chars() {
                let x = entries_area.x + col as u16;
                if x < entries_area.x + entries_area.width
                    && let Some(cell) = frame.buffer_mut().cell_mut((x, y))
                {
                    cell.set_char(ch);
                    cell.set_style(style);
                    cell.set_bg(entry_bg);
                }
                col += 1;
            }

            // Draw content, truncating if needed
            for (drawn, ch) in line.chars().enumerate() {
                if drawn >= content_width {
                    break;
                }
                let x = entries_area.x + col as u16;
                if x < entries_area.x + entries_area.width
                    && let Some(cell) = frame.buffer_mut().cell_mut((x, y))
                {
                    cell.set_char(ch);
                    cell.set_style(style);
                    cell.set_bg(entry_bg);
                }
                col += 1;
            }

            current_row += 1;
        }
    }
}
