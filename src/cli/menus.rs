// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Menu overlays for CLI mode.
//!
//! Provides model selection and other menus that render below the prompt input,
//! similar to the slash command menu.

use std::io::{self, Write};

use crossterm::cursor;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::queue;
use crossterm::style::{Color, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, ClearType};
use unicode_width::UnicodeWidthStr;

use crate::config::{ConfigFile, DefaultModel};
use crate::providers::{ModelChoice, build_model_choices};
use crate::session::{self, SessionInfo};

/// Maximum number of menu items to display at once
const MENU_MAX_VISIBLE: usize = 12;

/// Fuzzy match: checks if all characters of `query` appear in `target` in order.
/// Returns a score (lower is better) or None if no match.
fn fuzzy_match_score(query: &str, target: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    if target.contains(query) {
        if target == query {
            return Some(0); // Exact match
        }
        if target.starts_with(query) {
            return Some(1); // Prefix match
        }
        return Some(2); // Substring match
    }

    // Fuzzy matching: all query chars must appear in order
    let mut query_chars = query.chars().peekable();
    let mut score: u32 = 10;
    let mut last_match_pos: Option<usize> = None;

    for (pos, tc) in target.chars().enumerate() {
        if let Some(&qc) = query_chars.peek()
            && tc == qc
        {
            if let Some(last) = last_match_pos {
                let gap = pos - last - 1;
                score += gap as u32;
            }
            last_match_pos = Some(pos);
            query_chars.next();
        }
    }

    if query_chars.peek().is_none() {
        Some(score)
    } else {
        None
    }
}

/// Action from handling a key event in the model menu
#[derive(Debug)]
pub(super) enum ModelMenuAction {
    /// No action, continue showing menu
    None,
    /// Redraw the menu
    Redraw,
    /// Close the menu without selection
    Cancel,
    /// Model was selected
    Select(ModelChoice),
}

/// State for the model selection menu
pub(super) struct ModelMenuState {
    /// All available model choices
    choices: Vec<ModelChoice>,
    /// Current selection index (in filtered list)
    selected_index: usize,
    /// Search/filter query
    search_query: String,
    /// Current provider/model for highlighting
    current_model: String,
}

impl ModelMenuState {
    /// Create a new model menu state with an explicit current model string
    pub(super) fn with_current_model(current_model: String) -> Self {
        let mut choices = build_model_choices();

        // Sort: favorites first, then by display name
        choices.sort_by(|a, b| {
            b.is_favorite
                .cmp(&a.is_favorite)
                .then_with(|| a.display().cmp(&b.display()))
        });

        // Find index of current model
        let selected_index = choices
            .iter()
            .position(|c| c.short_display() == current_model)
            .unwrap_or(0);

        Self {
            choices,
            selected_index,
            search_query: String::new(),
            current_model,
        }
    }

    /// Get filtered choices matching the search query
    fn filtered_choices(&self) -> Vec<&ModelChoice> {
        if self.search_query.is_empty() {
            self.choices.iter().collect()
        } else {
            let query = self.search_query.to_lowercase();
            let mut matches: Vec<(&ModelChoice, u32)> = self
                .choices
                .iter()
                .filter_map(|c| {
                    let mut targets = vec![c.model_id.to_lowercase()];

                    if let Some(custom) = &c.custom_provider {
                        targets.push(custom.to_lowercase());
                    } else {
                        targets.push(c.provider.display_name().to_lowercase());
                        targets.push(c.provider.id().to_lowercase());
                    }

                    targets
                        .iter()
                        .filter_map(|t| fuzzy_match_score(&query, t))
                        .min()
                        .map(|score| (c, score))
                })
                .collect();

            matches.sort_by(|(_, score_a), (_, score_b)| score_a.cmp(score_b));
            matches.into_iter().map(|(c, _)| c).collect()
        }
    }

    fn toggle_selected_favorite(&mut self) -> bool {
        let filtered = self.filtered_choices();
        let Some(choice) = filtered.get(self.selected_index) else {
            return false;
        };

        let model_id = choice.short_display();
        let provider = choice.provider;
        let choice_model_id = choice.model_id.clone();
        let choice_custom_provider = choice.custom_provider.clone();

        let new_favorite_status = if let Ok(mut config) = ConfigFile::load() {
            let is_now_favorite = config.toggle_favorite(&model_id);
            let _ = config.save();
            is_now_favorite
        } else {
            return false;
        };

        if let Some(choice) = self.choices.iter_mut().find(|c| {
            c.provider == provider
                && c.model_id == choice_model_id
                && c.custom_provider == choice_custom_provider
        }) {
            choice.is_favorite = new_favorite_status;
            true
        } else {
            false
        }
    }

    /// Handle a key event, returning the action to take
    pub fn handle_key(&mut self, key: KeyEvent) -> ModelMenuAction {
        let filtered = self.filtered_choices();

        match (key.code, key.modifiers) {
            // Ctrl+F - toggle favorite
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                if self.toggle_selected_favorite() {
                    ModelMenuAction::Redraw
                } else {
                    ModelMenuAction::None
                }
            }

            // Enter - select current
            (KeyCode::Enter, _) => {
                if let Some(&choice) = filtered.get(self.selected_index) {
                    return ModelMenuAction::Select(choice.clone());
                }
                ModelMenuAction::Cancel
            }

            // Escape - cancel
            (KeyCode::Esc, _) => ModelMenuAction::Cancel,

            // Ctrl+M (if keyboard enhancement is enabled) or Ctrl+O - close (toggle)
            (KeyCode::Char('m'), KeyModifiers::CONTROL)
            | (KeyCode::Char('o'), KeyModifiers::CONTROL) => ModelMenuAction::Cancel,

            // Up arrow
            (KeyCode::Up, _) => {
                if !filtered.is_empty() {
                    if self.selected_index > 0 {
                        self.selected_index -= 1;
                    } else {
                        self.selected_index = filtered.len().saturating_sub(1);
                    }
                }
                ModelMenuAction::Redraw
            }

            // Down arrow
            (KeyCode::Down, _) => {
                if !filtered.is_empty() {
                    if self.selected_index + 1 < filtered.len() {
                        self.selected_index += 1;
                    } else {
                        self.selected_index = 0;
                    }
                }
                ModelMenuAction::Redraw
            }

            // Backspace
            (KeyCode::Backspace, _) => {
                if !self.search_query.is_empty() {
                    self.search_query.pop();
                    self.selected_index = 0;
                    ModelMenuAction::Redraw
                } else {
                    ModelMenuAction::None
                }
            }

            // Character input (filter)
            (KeyCode::Char(c), modifier) if !modifier.contains(KeyModifiers::CONTROL) => {
                self.search_query.push(c);
                self.selected_index = 0;
                ModelMenuAction::Redraw
            }

            // Ctrl+U - clear filter
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.search_query.clear();
                self.selected_index = 0;
                ModelMenuAction::Redraw
            }

            _ => ModelMenuAction::None,
        }
    }

    /// Calculate the height needed for the menu (in rows)
    pub fn height(&self) -> u16 {
        let filtered_count = self.filtered_choices().len();
        let visible = MENU_MAX_VISIBLE.min(filtered_count);
        // Header (search) + items + optional scroll indicator
        let scroll_indicator = if filtered_count > MENU_MAX_VISIBLE {
            1
        } else {
            0
        };
        (1 + visible + scroll_indicator) as u16
    }

    /// Total display height including all elements
    pub fn display_height(&self) -> u16 {
        self.height()
    }

    /// Render the menu at the specified row.
    /// Note: This is called within a synchronized update context from draw_with_model_menu,
    /// so we use queue! instead of execute! to batch operations.
    pub fn render(&self, stdout: &mut io::Stdout, start_row: u16) -> io::Result<()> {
        let filtered = self.filtered_choices();
        let total = filtered.len();
        let visible_count = MENU_MAX_VISIBLE.min(total);

        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        // Background colors matching slash_menu.rs popup style
        let bg_normal = Color::Rgb {
            r: 20,
            g: 20,
            b: 20,
        };
        let bg_selected = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };

        // Draw header/search line with popup background
        queue!(
            stdout,
            cursor::MoveTo(0, start_row),
            terminal::Clear(ClearType::CurrentLine),
            SetBackgroundColor(bg_normal),
            SetForegroundColor(Color::Yellow)
        )?;

        let header_text = if self.search_query.is_empty() {
            " Select model (^f fav, type to filter, ↑↓ navigate):".to_string()
        } else {
            format!(" Filter: {}", self.search_query)
        };
        let header_width = header_text.width();
        write!(stdout, "{}", header_text)?;
        // Fill the rest of the line with background
        let remaining = term_width.saturating_sub(header_width);
        write!(stdout, "{:width$}", "", width = remaining)?;
        queue!(stdout, ResetColor)?;

        if total == 0 {
            queue!(
                stdout,
                cursor::MoveTo(0, start_row + 1),
                terminal::Clear(ClearType::CurrentLine),
                SetBackgroundColor(bg_normal),
                SetForegroundColor(Color::DarkGrey)
            )?;
            let msg = "  No matching models";
            write!(stdout, "{}", msg)?;
            let remaining = term_width.saturating_sub(msg.width());
            write!(stdout, "{:width$}", "", width = remaining)?;
            queue!(stdout, ResetColor)?;
            return Ok(());
        }

        // Calculate scroll window
        let max_start = total.saturating_sub(visible_count);
        let scroll_start = self
            .selected_index
            .saturating_sub(visible_count.saturating_sub(1))
            .min(max_start);
        let scroll_end = (scroll_start + visible_count).min(total);
        let visible = &filtered[scroll_start..scroll_end];
        let selected_in_view = self.selected_index.saturating_sub(scroll_start);

        // Draw items with popup styling
        for (i, choice) in visible.iter().enumerate() {
            let row = start_row + 1 + i as u16;
            let is_selected = i == selected_in_view;

            let star = if choice.is_favorite { "★" } else { " " };
            let display_name = choice.display();
            let is_current = choice.short_display() == self.current_model;
            let current_marker = if is_current { " (current)" } else { "" };

            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            // Colors matching slash_menu.rs
            let (bg_color, name_color, desc_color) = if is_selected {
                (
                    bg_selected,
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
                    bg_normal,
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

            queue!(stdout, SetBackgroundColor(bg_color))?;

            // Leading space
            write!(stdout, " ")?;

            // Star for favorites (always yellow if favorite)
            if choice.is_favorite {
                queue!(stdout, SetForegroundColor(Color::Yellow))?;
            } else {
                queue!(stdout, SetForegroundColor(desc_color))?;
            }
            write!(stdout, "{} ", star)?;

            // Model name
            if is_current && !is_selected {
                queue!(stdout, SetForegroundColor(Color::Green))?;
            } else {
                queue!(stdout, SetForegroundColor(name_color))?;
            }
            write!(stdout, "{}", display_name)?;

            // Current marker in dimmer color
            queue!(stdout, SetForegroundColor(desc_color))?;
            write!(stdout, "{}", current_marker)?;

            // Fill rest of line with background
            // " " + star + " " + name + marker = 1 + star_width + 1 + name_width + marker_width
            let content_width =
                1 + star.width() + 1 + display_name.width() + current_marker.width();
            let remaining = term_width.saturating_sub(content_width);
            write!(stdout, "{:width$}", "", width = remaining)?;

            queue!(stdout, ResetColor)?;
        }

        // Scroll indicator with popup styling
        if total > visible_count {
            let indicator_row = start_row + 1 + visible_count as u16;
            queue!(
                stdout,
                cursor::MoveTo(0, indicator_row),
                terminal::Clear(ClearType::CurrentLine),
                SetBackgroundColor(bg_normal),
                SetForegroundColor(Color::DarkGrey)
            )?;
            let msg = format!(
                "  ({}/{} models, ↑↓ to scroll)",
                self.selected_index + 1,
                total
            );
            write!(stdout, "{}", msg)?;
            let remaining = term_width.saturating_sub(msg.width());
            write!(stdout, "{:width$}", "", width = remaining)?;
            queue!(stdout, ResetColor)?;
        }

        Ok(())
    }
}

/// A setting option in the settings menu
#[derive(Clone, Debug)]
pub(super) enum SettingOption {
    ShowNetworkStats(bool),
    ShowImagePreviews(bool),
    LspEnabled(bool),
    HideToolOutput(bool),
}

impl SettingOption {
    fn label(&self) -> &'static str {
        match self {
            SettingOption::ShowNetworkStats(_) => "Network Stats",
            SettingOption::ShowImagePreviews(_) => "Image Previews",
            SettingOption::LspEnabled(_) => "LSP Integration",
            SettingOption::HideToolOutput(_) => "Hide Tool Output",
        }
    }

    fn value_display(&self) -> String {
        match self {
            SettingOption::ShowNetworkStats(enabled)
            | SettingOption::ShowImagePreviews(enabled)
            | SettingOption::LspEnabled(enabled)
            | SettingOption::HideToolOutput(enabled) => {
                if *enabled { "Enabled" } else { "Disabled" }.to_string()
            }
        }
    }

    fn toggle(&mut self) {
        match self {
            SettingOption::ShowNetworkStats(enabled)
            | SettingOption::ShowImagePreviews(enabled)
            | SettingOption::LspEnabled(enabled)
            | SettingOption::HideToolOutput(enabled) => {
                *enabled = !*enabled;
            }
        }
    }

    fn save(&self) {
        if let Ok(mut config) = ConfigFile::load() {
            match self {
                SettingOption::ShowNetworkStats(enabled) => {
                    config.show_network_stats = *enabled;
                }
                SettingOption::ShowImagePreviews(enabled) => {
                    config.show_image_previews = *enabled;
                }
                SettingOption::LspEnabled(enabled) => {
                    config.lsp_enabled = *enabled;
                }
                SettingOption::HideToolOutput(enabled) => {
                    config.hide_tool_output = *enabled;
                }
            }
            let _ = config.save();
        }
    }
}

/// A choice for the default model selection
#[derive(Clone, Debug)]
pub(super) enum DefaultModelChoice {
    LastUsed,
    Specific(ModelChoice),
}

impl DefaultModelChoice {
    fn display(&self) -> String {
        match self {
            DefaultModelChoice::LastUsed => ":last-used".to_string(),
            DefaultModelChoice::Specific(model) => model.display(),
        }
    }

    fn display_suffix(&self) -> Option<String> {
        match self {
            DefaultModelChoice::LastUsed => None,
            DefaultModelChoice::Specific(model) => model.display_suffix(),
        }
    }

    fn to_default_model(&self) -> DefaultModel {
        match self {
            DefaultModelChoice::LastUsed => DefaultModel::LastUsed,
            DefaultModelChoice::Specific(model) => DefaultModel::Specific(model.short_display()),
        }
    }
}

/// Action from handling a key event in the settings menu
#[derive(Debug)]
pub(super) enum SettingsMenuAction {
    /// No action, continue showing menu
    None,
    /// Redraw the menu
    Redraw,
    /// Close the menu
    Close,
}

/// State for the settings menu
pub(super) struct SettingsMenuState {
    /// Setting options
    options: Vec<SettingOption>,
    /// Current selection index
    selected_index: usize,
    /// Default model submenu state (when open)
    default_model_submenu: Option<DefaultModelSubmenuState>,
}

/// State for the default model selection submenu
struct DefaultModelSubmenuState {
    choices: Vec<DefaultModelChoice>,
    selected_index: usize,
    search_query: String,
    current_default: DefaultModel,
}

impl DefaultModelSubmenuState {
    fn new(current_default: DefaultModel) -> Self {
        let model_choices = build_model_choices();
        let mut choices = vec![DefaultModelChoice::LastUsed];
        choices.extend(model_choices.into_iter().map(DefaultModelChoice::Specific));

        let selected_index = match &current_default {
            DefaultModel::LastUsed => 0,
            DefaultModel::Specific(model_str) => choices
                .iter()
                .position(|c| match c {
                    DefaultModelChoice::Specific(m) => m.short_display() == *model_str,
                    _ => false,
                })
                .unwrap_or(0),
        };

        Self {
            choices,
            selected_index,
            search_query: String::new(),
            current_default,
        }
    }

    fn filtered_choices(&self) -> Vec<&DefaultModelChoice> {
        if self.search_query.is_empty() {
            self.choices.iter().collect()
        } else {
            let query = self.search_query.to_lowercase();
            self.choices
                .iter()
                .filter(|c| c.display().to_lowercase().contains(&query))
                .collect()
        }
    }
}

impl SettingsMenuState {
    /// Create a new settings menu state
    pub fn new() -> Self {
        let config = ConfigFile::load().unwrap_or_default();
        Self {
            options: vec![
                SettingOption::ShowNetworkStats(config.show_network_stats),
                SettingOption::ShowImagePreviews(config.show_image_previews),
                SettingOption::LspEnabled(config.lsp_enabled),
                SettingOption::HideToolOutput(config.hide_tool_output),
            ],
            selected_index: 0,
            default_model_submenu: None,
        }
    }

    /// Handle a key event, returning the action to take
    pub fn handle_key(&mut self, key: KeyEvent) -> SettingsMenuAction {
        // Handle submenu if open
        if self.default_model_submenu.is_some() {
            return self.handle_submenu_key(key);
        }

        match (key.code, key.modifiers) {
            // Escape - close menu
            (KeyCode::Esc, _) => SettingsMenuAction::Close,

            // Enter or Space - toggle selected option
            (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => {
                self.toggle_selected();
                SettingsMenuAction::Redraw
            }

            // Up arrow
            (KeyCode::Up, _) => {
                // +1 for the Default Model option
                let total = self.options.len() + 1;
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                } else {
                    self.selected_index = total.saturating_sub(1);
                }
                SettingsMenuAction::Redraw
            }

            // Down arrow
            (KeyCode::Down, _) => {
                let total = self.options.len() + 1;
                if self.selected_index + 1 < total {
                    self.selected_index += 1;
                } else {
                    self.selected_index = 0;
                }
                SettingsMenuAction::Redraw
            }

            _ => SettingsMenuAction::None,
        }
    }

    fn handle_submenu_key(&mut self, key: KeyEvent) -> SettingsMenuAction {
        let Some(ref mut submenu) = self.default_model_submenu else {
            return SettingsMenuAction::None;
        };

        match (key.code, key.modifiers) {
            // Enter - select model
            (KeyCode::Enter, _) => {
                let filtered = submenu.filtered_choices();
                if let Some(&choice) = filtered.get(submenu.selected_index) {
                    let new_default = choice.to_default_model();
                    if let Ok(mut config) = ConfigFile::load() {
                        config.default_model = new_default;
                        let _ = config.save();
                    }
                }
                self.default_model_submenu = None;
                SettingsMenuAction::Redraw
            }

            // Escape - close submenu
            (KeyCode::Esc, _) => {
                self.default_model_submenu = None;
                SettingsMenuAction::Redraw
            }

            // Up arrow
            (KeyCode::Up, _) => {
                let filtered_len = submenu.filtered_choices().len();
                if filtered_len > 0 {
                    if submenu.selected_index > 0 {
                        submenu.selected_index -= 1;
                    } else {
                        submenu.selected_index = filtered_len.saturating_sub(1);
                    }
                }
                SettingsMenuAction::Redraw
            }

            // Down arrow
            (KeyCode::Down, _) => {
                let filtered_len = submenu.filtered_choices().len();
                if filtered_len > 0 {
                    if submenu.selected_index + 1 < filtered_len {
                        submenu.selected_index += 1;
                    } else {
                        submenu.selected_index = 0;
                    }
                }
                SettingsMenuAction::Redraw
            }

            // Character input (filter)
            (KeyCode::Char(c), modifier) if !modifier.contains(KeyModifiers::CONTROL) => {
                submenu.search_query.push(c);
                submenu.selected_index = 0;
                SettingsMenuAction::Redraw
            }

            // Backspace
            (KeyCode::Backspace, _) => {
                if !submenu.search_query.is_empty() {
                    submenu.search_query.pop();
                    submenu.selected_index = 0;
                    SettingsMenuAction::Redraw
                } else {
                    SettingsMenuAction::None
                }
            }

            // Ctrl+U - clear filter
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                submenu.search_query.clear();
                submenu.selected_index = 0;
                SettingsMenuAction::Redraw
            }

            _ => SettingsMenuAction::None,
        }
    }

    fn toggle_selected(&mut self) {
        // Check if it's the Default Model option (last item)
        if self.selected_index == self.options.len() {
            // Open the default model submenu
            let config = ConfigFile::load().unwrap_or_default();
            self.default_model_submenu = Some(DefaultModelSubmenuState::new(config.default_model));
            return;
        }

        if let Some(option) = self.options.get_mut(self.selected_index) {
            option.toggle();
            option.save();
        }
    }

    /// Calculate the height needed for the menu (in rows)
    pub fn height(&self) -> u16 {
        if let Some(ref submenu) = self.default_model_submenu {
            // Submenu replaces main menu
            let filtered_count = submenu.filtered_choices().len();
            let visible = MENU_MAX_VISIBLE.min(filtered_count);
            let scroll_indicator = if filtered_count > MENU_MAX_VISIBLE {
                1
            } else {
                0
            };
            // Header + items + scroll indicator
            (1 + visible + scroll_indicator) as u16
        } else {
            // Main menu: header + options + default model option
            (1 + self.options.len() + 1) as u16
        }
    }

    /// Total display height including all elements
    pub fn display_height(&self) -> u16 {
        self.height()
    }

    /// Render the menu at the specified row.
    pub fn render(&self, stdout: &mut io::Stdout, start_row: u16) -> io::Result<()> {
        if let Some(ref submenu) = self.default_model_submenu {
            self.render_submenu(stdout, start_row, submenu)
        } else {
            self.render_main_menu(stdout, start_row)
        }
    }

    fn render_main_menu(&self, stdout: &mut io::Stdout, start_row: u16) -> io::Result<()> {
        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        let bg_normal = Color::Rgb {
            r: 20,
            g: 20,
            b: 20,
        };
        let bg_selected = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };

        // Header line
        queue!(
            stdout,
            cursor::MoveTo(0, start_row),
            terminal::Clear(ClearType::CurrentLine),
            SetBackgroundColor(bg_normal),
            SetForegroundColor(Color::Yellow)
        )?;

        let header_text = " Settings (↑↓ navigate, Enter/Space toggle, Esc close):";
        let header_width = header_text.width();
        write!(stdout, "{}", header_text)?;
        let remaining = term_width.saturating_sub(header_width);
        write!(stdout, "{:width$}", "", width = remaining)?;
        queue!(stdout, ResetColor)?;

        // Calculate max label width for alignment
        let max_label_width = self
            .options
            .iter()
            .map(|o| o.label().width())
            .max()
            .unwrap_or(0)
            .max("Default Model".width());

        // Render setting options
        for (i, option) in self.options.iter().enumerate() {
            let row = start_row + 1 + i as u16;
            let is_selected = i == self.selected_index;

            self.render_option_line(
                stdout,
                row,
                is_selected,
                option.label(),
                &option.value_display(),
                max_label_width,
                term_width,
                bg_normal,
                bg_selected,
            )?;
        }

        // Render Default Model option (last item)
        let default_model_row = start_row + 1 + self.options.len() as u16;
        let is_default_model_selected = self.selected_index == self.options.len();

        let config = ConfigFile::load().unwrap_or_default();
        let default_model_value = match &config.default_model {
            DefaultModel::LastUsed => ":last-used".to_string(),
            DefaultModel::Specific(m) => m.clone(),
        };

        self.render_option_line(
            stdout,
            default_model_row,
            is_default_model_selected,
            "Default Model",
            &default_model_value,
            max_label_width,
            term_width,
            bg_normal,
            bg_selected,
        )?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn render_option_line(
        &self,
        stdout: &mut io::Stdout,
        row: u16,
        is_selected: bool,
        label: &str,
        value: &str,
        max_label_width: usize,
        term_width: usize,
        bg_normal: Color,
        bg_selected: Color,
    ) -> io::Result<()> {
        queue!(
            stdout,
            cursor::MoveTo(0, row),
            terminal::Clear(ClearType::CurrentLine)
        )?;

        let (bg_color, label_color, value_color) = if is_selected {
            (
                bg_selected,
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
                bg_normal,
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

        queue!(stdout, SetBackgroundColor(bg_color))?;

        // Leading space and indicator
        write!(stdout, " ")?;
        if is_selected {
            queue!(stdout, SetForegroundColor(Color::Cyan))?;
            write!(stdout, ">")?;
        } else {
            write!(stdout, " ")?;
        }
        write!(stdout, " ")?;

        // Label (padded for alignment)
        queue!(stdout, SetForegroundColor(label_color))?;
        write!(stdout, "{:width$}", label, width = max_label_width)?;

        // Separator
        queue!(stdout, SetForegroundColor(value_color))?;
        write!(stdout, ": ")?;

        // Value
        write!(stdout, "{}", value)?;

        // Fill rest of line
        let content_width = 3 + max_label_width + 2 + value.width();
        let remaining = term_width.saturating_sub(content_width);
        write!(stdout, "{:width$}", "", width = remaining)?;

        queue!(stdout, ResetColor)?;
        Ok(())
    }

    fn render_submenu(
        &self,
        stdout: &mut io::Stdout,
        start_row: u16,
        submenu: &DefaultModelSubmenuState,
    ) -> io::Result<()> {
        let filtered = submenu.filtered_choices();
        let total = filtered.len();
        let visible_count = MENU_MAX_VISIBLE.min(total);
        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        let bg_normal = Color::Rgb {
            r: 20,
            g: 20,
            b: 20,
        };
        let bg_selected = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };

        // Header/search line
        queue!(
            stdout,
            cursor::MoveTo(0, start_row),
            terminal::Clear(ClearType::CurrentLine),
            SetBackgroundColor(bg_normal),
            SetForegroundColor(Color::Yellow)
        )?;

        let header_text = if submenu.search_query.is_empty() {
            " Select default model (type to filter, ↑↓ navigate):".to_string()
        } else {
            format!(" Filter: {}", submenu.search_query)
        };
        let header_width = header_text.width();
        write!(stdout, "{}", header_text)?;
        let remaining = term_width.saturating_sub(header_width);
        write!(stdout, "{:width$}", "", width = remaining)?;
        queue!(stdout, ResetColor)?;

        if total == 0 {
            queue!(
                stdout,
                cursor::MoveTo(0, start_row + 1),
                terminal::Clear(ClearType::CurrentLine),
                SetBackgroundColor(bg_normal),
                SetForegroundColor(Color::DarkGrey)
            )?;
            let msg = "  No matching models";
            write!(stdout, "{}", msg)?;
            let remaining = term_width.saturating_sub(msg.width());
            write!(stdout, "{:width$}", "", width = remaining)?;
            queue!(stdout, ResetColor)?;
            return Ok(());
        }

        // Calculate scroll window
        let max_start = total.saturating_sub(visible_count);
        let scroll_start = submenu
            .selected_index
            .saturating_sub(visible_count.saturating_sub(1))
            .min(max_start);
        let scroll_end = (scroll_start + visible_count).min(total);
        let visible = &filtered[scroll_start..scroll_end];
        let selected_in_view = submenu.selected_index.saturating_sub(scroll_start);

        // Draw items
        for (i, choice) in visible.iter().enumerate() {
            let row = start_row + 1 + i as u16;
            let is_selected = i == selected_in_view;
            let is_current = match (&submenu.current_default, *choice) {
                (DefaultModel::LastUsed, DefaultModelChoice::LastUsed) => true,
                (DefaultModel::Specific(s), DefaultModelChoice::Specific(m)) => {
                    m.short_display() == *s
                }
                _ => false,
            };
            let current_marker = if is_current { " (current)" } else { "" };

            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            let (bg_color, name_color, desc_color) = if is_selected {
                (
                    bg_selected,
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
                    bg_normal,
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

            queue!(stdout, SetBackgroundColor(bg_color))?;

            // Leading space
            write!(stdout, " ")?;

            // Selection indicator
            if is_selected {
                queue!(stdout, SetForegroundColor(Color::Cyan))?;
                write!(stdout, ">")?;
            } else {
                write!(stdout, " ")?;
            }
            write!(stdout, " ")?;

            // Model name
            let display_name = choice.display();
            if is_current && !is_selected {
                queue!(stdout, SetForegroundColor(Color::Green))?;
            } else {
                queue!(stdout, SetForegroundColor(name_color))?;
            }
            write!(stdout, "{}", display_name)?;

            // Provider suffix
            if let Some(suffix) = choice.display_suffix() {
                queue!(stdout, SetForegroundColor(desc_color))?;
                write!(stdout, " ({})", suffix)?;
            }

            // Current marker
            queue!(stdout, SetForegroundColor(desc_color))?;
            write!(stdout, "{}", current_marker)?;

            // Fill rest of line
            let suffix_width = choice.display_suffix().map_or(0, |s| s.width() + 3);
            let content_width = 3 + display_name.width() + suffix_width + current_marker.width();
            let remaining = term_width.saturating_sub(content_width);
            write!(stdout, "{:width$}", "", width = remaining)?;

            queue!(stdout, ResetColor)?;
        }

        // Scroll indicator
        if total > visible_count {
            let indicator_row = start_row + 1 + visible_count as u16;
            queue!(
                stdout,
                cursor::MoveTo(0, indicator_row),
                terminal::Clear(ClearType::CurrentLine),
                SetBackgroundColor(bg_normal),
                SetForegroundColor(Color::DarkGrey)
            )?;
            let msg = format!(
                "  ({}/{} models, ↑↓ to scroll)",
                submenu.selected_index + 1,
                total
            );
            write!(stdout, "{}", msg)?;
            let remaining = term_width.saturating_sub(msg.width());
            write!(stdout, "{:width$}", "", width = remaining)?;
            queue!(stdout, ResetColor)?;
        }

        Ok(())
    }
}
#[derive(Debug)]
pub(super) enum SessionMenuAction {
    /// No action, continue showing menu
    None,
    /// Redraw the menu
    Redraw,
    /// Close the menu without selection
    Cancel,
    /// Session was selected
    Select(SessionInfo),
}

/// State for the session selection menu
pub(super) struct SessionMenuState {
    /// All available sessions
    sessions: Vec<SessionInfo>,
    /// Current selection index
    selected_index: usize,
    /// Current session ID (to highlight)
    current_session_id: Option<String>,
}

impl SessionMenuState {
    /// Create a new session menu state for the given working directory
    pub fn new(working_dir: &std::path::Path, current_session_id: Option<&str>) -> Self {
        let sessions = session::list_sessions(working_dir);
        Self {
            sessions,
            selected_index: 0,
            current_session_id: current_session_id.map(|s| s.to_string()),
        }
    }

    /// Handle a key event, returning the action to take
    pub fn handle_key(&mut self, key: KeyEvent) -> SessionMenuAction {
        let total = self.sessions.len();
        if total == 0 {
            // No sessions, Escape or Enter closes
            return match key.code {
                KeyCode::Esc | KeyCode::Enter => SessionMenuAction::Cancel,
                _ => SessionMenuAction::None,
            };
        }

        match (key.code, key.modifiers) {
            // Enter - select current
            (KeyCode::Enter, _) => {
                if let Some(session) = self.sessions.get(self.selected_index) {
                    return SessionMenuAction::Select(session.clone());
                }
                SessionMenuAction::Cancel
            }

            // Escape - cancel
            (KeyCode::Esc, _) => SessionMenuAction::Cancel,

            // Up arrow
            (KeyCode::Up, _) => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                } else {
                    self.selected_index = total.saturating_sub(1);
                }
                SessionMenuAction::Redraw
            }

            // Down arrow
            (KeyCode::Down, _) => {
                if self.selected_index + 1 < total {
                    self.selected_index += 1;
                } else {
                    self.selected_index = 0;
                }
                SessionMenuAction::Redraw
            }

            _ => SessionMenuAction::None,
        }
    }

    /// Calculate the height needed for the menu (in rows)
    pub fn height(&self) -> u16 {
        let session_count = self.sessions.len();
        if session_count == 0 {
            // Header + "no sessions" message
            return 2;
        }
        let visible = MENU_MAX_VISIBLE.min(session_count);
        // Header + items + optional scroll indicator
        let scroll_indicator = if session_count > MENU_MAX_VISIBLE {
            1
        } else {
            0
        };
        (1 + visible + scroll_indicator) as u16
    }

    /// Total display height including all elements
    pub fn display_height(&self) -> u16 {
        self.height()
    }

    /// Render the menu at the specified row.
    pub fn render(&self, stdout: &mut io::Stdout, start_row: u16) -> io::Result<()> {
        let total = self.sessions.len();
        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        // Draw header line
        queue!(
            stdout,
            cursor::MoveTo(0, start_row),
            terminal::Clear(ClearType::CurrentLine),
            SetForegroundColor(Color::Yellow)
        )?;
        write!(
            stdout,
            "Select session (↑↓ to navigate, Enter to select, Esc to cancel):"
        )?;
        queue!(stdout, ResetColor)?;

        if total == 0 {
            queue!(
                stdout,
                cursor::MoveTo(0, start_row + 1),
                terminal::Clear(ClearType::CurrentLine),
                SetForegroundColor(Color::DarkGrey)
            )?;
            write!(stdout, "  No sessions found for this directory")?;
            queue!(stdout, ResetColor)?;
            return Ok(());
        }

        let visible_count = MENU_MAX_VISIBLE.min(total);

        // Calculate scroll window
        let max_start = total.saturating_sub(visible_count);
        let scroll_start = self
            .selected_index
            .saturating_sub(visible_count.saturating_sub(1))
            .min(max_start);
        let scroll_end = (scroll_start + visible_count).min(total);
        let visible = &self.sessions[scroll_start..scroll_end];
        let selected_in_view = self.selected_index.saturating_sub(scroll_start);

        // Draw items
        for (i, session_info) in visible.iter().enumerate() {
            let row = start_row + 1 + i as u16;
            let is_selected = i == selected_in_view;
            let is_current = self
                .current_session_id
                .as_ref()
                .is_some_and(|id| id == &session_info.id);

            let age = session::format_age(&session_info.saved_at);
            let preview = session_info
                .preview
                .as_deref()
                .unwrap_or("(no preview)")
                .chars()
                .take(50)
                .collect::<String>();
            let current_marker = if is_current { " (current)" } else { "" };

            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            // Selection indicator
            if is_selected {
                queue!(stdout, SetForegroundColor(Color::Cyan))?;
                write!(stdout, ">")?;
            } else {
                write!(stdout, " ")?;
            }
            write!(stdout, " ")?;

            // Age in brackets
            queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
            write!(stdout, "[{}]", age)?;

            // Preview text
            if is_selected {
                queue!(stdout, SetForegroundColor(Color::Cyan))?;
            } else if is_current {
                queue!(stdout, SetForegroundColor(Color::Green))?;
            } else {
                queue!(stdout, ResetColor)?;
            }

            let line = format!(" {}{}", preview, current_marker);
            let max_len = term_width.saturating_sub(15 + age.len()); // Account for prefix
            let display_line: String = line.chars().take(max_len).collect();
            write!(stdout, "{}", display_line)?;

            queue!(stdout, ResetColor)?;
        }

        // Scroll indicator
        if total > visible_count {
            let indicator_row = start_row + 1 + visible_count as u16;
            queue!(
                stdout,
                cursor::MoveTo(0, indicator_row),
                terminal::Clear(ClearType::CurrentLine),
                SetForegroundColor(Color::DarkGrey)
            )?;
            write!(
                stdout,
                "  ({}/{} sessions, ↑↓ to scroll)",
                self.selected_index + 1,
                total
            )?;
            queue!(stdout, ResetColor)?;
        }

        Ok(())
    }
}

/// Action from handling a key event in the MCP menu
#[derive(Debug)]
pub(super) enum McpMenuAction {
    /// No action, continue showing menu
    None,
    /// Redraw the menu
    Redraw,
    /// Close the menu
    Close,
    /// Toggle server at the given index
    ToggleServer(usize),
}

/// Status of an MCP server in the menu
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum McpServerState {
    Disabled,
    Starting,
    Enabled,
}

/// An MCP server option in the menu
#[derive(Clone, Debug)]
pub(super) struct McpServerOption {
    pub name: String,
    pub state: McpServerState,
    pub tool_count: usize,
}

/// State for the MCP server selection menu
pub(super) struct McpMenuState {
    /// Server options
    servers: Vec<McpServerOption>,
    /// Current selection index
    selected_index: usize,
}

impl McpMenuState {
    /// Create a new MCP menu state from the current server statuses
    pub fn new(statuses: Vec<crate::mcp::McpServerStatus>) -> Self {
        let servers = statuses
            .into_iter()
            .map(|s| McpServerOption {
                name: s.name,
                state: if s.is_running {
                    McpServerState::Enabled
                } else {
                    McpServerState::Disabled
                },
                tool_count: s.tool_count,
            })
            .collect();
        Self {
            servers,
            selected_index: 0,
        }
    }

    /// Get the name of the selected server
    pub fn selected_server_name(&self) -> Option<&str> {
        self.servers
            .get(self.selected_index)
            .map(|s| s.name.as_str())
    }

    /// Check if a server is currently disabled
    pub fn is_server_disabled(&self, name: &str) -> bool {
        self.servers
            .iter()
            .find(|s| s.name == name)
            .is_some_and(|s| s.state == McpServerState::Disabled)
    }

    /// Mark a server as starting (called before async toggle)
    pub fn set_server_starting(&mut self, name: &str) {
        if let Some(server) = self.servers.iter_mut().find(|s| s.name == name) {
            server.state = McpServerState::Starting;
        }
    }

    /// Update server status after toggle completes
    pub fn update_server_status(&mut self, name: &str, is_running: bool, tool_count: usize) {
        if let Some(server) = self.servers.iter_mut().find(|s| s.name == name) {
            server.state = if is_running {
                McpServerState::Enabled
            } else {
                McpServerState::Disabled
            };
            server.tool_count = tool_count;
        }
    }

    /// Handle a key event, returning the action to take
    pub fn handle_key(&mut self, key: KeyEvent) -> McpMenuAction {
        let total = self.servers.len();

        match (key.code, key.modifiers) {
            // Escape - close menu
            (KeyCode::Esc, _) => McpMenuAction::Close,

            // Enter or Space - toggle selected server
            (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => {
                if total > 0 {
                    McpMenuAction::ToggleServer(self.selected_index)
                } else {
                    McpMenuAction::None
                }
            }

            // Up arrow
            (KeyCode::Up, _) => {
                if total > 0 {
                    if self.selected_index > 0 {
                        self.selected_index -= 1;
                    } else {
                        self.selected_index = total.saturating_sub(1);
                    }
                }
                McpMenuAction::Redraw
            }

            // Down arrow
            (KeyCode::Down, _) => {
                if total > 0 {
                    if self.selected_index + 1 < total {
                        self.selected_index += 1;
                    } else {
                        self.selected_index = 0;
                    }
                }
                McpMenuAction::Redraw
            }

            _ => McpMenuAction::None,
        }
    }

    /// Calculate the height needed for the menu (in rows)
    pub fn height(&self) -> u16 {
        if self.servers.is_empty() {
            // Header + "no servers" message
            2
        } else {
            // Header + servers
            (1 + self.servers.len()) as u16
        }
    }

    /// Total display height including all elements
    pub fn display_height(&self) -> u16 {
        self.height()
    }

    /// Render the menu at the specified row.
    pub fn render(&self, stdout: &mut io::Stdout, start_row: u16) -> io::Result<()> {
        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        let bg_normal = Color::Rgb {
            r: 20,
            g: 20,
            b: 20,
        };
        let bg_selected = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };

        // Header line
        queue!(
            stdout,
            cursor::MoveTo(0, start_row),
            terminal::Clear(ClearType::CurrentLine),
            SetBackgroundColor(bg_normal),
            SetForegroundColor(Color::Yellow)
        )?;

        let header_text = " MCP Servers (↑↓ navigate, Enter/Space toggle, Esc close):";
        let header_width = header_text.width();
        write!(stdout, "{}", header_text)?;
        let remaining = term_width.saturating_sub(header_width);
        write!(stdout, "{:width$}", "", width = remaining)?;
        queue!(stdout, ResetColor)?;

        if self.servers.is_empty() {
            queue!(
                stdout,
                cursor::MoveTo(0, start_row + 1),
                terminal::Clear(ClearType::CurrentLine),
                SetBackgroundColor(bg_normal),
                SetForegroundColor(Color::DarkGrey)
            )?;
            let msg = "  No MCP servers configured.";
            write!(stdout, "{}", msg)?;
            let remaining = term_width.saturating_sub(msg.width());
            write!(stdout, "{:width$}", "", width = remaining)?;
            queue!(stdout, ResetColor)?;
            return Ok(());
        }

        // Calculate max name width for alignment
        let max_name_width = self
            .servers
            .iter()
            .map(|s| s.name.width())
            .max()
            .unwrap_or(0);

        // Render server options
        for (i, server) in self.servers.iter().enumerate() {
            let row = start_row + 1 + i as u16;
            let is_selected = i == self.selected_index;

            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            let (bg_color, name_color, status_color) = if is_selected {
                (
                    bg_selected,
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
                    bg_normal,
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

            queue!(stdout, SetBackgroundColor(bg_color))?;

            // Leading space and indicator
            write!(stdout, " ")?;
            if is_selected {
                queue!(stdout, SetForegroundColor(Color::Cyan))?;
                write!(stdout, ">")?;
            } else {
                write!(stdout, " ")?;
            }
            write!(stdout, " ")?;

            // Status label with color
            let (status_label, label_color) = match server.state {
                McpServerState::Disabled => ("[Disabled]", Color::DarkGrey),
                McpServerState::Starting => ("[Starting]", Color::Yellow),
                McpServerState::Enabled => ("[Enabled] ", Color::Green),
            };
            queue!(stdout, SetForegroundColor(label_color))?;
            write!(stdout, "{}", status_label)?;
            write!(stdout, " ")?;

            // Server name (padded for alignment)
            queue!(stdout, SetForegroundColor(name_color))?;
            write!(stdout, "{:width$}", server.name, width = max_name_width)?;

            // Tool count for enabled servers
            if server.state == McpServerState::Enabled && server.tool_count > 0 {
                queue!(stdout, SetForegroundColor(status_color))?;
                write!(stdout, " ({} tools)", server.tool_count)?;
            }

            // Fill rest of line
            // " " + ">" or " " + " " = 3, plus "[Disabled]" (10) + " " = 11, plus name + optional tool count
            let tool_info = if server.state == McpServerState::Enabled && server.tool_count > 0 {
                format!(" ({} tools)", server.tool_count)
            } else {
                String::new()
            };
            let content_width = 3 + status_label.width() + 1 + max_name_width + tool_info.width();
            let remaining = term_width.saturating_sub(content_width);
            write!(stdout, "{:width$}", "", width = remaining)?;

            queue!(stdout, ResetColor)?;
        }

        Ok(())
    }
}
#[derive(Debug)]
pub(super) enum HistorySearchAction {
    /// No action, continue showing menu
    None,
    /// Redraw the menu
    Redraw,
    /// Close the menu without selection
    Cancel,
    /// History entry was selected
    Select(String),
}

/// State for the history search menu
pub(super) struct HistorySearchState {
    /// All history entries
    entries: Vec<String>,
    /// Current selection index (in filtered list)
    selected_index: usize,
    /// Search/filter query
    search_query: String,
}

impl HistorySearchState {
    /// Create a new history search state from a FileHistory
    pub(super) fn new(history: &crate::history::FileHistory) -> Self {
        // Get entries in reverse order (most recent first) and deduplicate
        // while preserving order (keeps most recent occurrence of each entry)
        let mut seen = std::collections::HashSet::new();
        let entries: Vec<String> = history
            .entries()
            .iter()
            .rev()
            .filter(|entry| seen.insert(entry.as_str()))
            .cloned()
            .collect();
        Self {
            entries,
            selected_index: 0,
            search_query: String::new(),
        }
    }

    /// Get filtered entries matching the search query
    fn filtered_entries(&self) -> Vec<&String> {
        if self.search_query.is_empty() {
            self.entries.iter().collect()
        } else {
            let query = self.search_query.to_lowercase();
            let mut matches: Vec<(&String, u32)> = self
                .entries
                .iter()
                .filter_map(|entry| {
                    fuzzy_match_score(&query, &entry.to_lowercase()).map(|score| (entry, score))
                })
                .collect();

            matches.sort_by(|(_, score_a), (_, score_b)| score_a.cmp(score_b));
            matches.into_iter().map(|(entry, _)| entry).collect()
        }
    }

    /// Handle a key event, returning the action to take
    pub fn handle_key(&mut self, key: KeyEvent) -> HistorySearchAction {
        let filtered = self.filtered_entries();

        match (key.code, key.modifiers) {
            // Enter - select current
            (KeyCode::Enter, _) => {
                if let Some(&entry) = filtered.get(self.selected_index) {
                    return HistorySearchAction::Select(entry.clone());
                }
                HistorySearchAction::Cancel
            }

            // Escape or Ctrl+R again - cancel
            (KeyCode::Esc, _) | (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                HistorySearchAction::Cancel
            }

            // Up arrow
            (KeyCode::Up, _) => {
                if !filtered.is_empty() {
                    if self.selected_index > 0 {
                        self.selected_index -= 1;
                    } else {
                        self.selected_index = filtered.len().saturating_sub(1);
                    }
                }
                HistorySearchAction::Redraw
            }

            // Down arrow
            (KeyCode::Down, _) => {
                if !filtered.is_empty() {
                    if self.selected_index + 1 < filtered.len() {
                        self.selected_index += 1;
                    } else {
                        self.selected_index = 0;
                    }
                }
                HistorySearchAction::Redraw
            }

            // Backspace
            (KeyCode::Backspace, _) => {
                if !self.search_query.is_empty() {
                    self.search_query.pop();
                    self.selected_index = 0;
                    HistorySearchAction::Redraw
                } else {
                    HistorySearchAction::None
                }
            }

            // Character input (filter)
            (KeyCode::Char(c), modifier) if !modifier.contains(KeyModifiers::CONTROL) => {
                self.search_query.push(c);
                self.selected_index = 0;
                HistorySearchAction::Redraw
            }

            // Ctrl+U - clear filter
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.search_query.clear();
                self.selected_index = 0;
                HistorySearchAction::Redraw
            }

            _ => HistorySearchAction::None,
        }
    }

    /// Calculate the height needed for the menu (in rows)
    pub fn height(&self) -> u16 {
        let filtered_count = self.filtered_entries().len();
        let visible = MENU_MAX_VISIBLE.min(filtered_count);
        // Header (search) + items + optional scroll indicator
        let scroll_indicator = if filtered_count > MENU_MAX_VISIBLE {
            1
        } else {
            0
        };
        (1 + visible + scroll_indicator) as u16
    }

    /// Total display height including all elements
    pub fn display_height(&self) -> u16 {
        self.height()
    }

    /// Render the menu at the specified row.
    pub fn render(&self, stdout: &mut io::Stdout, start_row: u16) -> io::Result<()> {
        let filtered = self.filtered_entries();
        let total = filtered.len();
        let visible_count = MENU_MAX_VISIBLE.min(total);

        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        // Background colors matching model menu style
        let bg_normal = Color::Rgb {
            r: 20,
            g: 20,
            b: 20,
        };
        let bg_selected = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };

        // Draw header/search line with popup background
        queue!(
            stdout,
            cursor::MoveTo(0, start_row),
            terminal::Clear(ClearType::CurrentLine),
            SetBackgroundColor(bg_normal),
            SetForegroundColor(Color::Yellow)
        )?;

        let header_text = if self.search_query.is_empty() {
            " History search (type to filter, ↑↓ navigate, Enter select, Esc cancel):".to_string()
        } else {
            format!(" Filter: {}", self.search_query)
        };
        let header_width = header_text.width();
        write!(stdout, "{}", header_text)?;
        // Fill the rest of the line with background
        let remaining = term_width.saturating_sub(header_width);
        write!(stdout, "{:width$}", "", width = remaining)?;
        queue!(stdout, ResetColor)?;

        if total == 0 {
            queue!(
                stdout,
                cursor::MoveTo(0, start_row + 1),
                terminal::Clear(ClearType::CurrentLine),
                SetBackgroundColor(bg_normal),
                SetForegroundColor(Color::DarkGrey)
            )?;
            let msg = if self.search_query.is_empty() {
                "  No history entries"
            } else {
                "  No matching history entries"
            };
            write!(stdout, "{}", msg)?;
            let remaining = term_width.saturating_sub(msg.width());
            write!(stdout, "{:width$}", "", width = remaining)?;
            queue!(stdout, ResetColor)?;
            return Ok(());
        }

        // Calculate scroll window
        let max_start = total.saturating_sub(visible_count);
        let scroll_start = self
            .selected_index
            .saturating_sub(visible_count.saturating_sub(1))
            .min(max_start);
        let scroll_end = (scroll_start + visible_count).min(total);
        let visible = &filtered[scroll_start..scroll_end];
        let selected_in_view = self.selected_index.saturating_sub(scroll_start);

        // Draw items with popup styling
        for (i, entry) in visible.iter().enumerate() {
            let row = start_row + 1 + i as u16;
            let is_selected = i == selected_in_view;

            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            let bg_color = if is_selected { bg_selected } else { bg_normal };

            queue!(stdout, SetBackgroundColor(bg_color))?;

            // Selection indicator
            if is_selected {
                queue!(
                    stdout,
                    SetForegroundColor(Color::Rgb {
                        r: 137,
                        g: 180,
                        b: 250
                    })
                )?;
                write!(stdout, " > ")?;
            } else {
                queue!(
                    stdout,
                    SetForegroundColor(Color::Rgb {
                        r: 120,
                        g: 120,
                        b: 120
                    })
                )?;
                write!(stdout, "   ")?;
            }

            // Display the entry (truncate and normalize whitespace)
            let display_text: String = entry
                .chars()
                .map(|c| if c.is_whitespace() { ' ' } else { c })
                .take(term_width.saturating_sub(6))
                .collect();
            write!(stdout, "{}", display_text)?;

            // Fill the rest of the line with background
            let used_width = 3 + display_text.width();
            let remaining = term_width.saturating_sub(used_width);
            write!(stdout, "{:width$}", "", width = remaining)?;

            queue!(stdout, ResetColor)?;
        }

        // Scroll indicator
        if total > visible_count {
            let indicator_row = start_row + 1 + visible_count as u16;
            queue!(
                stdout,
                cursor::MoveTo(0, indicator_row),
                terminal::Clear(ClearType::CurrentLine),
                SetBackgroundColor(bg_normal),
                SetForegroundColor(Color::DarkGrey)
            )?;
            let msg = format!(
                "  ({}/{} entries, ↑↓ to scroll)",
                self.selected_index + 1,
                total
            );
            write!(stdout, "{}", msg)?;
            let remaining = term_width.saturating_sub(msg.width());
            write!(stdout, "{:width$}", "", width = remaining)?;
            queue!(stdout, ResetColor)?;
        }

        Ok(())
    }
}

/// Action from handling a key event in the tools menu
#[derive(Debug)]
pub(super) enum ToolsMenuAction {
    /// No action, continue showing menu
    None,
    /// Redraw the menu
    Redraw,
    /// Close the menu
    Close,
}

/// A tool entry for the tools menu
pub(super) struct ToolEntry {
    /// Tool name (e.g., "bash", "file_read")
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Whether the tool is enabled (not in disabled_tools list)
    pub is_enabled: bool,
    /// Whether the tool is locked due to read-only mode (cannot be toggled)
    pub is_read_only_locked: bool,
}

/// State for the tools toggle menu
pub(super) struct ToolsMenuState {
    /// List of tool entries
    tools: Vec<ToolEntry>,
    /// Currently selected tool index
    selected_index: usize,
    /// Whether read-only mode is active
    read_only: bool,
}

impl ToolsMenuState {
    pub fn new(read_only: bool) -> Self {
        let config = crate::config::ConfigFile::load().unwrap_or_default();

        let tools = crate::tools::TOOL_INFO
            .iter()
            .map(|(name, description)| {
                let is_read_only_locked =
                    read_only && crate::tools::READ_ONLY_DISABLED_TOOLS.contains(name);

                let is_enabled = !config.is_tool_disabled(name);

                ToolEntry {
                    name: name.to_string(),
                    description: description.to_string(),
                    is_enabled,
                    is_read_only_locked,
                }
            })
            .collect();

        Self {
            tools,
            selected_index: 0,
            read_only,
        }
    }

    /// Toggle the selected tool's enabled status
    fn toggle_selected(&mut self) {
        let Some(tool) = self.tools.get_mut(self.selected_index) else {
            return;
        };

        // Don't allow toggling read-only locked tools
        if tool.is_read_only_locked {
            return;
        }

        // Toggle in config
        if let Ok(mut config) = crate::config::ConfigFile::load() {
            let is_enabled = config.toggle_tool_disabled(&tool.name);
            let _ = config.save();
            tool.is_enabled = is_enabled;
        }
    }

    /// Handle a key event, returning the action to take
    pub fn handle_key(&mut self, key: KeyEvent) -> ToolsMenuAction {
        let total = self.tools.len();

        match (key.code, key.modifiers) {
            // Escape - close menu
            (KeyCode::Esc, _) => ToolsMenuAction::Close,

            // Enter or Space - toggle selected tool
            (KeyCode::Enter, _) | (KeyCode::Char(' '), _) => {
                if total > 0 {
                    self.toggle_selected();
                }
                ToolsMenuAction::Redraw
            }

            // Up arrow
            (KeyCode::Up, _) => {
                if total > 0 {
                    if self.selected_index > 0 {
                        self.selected_index -= 1;
                    } else {
                        self.selected_index = total.saturating_sub(1);
                    }
                }
                ToolsMenuAction::Redraw
            }

            // Down arrow
            (KeyCode::Down, _) => {
                if total > 0 {
                    if self.selected_index + 1 < total {
                        self.selected_index += 1;
                    } else {
                        self.selected_index = 0;
                    }
                }
                ToolsMenuAction::Redraw
            }

            // Consume all other keys to prevent them from reaching the input
            _ => ToolsMenuAction::None,
        }
    }

    /// Calculate the height needed for the menu (in rows)
    pub fn height(&self) -> u16 {
        // Header + tools + optional read-only banner
        let banner = if self.read_only { 1 } else { 0 };
        (1 + self.tools.len() + banner) as u16
    }

    /// Total display height including all elements
    pub fn display_height(&self) -> u16 {
        self.height()
    }

    /// Render the menu at the specified row.
    pub fn render(&self, stdout: &mut io::Stdout, start_row: u16) -> io::Result<()> {
        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

        let bg_normal = Color::Rgb {
            r: 20,
            g: 20,
            b: 20,
        };
        let bg_selected = Color::Rgb {
            r: 30,
            g: 30,
            b: 30,
        };

        // Header line
        queue!(
            stdout,
            cursor::MoveTo(0, start_row),
            terminal::Clear(ClearType::CurrentLine),
            SetBackgroundColor(bg_normal),
            SetForegroundColor(Color::Yellow)
        )?;

        let header_text = " Tools (↑↓ navigate, Enter/Space toggle, Esc close):";
        let header_width = header_text.width();
        write!(stdout, "{}", header_text)?;
        let remaining = term_width.saturating_sub(header_width);
        write!(stdout, "{:width$}", "", width = remaining)?;
        queue!(stdout, ResetColor)?;

        let mut current_row = start_row + 1;

        // Show read-only banner if active
        if self.read_only {
            queue!(
                stdout,
                cursor::MoveTo(0, current_row),
                terminal::Clear(ClearType::CurrentLine),
                SetBackgroundColor(bg_normal),
                SetForegroundColor(Color::Yellow)
            )?;
            let banner = " ⚠ Read-only mode: some tools are locked";
            write!(stdout, "{}", banner)?;
            let remaining = term_width.saturating_sub(banner.width());
            write!(stdout, "{:width$}", "", width = remaining)?;
            queue!(stdout, ResetColor)?;
            current_row += 1;
        }

        // Calculate max name width for alignment
        let max_name_width = self.tools.iter().map(|t| t.name.len()).max().unwrap_or(0);

        // Render tool options
        for (i, tool) in self.tools.iter().enumerate() {
            let row = current_row + i as u16;
            let is_selected = i == self.selected_index;

            queue!(
                stdout,
                cursor::MoveTo(0, row),
                terminal::Clear(ClearType::CurrentLine)
            )?;

            let bg_color = if is_selected { bg_selected } else { bg_normal };
            queue!(stdout, SetBackgroundColor(bg_color))?;

            // Selection indicator
            write!(stdout, " ")?;
            if is_selected {
                queue!(stdout, SetForegroundColor(Color::Cyan))?;
                write!(stdout, ">")?;
            } else {
                write!(stdout, " ")?;
            }
            write!(stdout, " ")?;

            // Checkbox/lock icon with status color (all 4 display columns for alignment)
            if tool.is_read_only_locked {
                queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
                write!(stdout, "🔒  ")?; // lock (2) + 2 spaces = 4 cols
            } else if tool.is_enabled {
                queue!(stdout, SetForegroundColor(Color::Green))?;
                write!(stdout, "[x] ")?; // 3 + 1 space = 4 cols
            } else {
                queue!(stdout, SetForegroundColor(Color::DarkGrey))?;
                write!(stdout, "[ ] ")?; // 3 + 1 space = 4 cols
            }

            // Tool name (padded for alignment)
            let name_color = if tool.is_read_only_locked {
                Color::DarkGrey
            } else if is_selected {
                Color::Rgb {
                    r: 137,
                    g: 180,
                    b: 250,
                }
            } else {
                Color::Rgb {
                    r: 120,
                    g: 120,
                    b: 120,
                }
            };
            queue!(stdout, SetForegroundColor(name_color))?;
            write!(stdout, "{:width$}", tool.name, width = max_name_width)?;

            // Description
            let desc_color = if tool.is_read_only_locked {
                Color::DarkGrey
            } else {
                Color::Rgb {
                    r: 128,
                    g: 128,
                    b: 128,
                }
            };
            queue!(stdout, SetForegroundColor(desc_color))?;
            write!(stdout, "  {}", tool.description)?;

            // Fill rest of line
            // Prefix: " " + ">" or " " + " " = 3 cols
            // Checkbox: 4 cols (all cases padded to same width)
            // Name: max_name_width cols
            // Description prefix: "  " = 2 cols
            let content_width = 3 + 4 + max_name_width + 2 + tool.description.width();
            let remaining = term_width.saturating_sub(content_width);
            write!(stdout, "{:width$}", "", width = remaining)?;

            queue!(stdout, ResetColor)?;
        }

        Ok(())
    }
}
