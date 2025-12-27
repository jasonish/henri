// SPDX-License-Identifier: MIT
// Main application state for the TUI

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use ratatui::prelude::*;

use crate::config::{Config, ConfigFile, DefaultModel};
use crate::history::FileHistory;
use crate::output::OutputEvent;
use crate::provider::{ContentBlock, Message as ProviderMessage, MessageContent, Role};
use crate::providers::ProviderManager;
use crate::session::{self, RestoredSession};
use crate::tools::todo::clear_todos;
use crate::usage;

use super::clipboard;
use super::commands::filter_commands;
use super::input::{InputEditor, next_char_boundary};
use super::layout::{LayoutCache, message_display_height};
use super::messages::{
    Message, ShellEvent, ShellMessage, TextMessage, ThinkingMessage, ToolCallsMessage,
    UsageDisplay, UserMessage, bulletify, format_error_message, format_shell_display,
};
use super::models::{
    HistorySearchState, ModelChoice, ModelMenuState, build_model_choices, load_default_model,
};
use super::render::ExitPrompt;
use super::selection::{InputSelection, PositionMap, Selection};
use super::settings::{DefaultModelMenuState, SettingOption, SettingsMenuState};
use crate::commands::{Command, DynamicSlashCommand};
use crate::custom_commands::CustomCommand;

pub(crate) struct PendingImage {
    pub(crate) mime_type: String,
    pub(crate) data: Vec<u8>,
}

pub(crate) struct PendingPrompt {
    pub(crate) input: String,
    pub(crate) images: Vec<PendingImage>,
    pub(crate) display_text: String,
}

/// State for an in-progress compaction operation
pub(crate) struct CompactionState {
    /// Messages that will be preserved after compaction
    pub(crate) preserved_messages: Vec<ProviderMessage>,
    /// Number of messages being compacted
    pub(crate) messages_compacted: usize,
    /// The original chat_messages before compaction started (for rollback on error)
    pub(crate) original_messages: Vec<ProviderMessage>,
}

/// Configuration options for the TUI application
pub(crate) struct AppConfig {
    pub(crate) show_network_stats: bool,
    pub(crate) show_diffs: bool,
    pub(crate) lsp_enabled: bool,
}

pub(crate) struct App {
    pub(crate) input: String,
    pub(crate) cursor: usize, // byte index in input
    pub(crate) messages: Vec<Message>,
    pub(crate) scroll_lines: usize, // how many lines above the bottom we are looking at
    pub(crate) absolute_scroll_position: Option<usize>, // locked absolute position when scrolled up, None = auto-scroll mode
    pub(crate) scroll_accel: f64, // current scroll multiplier for acceleration
    pub(crate) last_scroll: Instant, // last scroll event time
    pub(crate) visible_message_count: usize, // messages that fit in the viewport last render
    pub(crate) last_viewport_height: u16,
    pub(crate) last_total_height: usize,
    pub(crate) layout_cache: LayoutCache,
    pub(crate) exit_prompt: Option<ExitPrompt>,
    pub(crate) pending_images: Vec<PendingImage>, // images waiting to be submitted with current input
    pub(crate) pending_prompts: VecDeque<PendingPrompt>, // Queued prompts waiting for current generation to finish
    pub(crate) input_history: FileHistory,
    pub(crate) history_index: Option<usize>, // None = not browsing history, Some(i) = viewing history.len()
    pub(crate) input_stash: String,          // stash current input when browsing history
    pub(crate) slash_menu_index: usize,
    pub(crate) should_exit: bool,
    pub(crate) should_switch_to_cli: bool,
    pub(crate) shell_tx: mpsc::Sender<ShellEvent>,
    pub(crate) shell_rx: mpsc::Receiver<ShellEvent>,
    // Model selection
    pub(crate) current_model: Option<ModelChoice>,
    pub(crate) model_menu: Option<ModelMenuState>,
    pub(crate) settings_menu: Option<SettingsMenuState>,
    // History search
    pub(crate) history_search: Option<HistorySearchState>,
    // Chat state
    pub(crate) provider_manager: Option<ProviderManager>,
    pub(crate) chat_messages: Vec<ProviderMessage>,
    pub(crate) chat_result_rx:
        Option<tokio::sync::oneshot::Receiver<(ProviderManager, Vec<ProviderMessage>)>>,
    pub(crate) chat_interrupted: Arc<AtomicBool>,
    pub(crate) is_chatting: bool,
    pub(crate) is_thinking: bool,
    pub(crate) is_compacting: bool,
    pub(crate) compaction_state: Option<CompactionState>,
    pub(crate) chat_task_spawned: bool,
    // Selection state
    pub(crate) selection: Selection,
    pub(crate) position_map: PositionMap,
    pub(crate) input_selection: InputSelection,
    pub(crate) input_text_area: Rect, // Area where input text is rendered (for mouse hit testing)
    pub(crate) last_click: Option<(Instant, u16, u16, u8)>, // For multi-click detection: (time, x, y, count)
    pub(crate) show_cursor: bool,                           // Whether to show the input cursor
    // Unified event receiver for all output events
    pub(crate) event_rx: tokio::sync::mpsc::UnboundedReceiver<OutputEvent>,
    // Output context for emitting events
    pub(crate) output: crate::output::OutputContext,
    // Spinner animation frame for waiting indicator
    pub(crate) spinner_frame: usize,
    // Streaming progress stats
    pub(crate) streaming_tokens: Option<u64>, // Target token count from API
    pub(crate) streaming_tokens_display: u64, // Animated display value
    pub(crate) streaming_duration: Option<f64>,
    pub(crate) accumulated_tokens: u64,
    pub(crate) accumulated_duration: f64,
    // Track when current streaming started for live time updates
    pub(crate) streaming_start_time: Option<Instant>,
    // Context usage info for display
    pub(crate) last_context_tokens: Option<u64>,
    pub(crate) context_limit: Option<u64>,
    // Working directory for session persistence
    pub(crate) working_dir: PathBuf,
    // New content indicator
    pub(crate) has_new_content_below: bool,
    // Thinking mode toggle state
    pub(crate) thinking_enabled: bool,
    pub(crate) thinking_available: bool,
    pub(crate) thinking_mode: super::thinking_mode::ThinkingMode,
    pub(crate) is_cleared: bool,
    pub(crate) input_scroll: u16, // lines scrolled within the input box (0 = top visible)
    // Pending content to show in external pager (bat)
    pub(crate) pending_pager_content: Option<String>,
    // Custom commands loaded from .claude/commands/*.md
    pub(crate) custom_commands: Vec<CustomCommand>,
    // Show network statistics in status line
    pub(crate) show_network_stats: bool,
    // Show diffs after file modifications
    pub(crate) show_diffs: bool,
    // LSP integration enabled (config setting, not runtime)
    pub(crate) lsp_enabled: bool,
    // Number of connected LSP servers
    pub(crate) lsp_server_count: usize,
    // Default model setting (last-used or specific model)
    pub(crate) default_model: DefaultModel,
}

impl App {
    pub(crate) fn new(
        provider_manager: Option<ProviderManager>,
        working_dir: PathBuf,
        restored_session: Option<RestoredSession>,
        event_rx: tokio::sync::mpsc::UnboundedReceiver<OutputEvent>,
        config: AppConfig,
        output: crate::output::OutputContext,
    ) -> Self {
        let (shell_tx, shell_rx) = mpsc::channel();

        // Get model from provider_manager (command line), falling back to default model setting
        let current_model = provider_manager
            .as_ref()
            .and_then(|pm| super::models::parse_model_string(&pm.current_model_string()))
            .or_else(load_default_model);

        // Determine if thinking is available for the current model
        let thinking_available = current_model
            .as_ref()
            .map(|m| super::supports_thinking(m.provider, &m.model_id))
            .unwrap_or(false);

        // Initialize thinking mode based on the current model
        let thinking_mode = current_model
            .as_ref()
            .map(|m| super::thinking_mode::ThinkingMode::default_for_model(&m.model_id))
            .unwrap_or(super::thinking_mode::ThinkingMode::Off);

        // Load config file to check for configured providers and default model
        let config_file = ConfigFile::load().unwrap_or_default();
        let default_model = config_file.default_model.clone();

        // Check if user has any configured providers (excluding Zen which is free/default)
        let has_configured_providers = config_file
            .providers
            .entries
            .iter()
            .any(|(_, p)| !matches!(p.provider_type(), crate::config::ProviderType::Zen));

        // Build display messages
        let mut messages = Vec::new();

        if has_configured_providers {
            messages.push(Message::Text(
                "Welcome to Henri, your Golden Retriever coding assistant! 🐕".into(),
            ));
        } else {
            messages.push(Message::Text(
                "Welcome to Henri! 🐕\n\nYou are currently using the free 'zen/grok-code' model. It's great for getting started!\nFor more powerful models (Claude, GPT-4), try connecting your accounts:\n\n  henri provider add      # Authenticate with GitHub Copilot, etc.\n\nType /help for more commands.".into(),
            ));
        }

        // Initialize chat_messages and thinking_enabled from restored session
        let (chat_messages, thinking_enabled) = if let Some(ref session) = restored_session {
            messages.push(Message::Text(format!(
                "Model: {} (thinking {}) · Saved: {} · Messages: {}",
                session.model_id,
                if session.thinking_enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                session::format_age(&session.state.meta.saved_at),
                session.messages.len()
            )));

            // Convert session to rich replay messages
            let replay_messages = session::extract_replay_messages(&session.state);

            for replay_msg in replay_messages {
                match replay_msg.role {
                    Role::User => {
                        let (text, has_images) = extract_user_display(&replay_msg.segments);
                        let display_text = if has_images {
                            format!("{}\n[images attached]", text)
                        } else {
                            text
                        };
                        messages.push(Message::User(UserMessage { display_text }));
                    }
                    Role::Assistant => {
                        build_assistant_messages(&replay_msg.segments, &mut messages);
                    }
                    Role::System => {}
                }
            }

            (session.messages.clone(), session.thinking_enabled)
        } else {
            // Clear todo state for fresh sessions
            clear_todos();
            (Vec::new(), true)
        };

        Self {
            input: String::new(),
            cursor: 0,
            messages,
            scroll_lines: 0,
            absolute_scroll_position: None,
            scroll_accel: 2.0,
            last_scroll: Instant::now(),
            visible_message_count: 0,
            last_viewport_height: 0,
            last_total_height: 0,
            layout_cache: LayoutCache::new(),
            exit_prompt: None,
            pending_images: Vec::new(),
            pending_prompts: VecDeque::new(),
            input_history: FileHistory::new(),
            history_index: None,
            input_stash: String::new(),
            slash_menu_index: 0,
            should_exit: false,
            should_switch_to_cli: false,
            shell_tx,
            shell_rx,
            current_model,
            model_menu: None,
            settings_menu: None,
            history_search: None,
            provider_manager,
            chat_messages,
            chat_result_rx: None,
            chat_interrupted: Arc::new(AtomicBool::new(false)),
            is_chatting: false,
            is_thinking: false,
            is_compacting: false,
            compaction_state: None,
            chat_task_spawned: false,
            selection: Selection::default(),
            position_map: PositionMap::default(),
            input_selection: InputSelection::default(),
            input_text_area: Rect::default(),
            last_click: None,
            show_cursor: true,
            event_rx,
            output,
            spinner_frame: 0,
            streaming_tokens: None,
            streaming_tokens_display: 0,
            streaming_duration: None,
            accumulated_tokens: 0,
            accumulated_duration: 0.0,
            streaming_start_time: None,
            last_context_tokens: None,
            context_limit: None,
            working_dir,
            has_new_content_below: false,
            thinking_enabled,
            thinking_available,
            thinking_mode,
            is_cleared: false,
            input_scroll: 0,
            pending_pager_content: None,
            custom_commands: crate::custom_commands::load_custom_commands()
                .unwrap_or_else(|_| Vec::new()),
            show_network_stats: config.show_network_stats,
            show_diffs: config.show_diffs,
            lsp_enabled: config.lsp_enabled,
            lsp_server_count: 0,
            default_model,
        }
    }

    pub(crate) fn open_model_menu(&mut self) {
        let mut choices = build_model_choices();
        // Sort: favorites first, then alphabetically by provider/model
        choices.sort_by(|a, b| match (a.is_favorite, b.is_favorite) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.short_display().cmp(&b.short_display()),
        });
        let mut menu = ModelMenuState {
            choices,
            selected_index: 0,
            search_query: String::new(),
        };
        // Find the current model in the filtered (sorted) list
        if let Some(ref current) = self.current_model {
            let filtered = menu.filtered_choices();
            if let Some(idx) = filtered
                .iter()
                .position(|c| c.provider == current.provider && c.model_id == current.model_id)
            {
                menu.selected_index = idx;
            }
        }
        self.model_menu = Some(menu);
    }

    pub(crate) fn close_model_menu(&mut self) {
        self.model_menu = None;
    }

    pub(crate) fn select_model(&mut self) {
        if let Some(menu) = &self.model_menu {
            let filtered = menu.filtered_choices();
            if let Some(&choice) = filtered.get(menu.selected_index) {
                let model_str = choice.short_display();
                self.current_model = Some(choice.clone());

                // Update the provider manager to use the new model
                if let Some(ref mut pm) = self.provider_manager {
                    pm.set_model(
                        choice.provider,
                        choice.model_id.to_string(),
                        choice.custom_provider.clone(),
                    );
                }

                // Update thinking_available based on the new model
                self.thinking_available =
                    super::supports_thinking(choice.provider, &choice.model_id);

                // Update thinking_mode based on the new model
                self.thinking_mode =
                    super::thinking_mode::ThinkingMode::default_for_model(&choice.model_id);

                self.messages
                    .push(Message::Text(format!("Model set to: {}", model_str)));
                self.layout_cache.invalidate();
                self.reset_scroll();
                // Save selection to config
                let _ = Config::save_state_model(&model_str);
            }
        }
        self.close_model_menu();
    }

    pub(crate) fn model_menu_active(&self) -> bool {
        self.model_menu.is_some()
    }

    pub(crate) fn open_history_search(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        self.input_stash = self.input.clone();
        self.history_search = Some(HistorySearchState::new(self.input_history.entries()));
    }

    pub(crate) fn close_history_search(&mut self, apply: bool) {
        if apply
            && let Some(ref search) = self.history_search
            && let Some(entry) = search.selected_entry(self.input_history.entries())
        {
            self.input = entry.clone();
            self.cursor = self.input.len();
        } else {
            self.input = self.input_stash.clone();
            self.cursor = self.input.len();
        }
        self.input_stash.clear();
        self.history_search = None;
        self.history_index = None;
    }

    pub(crate) fn history_search_active(&self) -> bool {
        self.history_search.is_some()
    }

    pub(crate) fn handle_history_search_key(
        &mut self,
        code: crossterm::event::KeyCode,
        _modifiers: crossterm::event::KeyModifiers,
    ) -> bool {
        use crossterm::event::KeyCode;

        let Some(search) = &mut self.history_search else {
            return false;
        };

        match code {
            KeyCode::Esc => {
                self.close_history_search(false);
                true
            }
            KeyCode::Enter => {
                self.close_history_search(true);
                true
            }
            KeyCode::Up => {
                search.move_up();
                true
            }
            KeyCode::Down => {
                search.move_down();
                true
            }
            KeyCode::Backspace => {
                search.search_query.pop();
                search.update_filter(self.input_history.entries());
                true
            }
            KeyCode::Char(c) => {
                search.search_query.push(c);
                search.update_filter(self.input_history.entries());
                true
            }
            _ => false,
        }
    }

    pub(crate) fn handle_model_menu_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> bool {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyModifiers;

        let Some(menu) = &mut self.model_menu else {
            return false;
        };

        // Helper to get filtered count
        let filtered_len = || menu.filtered_choices().len();

        // Handle Ctrl+F to toggle favorite
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('f') {
            self.toggle_selected_model_favorite();
            return true;
        }

        match code {
            KeyCode::Esc => {
                self.close_model_menu();
                true
            }
            KeyCode::Enter => {
                self.select_model();
                true
            }
            KeyCode::Up => {
                let len = filtered_len();
                if len > 0 {
                    if menu.selected_index > 0 {
                        menu.selected_index -= 1;
                    } else {
                        menu.selected_index = len.saturating_sub(1);
                    }
                }
                true
            }
            KeyCode::Down => {
                let len = filtered_len();
                if len > 0 {
                    if menu.selected_index + 1 < len {
                        menu.selected_index += 1;
                    } else {
                        menu.selected_index = 0;
                    }
                }
                true
            }
            KeyCode::PageUp => {
                menu.selected_index = menu
                    .selected_index
                    .saturating_sub(super::models::MODEL_MENU_MAX_VISIBLE);
                true
            }
            KeyCode::PageDown => {
                let len = filtered_len();
                menu.selected_index = (menu.selected_index + super::models::MODEL_MENU_MAX_VISIBLE)
                    .min(len.saturating_sub(1));
                true
            }
            KeyCode::Home => {
                menu.selected_index = 0;
                true
            }
            KeyCode::End => {
                menu.selected_index = filtered_len().saturating_sub(1);
                true
            }
            KeyCode::Backspace => {
                menu.search_query.pop();
                // Reset selection to first match when search changes
                menu.selected_index = 0;
                true
            }
            KeyCode::Char(c) => {
                menu.search_query.push(c);
                // Reset selection to first match when search changes
                menu.selected_index = 0;
                true
            }
            _ => false,
        }
    }

    /// Toggle the favorite status of the currently selected model in the menu
    fn toggle_selected_model_favorite(&mut self) {
        let Some(menu) = &mut self.model_menu else {
            return;
        };

        // Get the model info from filtered choices
        let filtered = menu.filtered_choices();
        let Some(choice) = filtered.get(menu.selected_index) else {
            return;
        };
        let model_id = choice.short_display();
        let provider = choice.provider;
        let choice_model_id = choice.model_id.clone();

        // Toggle favorite in config and save
        let new_favorite_status = if let Ok(mut config) = ConfigFile::load() {
            let is_now_favorite = config.toggle_favorite(&model_id);
            let _ = config.save();
            is_now_favorite
        } else {
            return;
        };

        // Update the is_favorite flag in-place (don't reorder - that happens on next menu open)
        if let Some(choice) = menu
            .choices
            .iter_mut()
            .find(|c| c.provider == provider && c.model_id == choice_model_id)
        {
            choice.is_favorite = new_favorite_status;
        }
    }

    /// Cycle through favorite models (triggered by Shift+Tab)
    pub(crate) fn cycle_favorite_model(&mut self) {
        let choices = build_model_choices();
        let favorites: Vec<_> = choices.iter().filter(|c| c.is_favorite).collect();

        if favorites.is_empty() {
            return;
        }

        // Find current model's position in favorites
        let current_idx = if let Some(ref current) = self.current_model {
            favorites
                .iter()
                .position(|c| c.provider == current.provider && c.model_id == current.model_id)
        } else {
            None
        };

        // Cycle to next favorite (or first if not found)
        let next_idx = match current_idx {
            Some(idx) => (idx + 1) % favorites.len(),
            None => 0,
        };

        let next_model = favorites[next_idx];
        let model_str = next_model.short_display();

        // Update current model
        self.current_model = Some(next_model.clone());

        // Update provider manager
        if let Some(ref mut pm) = self.provider_manager {
            pm.set_model(
                next_model.provider,
                next_model.model_id.clone(),
                next_model.custom_provider.clone(),
            );
        }

        // Update thinking availability and mode
        self.thinking_available =
            super::supports_thinking(next_model.provider, &next_model.model_id);
        self.thinking_mode =
            super::thinking_mode::ThinkingMode::default_for_model(&next_model.model_id);

        // Show model change message
        self.messages
            .push(Message::Text(format!("Model set to: {}", model_str)));
        self.layout_cache.invalidate();
        self.reset_scroll();

        // Save to config
        let _ = crate::config::Config::save_state_model(&model_str);
    }

    pub(crate) fn open_settings_menu(&mut self) {
        // Load default UI from config
        let default_ui = ConfigFile::load().map(|c| c.ui.default).unwrap_or_default();
        self.settings_menu = Some(SettingsMenuState::new(
            self.show_network_stats,
            self.show_diffs,
            self.lsp_enabled,
            self.default_model.clone(),
            default_ui,
        ));
    }

    pub(crate) fn close_settings_menu(&mut self) {
        self.settings_menu = None;
    }

    pub(crate) fn toggle_setting(&mut self) {
        if let Some(menu) = &mut self.settings_menu
            && let Some(option) = menu.options.get_mut(menu.selected_index)
        {
            match option {
                SettingOption::ShowNetworkStats(enabled) => {
                    *enabled = !*enabled;
                    self.show_network_stats = *enabled;
                    // Save to config
                    if let Ok(mut config) = ConfigFile::load() {
                        config.show_network_stats = *enabled;
                        let _ = config.save();
                    }
                    self.layout_cache.invalidate();
                    self.reset_scroll();
                }
                SettingOption::ShowDiffs(enabled) => {
                    *enabled = !*enabled;
                    self.show_diffs = *enabled;
                    // Save to config
                    if let Ok(mut config) = ConfigFile::load() {
                        config.show_diffs = *enabled;
                        let _ = config.save();
                    }
                    self.layout_cache.invalidate();
                    self.reset_scroll();
                }
                SettingOption::LspEnabled(enabled) => {
                    *enabled = !*enabled;
                    self.lsp_enabled = *enabled;
                    // Save to config
                    if let Ok(mut config) = ConfigFile::load() {
                        config.lsp_enabled = *enabled;
                        let _ = config.save();
                    }

                    // Reload LSP servers in real-time
                    let working_dir = self.working_dir.clone();
                    tokio::spawn(async move {
                        let _ = crate::lsp::reload_from_config(&working_dir).await;
                    });
                    // Note: lsp_server_count will be updated automatically on next tick
                }
                SettingOption::DefaultModel(dm) => {
                    // Open the submenu for selecting default model
                    let model_choices = build_model_choices();
                    menu.default_model_submenu =
                        Some(DefaultModelMenuState::new(model_choices, dm));
                }
                SettingOption::DefaultUi(ui) => {
                    // Toggle between tui and cli
                    *ui = match ui {
                        crate::config::UiDefault::Tui => crate::config::UiDefault::Cli,
                        crate::config::UiDefault::Cli => crate::config::UiDefault::Tui,
                    };
                    // Save to config
                    if let Ok(mut config) = ConfigFile::load() {
                        config.ui.default = *ui;
                        let _ = config.save();
                    }
                }
            }
        }
    }

    /// Select a model from the default model submenu
    fn select_default_model(&mut self) {
        if let Some(menu) = &mut self.settings_menu
            && let Some(submenu) = &menu.default_model_submenu
        {
            let filtered = submenu.filtered_choices();
            if let Some((original_idx, _)) = filtered.get(submenu.selected_index)
                && let Some(choice) = submenu.choices.get(*original_idx)
            {
                let new_default = choice.to_default_model();

                // Update the setting option
                if let Some(SettingOption::DefaultModel(dm)) =
                    menu.options.get_mut(menu.selected_index)
                {
                    *dm = new_default.clone();
                }

                // Update app state
                self.default_model = new_default.clone();

                // Save to config
                if let Ok(mut config) = ConfigFile::load() {
                    config.default_model = new_default;
                    let _ = config.save();
                }
            }
            // Close the submenu
            menu.default_model_submenu = None;
        }
    }

    pub(crate) fn settings_menu_active(&self) -> bool {
        self.settings_menu.is_some()
    }

    pub(crate) fn handle_settings_menu_key(
        &mut self,
        code: crossterm::event::KeyCode,
        _modifiers: crossterm::event::KeyModifiers,
    ) -> bool {
        use crossterm::event::KeyCode;

        let Some(menu) = &mut self.settings_menu else {
            return false;
        };

        // Handle submenu if open
        if menu.default_model_submenu.is_some() {
            // Check if this is an Enter key to select
            if code == KeyCode::Enter {
                self.select_default_model();
                return true;
            }

            // Re-borrow for other operations
            let submenu = menu.default_model_submenu.as_mut().unwrap();
            match code {
                KeyCode::Esc => {
                    // Close submenu, stay in settings
                    menu.default_model_submenu = None;
                    return true;
                }
                KeyCode::Up => {
                    let filtered_len = submenu.filtered_choices().len();
                    if filtered_len > 0 {
                        if submenu.selected_index > 0 {
                            submenu.selected_index -= 1;
                        } else {
                            submenu.selected_index = filtered_len - 1;
                        }
                    }
                    return true;
                }
                KeyCode::Down => {
                    let filtered_len = submenu.filtered_choices().len();
                    if filtered_len > 0 {
                        if submenu.selected_index + 1 < filtered_len {
                            submenu.selected_index += 1;
                        } else {
                            submenu.selected_index = 0;
                        }
                    }
                    return true;
                }
                KeyCode::Char(c) => {
                    submenu.search_query.push(c);
                    submenu.selected_index = 0;
                    return true;
                }
                KeyCode::Backspace => {
                    submenu.search_query.pop();
                    submenu.selected_index = 0;
                    return true;
                }
                _ => return false,
            }
        }

        // Main settings menu handling
        match code {
            KeyCode::Esc => {
                self.close_settings_menu();
                true
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.toggle_setting();
                true
            }
            KeyCode::Up => {
                if menu.selected_index > 0 {
                    menu.selected_index -= 1;
                } else {
                    menu.selected_index = menu.options.len().saturating_sub(1);
                }
                true
            }
            KeyCode::Down => {
                if menu.selected_index + 1 < menu.options.len() {
                    menu.selected_index += 1;
                } else {
                    menu.selected_index = 0;
                }
                true
            }
            _ => false,
        }
    }

    pub(crate) fn submit_message(&mut self) {
        self.is_cleared = false;

        let trimmed = self.input.trim().to_string();
        if trimmed.is_empty() {
            return;
        }

        // Handle slash commands
        if let Some(cmd_str) = trimmed.strip_prefix('/')
            && let Some(cmd) = crate::commands::parse(cmd_str, &self.custom_commands)
        {
            // Save custom commands to history (but not built-in commands)
            if matches!(cmd, Command::Custom { .. }) {
                self.add_to_history(trimmed.clone());
            }
            self.handle_slash_command(cmd);
            return;
        }

        if let Some(command) = trimmed.strip_prefix('!') {
            let command = command.trim_start();
            if command.is_empty() {
                // Return to normal prompt mode without running a command.
                self.input.clear();
                self.cursor = 0;
                self.reset_scroll();
                return;
            }

            let command = command.to_string();
            self.add_to_history(trimmed);
            self.run_shell_command(&command);
            self.input.clear();
            self.cursor = 0;
            self.reset_scroll();
            return;
        }

        // Take pending images (already shown as [Image #N] markers in display_text)
        let pending_images = std::mem::take(&mut self.pending_images);

        let display_text = bulletify(&trimmed);
        self.add_to_history(trimmed.clone());
        self.input.clear();
        self.cursor = 0;
        self.reset_scroll();

        // Start chat interaction if provider is available
        self.start_chat(trimmed, pending_images, display_text);
    }

    fn handle_slash_command(&mut self, cmd: Command) {
        match cmd {
            Command::Quit => {
                self.should_exit = true;
                self.input.clear();
                self.cursor = 0;
            }
            Command::Cli => {
                self.should_exit = true;
                self.should_switch_to_cli = true;
                self.input.clear();
                self.cursor = 0;
            }
            // TUI command is already handled via Availability::CliOnly, but if reached:
            Command::Tui => {
                self.messages
                    .push(Message::Error("Already in TUI mode.".into()));
                self.layout_cache.invalidate();
                self.input.clear();
                self.cursor = 0;
                self.reset_scroll();
            }
            Command::Clear => {
                // Interrupt any ongoing chat before clearing
                self.chat_interrupted.store(true, Ordering::SeqCst);
                self.clear_messages();
                self.input.clear();
                self.cursor = 0;
                self.is_cleared = true;
                // Clear network stats
                usage::network_stats().clear();
            }
            Command::Model => {
                self.open_model_menu();
                self.input.clear();
                self.cursor = 0;
            }
            Command::Settings => {
                self.open_settings_menu();
                self.input.clear();
                self.cursor = 0;
            }
            Command::StartTransactionLogging => {
                let path = crate::provider::transaction_log::start(None);
                self.messages.push(Message::Text(format!(
                    "Transaction logging started to: {}",
                    path.display()
                )));
                self.layout_cache.invalidate();
                self.input.clear();
                self.cursor = 0;
                self.reset_scroll();
            }
            Command::StopTransactionLogging => {
                crate::provider::transaction_log::stop();
                self.messages
                    .push(Message::Text("Transaction logging stopped.".into()));
                self.layout_cache.invalidate();
                self.input.clear();
                self.cursor = 0;
                self.reset_scroll();
            }
            Command::Usage => {
                let has_claude_oauth = crate::commands::has_claude_oauth_provider();

                if has_claude_oauth {
                    self.messages
                        .push(Message::Text("Fetching rate limits...".into()));
                    self.layout_cache.invalidate();
                    self.input.clear();
                    self.cursor = 0;
                    self.reset_scroll();

                    let shell_tx = self.shell_tx.clone();
                    tokio::spawn(async move {
                        match crate::usage::fetch_anthropic_rate_limits().await {
                            Ok(limits) => {
                                let _ = shell_tx.send(ShellEvent::UsageData(limits));
                            }
                            Err(e) => {
                                let _ =
                                    shell_tx.send(ShellEvent::UsageError(format!("Failed: {}", e)));
                            }
                        }
                    });
                } else {
                    self.messages.push(Message::Error(
                        "/claude-usage requires a Claude provider with OAuth authentication configured."
                            .into(),
                    ));
                    self.layout_cache.invalidate();
                    self.input.clear();
                    self.cursor = 0;
                    self.reset_scroll();
                }
            }
            Command::DumpPrompt => {
                if let Some(mut provider_manager) = self.provider_manager.take() {
                    self.messages
                        .push(Message::Text("Preparing request...".into()));
                    self.layout_cache.invalidate();
                    self.input.clear();
                    self.cursor = 0;
                    self.reset_scroll();

                    let shell_tx = self.shell_tx.clone();
                    let chat_messages = self.chat_messages.clone();
                    tokio::spawn(async move {
                        match provider_manager.prepare_request(chat_messages).await {
                            Ok(json) => {
                                let pretty = serde_json::to_string_pretty(&json)
                                    .unwrap_or_else(|_| json.to_string());
                                let _ = shell_tx
                                    .send(ShellEvent::ContextReady(pretty, provider_manager));
                            }
                            Err(e) => {
                                let _ = shell_tx.send(ShellEvent::ContextError(
                                    e.to_string(),
                                    provider_manager,
                                ));
                            }
                        }
                    });
                } else {
                    self.messages.push(Message::Error(
                        "No provider configured or provider busy.".into(),
                    ));
                    self.layout_cache.invalidate();
                    self.input.clear();
                    self.cursor = 0;
                    self.reset_scroll();
                }
            }
            Command::ClaudeCountTokens => {
                let is_claude = self
                    .current_model
                    .as_ref()
                    .is_some_and(|m| matches!(m.provider, crate::providers::ModelProvider::Claude));

                if is_claude {
                    if let Some(mut provider_manager) = self.provider_manager.take() {
                        self.messages
                            .push(Message::Text("Counting tokens...".into()));
                        self.layout_cache.invalidate();
                        self.input.clear();
                        self.cursor = 0;
                        self.reset_scroll();

                        let shell_tx = self.shell_tx.clone();
                        let chat_messages = self.chat_messages.clone();
                        tokio::spawn(async move {
                            match provider_manager.count_tokens(&chat_messages).await {
                                Ok(json) => {
                                    let pretty = serde_json::to_string_pretty(&json)
                                        .unwrap_or_else(|_| json.to_string());
                                    let _ = shell_tx
                                        .send(ShellEvent::TokenCount(pretty, provider_manager));
                                }
                                Err(e) => {
                                    let _ = shell_tx.send(ShellEvent::TokenCountError(
                                        e.to_string(),
                                        provider_manager,
                                    ));
                                }
                            }
                        });
                    } else {
                        self.messages.push(Message::Error(
                            "No provider configured or provider busy.".into(),
                        ));
                        self.layout_cache.invalidate();
                        self.input.clear();
                        self.cursor = 0;
                        self.reset_scroll();
                    }
                } else {
                    self.messages.push(Message::Error(
                        "/claude-count-tokens is only available when using Claude (Anthropic) provider."
                            .into(),
                    ));
                    self.layout_cache.invalidate();
                    self.input.clear();
                    self.cursor = 0;
                    self.reset_scroll();
                }
            }
            Command::DumpConversation => {
                self.dump_context_as_json();
                self.input.clear();
                self.cursor = 0;
            }
            Command::Compact => {
                self.start_compaction();
                self.input.clear();
                self.cursor = 0;
            }
            // CLI-only commands - should not be reachable in TUI mode
            Command::Help | Command::Status => {
                self.messages.push(Message::Error(
                    "This command is only available in CLI mode.".into(),
                ));
                self.layout_cache.invalidate();
                self.input.clear();
                self.cursor = 0;
                self.reset_scroll();
            }
            Command::Custom { name, args } => {
                if let Some(custom) = self.custom_commands.iter().find(|c| c.name == name) {
                    // Clone the prompt and apply variable substitution
                    let prompt =
                        crate::custom_commands::substitute_variables(&custom.prompt, &args);
                    // Display custom command prompt as user input
                    let display_text = super::messages::bulletify(&prompt);
                    // Treat custom command as user input (no images for custom commands)
                    self.start_chat(prompt, Vec::new(), display_text);
                } else {
                    self.messages.push(Message::Error(format!(
                        "Custom command '{}' not found",
                        name
                    )));
                    self.layout_cache.invalidate();
                    self.reset_scroll();
                }
                self.input.clear();
                self.cursor = 0;
            }
            Command::BuildAgentsMd => {
                let prompt = crate::prompts::BUILD_AGENTS_MD_PROMPT.to_string();
                // Display the build agents prompt as user input
                let display_text = super::messages::bulletify(&prompt);

                // Treat as user input and start chat
                self.start_chat(prompt, Vec::new(), display_text);
                self.input.clear();
                self.cursor = 0;
            }
        }
    }

    /// Start a chat interaction with the provider
    pub(crate) fn start_chat(
        &mut self,
        user_input: String,
        images: Vec<PendingImage>,
        display_text: String,
    ) {
        // If there's a pending chat result, try to receive it first to restore provider_manager
        // This handles the race condition where user submits a new message right after
        // ChatEvent::Done but before the result channel is polled
        if let Some(ref mut result_rx) = self.chat_result_rx {
            match result_rx.try_recv() {
                Ok((provider_manager, messages)) => {
                    self.provider_manager = Some(provider_manager);
                    self.chat_messages = messages;
                    self.chat_result_rx = None;
                    self.chat_task_spawned = false;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    // Task panicked - clean up state
                    self.chat_result_rx = None;
                    self.chat_task_spawned = false;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Result not ready yet - previous chat still in progress
                }
            }
        }

        // Check if we are busy. If so, queue the prompt.
        if self.is_chatting || self.chat_result_rx.is_some() {
            self.pending_prompts.push_back(PendingPrompt {
                input: user_input,
                images,
                display_text,
            });
            return;
        }

        if self.provider_manager.is_none() {
            self.messages.push(Message::Text(
                "No provider configured. Use /model to select one.".into(),
            ));
            self.layout_cache.invalidate();
            return;
        }

        // Add the user message to the display only now that we are actually starting
        self.messages
            .push(Message::User(UserMessage { display_text }));
        self.layout_cache.invalidate();
        self.reset_scroll();

        // Build user message with text and images
        let message = if images.is_empty() {
            ProviderMessage::user(&user_input)
        } else {
            let mut blocks = Vec::new();

            // Add text block if not empty
            if !user_input.trim().is_empty() {
                blocks.push(ContentBlock::Text { text: user_input });
            }

            // Add image blocks
            for image in images {
                blocks.push(ContentBlock::Image {
                    mime_type: image.mime_type,
                    data: image.data,
                });
            }

            ProviderMessage {
                role: Role::User,
                content: MessageContent::Blocks(blocks),
            }
        };

        self.chat_messages.push(message);

        // Reset streaming stats
        self.streaming_tokens = None;
        self.streaming_tokens_display = 0;
        self.streaming_duration = None;
        self.accumulated_tokens = 0;
        self.accumulated_duration = 0.0;
        self.streaming_start_time = Some(Instant::now());

        // Reset interrupted flag and mark chat as starting
        self.chat_interrupted.store(false, Ordering::SeqCst);
        self.is_chatting = true;
        self.chat_task_spawned = false;
        self.layout_cache.invalidate();
    }

    /// Start a compaction operation
    pub(crate) fn start_compaction(&mut self) {
        use crate::compaction;

        if self.chat_messages.is_empty() {
            self.messages
                .push(Message::Text("No messages to compact.".into()));
            self.layout_cache.invalidate();
            self.reset_scroll();
            return;
        }

        if self.provider_manager.is_none() {
            self.messages.push(Message::Error(
                "No provider configured. Use /model to select one.".into(),
            ));
            self.layout_cache.invalidate();
            return;
        }

        // Segment messages - compact all (preserve_recent_turns = 0)
        let (to_compact, to_preserve) = compaction::segment_messages(&self.chat_messages, 0);

        if to_compact.is_empty() {
            self.messages
                .push(Message::Text("No messages to compact.".into()));
            self.layout_cache.invalidate();
            self.reset_scroll();
            return;
        }

        // Build the summarization request text
        let request_text = compaction::build_summarization_request_text(&to_compact);
        let messages_compacted = to_compact.len();

        // Display the request as a user message (full text)
        self.messages.push(Message::User(UserMessage {
            display_text: request_text.clone(),
        }));

        // Store compaction state for when the chat completes
        self.compaction_state = Some(CompactionState {
            preserved_messages: to_preserve,
            messages_compacted,
            original_messages: self.chat_messages.clone(),
        });

        // Set up chat messages for summarization:
        // System prompt + user request
        self.chat_messages = vec![
            ProviderMessage::system(compaction::summarization_system_prompt()),
            ProviderMessage::user(&request_text),
        ];

        // Start the chat using the normal flow
        self.streaming_tokens = None;
        self.streaming_tokens_display = 0;
        self.streaming_duration = None;
        self.accumulated_tokens = 0;
        self.accumulated_duration = 0.0;
        self.streaming_start_time = Some(Instant::now());
        self.chat_interrupted.store(false, Ordering::SeqCst);
        self.is_chatting = true;
        self.is_compacting = true;
        self.chat_task_spawned = false;
        self.layout_cache.invalidate();
        self.reset_scroll();
    }

    /// Finalize compaction after the summarization chat completes successfully
    pub(super) fn finalize_compaction(&mut self) {
        let Some(state) = self.compaction_state.take() else {
            return;
        };

        // Extract summary from the last assistant message in chat_messages
        // The chat_messages will contain: [system, user_request, assistant_response]
        let summary = self
            .chat_messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .map(|m| match &m.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| {
                        if let ContentBlock::Text { text } = b {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            })
            .unwrap_or_default();

        // Build the new chat_messages:
        // 1. Summary block as a user message
        // 2. Preserved messages
        let summary_message = ProviderMessage {
            role: Role::User,
            content: MessageContent::Blocks(vec![ContentBlock::Summary {
                summary,
                messages_compacted: state.messages_compacted,
            }]),
        };

        let mut new_messages = vec![summary_message];
        new_messages.extend(state.preserved_messages);
        self.chat_messages = new_messages;

        // Save session with compacted messages
        if let Some(ref model) = self.current_model {
            let _ = session::save_session(
                &self.working_dir,
                &self.chat_messages,
                &model.provider,
                &model.model_id,
                self.thinking_enabled,
            );
        }
    }

    /// Rollback compaction on error or interrupt
    pub(super) fn rollback_compaction(&mut self) {
        if let Some(state) = self.compaction_state.take() {
            self.chat_messages = state.original_messages;
        }
    }

    pub(crate) fn add_to_history(&mut self, input: String) {
        // FileHistory handles deduplication and persistence
        self.input_history.add_with_images(&input, Vec::new());
        self.history_index = None;
        self.input_stash.clear();
    }

    pub(crate) fn history_up(&mut self) {
        if self.input_history.is_empty() {
            return;
        }

        match self.history_index {
            None => {
                // Starting to browse history, stash current input
                self.input_stash = self.input.clone();
                self.history_index = Some(self.input_history.len() - 1);
            }
            Some(0) => {
                // Already at oldest entry
                return;
            }
            Some(i) => {
                self.history_index = Some(i - 1);
            }
        }

        if let Some(i) = self.history_index
            && let Some(entry) = self.input_history.get(i)
        {
            self.input = entry.clone();
            self.cursor = 0;

            // Restore images for this history entry
            self.restore_images_for_history_entry(i);
        }
    }

    pub(crate) fn history_down(&mut self) {
        match self.history_index {
            None => {
                // Not browsing history
            }
            Some(i) if i + 1 >= self.input_history.len() => {
                // Back to current input
                self.input = self.input_stash.clone();
                self.cursor = 0;
                self.history_index = None;
                self.input_stash.clear();
                self.pending_images.clear();
            }
            Some(i) => {
                self.history_index = Some(i + 1);
                if let Some(entry) = self.input_history.get(i + 1) {
                    self.input = entry.clone();
                    self.cursor = 0;

                    // Restore images for this history entry
                    self.restore_images_for_history_entry(i + 1);
                }
            }
        }
    }

    fn restore_images_for_history_entry(&mut self, index: usize) {
        self.pending_images.clear();
        if let Some(history_images) = self.input_history.get_images_for_entry(index) {
            for history_image in history_images {
                if let Ok(raw_data) = history_image.to_raw_data() {
                    self.pending_images.push(PendingImage {
                        mime_type: history_image.mime_type.clone(),
                        data: raw_data,
                    });
                }
            }
        }
    }

    /// Move cursor up one line in multiline input.
    /// Returns true if the cursor moved, false if already on the first line.
    pub(crate) fn move_cursor_up(&mut self) -> bool {
        let width = self.input_text_area.width.max(1);
        let (current_line, current_col) =
            super::layout::cursor_position(&self.input, self.cursor, width);

        if current_line == 0 {
            return false;
        }

        let new_offset = super::input::line_col_to_offset(
            &self.input,
            current_line.saturating_sub(1),
            current_col,
            width,
        );
        self.cursor = new_offset.min(self.input.len());
        if !self.input.is_char_boundary(self.cursor) {
            self.cursor = super::input::prev_char_boundary(&self.input, self.cursor);
        }
        true
    }

    /// Move cursor down one line in multiline input.
    /// Returns true if the cursor moved, false if already on the last line.
    pub(crate) fn move_cursor_down(&mut self) -> bool {
        let width = self.input_text_area.width.max(1);
        let total_lines = super::layout::input_display_lines(&self.input, width) as u16;
        let (current_line, current_col) =
            super::layout::cursor_position(&self.input, self.cursor, width);

        if current_line + 1 >= total_lines {
            return false;
        }

        let new_offset =
            super::input::line_col_to_offset(&self.input, current_line + 1, current_col, width);
        self.cursor = new_offset.min(self.input.len());
        if !self.input.is_char_boundary(self.cursor) {
            self.cursor = super::input::prev_char_boundary(&self.input, self.cursor);
        }
        true
    }

    /// Update streaming duration based on elapsed time (called every tick)
    pub(crate) fn update_streaming_duration(&mut self) {
        if let Some(start_time) = self.streaming_start_time {
            let elapsed = start_time.elapsed().as_secs_f64();
            let total_duration = self.accumulated_duration + elapsed;
            self.streaming_duration = Some(total_duration);
        }
    }

    pub(crate) fn clear_messages(&mut self) {
        self.messages.clear();
        self.chat_messages.clear();
        self.pending_images.clear();
        self.pending_prompts.clear();
        self.streaming_tokens = None;
        self.streaming_tokens_display = 0;
        self.streaming_duration = None;
        self.streaming_start_time = None;
        self.last_context_tokens = None;
        self.context_limit = None;
        self.is_chatting = false;
        self.is_compacting = false;
        self.layout_cache.invalidate();
        self.reset_scroll();
        // Delete saved session
        let _ = session::delete_session(&self.working_dir);
    }

    /// Dump the current conversation context as formatted JSON
    pub(crate) fn dump_context_as_json(&mut self) {
        match serde_json::to_string_pretty(&self.chat_messages) {
            Ok(json) => {
                self.pending_pager_content = Some(json);
            }
            Err(e) => {
                self.messages.push(Message::Error(format!(
                    "Failed to serialize context to JSON: {}",
                    e
                )));
                self.layout_cache.invalidate();
                self.reset_scroll();
            }
        }
    }

    pub(crate) fn slash_menu_query(&self) -> Option<String> {
        let trimmed = self.input.trim_start();
        if !trimmed.starts_with('/') {
            return None;
        }

        let query = trimmed
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        Some(query)
    }

    pub(crate) fn slash_menu_active(&self) -> bool {
        // Don't show menu when browsing history - allow up/down to navigate history
        if self.history_index.is_some() {
            return false;
        }

        let trimmed = self.input.trim_start();
        if !trimmed.starts_with('/') {
            return false;
        }

        // Extract the part after the leading slash
        let after_slash = trimmed.trim_start_matches('/');

        // If there's a space, it means the user has entered a command and is now
        // typing arguments, so the menu should close
        !after_slash.contains(char::is_whitespace)
    }

    pub(crate) fn slash_menu_items(&mut self) -> Vec<DynamicSlashCommand> {
        let Some(query) = self.slash_menu_query() else {
            self.slash_menu_index = 0;
            return Vec::new();
        };

        // Reload custom commands to pick up any newly added files
        self.custom_commands =
            crate::custom_commands::load_custom_commands().unwrap_or_else(|_| Vec::new());

        let is_claude = self
            .current_model
            .as_ref()
            .is_some_and(|m| matches!(m.provider, crate::providers::ModelProvider::Claude));
        let has_claude_oauth = crate::commands::has_claude_oauth_provider();

        let matches = filter_commands(
            &query,
            true,
            is_claude,
            has_claude_oauth,
            &self.custom_commands,
        );

        if matches.is_empty() || self.slash_menu_index >= matches.len() {
            self.slash_menu_index = 0;
        }

        matches
    }

    pub(crate) fn apply_slash_command_to_input(&mut self, name: &str) {
        let trimmed = self.input.trim_start();
        let leading_ws_len = self.input.len().saturating_sub(trimmed.len());
        let leading_ws = &self.input[..leading_ws_len];

        let rest = trimmed
            .strip_prefix('/')
            .and_then(|after_slash| {
                after_slash
                    .find(|c: char| c.is_whitespace())
                    .map(|idx| &after_slash[idx..])
            })
            .unwrap_or("");

        self.input = format!("{leading_ws}/{name}{rest}");
        self.cursor = self.input.len();
        self.history_index = None;
        self.input_stash.clear();
    }

    pub(crate) fn move_slash_selection(&mut self, delta: isize) -> bool {
        let matches = self.slash_menu_items();
        if matches.is_empty() {
            return false;
        }

        let len = matches.len() as isize;
        let new_index = (self.slash_menu_index as isize + delta).rem_euclid(len);
        self.slash_menu_index = new_index as usize;
        true
    }

    pub(crate) fn apply_slash_selection(&mut self) -> bool {
        let Some(selected) = self.slash_menu_items().get(self.slash_menu_index).cloned() else {
            return false;
        };

        // Special handling for /model and /settings commands - open the menu immediately
        if matches!(selected.command, Command::Model) {
            self.open_model_menu();
            self.input.clear();
            self.cursor = 0;
            return true;
        }

        if matches!(selected.command, Command::Settings) {
            self.open_settings_menu();
            self.input.clear();
            self.cursor = 0;
            return true;
        }

        self.apply_slash_command_to_input(&selected.name);
        true
    }

    pub(crate) fn execute_slash_selection(&mut self) -> bool {
        let Some(selected) = self.slash_menu_items().get(self.slash_menu_index).cloned() else {
            return false;
        };

        // Check if this is a custom command
        if matches!(selected.command, Command::Custom { .. }) {
            // For custom commands, insert the command name with a trailing space
            // This closes the menu (due to whitespace check) and positions cursor for arguments
            self.input = format!("/{} ", selected.name);
            self.cursor = self.input.len();
            self.history_index = None;
            self.input_stash.clear();
            return true;
        }

        // For built-in commands, preserve existing behavior (immediate execution)
        // Preserve any arguments that were already typed
        let current_input = self.input.trim();
        let args = if let Some(cmd_part) = current_input.strip_prefix('/') {
            // Extract any text after the command name
            if let Some(space_pos) = cmd_part.find(char::is_whitespace) {
                &cmd_part[space_pos..]
            } else {
                ""
            }
        } else {
            ""
        };

        self.input = format!("/{}{}", selected.name, args);
        self.cursor = self.input.len();
        self.submit_message();
        true
    }

    pub(crate) fn handle_slash_menu_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> bool {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyModifiers;

        if !self.slash_menu_active() {
            return false;
        }
        if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }

        match code {
            KeyCode::Enter => self.execute_slash_selection(),
            KeyCode::Up => self.move_slash_selection(-1),
            KeyCode::Down => self.move_slash_selection(1),
            KeyCode::Tab => self.apply_slash_selection(),
            KeyCode::BackTab => self.move_slash_selection(-1),
            _ => false,
        }
    }

    pub(crate) fn scroll_up(&mut self) {
        let amount = self.accelerated_scroll();
        self.scroll_lines = (self.scroll_lines + amount).min(self.max_scroll());

        // Update absolute position when scrolling up
        if self.scroll_lines > 0 {
            let viewport = self.last_viewport_height as usize;
            let abs_pos = self
                .last_total_height
                .saturating_sub(viewport + self.scroll_lines);
            self.absolute_scroll_position = Some(abs_pos);
        }
    }

    pub(crate) fn scroll_down(&mut self) {
        let amount = self.accelerated_scroll();
        self.scroll_lines = self.scroll_lines.saturating_sub(amount);

        // Resume auto-scroll when reaching bottom, otherwise update absolute position
        if self.scroll_lines == 0 {
            self.absolute_scroll_position = None;
            self.has_new_content_below = false;
        } else if self.absolute_scroll_position.is_some() {
            let viewport = self.last_viewport_height as usize;
            let abs_pos = self
                .last_total_height
                .saturating_sub(viewport + self.scroll_lines);
            self.absolute_scroll_position = Some(abs_pos);
        }
    }

    pub(crate) fn scroll_up_by(&mut self, amount: usize) {
        self.scroll_lines = (self.scroll_lines + amount).min(self.max_scroll());

        // Update absolute position when scrolling up
        if self.scroll_lines > 0 {
            let viewport = self.last_viewport_height as usize;
            let abs_pos = self
                .last_total_height
                .saturating_sub(viewport + self.scroll_lines);
            self.absolute_scroll_position = Some(abs_pos);
        }
    }

    pub(crate) fn scroll_down_by(&mut self, amount: usize) {
        self.scroll_lines = self.scroll_lines.saturating_sub(amount);

        // Resume auto-scroll when reaching bottom, otherwise update absolute position
        if self.scroll_lines == 0 {
            self.absolute_scroll_position = None;
            self.has_new_content_below = false;
        } else if self.absolute_scroll_position.is_some() {
            let viewport = self.last_viewport_height as usize;
            let abs_pos = self
                .last_total_height
                .saturating_sub(viewport + self.scroll_lines);
            self.absolute_scroll_position = Some(abs_pos);
        }
    }

    fn accelerated_scroll(&mut self) -> usize {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_scroll);
        self.last_scroll = now;

        // If scrolling rapidly (within 150ms), increase acceleration
        // Wider window to work well in debug builds
        if elapsed.as_millis() < 150 {
            self.scroll_accel = (self.scroll_accel + 1.0).min(8.0);
        } else {
            // Decay acceleration based on pause length
            let decay = (elapsed.as_millis() as f64 / 150.0).min(1.0);
            self.scroll_accel = (self.scroll_accel - decay * 3.0).max(2.0);
        }

        self.scroll_accel.round() as usize
    }

    pub(crate) fn max_scroll(&self) -> usize {
        let viewport = self.last_viewport_height as usize;
        if viewport == 0 {
            return 0;
        }
        self.last_total_height.saturating_sub(viewport)
    }

    pub(crate) fn reset_scroll(&mut self) {
        self.scroll_lines = 0;
        self.absolute_scroll_position = None;
        self.scroll_accel = 2.0;
        self.has_new_content_below = false;
    }

    pub(super) fn mark_new_content_if_scrolled(&mut self) {
        if self.absolute_scroll_position.is_some() {
            self.has_new_content_below = true;
        }
    }

    pub(super) fn adjust_scroll_for_content_change(&mut self, width: u16) {
        if let Some(abs_pos) = self.absolute_scroll_position {
            // Recompute heights to get new total
            let _ = self.message_heights(width);
            let new_total = self.layout_cache.total_height;
            let viewport = self.last_viewport_height as usize;

            // Recalculate scroll_lines from absolute position
            self.scroll_lines = new_total.saturating_sub(viewport + abs_pos);
            self.last_total_height = new_total;
        }
    }

    pub(crate) fn message_heights(&mut self, width: u16) -> &[u16] {
        if self.layout_cache.width != width
            || self.layout_cache.heights.len() != self.messages.len()
        {
            let heights: Vec<u16> = self
                .messages
                .iter()
                .map(|m| message_display_height(m, width))
                .collect();

            self.layout_cache.heights = heights;
            // Total height is just the sum of message heights
            let content_height: usize = self.layout_cache.heights.iter().map(|h| *h as usize).sum();
            self.layout_cache.total_height = content_height;
            self.layout_cache.width = width;
        }

        &self.layout_cache.heights
    }

    pub(crate) fn run_shell_command(&mut self, command: &str) {
        if command.is_empty() {
            self.messages.push(Message::Shell(ShellMessage {
                command: String::new(),
                stdout: String::new(),
                stderr: "No command provided".to_string(),
                status: None,
                display: format_shell_display("", "", "No command provided", None, false),
                running: false,
            }));
            self.layout_cache.invalidate();
            self.reset_scroll();
            return;
        }

        // Create initial message showing the command is running
        let display = format!("! {command}\n  ...");
        let message_idx = self.messages.len();
        self.messages.push(Message::Shell(ShellMessage {
            command: command.to_string(),
            stdout: String::new(),
            stderr: String::new(),
            status: None,
            display,
            running: true,
        }));
        self.layout_cache.invalidate();
        self.reset_scroll();

        // Spawn the command in a background thread
        let cmd = command.to_string();
        let tx = self.shell_tx.clone();

        thread::spawn(move || {
            let result = ProcessCommand::new("sh")
                .arg("-c")
                .arg(&cmd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();

            match result {
                Ok(mut child) => {
                    // Read stdout and stderr in separate threads and merge
                    let stdout = child.stdout.take();
                    let stderr = child.stderr.take();

                    let tx_out = tx.clone();
                    let idx = message_idx;
                    let stdout_handle = stdout.map(|out| {
                        thread::spawn(move || {
                            let reader = BufReader::new(out);
                            for line in reader.lines().map_while(Result::ok) {
                                let _ = tx_out.send(ShellEvent::Output {
                                    message_idx: idx,
                                    line,
                                });
                            }
                        })
                    });

                    let tx_err = tx.clone();
                    let idx = message_idx;
                    let stderr_handle = stderr.map(|err| {
                        thread::spawn(move || {
                            let reader = BufReader::new(err);
                            for line in reader.lines().map_while(Result::ok) {
                                let _ = tx_err.send(ShellEvent::Output {
                                    message_idx: idx,
                                    line,
                                });
                            }
                        })
                    });

                    // Wait for output threads to finish
                    if let Some(h) = stdout_handle {
                        let _ = h.join();
                    }
                    if let Some(h) = stderr_handle {
                        let _ = h.join();
                    }

                    // Wait for the command to complete
                    let status = child.wait().ok().and_then(|s| s.code());
                    let _ = tx.send(ShellEvent::Done {
                        message_idx,
                        status,
                    });
                }
                Err(err) => {
                    let _ = tx.send(ShellEvent::Output {
                        message_idx,
                        line: format!("Failed to run command: {err}"),
                    });
                    let _ = tx.send(ShellEvent::Done {
                        message_idx,
                        status: None,
                    });
                }
            }
        });
    }

    /// Process any pending shell output events
    pub(crate) fn poll_shell_events(&mut self) -> bool {
        let mut updated = false;
        let width = self.layout_cache.width;
        while let Ok(event) = self.shell_rx.try_recv() {
            match event {
                ShellEvent::Output { message_idx, line } => {
                    if let Some(Message::Shell(shell)) = self.messages.get_mut(message_idx) {
                        // Append output line
                        if !shell.stdout.is_empty() {
                            shell.stdout.push('\n');
                        }
                        shell.stdout.push_str(&line);
                        // Update display
                        shell.display =
                            format_shell_display(&shell.command, &shell.stdout, "", None, true);
                        self.layout_cache.invalidate();
                        if width > 0 {
                            self.adjust_scroll_for_content_change(width);
                        }
                        updated = true;
                    }
                }
                ShellEvent::Done {
                    message_idx,
                    status,
                } => {
                    if let Some(Message::Shell(shell)) = self.messages.get_mut(message_idx) {
                        shell.status = status;
                        shell.running = false;
                        // Final display update
                        shell.display = format_shell_display(
                            &shell.command,
                            &shell.stdout,
                            &shell.stderr,
                            status,
                            false,
                        );
                        self.layout_cache.invalidate();
                        self.reset_scroll();
                        updated = true;
                    }
                }
                ShellEvent::UsageData(limits) => {
                    if self.is_cleared {
                        break;
                    }
                    // Remove the "Fetching rate limits..." message
                    if let Some(pos) = self.messages.iter().position(
                        |m| matches!(m, Message::Text(t) if t == "Fetching rate limits..."),
                    ) {
                        self.messages.remove(pos);
                    }
                    let display_text = format_usage_text(&limits);
                    self.messages.push(Message::Usage(UsageDisplay {
                        limits,
                        display_text,
                    }));
                    self.layout_cache.invalidate();
                    self.reset_scroll();
                    updated = true;
                }
                ShellEvent::UsageError(err) => {
                    // Remove the "Fetching rate limits..." message
                    if let Some(pos) = self.messages.iter().position(
                        |m| matches!(m, Message::Text(t) if t == "Fetching rate limits..."),
                    ) {
                        self.messages.remove(pos);
                    }
                    self.messages.push(Message::Error(err));
                    self.layout_cache.invalidate();
                    self.reset_scroll();
                    updated = true;
                }
                ShellEvent::ContextReady(content, provider_manager) => {
                    // Restore the provider_manager
                    self.provider_manager = Some(provider_manager);
                    // Remove the "Preparing request..." message
                    if let Some(pos) = self
                        .messages
                        .iter()
                        .position(|m| matches!(m, Message::Text(t) if t == "Preparing request..."))
                    {
                        self.messages.remove(pos);
                        self.layout_cache.invalidate();
                    }
                    // Set pending pager content - main loop will handle opening bat
                    self.pending_pager_content = Some(content);
                    updated = true;
                }
                ShellEvent::ContextError(err, provider_manager) => {
                    // Restore the provider_manager
                    self.provider_manager = Some(provider_manager);
                    // Remove the "Preparing request..." message
                    if let Some(pos) = self
                        .messages
                        .iter()
                        .position(|m| matches!(m, Message::Text(t) if t == "Preparing request..."))
                    {
                        self.messages.remove(pos);
                        self.layout_cache.invalidate();
                    }
                    self.messages.push(Message::Error(err));
                    self.layout_cache.invalidate();
                    self.reset_scroll();
                    updated = true;
                }
                ShellEvent::TokenCount(content, provider_manager) => {
                    // Restore the provider_manager
                    self.provider_manager = Some(provider_manager);
                    // Remove the "Counting tokens..." message
                    if let Some(pos) = self
                        .messages
                        .iter()
                        .position(|m| matches!(m, Message::Text(t) if t == "Counting tokens..."))
                    {
                        self.messages.remove(pos);
                    }
                    // Display the token count inline
                    self.messages.push(Message::Text(content));
                    self.layout_cache.invalidate();
                    self.reset_scroll();
                    updated = true;
                }
                ShellEvent::TokenCountError(err, provider_manager) => {
                    // Restore the provider_manager
                    self.provider_manager = Some(provider_manager);
                    // Remove the "Counting tokens..." message
                    if let Some(pos) = self
                        .messages
                        .iter()
                        .position(|m| matches!(m, Message::Text(t) if t == "Counting tokens..."))
                    {
                        self.messages.remove(pos);
                    }
                    self.messages.push(Message::Error(err));
                    self.layout_cache.invalidate();
                    self.reset_scroll();
                    updated = true;
                }
            }
        }
        updated
    }

    /// Get the display text for a message by index
    pub(crate) fn message_display_text(&self, index: usize) -> Option<String> {
        if index < self.messages.len() {
            self.messages.get(index).map(|msg| match msg {
                Message::Text(text) => bulletify(text),
                Message::Error(err) => format_error_message(err),
                Message::User(user_msg) => user_msg.display_text.clone(),
                Message::AssistantThinking(msg) => {
                    let trimmed = msg.text.trim();
                    let mut indented = String::new();
                    for line in trimmed.lines() {
                        indented.push_str("  ");
                        indented.push_str(line.trim_end());
                        indented.push('\n');
                    }
                    if indented.ends_with('\n') {
                        indented.pop();
                    }
                    indented
                }
                Message::AssistantToolCalls(msg) => {
                    let mut text = String::new();
                    for (i, call) in msg.calls.iter().enumerate() {
                        text.push_str("  ");
                        text.push_str(call.trim());
                        if i < msg.calls.len() - 1 {
                            text.push('\n');
                        }
                    }
                    text
                }
                Message::AssistantText(msg) => msg.text.trim().to_string(),
                Message::Shell(shell) => shell.display.clone(),
                Message::Usage(usage) => usage.display_text.clone(),
                Message::TodoList(todo) => todo.display_text.clone(),
                Message::FileDiff(diff) => {
                    format!(
                        "{} +{} -{}\n{}",
                        diff.path, diff.lines_added, diff.lines_removed, diff.diff
                    )
                }
            })
        } else {
            let pending_idx = index - self.messages.len();
            self.pending_prompts
                .get(pending_idx)
                .map(|p| p.display_text.clone())
        }
    }

    /// Get the selected text from messages
    pub(crate) fn get_selected_text(&self) -> Option<String> {
        let (start, end) = self.selection.ordered()?;
        let mut result = String::new();

        for idx in start.message_idx..=end.message_idx {
            let Some(text) = self.message_display_text(idx) else {
                continue;
            };

            let msg_start = if idx == start.message_idx {
                start.byte_offset
            } else {
                0
            };
            let msg_end = if idx == end.message_idx {
                // Use next_char_boundary to properly include the last character
                next_char_boundary(&text, end.byte_offset)
            } else {
                text.len()
            };

            if msg_start < text.len() && msg_end <= text.len() {
                if !result.is_empty() && idx > start.message_idx {
                    result.push('\n');
                }
                result.push_str(&text[msg_start..msg_end]);
            }
        }

        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    /// Handle clipboard paste
    pub(crate) fn handle_clipboard_paste(&mut self) -> io::Result<()> {
        // Try image paste first (check clipboard types before fetching content)
        // This prevents raw image bytes from being interpreted as text
        if let Ok((bytes, mime)) = clipboard::paste_image() {
            self.add_pending_image(mime, bytes);
            return Ok(());
        }

        // Fall back to text paste
        if let Ok(text) = clipboard::paste_text() {
            self.insert_str_at_cursor(&text);
            self.reset_scroll();
            return Ok(());
        }

        Err(io::Error::other("No text or image in clipboard"))
    }

    /// Handle primary selection paste (middle mouse button)
    pub(crate) fn handle_primary_paste(&mut self) -> io::Result<()> {
        if let Ok(text) = clipboard::paste_primary() {
            self.insert_str_at_cursor(&text);
            self.reset_scroll();
            return Ok(());
        }

        Err(io::Error::other("No text in primary selection"))
    }

    fn add_pending_image(&mut self, mime_type: String, data: Vec<u8>) {
        let image_num = self.pending_images.len() + 1;
        let size_str = format_bytes(data.len());
        self.pending_images.push(PendingImage { mime_type, data });
        let marker = format!("[Image #{} {}]", image_num, size_str);
        self.insert_str_at_cursor(&marker);
    }
}

fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;

    let bytes_f = bytes as f64;
    if bytes_f >= MB {
        format!("{:.1}MB", bytes_f / MB)
    } else if bytes_f >= KB {
        format!("{:.1}KB", bytes_f / KB)
    } else {
        format!("{}B", bytes)
    }
}

// Implement InputEditor trait for App
impl InputEditor for App {
    fn input(&self) -> &str {
        &self.input
    }

    fn input_mut(&mut self) -> &mut String {
        &mut self.input
    }

    fn cursor(&self) -> usize {
        self.cursor
    }

    fn set_cursor(&mut self, pos: usize) {
        // Ensure cursor is at a valid UTF-8 character boundary
        let pos = pos.min(self.input.len());
        self.cursor = if self.input.is_char_boundary(pos) {
            pos
        } else {
            // Snap to previous character boundary
            super::input::prev_char_boundary(&self.input, pos)
        };
    }
}

/// Extract display text from user replay segments
fn extract_user_display(segments: &[session::ReplaySegment]) -> (String, bool) {
    let mut text = String::new();
    let mut has_images = false;

    for segment in segments {
        match segment {
            session::ReplaySegment::UserText {
                text: t,
                has_images: img,
            } => {
                text = t.clone();
                has_images = *img;
            }
            session::ReplaySegment::Summary {
                summary,
                messages_compacted,
            } => {
                text.push_str(&format!(
                    "── Compacted {} messages ──\n",
                    messages_compacted
                ));
                text.push_str(summary);
            }
            _ => {}
        }
    }

    (text, has_images)
}

/// Build assistant display messages from replay segments
fn build_assistant_messages(segments: &[session::ReplaySegment], messages: &mut Vec<Message>) {
    let mut current_thinking: Option<String> = None;
    let mut current_tool_calls: Vec<String> = Vec::new();
    let mut current_text = String::new();

    for segment in segments {
        match segment {
            session::ReplaySegment::Thinking { text: t } => {
                // Flush any pending tool calls or text
                if !current_tool_calls.is_empty() {
                    messages.push(Message::AssistantToolCalls(ToolCallsMessage {
                        calls: std::mem::take(&mut current_tool_calls),
                        is_streaming: false,
                    }));
                }
                if !current_text.is_empty() {
                    messages.push(Message::AssistantText(TextMessage {
                        text: std::mem::take(&mut current_text),
                        is_streaming: false,
                    }));
                }

                // Accumulate thinking text (may span multiple segments)
                if let Some(existing) = &mut current_thinking {
                    existing.push('\n');
                    existing.push_str(t);
                } else {
                    current_thinking = Some(t.clone());
                }
            }
            session::ReplaySegment::ToolCall {
                description,
                status,
                ..
            } => {
                // Flush any pending thinking or text
                if let Some(thinking_text) = current_thinking.take() {
                    messages.push(Message::AssistantThinking(ThinkingMessage {
                        text: thinking_text,
                        is_streaming: false,
                    }));
                }
                if !current_text.is_empty() {
                    messages.push(Message::AssistantText(TextMessage {
                        text: std::mem::take(&mut current_text),
                        is_streaming: false,
                    }));
                }

                // Add tool call to current batch
                let indicator = match status {
                    session::ToolStatus::Success => "✓",
                    session::ToolStatus::Error => "✗",
                    session::ToolStatus::Pending => "▶",
                };
                current_tool_calls.push(format!("{} {}", indicator, description));
            }
            session::ReplaySegment::ToolResult {
                is_error,
                error_preview,
            } => {
                // Tool results append to the current tool calls batch
                if *is_error && let Some(preview) = error_preview {
                    current_tool_calls.push(format!("  ✗ Error: {}", preview));
                }
            }
            session::ReplaySegment::Text { text: t } => {
                // Flush any pending thinking or tool calls
                if let Some(thinking_text) = current_thinking.take() {
                    messages.push(Message::AssistantThinking(ThinkingMessage {
                        text: thinking_text,
                        is_streaming: false,
                    }));
                }
                if !current_tool_calls.is_empty() {
                    messages.push(Message::AssistantToolCalls(ToolCallsMessage {
                        calls: std::mem::take(&mut current_tool_calls),
                        is_streaming: false,
                    }));
                }

                // Accumulate text (may span multiple segments)
                if !current_text.is_empty() {
                    current_text.push('\n');
                }
                current_text.push_str(t);
            }
            _ => {}
        }
    }

    // Flush any remaining content
    if let Some(thinking_text) = current_thinking {
        messages.push(Message::AssistantThinking(ThinkingMessage {
            text: thinking_text,
            is_streaming: false,
        }));
    }
    if !current_tool_calls.is_empty() {
        messages.push(Message::AssistantToolCalls(ToolCallsMessage {
            calls: current_tool_calls,
            is_streaming: false,
        }));
    }
    if !current_text.is_empty() {
        messages.push(Message::AssistantText(TextMessage {
            text: current_text,
            is_streaming: false,
        }));
    }
}

/// Format rate limits as plain text for selection
fn format_usage_text(limits: &crate::usage::RateLimits) -> String {
    fn format_reset_time_local(timestamp: i64) -> String {
        use chrono::{DateTime, Local, TimeZone, Utc};

        let dt = Utc.timestamp_opt(timestamp, 0).unwrap();
        let now = Utc::now();
        let duration = dt.signed_duration_since(now);

        if duration.num_seconds() <= 0 {
            return "now".to_string();
        }

        let hours = duration.num_hours();
        let minutes = (duration.num_minutes() % 60).abs();

        let duration_str = if hours > 24 {
            let days = hours / 24;
            let remaining_hours = hours % 24;
            format!("{}d {}h", days, remaining_hours)
        } else if hours > 0 {
            format!("{}h {}m", hours, minutes)
        } else {
            format!("{}m", minutes)
        };

        let local_time: DateTime<Local> = dt.into();
        let time_str = local_time.format("%a %b %d %H:%M").to_string();

        format!("{} ({})", duration_str, time_str)
    }

    fn progress_bar(util: f64) -> String {
        let width = 20;
        let filled = ((util * width as f64).round() as usize).min(width);
        let empty = width - filled;
        format!("{}{}", "█".repeat(filled), "░".repeat(empty))
    }

    let mut output = String::new();
    output.push_str("Anthropic Rate Limits\n\n");

    if let (Some(util), Some(reset)) = (limits.unified_5h_utilization, limits.unified_5h_reset) {
        output.push_str(&format!(
            "  5-hour limit:  {} {:5.1}%  resets in {}\n",
            progress_bar(util),
            util * 100.0,
            format_reset_time_local(reset)
        ));
    }

    if let (Some(util), Some(reset)) = (limits.unified_7d_utilization, limits.unified_7d_reset) {
        output.push_str(&format!(
            "  7-day limit:   {} {:5.1}%  resets in {}\n",
            progress_bar(util),
            util * 100.0,
            format_reset_time_local(reset)
        ));
    }

    if let (Some(util), Some(reset)) = (
        limits.unified_7d_sonnet_utilization,
        limits.unified_7d_sonnet_reset,
    ) {
        output.push_str(&format!(
            "  7d Sonnet:     {} {:5.1}%  resets in {}\n",
            progress_bar(util),
            util * 100.0,
            format_reset_time_local(reset)
        ));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_queue_logic() {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            None,
            PathBuf::from("."),
            None,
            rx,
            AppConfig {
                show_network_stats: true,
                show_diffs: true,
                lsp_enabled: true,
            },
            crate::output::OutputContext::null(),
        );

        // Simulate busy state
        app.is_chatting = true;

        // Try to start chat
        app.start_chat("Prompt 1".to_string(), Vec::new(), "Prompt 1".to_string());
        assert_eq!(app.pending_prompts.len(), 1);
        assert_eq!(app.pending_prompts[0].input, "Prompt 1");

        // Add another
        app.start_chat("Prompt 2".to_string(), Vec::new(), "Prompt 2".to_string());
        assert_eq!(app.pending_prompts.len(), 2);
        assert_eq!(app.pending_prompts[1].input, "Prompt 2");

        // Simulate not busy
        app.is_chatting = false;

        // In the real app, the loop pops the queue. Here we can manually verify draining behavior matches expectation.
        let next = app.pending_prompts.pop_front().unwrap();
        assert_eq!(next.input, "Prompt 1");

        // Queue still has "Prompt 2"
        assert_eq!(app.pending_prompts.len(), 1);

        // start_chat should now NOT queue since is_chatting is false (and chat_result_rx is None)
        app.start_chat(next.input, Vec::new(), next.display_text);

        // Queue should still have only "Prompt 2" (size 1), Prompt 1 was NOT added back
        assert_eq!(app.pending_prompts.len(), 1);
        assert_eq!(app.pending_prompts[0].input, "Prompt 2");

        // Since no provider, it should have added an error message
        match app.messages.last().unwrap() {
            Message::Text(t) => assert!(t.contains("No provider configured")),
            _ => panic!("Expected error text"),
        }
    }

    #[test]
    fn test_open_model_menu() {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            None,
            PathBuf::from("."),
            None,
            rx,
            AppConfig {
                show_network_stats: true,
                show_diffs: true,
                lsp_enabled: true,
            },
            crate::output::OutputContext::null(),
        );

        // Initially no model menu should be open
        assert!(!app.model_menu_active());

        // Open model menu
        app.open_model_menu();

        // Model menu should now be active
        assert!(app.model_menu_active());
        assert!(app.model_menu.is_some());

        // Close model menu
        app.close_model_menu();

        // Model menu should no longer be active
        assert!(!app.model_menu_active());
        assert!(app.model_menu.is_none());
    }
}
