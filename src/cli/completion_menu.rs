// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! File completion menu rendering for CLI mode.
//!
//! Uses the same popup style as the slash command menu.

use std::borrow::Cow;
use std::io::{self, Write};

use crossterm::cursor;
use crossterm::queue;
use crossterm::style::{Color, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, ClearType};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Maximum number of completion items to display at once.
const MENU_MAX_VISIBLE: usize = 10;

pub(super) fn display_height(total: usize) -> u16 {
    let base = MENU_MAX_VISIBLE.min(total) as u16;
    if base > 0 && total > MENU_MAX_VISIBLE {
        base + 1 // scroll indicator row
    } else {
        base
    }
}

pub(super) fn render(
    stdout: &mut io::Stdout,
    items: &[String],
    selected: usize,
    start_row: u16,
) -> io::Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    let total = items.len();
    let visible_count = MENU_MAX_VISIBLE.min(total);
    let selected = selected.min(total.saturating_sub(1));

    // Keep selected item visible.
    let max_start = total.saturating_sub(visible_count);
    let start = selected
        .saturating_sub(visible_count.saturating_sub(1))
        .min(max_start);
    let end = (start + visible_count).min(total);
    let visible = &items[start..end];
    let selected_in_view = selected.saturating_sub(start);

    let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

    for (i, item) in visible.iter().enumerate() {
        let row = start_row + i as u16;
        let is_selected = i == selected_in_view;

        queue!(
            stdout,
            cursor::MoveTo(0, row),
            terminal::Clear(ClearType::CurrentLine)
        )?;

        let (bg_color, fg_color) = if is_selected {
            (
                Color::Rgb {
                    r: 30,
                    g: 30,
                    b: 30,
                },
                Color::Rgb {
                    r: 200,
                    g: 200,
                    b: 200,
                },
            )
        } else {
            (
                Color::Rgb {
                    r: 20,
                    g: 20,
                    b: 20,
                },
                Color::Rgb {
                    r: 150,
                    g: 150,
                    b: 150,
                },
            )
        };

        // One leading space, then item text.
        let max_item_width = term_width.saturating_sub(1);
        let item_width = item.width();
        let item_text: Cow<'_, str> = if item_width > max_item_width {
            let mut width = 0;
            let truncated: String = item
                .chars()
                .take_while(|c| {
                    width += c.width().unwrap_or(0);
                    width < max_item_width // leave room for '…'
                })
                .chain(std::iter::once('…'))
                .collect();
            Cow::Owned(truncated)
        } else {
            Cow::Borrowed(item.as_str())
        };

        let shown_width = item_text.width();
        let content_width = 1 + shown_width;
        let trailing = term_width.saturating_sub(content_width);

        queue!(
            stdout,
            SetBackgroundColor(bg_color),
            SetForegroundColor(fg_color)
        )?;
        write!(stdout, " {}", item_text)?;
        write!(stdout, "{}", " ".repeat(trailing))?;
        queue!(stdout, ResetColor)?;
    }

    if total > visible_count {
        let indicator_row = start_row + visible_count as u16;
        queue!(
            stdout,
            cursor::MoveTo(0, indicator_row),
            terminal::Clear(ClearType::CurrentLine),
            SetForegroundColor(Color::DarkGrey)
        )?;
        write!(
            stdout,
            "  ({}/{} matches, ↑↓ to scroll)",
            selected + 1,
            total
        )?;
        queue!(stdout, ResetColor)?;
    }

    Ok(())
}
