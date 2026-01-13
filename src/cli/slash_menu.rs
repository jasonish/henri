// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Slash command menu for CLI mode.
//!
//! Provides an interactive menu for selecting slash commands when the user
//! types "/" at the start of input. The menu appears above the prompt to
//! avoid shifting the input position.

use std::io::{self, Write};

use crossterm::cursor;
use crossterm::queue;
use crossterm::style::{Color, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, ClearType};

use crate::commands::{DynamicSlashCommand, filter_commands};
use crate::custom_commands::{CustomCommand, load_custom_commands};

/// Maximum number of menu items to display at once
const MENU_MAX_VISIBLE: usize = 10;

/// State for the slash menu
pub(super) struct SlashMenuState {
    /// All available items (filtered)
    pub items: Vec<DynamicSlashCommand>,
    /// Current selection index
    pub selected: usize,
    /// Custom commands cache
    custom_commands: Vec<CustomCommand>,
    /// Cached provider info for filtering
    is_claude: bool,
    has_claude_oauth: bool,
}

impl SlashMenuState {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            custom_commands: load_custom_commands().unwrap_or_default(),
            is_claude: false,
            has_claude_oauth: crate::commands::has_claude_oauth_provider(),
        }
    }

    /// Update the menu items based on the current query
    pub fn update(&mut self, query: &str) {
        // Reload custom commands to pick up any newly added files
        self.custom_commands = load_custom_commands().unwrap_or_default();

        self.items = filter_commands(
            query,
            self.is_claude,
            self.has_claude_oauth,
            &self.custom_commands,
        );

        // Reset selection if it's out of bounds
        if self.items.is_empty() || self.selected >= self.items.len() {
            self.selected = 0;
        }
    }

    /// Move selection up
    pub fn move_up(&mut self) {
        if !self.items.is_empty() {
            if self.selected == 0 {
                self.selected = self.items.len() - 1;
            } else {
                self.selected -= 1;
            }
        }
    }

    /// Move selection down
    pub fn move_down(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    /// Get the currently selected item
    pub fn current(&self) -> Option<&DynamicSlashCommand> {
        self.items.get(self.selected)
    }

    /// Calculate the height needed for the menu (in rows)
    pub fn height(&self) -> u16 {
        if self.items.is_empty() {
            0
        } else {
            // Show up to MENU_MAX_VISIBLE items
            MENU_MAX_VISIBLE.min(self.items.len()) as u16
        }
    }

    /// Render the menu at the specified row.
    /// Note: This is called within a synchronized update context from draw_with_stdout,
    /// so we use queue! instead of execute! to batch operations.
    pub fn render(&self, stdout: &mut io::Stdout, start_row: u16) -> io::Result<()> {
        if self.items.is_empty() {
            return Ok(());
        }

        let total = self.items.len();
        let visible_count = MENU_MAX_VISIBLE.min(total);

        // Calculate scroll window to keep selection visible
        let max_start = total.saturating_sub(visible_count);
        let start = self
            .selected
            .saturating_sub(visible_count.saturating_sub(1))
            .min(max_start);
        let end = (start + visible_count).min(total);
        let visible = &self.items[start..end];
        let selected_in_view = self.selected.saturating_sub(start);

        // Calculate max command name length for alignment (add 1 for "/" prefix)
        let max_name_len = visible.iter().map(|cmd| cmd.name.len()).max().unwrap_or(0);
        let name_padding = max_name_len + 2; // +2 for spacing after name

        // Get terminal width for truncation and background fill
        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        for (i, cmd) in visible.iter().enumerate() {
            let row = start_row + i as u16;
            let is_selected = i == selected_in_view;

            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            // Colors matching cli/input.rs menu rendering
            let (bg_color, name_color, desc_color) = if is_selected {
                (
                    Color::Rgb {
                        r: 30,
                        g: 30,
                        b: 30,
                    },
                    Color::Rgb {
                        r: 137,
                        g: 180,
                        b: 250,
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
                        r: 120,
                        g: 120,
                        b: 120,
                    },
                    Color::Rgb {
                        r: 150,
                        g: 150,
                        b: 150,
                    },
                )
            };

            // Calculate spacing for alignment
            let name_width = cmd.name.len() + 1; // +1 for "/" prefix
            let padding_needed = name_padding.saturating_sub(name_width);
            let desc_len = cmd.description.len();
            let content_width = 1 + name_width + padding_needed + desc_len; // leading space + /name + padding + desc
            let remaining_space = term_width.saturating_sub(content_width);

            let padding_str: String = " ".repeat(padding_needed);
            let trailing_str: String = " ".repeat(remaining_space);

            queue!(
                stdout,
                SetBackgroundColor(bg_color),
                SetForegroundColor(name_color)
            )?;
            write!(stdout, " /{}", cmd.name)?;
            write!(stdout, "{}", padding_str)?;
            queue!(stdout, SetForegroundColor(desc_color))?;
            write!(stdout, "{}", cmd.description)?;
            write!(stdout, "{}", trailing_str)?;
            queue!(stdout, ResetColor)?;
        }

        // Show scroll indicator if there are more items
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
                "  ({}/{} commands, ↑↓ to scroll)",
                self.selected + 1,
                total
            )?;
            queue!(stdout, ResetColor)?;
        }

        Ok(())
    }

    /// Height including scroll indicator if needed
    pub fn display_height(&self) -> u16 {
        let base = self.height();
        if base > 0 && self.items.len() > MENU_MAX_VISIBLE {
            base + 1 // Add row for scroll indicator
        } else {
            base
        }
    }
}

/// Extract the slash command query from input.
/// Returns Some(query) if input starts with "/" and has no whitespace after the slash.
pub(super) fn extract_query(input: &str) -> Option<&str> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with('/') {
        return None;
    }

    let after_slash = &trimmed[1..];

    // If there's whitespace after the slash, the user is
    // typing arguments, so the menu should close
    if after_slash.contains(char::is_whitespace) {
        return None;
    }

    Some(after_slash)
}
