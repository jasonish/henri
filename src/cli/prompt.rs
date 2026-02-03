// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Prompt rendering for CLI mode.
//!
//! The prompt box stays at the bottom of the terminal. This module handles
//! rendering the prompt box, status line, and managing terminal position.
//! Input handling is delegated to the input module.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::Instant;

use crossterm::style::{
    Attribute, Color, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{self, ClearType};
use crossterm::{SynchronizedUpdate, cursor, execute, queue};

use crate::usage;

use super::input::{InputState, display_width};
use super::menus::{
    HistorySearchState, McpMenuState, ModelMenuState, SessionMenuState, SettingsMenuState,
    ToolsMenuState,
};
use super::render::colorize_image_markers;
use super::terminal as cli_terminal;

const BORDER_COLOR: Color = Color::Rgb {
    r: 68,
    g: 68,
    b: 68,
};

/// Background color for user prompts (ANSI 256-color 236 = dark grey)
const PROMPT_BG_COLOR: Color = Color::AnsiValue(236);

const EXIT_HINT_TEXT: &str = "Press Ctrl+C again within 2s to exit";
const WELCOME_HINT_TEXT: &str = "Welcome to Henri 🐕, type /help for more info";

/// Thinking status for the status line.
#[derive(Clone, Default)]
pub(super) struct ThinkingStatus {
    /// Whether thinking is available for the current model
    pub available: bool,
    /// Whether thinking is enabled
    pub enabled: bool,
    /// Thinking mode (e.g., "medium", "high") if applicable
    pub mode: Option<String>,
}

/// Security mode status for the status line.
#[derive(Clone)]
pub(super) struct SecurityStatus {
    /// True if read-only mode is enabled
    pub read_only: bool,
    /// True if sandbox is enabled (only relevant when not read_only)
    pub sandbox_enabled: bool,
}

impl Default for SecurityStatus {
    fn default() -> Self {
        Self {
            read_only: false,
            sandbox_enabled: true,
        }
    }
}

/// Info needed for the status line.
#[derive(Clone, Default)]
pub(super) struct StatusInfo {
    pub provider: String,
    pub model: String,
    pub cwd: String,
    pub thinking: ThinkingStatus,
    pub security: SecurityStatus,
    pub lsp_server_count: usize,
    pub mcp_server_count: usize,
}

/// Manages the persistent prompt box at the bottom of the terminal.
#[derive(Clone)]
pub(super) struct PromptBox {
    width: usize,
    border: String,
    last_start_row: Option<u16>,
    last_height: u16,
    status: StatusInfo,
    show_network_stats: bool,
    exit_hint_until: Option<Instant>,
    welcome_hint_active: bool,
}

impl PromptBox {
    pub(super) fn new() -> Self {
        let width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
        let show_network_stats = crate::config::ConfigFile::load()
            .map(|c| c.show_network_stats)
            .unwrap_or(false);
        Self {
            width,
            border: "─".repeat(width),
            last_start_row: None,
            last_height: 0,
            status: StatusInfo::default(),
            show_network_stats,
            exit_hint_until: None,
            welcome_hint_active: false,
        }
    }

    /// Reload the show_network_stats setting from config
    pub(super) fn reload_settings(&mut self) {
        self.show_network_stats = crate::config::ConfigFile::load()
            .map(|c| c.show_network_stats)
            .unwrap_or(false);
    }

    /// Get the last known height of the prompt box.
    pub(super) fn height(&self) -> u16 {
        self.last_height
    }

    pub(super) fn set_exit_hint(&mut self, until: Option<Instant>) {
        self.exit_hint_until = until;
    }

    pub(super) fn set_welcome_hint(&mut self, active: bool) {
        self.welcome_hint_active = active;
    }

    fn should_show_exit_hint(&self, state: &InputState, show_exit_hint: bool) -> bool {
        if !show_exit_hint {
            return false;
        }
        if !state.is_empty() || state.slash_menu_active() {
            return false;
        }
        self.exit_hint_until
            .is_some_and(|until| Instant::now() < until)
    }

    fn should_show_welcome_hint(&self, state: &InputState) -> bool {
        if !self.welcome_hint_active {
            return false;
        }
        state.is_empty() && !state.slash_menu_active()
    }

    /// Update the status line info.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn set_status(
        &mut self,
        provider: String,
        model: String,
        cwd: String,
        thinking: ThinkingStatus,
        security: SecurityStatus,
        lsp_server_count: usize,
        mcp_server_count: usize,
    ) {
        self.status = StatusInfo {
            provider,
            model,
            cwd,
            thinking,
            security,
            lsp_server_count,
            mcp_server_count,
        };
    }

    /// Draw the prompt box with the given input state.
    /// If `inline` is true, positions relative to cursor; otherwise uses last known position.
    pub(super) fn draw(&mut self, state: &InputState, inline: bool) -> io::Result<()> {
        self.draw_with_hint(state, inline, true)
    }

    pub(super) fn draw_with_hint(
        &mut self,
        state: &InputState,
        inline: bool,
        show_exit_hint: bool,
    ) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        let mut stdout = io::stdout();
        self.draw_with_stdout(&mut stdout, state, inline, show_exit_hint)?;

        // Update tracked cursor position using the same viewport calculation as draw_with_stdout
        if let Some(start_row) = self.last_start_row {
            let (wrapped_rows, cursor_pos) = state.display_lines_and_cursor(self.width);
            let menu_height = state
                .slash_menu
                .as_ref()
                .map(|m| m.display_height())
                .unwrap_or(0);

            let max_content_rows = self.max_input_content_rows(menu_height, 0);
            let rows_to_display = wrapped_rows.len().min(max_content_rows);
            let viewport_start =
                self.viewport_start_for_cursor(wrapped_rows.len(), rows_to_display, cursor_pos.row);

            let display_cursor_row = cursor_pos.row.saturating_sub(viewport_start);
            let cursor_row = start_row + 1 + display_cursor_row as u16;
            let prefix_width = state.display_prefix_width(&cursor_pos);
            let cursor_col = prefix_width + cursor_pos.col;
            cli_terminal::set_prompt_cursor(
                cursor_row.saturating_sub(start_row),
                cursor_col as u16,
            );
        }

        Ok(())
    }

    /// Draw the prompt box using a provided stdout handle.
    pub(super) fn draw_with_stdout(
        &mut self,
        stdout: &mut io::Stdout,
        state: &InputState,
        inline: bool,
        show_exit_hint: bool,
    ) -> io::Result<()> {
        self.sync_prompt_position();

        // Refresh dimensions before computing wrapped display so our width is current.
        self.refresh_dimensions();

        // Compute wrapped display
        let (wrapped_rows, cursor_pos) = state.display_lines_and_cursor(self.width);

        let menu_height = state
            .slash_menu
            .as_ref()
            .map(|m| m.display_height())
            .unwrap_or(0);

        // Cap the prompt height (including borders + status line + menu).
        let max_content_rows = self.max_input_content_rows(menu_height, 0);

        // Calculate viewport window.
        let total_rows = wrapped_rows.len();
        let rows_to_display = total_rows.min(max_content_rows);
        let viewport_start =
            self.viewport_start_for_cursor(total_rows, rows_to_display, cursor_pos.row);

        let hidden_above = viewport_start;
        let hidden_below = total_rows.saturating_sub(viewport_start + rows_to_display);
        let border_indicator = self.input_scroll_indicator(hidden_above, hidden_below);

        // Calculate final heights based on capped content display.
        let display_height = (rows_to_display + 2) as u16; // top border + visible rows + bottom border
        let status_row_offset = display_height;
        let actual_total_height = display_height + menu_height + 1;

        // Calculate where everything starts.
        let (start_row, scroll_up) = if inline {
            self.inline_start_row(actual_total_height)?
        } else {
            self.redraw_start_row(actual_total_height)?
        };

        // Compute adjusted old_start_row accounting for pending scroll.
        let old_start_row = self.last_start_row.map(|r| r.saturating_sub(scroll_up));
        let old_height = self.last_height;
        self.last_start_row = Some(start_row);
        self.last_height = actual_total_height;

        // Use synchronized update to prevent flicker
        stdout.sync_update(|stdout| {
            if scroll_up > 0 {
                queue!(stdout, terminal::ScrollUp(scroll_up))?;
            }

            // Clear from the old position to the new position
            // After scrolling, old content moved up, so we need to clear based on new start_row
            self.clear_from_row(
                stdout,
                start_row,
                actual_total_height,
                old_start_row,
                old_height,
            )?;

            // Draw input box first
            self.draw_border_line(stdout, start_row, true)?;

            // Draw visible wrapped rows (viewport window)
            for (display_idx, row) in wrapped_rows
                .iter()
                .skip(viewport_start)
                .take(rows_to_display)
                .enumerate()
            {
                let term_row = start_row + 1 + display_idx as u16;
                queue!(
                    stdout,
                    cursor::MoveTo(0, term_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                let prefix = state.display_prefix_for_row(row);
                let styled_text = colorize_image_markers(&row.text, None);
                write!(stdout, "{}{}", prefix, styled_text)?;
            }

            let input_bottom_row = start_row + 1 + rows_to_display as u16;
            let hint_text = if self.should_show_exit_hint(state, show_exit_hint) {
                Some(EXIT_HINT_TEXT)
            } else if self.should_show_welcome_hint(state) {
                Some(WELCOME_HINT_TEXT)
            } else {
                None
            };
            if let Some(hint_text) = hint_text {
                let hint_row = start_row + 1;
                queue!(
                    stdout,
                    cursor::MoveTo(0, hint_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                let prefix = state.display_prefix_for_row(&wrapped_rows[0]);
                let max_text_width = self.width.saturating_sub(display_width(prefix));
                let display_text = if display_width(hint_text) > max_text_width {
                    truncate_to_width(hint_text, max_text_width.saturating_sub(1))
                } else {
                    hint_text.to_string()
                };
                queue!(stdout, SetAttribute(Attribute::Dim))?;
                write!(stdout, "{}{}", prefix, display_text)?;
                queue!(stdout, SetAttribute(Attribute::Reset))?;
            }
            self.draw_bottom_border_line(stdout, input_bottom_row, border_indicator.as_deref())?;

            // Draw prompt status line below the bottom border
            let status_row = input_bottom_row + 1;
            self.draw_prompt_status_line(stdout, status_row)?;

            // Draw slash menu BELOW the status line if active
            if let Some(ref menu) = state.slash_menu
                && menu_height > 0
            {
                let menu_start_row = status_row + 1;
                menu.render(stdout, menu_start_row)?;
            }

            // Position cursor in input - adjust for viewport offset
            let display_cursor_row = cursor_pos.row.saturating_sub(viewport_start);
            let cursor_row = start_row + 1 + display_cursor_row as u16;
            let prefix_width = state.display_prefix_width(&cursor_pos);
            let cursor_col = prefix_width + cursor_pos.col;
            queue!(
                stdout,
                cursor::MoveTo(cursor_col as u16, cursor_row),
                cursor::Show
            )?;

            io::Result::Ok(())
        })??;

        cli_terminal::set_prompt_visible(actual_total_height, start_row, status_row_offset);

        Ok(())
    }

    /// Draw the prompt box with the model menu visible above.
    pub(super) fn draw_with_model_menu(
        &mut self,
        state: &InputState,
        model_menu: &ModelMenuState,
    ) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        let mut stdout = io::stdout();
        self.sync_prompt_position();

        // Refresh dimensions before computing wrapped display so our width is current.
        self.refresh_dimensions();

        // Compute wrapped display (cursor_pos unused since cursor is hidden in menu mode)
        let (wrapped_rows, _) = state.display_lines_and_cursor(self.width);

        let menu_height = model_menu.display_height();

        // Cap the prompt height so large inputs don't push the menu off-screen.
        let max_content_rows = self.max_input_content_rows(menu_height, 0);
        let total_rows = wrapped_rows.len();
        let rows_to_display = total_rows.min(max_content_rows);
        let border_indicator = self.input_scroll_indicator(total_rows - rows_to_display, 0);

        let input_height = (rows_to_display + 2) as u16; // top border + visible rows + bottom border
        let total_height = input_height + menu_height + 1;
        let status_row_offset = input_height;

        // Use redraw positioning (not inline) for menu display
        let (start_row, scroll_up) = self.redraw_start_row(total_height)?;

        // Compute adjusted old_start_row accounting for pending scroll
        let old_start_row = self.last_start_row.map(|r| r.saturating_sub(scroll_up));
        let old_height = self.last_height;
        self.last_start_row = Some(start_row);
        self.last_height = total_height;

        // Use synchronized update to prevent flicker
        stdout.sync_update(|stdout| {
            if scroll_up > 0 {
                queue!(stdout, terminal::ScrollUp(scroll_up))?;
            }

            // Clear the entire area (input + menu below)
            self.clear_from_row(stdout, start_row, total_height, old_start_row, old_height)?;

            // Draw input box first
            self.draw_border_line(stdout, start_row, true)?;

            // Draw wrapped rows (capped)
            for (display_idx, row) in wrapped_rows.iter().take(rows_to_display).enumerate() {
                let term_row = start_row + 1 + display_idx as u16;
                queue!(
                    stdout,
                    cursor::MoveTo(0, term_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                let prefix = state.display_prefix_for_row(row);
                write!(stdout, "{}{}", prefix, row.text)?;
            }

            let input_bottom_row = start_row + 1 + rows_to_display as u16;
            self.draw_bottom_border_line(stdout, input_bottom_row, border_indicator.as_deref())?;

            // Draw prompt status line below the bottom border
            let status_row = input_bottom_row + 1;
            self.draw_prompt_status_line(stdout, status_row)?;

            // Draw model menu BELOW the status line
            if menu_height > 0 {
                let menu_start_row = status_row + 1;
                model_menu.render(stdout, menu_start_row)?;
            }

            // Hide cursor when menu is active (no input focus)
            queue!(stdout, cursor::Hide)?;

            io::Result::Ok(())
        })??;

        cli_terminal::set_prompt_visible(total_height, start_row, status_row_offset);
        cli_terminal::clear_prompt_cursor();

        Ok(())
    }

    /// Draw the prompt box with the history search menu visible below.
    pub(super) fn draw_with_history_search(
        &mut self,
        state: &InputState,
        history_search: &HistorySearchState,
    ) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        let mut stdout = io::stdout();
        self.sync_prompt_position();

        // Refresh dimensions before computing wrapped display so our width is current.
        self.refresh_dimensions();

        // Compute wrapped display (cursor_pos unused since cursor is hidden in menu mode)
        let (wrapped_rows, _) = state.display_lines_and_cursor(self.width);

        let menu_height = history_search.display_height();

        // Cap the prompt height so large inputs don't push the menu off-screen.
        let max_content_rows = self.max_input_content_rows(menu_height, 0);
        let total_rows = wrapped_rows.len();
        let rows_to_display = total_rows.min(max_content_rows);
        let border_indicator = self.input_scroll_indicator(total_rows - rows_to_display, 0);

        let input_height = (rows_to_display + 2) as u16; // top border + visible rows + bottom border
        let total_height = input_height + menu_height + 1;
        let status_row_offset = input_height;

        // Use redraw positioning (not inline) for menu display
        let (start_row, scroll_up) = self.redraw_start_row(total_height)?;

        // Compute adjusted old_start_row accounting for pending scroll
        let old_start_row = self.last_start_row.map(|r| r.saturating_sub(scroll_up));
        let old_height = self.last_height;
        self.last_start_row = Some(start_row);
        self.last_height = total_height;

        // Use synchronized update to prevent flicker
        stdout.sync_update(|stdout| {
            if scroll_up > 0 {
                queue!(stdout, terminal::ScrollUp(scroll_up))?;
            }

            // Clear the entire area (input + menu below)
            self.clear_from_row(stdout, start_row, total_height, old_start_row, old_height)?;

            // Draw input box first
            self.draw_border_line(stdout, start_row, true)?;

            // Draw wrapped rows (capped)
            for (display_idx, row) in wrapped_rows.iter().take(rows_to_display).enumerate() {
                let term_row = start_row + 1 + display_idx as u16;
                queue!(
                    stdout,
                    cursor::MoveTo(0, term_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                let prefix = state.display_prefix_for_row(row);
                write!(stdout, "{}{}", prefix, row.text)?;
            }

            let input_bottom_row = start_row + 1 + rows_to_display as u16;
            self.draw_bottom_border_line(stdout, input_bottom_row, border_indicator.as_deref())?;

            // Draw prompt status line below the bottom border
            let status_row = input_bottom_row + 1;
            self.draw_prompt_status_line(stdout, status_row)?;

            // Draw history search menu BELOW the status line
            if menu_height > 0 {
                let menu_start_row = status_row + 1;
                history_search.render(stdout, menu_start_row)?;
            }

            // Hide cursor when menu is active (no input focus)
            queue!(stdout, cursor::Hide)?;

            io::Result::Ok(())
        })??;

        cli_terminal::set_prompt_visible(total_height, start_row, status_row_offset);
        cli_terminal::clear_prompt_cursor();

        Ok(())
    }

    /// Draw the prompt box with the sessions menu visible below.
    pub(super) fn draw_with_sessions_menu(
        &mut self,
        state: &InputState,
        sessions_menu: &SessionMenuState,
    ) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        let mut stdout = io::stdout();
        self.sync_prompt_position();

        // Refresh dimensions before computing wrapped display so our width is current.
        self.refresh_dimensions();

        let (wrapped_rows, _) = state.display_lines_and_cursor(self.width);

        let menu_height = sessions_menu.display_height();

        // Cap the prompt height so large inputs don't push the menu off-screen.
        let max_content_rows = self.max_input_content_rows(menu_height, 0);
        let total_rows = wrapped_rows.len();
        let rows_to_display = total_rows.min(max_content_rows);
        let border_indicator = self.input_scroll_indicator(total_rows - rows_to_display, 0);

        let input_height = (rows_to_display + 2) as u16; // top border + visible rows + bottom border
        let total_height = input_height + menu_height + 1;
        let status_row_offset = input_height;

        // Use redraw positioning (not inline) for menu display
        let (start_row, scroll_up) = self.redraw_start_row(total_height)?;

        // Compute adjusted old_start_row accounting for pending scroll
        let old_start_row = self.last_start_row.map(|r| r.saturating_sub(scroll_up));
        let old_height = self.last_height;
        self.last_start_row = Some(start_row);
        self.last_height = total_height;

        // Use synchronized update to prevent flicker
        stdout.sync_update(|stdout| {
            if scroll_up > 0 {
                queue!(stdout, terminal::ScrollUp(scroll_up))?;
            }

            // Clear the entire area (input + menu below)
            self.clear_from_row(stdout, start_row, total_height, old_start_row, old_height)?;

            // Draw input box first (at the top)
            self.draw_border_line(stdout, start_row, true)?;

            for (i, row) in wrapped_rows.iter().enumerate() {
                let term_row = start_row + 1 + i as u16;
                queue!(
                    stdout,
                    cursor::MoveTo(0, term_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                let prefix = state.display_prefix_for_row(row);
                write!(stdout, "{}{}", prefix, row.text)?;
            }

            let input_bottom_row = start_row + 1 + wrapped_rows.len() as u16;
            self.draw_bottom_border_line(stdout, input_bottom_row, border_indicator.as_deref())?;

            // Draw prompt status line below the bottom border
            let status_row = input_bottom_row + 1;
            self.draw_prompt_status_line(stdout, status_row)?;

            // Draw the sessions menu BELOW the status line
            let menu_start_row = status_row + 1;
            sessions_menu.render(stdout, menu_start_row)?;

            // Hide cursor when menu is active (no input focus)
            queue!(stdout, cursor::Hide)?;

            io::Result::Ok(())
        })??;

        cli_terminal::set_prompt_visible(total_height, start_row, status_row_offset);
        cli_terminal::clear_prompt_cursor();

        Ok(())
    }

    /// Draw the prompt box with the settings menu visible above.
    pub(super) fn draw_with_settings_menu(
        &mut self,
        state: &InputState,
        settings_menu: &SettingsMenuState,
    ) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        let mut stdout = io::stdout();
        self.sync_prompt_position();

        // Refresh dimensions before computing wrapped display so our width is current.
        self.refresh_dimensions();

        // Compute wrapped display (cursor_pos unused since cursor is hidden in menu mode)
        let (wrapped_rows, _) = state.display_lines_and_cursor(self.width);

        let menu_height = settings_menu.display_height();

        // Cap the prompt height so large inputs don't push the menu off-screen.
        let max_content_rows = self.max_input_content_rows(menu_height, 0);
        let total_rows = wrapped_rows.len();
        let rows_to_display = total_rows.min(max_content_rows);
        let border_indicator = self.input_scroll_indicator(total_rows - rows_to_display, 0);

        let input_height = (rows_to_display + 2) as u16; // top border + visible rows + bottom border
        let total_height = input_height + menu_height + 1;
        let status_row_offset = input_height;

        // Use redraw positioning (not inline) for menu display
        let (start_row, scroll_up) = self.redraw_start_row(total_height)?;

        // Compute adjusted old_start_row accounting for pending scroll
        let old_start_row = self.last_start_row.map(|r| r.saturating_sub(scroll_up));
        let old_height = self.last_height;
        self.last_start_row = Some(start_row);
        self.last_height = total_height;

        // Use synchronized update to prevent flicker
        stdout.sync_update(|stdout| {
            if scroll_up > 0 {
                queue!(stdout, terminal::ScrollUp(scroll_up))?;
            }

            // Clear the entire area (input + menu below)
            self.clear_from_row(stdout, start_row, total_height, old_start_row, old_height)?;

            // Draw input box first
            self.draw_border_line(stdout, start_row, true)?;

            // Draw wrapped rows (capped)
            for (display_idx, row) in wrapped_rows.iter().take(rows_to_display).enumerate() {
                let term_row = start_row + 1 + display_idx as u16;
                queue!(
                    stdout,
                    cursor::MoveTo(0, term_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                let prefix = state.display_prefix_for_row(row);
                write!(stdout, "{}{}", prefix, row.text)?;
            }

            let input_bottom_row = start_row + 1 + rows_to_display as u16;
            self.draw_bottom_border_line(stdout, input_bottom_row, border_indicator.as_deref())?;

            // Draw prompt status line below the bottom border
            let status_row = input_bottom_row + 1;
            self.draw_prompt_status_line(stdout, status_row)?;

            // Draw settings menu BELOW the status line
            if menu_height > 0 {
                let menu_start_row = status_row + 1;
                settings_menu.render(stdout, menu_start_row)?;
            }

            // Hide cursor when menu is active (no input focus)
            queue!(stdout, cursor::Hide)?;

            io::Result::Ok(())
        })??;

        cli_terminal::set_prompt_visible(total_height, start_row, status_row_offset);
        cli_terminal::clear_prompt_cursor();

        Ok(())
    }

    /// Draw the prompt box with the MCP menu visible below.
    pub(super) fn draw_with_mcp_menu(
        &mut self,
        state: &InputState,
        mcp_menu: &McpMenuState,
    ) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        let mut stdout = io::stdout();
        self.sync_prompt_position();

        // Refresh dimensions before computing wrapped display so our width is current.
        self.refresh_dimensions();

        // Compute wrapped display (cursor_pos unused since cursor is hidden in menu mode)
        let (wrapped_rows, _) = state.display_lines_and_cursor(self.width);

        let menu_height = mcp_menu.display_height();

        // Cap the prompt height so large inputs don't push the menu off-screen.
        let max_content_rows = self.max_input_content_rows(menu_height, 0);
        let total_rows = wrapped_rows.len();
        let rows_to_display = total_rows.min(max_content_rows);
        let border_indicator = self.input_scroll_indicator(total_rows - rows_to_display, 0);

        let input_height = (rows_to_display + 2) as u16; // top border + visible rows + bottom border
        let total_height = input_height + menu_height + 1;
        let status_row_offset = input_height;

        // Use redraw positioning (not inline) for menu display
        let (start_row, scroll_up) = self.redraw_start_row(total_height)?;

        // Compute adjusted old_start_row accounting for pending scroll
        let old_start_row = self.last_start_row.map(|r| r.saturating_sub(scroll_up));
        let old_height = self.last_height;
        self.last_start_row = Some(start_row);
        self.last_height = total_height;

        // Use synchronized update to prevent flicker
        stdout.sync_update(|stdout| {
            if scroll_up > 0 {
                queue!(stdout, terminal::ScrollUp(scroll_up))?;
            }

            // Clear the entire area (input + menu below)
            self.clear_from_row(stdout, start_row, total_height, old_start_row, old_height)?;

            // Draw input box first
            self.draw_border_line(stdout, start_row, true)?;

            // Draw wrapped rows (capped)
            for (display_idx, row) in wrapped_rows.iter().take(rows_to_display).enumerate() {
                let term_row = start_row + 1 + display_idx as u16;
                queue!(
                    stdout,
                    cursor::MoveTo(0, term_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                let prefix = state.display_prefix_for_row(row);
                write!(stdout, "{}{}", prefix, row.text)?;
            }

            let input_bottom_row = start_row + 1 + rows_to_display as u16;
            self.draw_bottom_border_line(stdout, input_bottom_row, border_indicator.as_deref())?;

            // Draw prompt status line below the bottom border
            let status_row = input_bottom_row + 1;
            self.draw_prompt_status_line(stdout, status_row)?;

            // Draw MCP menu BELOW the status line
            if menu_height > 0 {
                let menu_start_row = status_row + 1;
                mcp_menu.render(stdout, menu_start_row)?;
            }

            // Hide cursor when menu is active (no input focus)
            queue!(stdout, cursor::Hide)?;

            io::Result::Ok(())
        })??;

        cli_terminal::set_prompt_visible(total_height, start_row, status_row_offset);
        cli_terminal::clear_prompt_cursor();

        Ok(())
    }

    /// Draw the prompt box with the tools menu visible below.
    pub(super) fn draw_with_tools_menu(
        &mut self,
        state: &InputState,
        tools_menu: &ToolsMenuState,
    ) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        let mut stdout = io::stdout();
        self.sync_prompt_position();

        // Refresh dimensions before computing wrapped display so our width is current.
        self.refresh_dimensions();

        // Compute wrapped display (cursor_pos unused since cursor is hidden in menu mode)
        let (wrapped_rows, _) = state.display_lines_and_cursor(self.width);

        let menu_height = tools_menu.display_height();

        // Cap the prompt height so large inputs don't push the menu off-screen.
        let max_content_rows = self.max_input_content_rows(menu_height, 0);
        let total_rows = wrapped_rows.len();
        let rows_to_display = total_rows.min(max_content_rows);
        let border_indicator = self.input_scroll_indicator(total_rows - rows_to_display, 0);

        let input_height = (rows_to_display + 2) as u16; // top border + visible rows + bottom border
        let total_height = input_height + menu_height + 1;
        let status_row_offset = input_height;

        // Use redraw positioning (not inline) for menu display
        let (start_row, scroll_up) = self.redraw_start_row(total_height)?;

        // Compute adjusted old_start_row accounting for pending scroll
        let old_start_row = self.last_start_row.map(|r| r.saturating_sub(scroll_up));
        let old_height = self.last_height;
        self.last_start_row = Some(start_row);
        self.last_height = total_height;

        // Use synchronized update to prevent flicker
        stdout.sync_update(|stdout| {
            if scroll_up > 0 {
                queue!(stdout, terminal::ScrollUp(scroll_up))?;
            }

            // Clear the entire area (input + menu below)
            self.clear_from_row(stdout, start_row, total_height, old_start_row, old_height)?;

            // Draw input box first
            self.draw_border_line(stdout, start_row, true)?;

            // Draw wrapped rows (capped)
            for (display_idx, row) in wrapped_rows.iter().take(rows_to_display).enumerate() {
                let term_row = start_row + 1 + display_idx as u16;
                queue!(
                    stdout,
                    cursor::MoveTo(0, term_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                let prefix = state.display_prefix_for_row(row);
                write!(stdout, "{}{}", prefix, row.text)?;
            }

            let input_bottom_row = start_row + 1 + rows_to_display as u16;
            self.draw_bottom_border_line(stdout, input_bottom_row, border_indicator.as_deref())?;

            // Draw prompt status line below the bottom border
            let status_row = input_bottom_row + 1;
            self.draw_prompt_status_line(stdout, status_row)?;

            // Draw tools menu BELOW the status line
            if menu_height > 0 {
                let menu_start_row = status_row + 1;
                tools_menu.render(stdout, menu_start_row)?;
            }

            // Hide cursor when menu is active (no input focus)
            queue!(stdout, cursor::Hide)?;

            io::Result::Ok(())
        })??;

        cli_terminal::set_prompt_visible(total_height, start_row, status_row_offset);
        cli_terminal::clear_prompt_cursor();

        Ok(())
    }

    /// Draw the prompt box with pending prompts displayed above it.
    /// Pending prompts are shown with an hourglass prefix in dimmed text.
    pub(super) fn draw_with_pending(
        &mut self,
        state: &InputState,
        pending_prompts: &VecDeque<super::PendingPrompt>,
    ) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        let mut stdout = io::stdout();
        self.sync_prompt_position();

        // Compute wrapped display
        let (wrapped_rows, cursor_pos) = state.display_lines_and_cursor(self.width);

        // Each pending prompt takes one line (no wrapping)
        let pending_height: u16 = pending_prompts.len() as u16;

        // Cap the prompt height (including pending rows, borders, status, and menu).
        let menu_height = state
            .slash_menu
            .as_ref()
            .map(|m| m.display_height())
            .unwrap_or(0);
        let pending_extra = pending_height;

        let max_content_rows = self.max_input_content_rows(menu_height, pending_extra);
        let total_rows = wrapped_rows.len();
        let rows_to_display = total_rows.min(max_content_rows);
        let viewport_start =
            self.viewport_start_for_cursor(total_rows, rows_to_display, cursor_pos.row);

        let hidden_above = viewport_start;
        let hidden_below = total_rows.saturating_sub(viewport_start + rows_to_display);
        let border_indicator = self.input_scroll_indicator(hidden_above, hidden_below);

        let display_height = (rows_to_display + 2) as u16; // top border + visible rows + bottom border
        let status_row_offset = pending_extra + display_height;
        let total_height = pending_extra + display_height + menu_height + 1;

        let (start_row, scroll_up) = self.redraw_start_row(total_height)?;

        // Compute adjusted old_start_row accounting for pending scroll
        let old_start_row = self.last_start_row.map(|r| r.saturating_sub(scroll_up));
        let old_height = self.last_height;
        self.last_start_row = Some(start_row);
        self.last_height = total_height;

        stdout.sync_update(|stdout| {
            // Clear only the parts that can change. Clearing the full prompt area causes a
            // noticeable blink/flicker on some terminals.

            // If the prompt is shrinking (fewer pending prompts), clear the old area so we don't
            // leave artifacts behind.
            if old_height > total_height {
                self.clear_from_row(stdout, start_row, total_height, old_start_row, old_height)?;
            } else if scroll_up > 0 {
                // When the prompt grows, we may need to scroll to keep it visible.
                queue!(stdout, terminal::ScrollUp(scroll_up))?;
                // Ensure any newly exposed rows are cleared.
                self.clear_from_row(stdout, start_row, total_height, old_start_row, old_height)?;
            }

            let mut current_row = start_row;

            // Draw pending prompts (above the input box)
            if pending_height > 0 {
                for prompt in pending_prompts {
                    queue!(
                        stdout,
                        cursor::MoveTo(0, current_row),
                        terminal::Clear(ClearType::CurrentLine),
                        SetBackgroundColor(PROMPT_BG_COLOR)
                    )?;

                    // Hourglass in yellow on grey background
                    queue!(stdout, SetForegroundColor(Color::DarkYellow))?;
                    write!(stdout, "⧖ ")?;

                    // Text in white on grey background (truncated to fit)
                    queue!(stdout, SetForegroundColor(Color::White))?;
                    let max_text_width = self.width.saturating_sub(2);
                    let text = prompt.input.replace('\n', " ⏎ ");
                    let display_text = if display_width(&text) > max_text_width {
                        truncate_to_width(&text, max_text_width.saturating_sub(1))
                    } else {
                        text
                    };
                    // Calculate padding to fill the rest of the line with background
                    let content_width = 2 + display_width(&display_text); // "⧖ " + text
                    let padding = self.width.saturating_sub(content_width);
                    write!(stdout, "{}{:padding$}", display_text, "", padding = padding)?;
                    queue!(stdout, ResetColor)?;

                    current_row += 1;
                }
            }

            // Draw input box
            self.draw_border_line(stdout, current_row, true)?;

            // Draw wrapped rows (viewport window)
            for (display_idx, row) in wrapped_rows
                .iter()
                .skip(viewport_start)
                .take(rows_to_display)
                .enumerate()
            {
                let term_row = current_row + 1 + display_idx as u16;
                queue!(
                    stdout,
                    cursor::MoveTo(0, term_row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
                let prefix = state.display_prefix_for_row(row);
                write!(stdout, "{}{}", prefix, row.text)?;
            }

            let input_bottom_row = current_row + 1 + rows_to_display as u16;
            self.draw_bottom_border_line(stdout, input_bottom_row, border_indicator.as_deref())?;

            // Draw prompt status line below the bottom border
            let status_row = input_bottom_row + 1;
            self.draw_prompt_status_line(stdout, status_row)?;

            // Draw slash menu BELOW the status line if active
            if let Some(ref menu) = state.slash_menu
                && menu_height > 0
            {
                let menu_start_row = status_row + 1;
                menu.render(stdout, menu_start_row)?;
            }

            // Position cursor in input - adjust for viewport offset
            let display_cursor_row = cursor_pos.row.saturating_sub(viewport_start);
            let cursor_row = current_row + 1 + display_cursor_row as u16;
            let prefix_width = state.display_prefix_width(&cursor_pos);
            let cursor_col = prefix_width + cursor_pos.col;
            queue!(
                stdout,
                cursor::MoveTo(cursor_col as u16, cursor_row),
                cursor::Show
            )?;

            io::Result::Ok(())
        })??;

        cli_terminal::set_prompt_visible(total_height, start_row, status_row_offset);

        // Update tracked cursor position (pending, then input; menu is below)
        let input_start = start_row + pending_extra;
        let display_cursor_row = cursor_pos.row.saturating_sub(viewport_start);
        let cursor_row = input_start + 1 + display_cursor_row as u16;
        let prefix_width = state.display_prefix_width(&cursor_pos);
        let cursor_col = prefix_width + cursor_pos.col;
        cli_terminal::set_prompt_cursor(cursor_row.saturating_sub(start_row), cursor_col as u16);

        Ok(())
    }

    /// Handle resize during input. Returns the new dimensions.
    pub(super) async fn handle_resize(
        &mut self,
        state: &InputState,
        pending_prompts: &VecDeque<super::PendingPrompt>,
        mut cols: u16,
        mut rows: u16,
    ) -> io::Result<()> {
        let mut stdout = io::stdout();

        self.sync_prompt_position();
        if let Some(start_row) = self.last_start_row
            && self.last_height > 0
        {
            self.clear_prompt_area(&mut stdout, start_row, self.last_height)?;
            stdout.flush()?;
        }
        self.hide()?;

        // Wait briefly for two consecutive size reads to match.
        let mut last_size = terminal::size().unwrap_or((cols, rows));
        for _ in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(8)).await;
            let current_size = terminal::size().unwrap_or(last_size);
            if current_size == last_size {
                break;
            }
            last_size = current_size;
        }
        cols = last_size.0;
        rows = last_size.1;

        let _buffer_guard = super::terminal::buffer_output();
        let _output_guard = super::terminal::lock_output();

        // Redraw everything from history at new width.
        // Use a conservative prompt height based on the current input state so
        // history output does not render into the spacer/status rows above the prompt.
        //
        // If tool output is actively streaming in *viewport mode*, add its viewport height so
        // history leaves space for it. In expanded mode, tool output is printed as normal
        // scrolling output and should be included in the history render.
        let tool_output_active = super::listener::is_tool_output_viewport_active();
        let tool_output_height = if tool_output_active {
            super::listener::active_tool_output_height()
        } else {
            0
        };

        let prompt_height = {
            let (wrapped_rows, cursor_pos) = state.display_lines_and_cursor(cols as usize);
            let menu_height = state
                .slash_menu
                .as_ref()
                .map(|m| m.display_height())
                .unwrap_or(0);
            let pending_extra = pending_prompts.len() as u16;

            let max_content_rows = self.max_input_content_rows(menu_height, pending_extra);
            let total_rows = wrapped_rows.len();
            let rows_to_display = total_rows.min(max_content_rows);
            let viewport_start =
                self.viewport_start_for_cursor(total_rows, rows_to_display, cursor_pos.row);

            let _hidden_above = viewport_start;
            let _hidden_below = total_rows.saturating_sub(viewport_start + rows_to_display);

            let display_height = (rows_to_display + 2) as u16;
            let input_height = display_height;

            input_height
                .saturating_add(menu_height)
                .saturating_add(pending_extra)
                .saturating_add(1)
                .saturating_add(tool_output_height) // Reserve space for tool output viewport
                .max(4)
        };

        super::terminal::redraw_from_history_with_size_locked(
            prompt_height,
            Some(cols),
            Some(rows),
        );

        // Tool output is skipped in history render when streaming in viewport mode.
        // Set reserved_lines to match what we reserved in prompt_height.
        if tool_output_active {
            super::listener::set_tool_output_reserved(tool_output_height);
        } else {
            super::listener::reset_tool_output_viewport();
        }

        self.refresh_dimensions();
        self.reset_position();
        // Use redraw positioning after a resize redraw so the prompt is anchored
        // to the bottom of the terminal (and keeps the reserved status/spacer rows intact).
        // If there are pending prompts, use draw_with_pending to show them.
        if pending_prompts.is_empty() {
            self.draw_with_stdout(&mut stdout, state, false, false)?;
        } else {
            drop(_output_guard);
            self.draw_with_pending(state, pending_prompts)?;
            super::listener::redraw_status_line();
            super::listener::CliListener::flush_buffered();
            // Render tool output viewport after prompt is visible
            if tool_output_active {
                super::listener::force_tool_output_rerender();
            }
            drop(_buffer_guard);
            super::listener::CliListener::flush_buffered();
            return Ok(());
        }
        drop(_output_guard);
        super::listener::redraw_status_line();
        super::listener::CliListener::flush_buffered();
        // Render tool output viewport after prompt is visible
        if tool_output_active {
            super::listener::force_tool_output_rerender();
        }
        drop(_buffer_guard);
        super::listener::CliListener::flush_buffered();
        Ok(())
    }

    /// Hide the prompt box and mark it as not visible.
    pub(super) fn hide(&mut self) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        self.sync_prompt_position();
        cli_terminal::set_prompt_hidden();
        Ok(())
    }

    /// Hide the prompt and clear its lines from the terminal.
    /// Use this before showing external UI like inquire menus.
    pub(super) fn hide_and_clear(&mut self) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        self.sync_prompt_position();

        if let Some(start_row) = self.last_start_row {
            let mut stdout = io::stdout();
            // Clear all lines that the prompt occupied
            for row in start_row..start_row.saturating_add(self.last_height) {
                execute!(
                    stdout,
                    cursor::MoveTo(0, row),
                    terminal::Clear(ClearType::CurrentLine)
                )?;
            }
            // Move cursor to where the prompt started
            execute!(stdout, cursor::MoveTo(0, start_row))?;
        }

        // Reset position tracking since we've cleared the prompt
        self.reset_position();
        cli_terminal::set_prompt_hidden();
        Ok(())
    }

    /// Hide the prompt and move cursor below it for a clean exit.
    pub(super) fn hide_and_exit(&mut self) -> io::Result<()> {
        let _guard = cli_terminal::lock_output();
        self.sync_prompt_position();

        if let Some(start_row) = self.last_start_row {
            let mut stdout = io::stdout();
            let below_prompt = start_row.saturating_add(self.last_height);
            let term_height = cli_terminal::term_height();

            if below_prompt >= term_height {
                execute!(stdout, cursor::MoveTo(0, term_height.saturating_sub(1)))?;
                writeln!(stdout)?;
            } else {
                execute!(stdout, cursor::MoveTo(0, below_prompt))?;
            }
        }

        cli_terminal::set_prompt_hidden();
        Ok(())
    }

    /// Reset internal state about prompt position.
    /// Use this when the terminal has been cleared or significantly altered
    /// by external commands.
    pub(super) fn reset_position(&mut self) {
        self.last_start_row = None;
        self.last_height = 0;
    }

    /// Redraw history from scratch (for the /redraw command).
    /// Clears the output area and redraws from saved history, keeping the prompt intact.
    pub(super) fn redraw_history(&mut self) -> io::Result<()> {
        // Minimum prompt height: top border + 1 input row + bottom border + status line = 4
        let prompt_height: u16 = 4;

        // Hide the prompt temporarily
        self.hide()?;

        // Redraw the history at current width
        cli_terminal::redraw_from_history(prompt_height);

        // After redraw, the prompt should be at the bottom of the terminal.
        // Set position explicitly to avoid querying cursor (which may fail after
        // returning from alternate screen).
        let term_height = cli_terminal::term_height();
        let start_row = term_height.saturating_sub(prompt_height);
        self.last_start_row = Some(start_row);
        self.last_height = prompt_height;

        // Show a fresh "ready" prompt using redraw mode (inline=false)
        // since we've set the position explicitly
        let state = InputState::new(std::env::current_dir().unwrap_or_default());
        self.draw(&state, false)?;
        super::listener::redraw_status_line();
        super::listener::CliListener::flush_buffered();

        Ok(())
    }

    fn refresh_dimensions(&mut self) {
        self.width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
        self.border = "─".repeat(self.width);
    }

    fn sync_prompt_position(&mut self) {
        if let Some((start_row, height)) = cli_terminal::prompt_position() {
            self.last_start_row = Some(start_row);
            self.last_height = height;
        }
    }

    fn max_prompt_height(&self) -> u16 {
        // Keep the prompt usable when the input becomes very large by capping
        // the prompt block to about half the terminal height.
        (cli_terminal::term_height() / 2).max(4)
    }

    fn max_input_content_rows(&self, menu_height: u16, pending_extra: u16) -> usize {
        // Prompt layout:
        // - pending_extra rows (optional)
        // - top border (1)
        // - content rows (N)
        // - bottom border (1)
        // - status line (1)
        // - menu_height rows (optional)
        let non_content_rows: u16 = pending_extra.saturating_add(menu_height).saturating_add(3);

        self.max_prompt_height()
            .saturating_sub(non_content_rows)
            .max(1) as usize
    }

    fn viewport_start_for_cursor(
        &self,
        total_rows: usize,
        rows_to_display: usize,
        cursor_row: usize,
    ) -> usize {
        if total_rows <= rows_to_display {
            return 0;
        }

        let half_viewport = rows_to_display / 2;
        let ideal_start = cursor_row.saturating_sub(half_viewport);
        let max_start = total_rows.saturating_sub(rows_to_display);
        ideal_start.min(max_start)
    }

    fn inline_start_row(&self, height: u16) -> io::Result<(u16, u16)> {
        let (_, cursor_row) = crossterm::cursor::position()?;
        let term_height = cli_terminal::term_height();
        let max_start_row = term_height.saturating_sub(height);

        let start_row = if let Some(last_start) = self.last_start_row {
            // If the last known prompt position is below the current cursor,
            // the prompt was pushed down by output but the cursor is still "behind".
            // Respect the last known position to avoid overwriting output.
            if last_start > cursor_row {
                last_start
            } else {
                let last_end = last_start.saturating_add(self.last_height);
                if cursor_row >= last_start && cursor_row < last_end {
                    cursor_row.saturating_sub(1)
                } else {
                    cursor_row
                }
            }
        } else {
            // First draw: start at current cursor position.
            let reserve_rows = cli_terminal::streaming_status_line_reserved_rows();
            if reserve_rows > 0 {
                cursor_row.saturating_add(reserve_rows)
            } else {
                cursor_row
            }
        };

        // Calculate scroll needed to keep prompt visible.
        let mut start_row = start_row;
        let mut scroll_up: u16 = 0;

        let end_row = start_row.saturating_add(height);
        if end_row > term_height {
            let delta = end_row - term_height;
            scroll_up = delta;
            start_row = start_row.saturating_sub(delta);
        }

        start_row = start_row.min(max_start_row);

        Ok((start_row, scroll_up))
    }

    fn compute_redraw_start_row(&self, height: u16) -> (u16, u16) {
        let term_height = cli_terminal::term_height();

        // Calculate the maximum start_row that keeps the entire prompt visible
        let max_start_row = term_height.saturating_sub(height);

        let Some(last_start) = self.last_start_row else {
            return (max_start_row, 0);
        };

        // If the prompt grew and would need to move up, scroll instead.
        let mut scroll_up = 0;
        if height > self.last_height && last_start > max_start_row {
            scroll_up = last_start - max_start_row;
        }

        // Apply the scroll to the computed start row.
        let mut start_row = last_start.saturating_sub(scroll_up).min(max_start_row);

        // If end still extends past terminal (edge case), scroll more.
        let end_row = start_row.saturating_add(height);
        if end_row > term_height {
            let delta = end_row - term_height;
            scroll_up = scroll_up.saturating_add(delta);
            start_row = start_row.saturating_sub(delta);
        }

        (start_row, scroll_up)
    }

    fn redraw_start_row(&self, height: u16) -> io::Result<(u16, u16)> {
        if self.last_start_row.is_none() {
            let term_height = cli_terminal::term_height();
            let max_start_row = term_height.saturating_sub(height);
            if max_start_row == 0 {
                return self.inline_start_row(height);
            }
            return Ok((max_start_row, 0));
        }

        let (start_row, scroll_up) = self.compute_redraw_start_row(height);
        Ok((start_row, scroll_up))
    }

    fn clear_prompt_area(
        &mut self,
        stdout: &mut io::Stdout,
        start_row: u16,
        height: u16,
    ) -> io::Result<()> {
        let term_height = cli_terminal::term_height();
        let (clear_start, clear_end) = match self.last_start_row {
            Some(old_start) => {
                let old_end = old_start.saturating_add(self.last_height);
                let new_end = start_row.saturating_add(height);
                (old_start.min(start_row), old_end.max(new_end))
            }
            None => (start_row, start_row.saturating_add(height)),
        };

        let end = clear_end.min(term_height);
        for row in clear_start..end {
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
        }

        Ok(())
    }

    /// Clear the prompt area using explicit old position parameters.
    /// This is used when we've already updated self.last_start_row and need to
    /// clear based on the old position.
    fn clear_from_row(
        &self,
        stdout: &mut io::Stdout,
        start_row: u16,
        height: u16,
        old_start_row: Option<u16>,
        old_height: u16,
    ) -> io::Result<()> {
        let term_height = cli_terminal::term_height();
        let (clear_start, clear_end) = match old_start_row {
            Some(old_start) => {
                let old_end = old_start.saturating_add(old_height);
                let new_end = start_row.saturating_add(height);
                (old_start.min(start_row), old_end.max(new_end))
            }
            None => (start_row, start_row.saturating_add(height)),
        };

        let end = clear_end.min(term_height);
        for row in clear_start..end {
            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;
        }

        Ok(())
    }

    fn draw_border_line(&self, stdout: &mut io::Stdout, row: u16, is_top: bool) -> io::Result<()> {
        queue!(
            stdout,
            cursor::MoveTo(0, row),
            terminal::Clear(ClearType::CurrentLine),
            SetForegroundColor(BORDER_COLOR)
        )?;

        if is_top {
            // Draw the security indicator in the top border
            let (status_text, status_color) = if self.status.security.read_only {
                ("RO", Color::Yellow)
            } else if self.status.security.sandbox_enabled {
                ("RW", Color::Green)
            } else {
                ("YOLO", Color::Red)
            };

            // Draw: ─[RW]───...
            write!(stdout, "─[")?;
            queue!(stdout, SetForegroundColor(status_color))?;
            write!(stdout, "{}", status_text)?;
            queue!(stdout, SetForegroundColor(BORDER_COLOR))?;
            write!(stdout, "]")?;

            // Calculate widths
            let left_width = 3 + status_text.len(); // "─[" + status + "]"

            // Calculate right side indicators (MCP and LSP if active)
            let mcp_text = if self.status.mcp_server_count > 0 {
                Some(format!("MCP: {}", self.status.mcp_server_count))
            } else {
                None
            };
            let lsp_text = if self.status.lsp_server_count > 0 {
                Some(format!("LSP: {}", self.status.lsp_server_count))
            } else {
                None
            };

            // Calculate total right width: each indicator is "[text]─"
            let mcp_width = mcp_text.as_ref().map(|t| t.len() + 3).unwrap_or(0);
            let lsp_width = lsp_text.as_ref().map(|t| t.len() + 3).unwrap_or(0);
            let right_width = mcp_width + lsp_width;

            // Fill the middle with border characters
            let middle_width = self.width.saturating_sub(left_width + right_width);
            for _ in 0..middle_width {
                write!(stdout, "─")?;
            }

            // Draw MCP indicator on the right if active
            if let Some(mcp_text) = mcp_text {
                write!(stdout, "[")?;
                queue!(stdout, SetForegroundColor(Color::Cyan))?;
                write!(stdout, "{}", mcp_text)?;
                queue!(stdout, SetForegroundColor(BORDER_COLOR))?;
                write!(stdout, "]─")?;
            }

            // Draw LSP indicator on the right if active
            if let Some(lsp_text) = lsp_text {
                write!(stdout, "[")?;
                queue!(stdout, SetForegroundColor(Color::Green))?;
                write!(stdout, "{}", lsp_text)?;
                queue!(stdout, SetForegroundColor(BORDER_COLOR))?;
                write!(stdout, "]─")?;
            }
        } else {
            write!(stdout, "{}", self.border)?;
        }

        queue!(stdout, ResetColor)?;
        Ok(())
    }

    fn draw_bottom_border_line(
        &self,
        stdout: &mut io::Stdout,
        row: u16,
        indicator: Option<&str>,
    ) -> io::Result<()> {
        queue!(
            stdout,
            cursor::MoveTo(0, row),
            terminal::Clear(ClearType::CurrentLine),
            SetForegroundColor(BORDER_COLOR)
        )?;

        let Some(indicator) = indicator else {
            write!(stdout, "{}", self.border)?;
            queue!(stdout, ResetColor)?;
            return Ok(());
        };

        let tag = format!("[{}]", indicator);
        let tag_width = display_width(&tag);
        if tag_width >= self.width {
            write!(stdout, "{}", self.border)?;
            queue!(stdout, ResetColor)?;
            return Ok(());
        }

        let remaining = self.width.saturating_sub(tag_width);
        let left = remaining / 2;
        let right = remaining.saturating_sub(left);

        for _ in 0..left {
            write!(stdout, "─")?;
        }
        write!(stdout, "{}", tag)?;
        for _ in 0..right {
            write!(stdout, "─")?;
        }

        queue!(stdout, ResetColor)?;
        Ok(())
    }

    fn input_scroll_indicator(&self, hidden_above: usize, hidden_below: usize) -> Option<String> {
        match (hidden_above, hidden_below) {
            (0, 0) => None,
            (a, 0) => Some(format!("↑{}", a)),
            (0, b) => Some(format!("↓{}", b)),
            (a, b) => Some(format!("↑{} ↓{}", a, b)),
        }
    }

    fn draw_prompt_status_line(&self, stdout: &mut io::Stdout, row: u16) -> io::Result<()> {
        queue!(
            stdout,
            cursor::MoveTo(0, row),
            terminal::Clear(ClearType::CurrentLine),
        )?;

        let width = self.width;
        let provider = &self.status.provider;
        let model = &self.status.model;
        let cwd = &self.status.cwd;

        // Build thinking suffix for model name (e.g., "#high", "#off").
        let mut thinking_suffix: Option<(String, Color)> = if self.status.thinking.available {
            if !self.status.thinking.enabled {
                Some(("#off".to_string(), Color::Yellow))
            } else {
                self.status
                    .thinking
                    .mode
                    .as_ref()
                    .map(|mode| (format!("#{}", mode), Color::Green))
            }
        } else {
            None
        };

        // Avoid duplicating variant suffixes already in the model name.
        let mut model_display = model.as_str();
        if let Some((base, suffix)) = model.split_once('#')
            && let Some((suffix_text, suffix_color)) = &thinking_suffix
            && suffix_text == &format!("#{}", suffix)
        {
            model_display = base;
            thinking_suffix = Some((format!("#{}", suffix), *suffix_color));
        }

        // Build network stats if enabled.
        let net_text = if self.show_network_stats {
            let stats = usage::network_stats();
            Some(format!(
                "↓{} ↑{}",
                format_bytes(stats.rx_bytes()),
                format_bytes(stats.tx_bytes())
            ))
        } else {
            None
        };

        // Calculate widths of fixed elements.
        let provider_width = display_width(provider);
        let model_width = display_width(model_display);
        let cwd_width = display_width(cwd);
        // Thinking suffix width (e.g., "#high" appended to model)
        let thinking_suffix_width = thinking_suffix
            .as_ref()
            .map(|(s, _)| display_width(s))
            .unwrap_or(0);
        let net_width = net_text.as_ref().map(|s| display_width(s)).unwrap_or(0);

        // Fixed: "provider/model#suffix " (the slash and trailing space)
        let fixed_left = provider_width + 1 + model_width + thinking_suffix_width + 1;
        let total_left = fixed_left + cwd_width;
        let total_with_net = total_left + if net_width > 0 { 1 + net_width } else { 0 };

        // Decide what fits. Priority: hide net first, then truncate cwd.
        let show_net = net_text.is_some() && total_with_net <= width;
        let available = width.saturating_sub(if show_net { 1 + net_width } else { 0 });

        // Truncate cwd if needed.
        let cwd_display = if fixed_left + cwd_width > available {
            let max_cwd = available.saturating_sub(fixed_left + 1);
            truncate_to_width(cwd, max_cwd)
        } else {
            cwd.to_string()
        };

        // Calculate net column if we plan to show it.
        let net_col = if show_net {
            (width.saturating_sub(net_width)) as u16
        } else {
            0
        };
        let left_width = fixed_left + display_width(&cwd_display);

        // Track bandwidth layout so the streaming updater can render during a turn.
        let bandwidth_allowed = show_net;
        let bandwidth_min_col = if show_net {
            left_width.saturating_add(1).min(width) as u16
        } else {
            0
        };
        let bandwidth_clear = !show_net;

        // Render: provider (magenta) / (grey) model (cyan) #suffix (colored) space cwd (blue)
        queue!(stdout, SetForegroundColor(Color::Magenta))?;
        write!(stdout, "{}", provider)?;

        queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
        write!(stdout, "/")?;

        queue!(stdout, SetForegroundColor(Color::Cyan))?;
        write!(stdout, "{}", model_display)?;

        // Append thinking suffix to model name (e.g., "#high", "#off")
        if let Some((suffix, color)) = &thinking_suffix {
            queue!(stdout, SetForegroundColor(*color))?;
            write!(stdout, "{}", suffix)?;
        }

        queue!(stdout, ResetColor)?;
        write!(stdout, " ")?;

        queue!(stdout, SetForegroundColor(Color::Blue))?;
        write!(stdout, "{}", cwd_display)?;

        if show_net && let Some(ref text) = net_text {
            queue!(
                stdout,
                cursor::MoveTo(net_col, row),
                SetForegroundColor(Color::DarkGrey)
            )?;
            write!(stdout, "{}", text)?;
        }

        queue!(stdout, ResetColor)?;

        cli_terminal::set_bandwidth_allowed(bandwidth_allowed);
        cli_terminal::set_bandwidth_min_col(bandwidth_min_col);
        if bandwidth_clear {
            cli_terminal::set_bandwidth_col(None);
        }
        Ok(())
    }
}

/// Format bytes as human-readable string (B, KB, MB)
fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;

    let bytes_f = bytes as f64;
    if bytes_f >= MB {
        format!("{:.1} MB", bytes_f / MB)
    } else if bytes_f >= KB {
        format!("{:.1} KB", bytes_f / KB)
    } else {
        format!("{} B", bytes)
    }
}

/// Truncate a string to fit within a display width, adding ellipsis.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthChar;

    let mut result = String::new();
    let mut width = 0;

    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if width + cw > max_width {
            break;
        }
        result.push(c);
        width += cw;
    }
    result.push('…');
    result
}
