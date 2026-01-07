// SPDX-License-Identifier: MIT
// TUI interface for Henri - based on rtui2

mod app;
mod clipboard;
mod commands;
mod draw;
mod events;
mod input;
mod layout;
mod markdown;
mod menus;
mod messages;
mod models;
mod render;
mod selection;
mod settings;
mod syntax;

use std::io::{self, Read, Write};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::config;
use crossterm::{
    cursor::{Hide as HideCursor, Show as ShowCursor},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        MouseButton, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::prelude::*;
use tokio::time::interval;

use crate::config::{Config, ConfigFile};
use crate::provider::zen::ZenProvider;
use crate::providers::ProviderManager;
use crate::session::RestoredSession;

use crate::commands::{ExitStatus, ModeTransferSession};
use app::App;
use clipboard::copy_selection;
use draw::draw;
use input::{
    InputEditor, find_line_boundaries, find_word_boundaries, prev_char_boundary,
    screen_to_input_offset, snap_to_char_boundary,
};
use layout::{INPUT_PROMPT_GAP, INPUT_PROMPT_WIDTH};
use messages::Message;
use render::{ExitKey, ExitPrompt};
use selection::ContentPosition;

pub(crate) fn supports_thinking(provider: crate::providers::ModelProvider, model: &str) -> bool {
    match provider {
        crate::providers::ModelProvider::Antigravity => true,
        crate::providers::ModelProvider::OpenCodeZen => {
            ZenProvider::model_thinking_toggleable(model)
        }
        crate::providers::ModelProvider::GitHubCopilot => model.starts_with("gpt-5"),
        crate::providers::ModelProvider::Claude => true,
        crate::providers::ModelProvider::OpenAi => false,
        crate::providers::ModelProvider::OpenAiCompat => false,
        crate::providers::ModelProvider::OpenRouter => true,
    }
}

/// Main entry point for the TUI interface
pub async fn run(
    working_dir: std::path::PathBuf,
    restored_session: Option<RestoredSession>,
    initial_prompt: Option<String>,
    model: Option<String>,
    lsp_override: Option<bool>,
    read_only: bool,
) -> io::Result<ExitStatus> {
    setup_terminal()?;

    // Install panic hook to restore terminal on panic
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = restore_terminal();
        original_hook(panic_info);
    }));

    // Suppress direct output in TUI mode - we handle display through events
    // Set up unified event channel for all OutputEvents
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let output = crate::output::OutputContext::new_tui(event_tx);

    // Load config file for settings
    let config_file = ConfigFile::load().unwrap_or_default();
    let show_network_stats = config_file.show_network_stats;
    let show_diffs = config_file.show_diffs;
    let lsp_enabled = config_file.lsp_enabled;
    let todo_enabled = config_file.todo_enabled;

    // Initialize MCP and LSP servers
    config::initialize_servers(&working_dir, lsp_override).await;

    let services = crate::services::Services::new();

    // Enable read-only mode if --read-only was passed
    if read_only {
        services.set_read_only(true);
    }

    // If no model specified on CLI, try to use the one from the restored session
    let model = model.or_else(|| {
        restored_session
            .as_ref()
            .map(|s| format!("{}/{}", s.provider, s.model_id))
    });

    // Initialize provider manager from config
    let provider_manager = Config::load(model)
        .ok()
        .map(|config| ProviderManager::new(&config, services.clone()));

    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App::new(
        provider_manager,
        working_dir,
        restored_session,
        event_rx,
        app::AppConfig {
            show_network_stats,
            show_diffs,
            lsp_enabled,
            todo_enabled,
            services,
        },
        output,
    );

    // Submit initial prompt if provided
    if let Some(prompt) = initial_prompt {
        app.input = prompt;
        app.cursor = app.input.len();
        app.submit_message();
    }

    let res = run_app(&mut terminal, app).await;

    restore_terminal()?;

    match res {
        Ok(status) => Ok(status),
        Err(e) => Err(e),
    }
}

fn setup_terminal() -> io::Result<()> {
    enable_raw_mode()?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        )
    )?;
    execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture)?;
    // Set mouse pointer to text/I-beam style (OSC 22)
    // Supported by xterm, kitty, foot, and other modern terminals
    print!("\x1b]22;text\x07");
    io::Write::flush(&mut io::stdout())?;
    Ok(())
}

fn restore_terminal() -> io::Result<()> {
    // Restore default mouse pointer (OSC 22)
    print!("\x1b]22;\x07");
    io::Write::flush(&mut io::stdout())?;
    disable_raw_mode()?;
    execute!(
        io::stdout(),
        ShowCursor,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    Ok(())
}

/// Edit text in an external editor. Returns the edited text or None if editing was cancelled.
fn edit_in_external_editor(text: &str) -> io::Result<Option<String>> {
    // Get editor from environment, falling back to sensible defaults
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    // Create a secure temporary file with .md extension for better editor support
    let mut temp_file = tempfile::Builder::new()
        .prefix("henri-edit-")
        .suffix(".md")
        .tempfile()?;

    // Write current input to the temp file
    temp_file.write_all(text.as_bytes())?;
    temp_file.flush()?;

    // Temporarily restore terminal for editor
    restore_terminal()?;

    // Launch editor
    let status = Command::new(&editor).arg(temp_file.path()).status();

    // Re-setup terminal regardless of editor result
    setup_terminal()?;

    // Check if editor ran successfully
    match status {
        Ok(exit_status) if exit_status.success() => {
            // Read the edited content
            let mut content = String::new();
            std::fs::File::open(temp_file.path())?.read_to_string(&mut content)?;

            // temp_file is automatically cleaned up when dropped
            Ok(Some(content))
        }
        Ok(_) => {
            // Editor exited with non-zero status, treat as cancel
            // temp_file is automatically cleaned up when dropped
            Ok(None)
        }
        Err(e) => {
            // temp_file is automatically cleaned up when dropped
            Err(io::Error::other(format!(
                "Failed to run editor '{}': {}",
                editor, e
            )))
        }
    }
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    mut app: App,
) -> io::Result<ExitStatus> {
    let mut last_escape: Option<Instant> = None;
    let mut last_ctrl_a: Option<Instant> = None;
    let mut last_ctrl_e: Option<Instant> = None;
    let mut event_stream = EventStream::new();
    let mut tick_interval = interval(Duration::from_millis(50)); // Faster tick for animation
    let mut needs_redraw = true;

    loop {
        if app.should_exit {
            if app.should_switch_to_cli {
                // Build session transfer if we have a model and messages
                let session = app.current_model.as_ref().map(|model| ModeTransferSession {
                    messages: app.chat_messages.clone(),
                    provider: model.provider,
                    model_id: model.model_id.clone(),
                    thinking_enabled: app.thinking_enabled,
                    read_only: app.read_only,
                    session_id: app.current_session_id.clone(),
                });
                return Ok(ExitStatus::SwitchToCli(session));
            }
            return Ok(ExitStatus::Quit);
        }

        // Check if we need to open an external pager
        if let Some(content) = app.pending_pager_content.take() {
            // Create a secure temporary file
            let temp_file = match tempfile::Builder::new()
                .prefix("henri-prompt-")
                .suffix(".json")
                .tempfile()
            {
                Ok(f) => f,
                Err(e) => {
                    app.messages
                        .push(Message::Error(format!("Failed to create temp file: {}", e)));
                    app.layout_cache.invalidate();
                    continue;
                }
            };

            // Write content to temp file
            if let Err(e) = std::fs::write(temp_file.path(), &content) {
                app.messages
                    .push(Message::Error(format!("Failed to write temp file: {}", e)));
                app.layout_cache.invalidate();
                continue;
            }

            // Fully restore terminal for pager
            restore_terminal()?;

            // Open in vim (read-only)
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vim".to_string());

            let status = std::process::Command::new(&editor)
                .arg(temp_file.path())
                .status();

            if let Err(e) = status {
                eprintln!("Failed to run editor: {}", e);
            }

            // temp_file is automatically cleaned up when dropped

            // Re-setup terminal and force full redraw
            setup_terminal()?;
            terminal.clear()?;
            needs_redraw = true;
            continue;
        }

        if needs_redraw {
            terminal.draw(|frame| draw(frame, &mut app))?;
            // Explicitly hide/show cursor after draw since ratatui doesn't always hide it
            if app.show_cursor {
                execute!(std::io::stdout(), ShowCursor)?;
            } else {
                execute!(std::io::stdout(), HideCursor)?;
            }
            needs_redraw = false;
        }

        tokio::select! {
            _ = tick_interval.tick() => {
                let mut tick_updated = false;

                let now = Instant::now();
                if let Some(prompt) = app.exit_prompt
                    && now >= prompt.expires_at
                {
                    app.exit_prompt = None;
                    tick_updated = true;
                }

                // Advance spinner when chatting, compacting, or fetching
                if app.is_chatting || app.is_compacting || app.is_fetching {
                    app.spinner_frame = app.spinner_frame.wrapping_add(1);
                    tick_updated = true;
                }

                // Animate token count increments
                if let Some(target) = app.streaming_tokens
                    && app.streaming_tokens_display < target
                {
                    // Calculate increment based on distance to target
                    // For small gaps (< 10), increment by 1
                    // For larger gaps, increment faster to catch up within ~100-200ms
                    let diff = target - app.streaming_tokens_display;
                    let increment = if diff < 10 {
                        1
                    } else if diff < 50 {
                        diff / 5 // Catch up in ~5 ticks (250ms)
                    } else {
                        diff / 3 // Catch up in ~3 ticks (150ms)
                    }.max(1);

                    app.streaming_tokens_display = (app.streaming_tokens_display + increment).min(target);
                    tick_updated = true;
                }

                // Update streaming duration every tick for live time display
                if app.streaming_start_time.is_some() {
                    app.update_streaming_duration();
                    tick_updated = true;
                }

                // Poll for shell command output
                if app.poll_shell_events() {
                    tick_updated = true;
                }

                // Check if chat task completed and restore state
                // NOTE: This must happen BEFORE poll_output_events so that the assistant
                // response is in chat_messages when finalize_compaction runs.
                if let Some(ref mut result_rx) = app.chat_result_rx {
                    match result_rx.try_recv() {
                        Ok((provider_manager, messages)) => {
                            app.provider_manager = Some(provider_manager);
                            // Always update chat_messages with the task result.
                            // For compaction, this includes the assistant's summary response.
                            // finalize_compaction() will then extract the summary and replace
                            // chat_messages with [summary_block + preserved_messages].
                            app.chat_messages = messages;
                            app.chat_result_rx = None;
                            app.chat_task_spawned = false;
                            tick_updated = true;
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                            // Task panicked or was dropped - clean up state
                            // Note: provider_manager is lost in this case
                            app.chat_result_rx = None;
                            app.chat_task_spawned = false;
                            app.is_chatting = false;
                            app.messages.push(Message::Error(
                                "Chat task failed unexpectedly. Provider lost - use /model to reconfigure.".into(),
                            ));
                            app.layout_cache.invalidate();
                            tick_updated = true;
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                            // Result not ready yet, keep waiting
                        }
                    }
                }

                // Poll for all output events (unified channel)
                // For compaction, this processes the Done event and calls finalize_compaction,
                // which extracts the summary from chat_messages and replaces it with
                // [summary_block + preserved_messages].
                if app.poll_output_events() {
                    tick_updated = true;
                }

                if !app.is_chatting && app.chat_result_rx.is_none() {
                    if let Some(original) = app.model_override_state.take() {
                        app.restore_model_override(Some(original));
                        tick_updated = true;
                    }

                    if let Some(next) = app.pending_prompts.pop_front() {
                        app.start_chat(
                            next.input,
                            next.images,
                            next.display_text,
                            next.model_override,
                        );
                        tick_updated = true;
                    }
                }

                // Update LSP server count
                let lsp_mgr = crate::lsp::manager();
                let count = lsp_mgr.server_count().await;
                if app.lsp_server_count != count {
                    app.lsp_server_count = count;
                    tick_updated = true;
                }

                // Update MCP server count
                let mcp_count = crate::mcp::running_server_count().await;
                if app.mcp_server_count != mcp_count {
                    app.mcp_server_count = mcp_count;
                    tick_updated = true;
                }

                // Check if MCP toggle completed
                if let Some(ref mut result_rx) = app.mcp_toggle_rx {
                    match result_rx.try_recv() {
                        Ok(result) => {
                            app.mcp_toggle_rx = None;
                            app.handle_mcp_toggle_result(result);
                            tick_updated = true;
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                            // Task failed
                            app.mcp_toggle_rx = None;
                            app.refresh_mcp_menu();
                            tick_updated = true;
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                            // Result not ready yet
                        }
                    }
                }

                // Spawn chat task if needed and we haven't spawned it yet
                if app.is_chatting && !app.chat_task_spawned && app.provider_manager.is_some() {
                    // Take ownership of what we need for the async task
                    if let Some(mut provider_manager) = app.provider_manager.take() {
                        let mut messages = std::mem::take(&mut app.chat_messages);
                        let interrupted = Arc::clone(&app.chat_interrupted);
                        let output = app.output.clone();
                        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

                        // Set thinking state before starting chat
                        provider_manager.set_thinking_enabled(app.thinking_enabled);

                        // Set thinking mode for Gemini models
                        if let Some(mode_str) = app.thinking_mode.as_deref() {
                            provider_manager.set_thinking_mode(Some(mode_str.to_string()));
                        } else {
                            provider_manager.set_thinking_mode(None);
                        }

                        app.chat_result_rx = Some(result_rx);
                        app.chat_task_spawned = true;

                        tokio::spawn(async move {
                            let _ = provider_manager.chat(&mut messages, &interrupted, &output).await;
                            let _ = result_tx.send((provider_manager, messages));
                        });
                    }
                }

                if tick_updated {
                    needs_redraw = true;
                }
            }
            maybe_event = event_stream.next() => {
                let Some(event_result) = maybe_event else {
                    break;
                };
                let event = event_result?;

                match event {
                    Event::Key(key) => {
                        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                            continue;
                        }

                        // Any keypress shows cursor again
                        app.show_cursor = true;

                        // Model menu takes priority when active
                        if app.model_menu_active()
                            && app.handle_model_menu_key(key.code, key.modifiers)
                        {
                            needs_redraw = true;
                            continue;
                        }

                        // Settings menu takes priority when active
                        if app.settings_menu_active()
                            && app.handle_settings_menu_key(key.code, key.modifiers)
                        {
                            needs_redraw = true;
                            continue;
                        }

                        // MCP menu takes priority when active
                        if app.mcp_menu_active()
                            && app.handle_mcp_menu_key(key.code, key.modifiers)
                        {
                            needs_redraw = true;
                            continue;
                        }

                        // Tools menu takes priority when active
                        if app.tools_menu_active()
                            && app.handle_tools_menu_key(key.code, key.modifiers)
                        {
                            needs_redraw = true;
                            continue;
                        }

                        // Sessions menu takes priority when active
                        if app.sessions_menu_active()
                            && app.handle_sessions_menu_key(key.code, key.modifiers)
                        {
                            needs_redraw = true;
                            continue;
                        }

                        // History search takes priority when active
                        if app.history_search_active()
                            && app.handle_history_search_key(key.code, key.modifiers)
                        {
                            needs_redraw = true;
                            continue;
                        }

                        if app.handle_slash_menu_key(key.code, key.modifiers) {
                            needs_redraw = true;
                            continue;
                        }

                        let mut needs_terminal_clear = false;

                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                                // Interrupt chat if running
                                if app.is_chatting {
                                    app.chat_interrupted.store(true, Ordering::SeqCst);
                                } else if app.input.is_empty() {
                                    if handle_exit_request(&mut app, ExitKey::CtrlC, Instant::now()) {
                                        break;
                                    }
                                } else {
                                    app.clear_input();
                                }
                            }
                            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                                if app.input.is_empty() {
                                    if handle_exit_request(&mut app, ExitKey::CtrlD, Instant::now()) {
                                        break;
                                    }
                                } else {
                                    app.delete_forward();
                                }
                            }
                            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                                if let Err(err) = app.handle_clipboard_paste() {
                                    app.messages
                                        .push(Message::Error(format!("Paste failed: {err}")));
                                    app.reset_scroll();
                                }
                            }
                            (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                                if app.thinking_available {
                                    if let Some(ref model) = app.current_model {
                                        let current = crate::providers::ThinkingState::new(
                                            app.thinking_enabled,
                                            app.thinking_mode.clone(),
                                        );
                                        let next = crate::providers::cycle_thinking_state(
                                            model.provider,
                                            &model.model_id,
                                            &current,
                                        );
                                        app.thinking_enabled = next.enabled;
                                        app.thinking_mode = next.mode;
                                    } else {
                                        app.thinking_enabled = !app.thinking_enabled;
                                        if !app.thinking_enabled {
                                            app.thinking_mode = None;
                                        }
                                    }
                                }
                            }
                            (KeyCode::Char('g'), KeyModifiers::CONTROL) => {
                                // Edit input in external editor
                                match edit_in_external_editor(&app.input) {
                                    Ok(Some(new_content)) => {
                                        // Trim trailing newlines that editors often add
                                        let trimmed = new_content.trim_end_matches('\n');
                                        app.input = trimmed.to_string();
                                        app.cursor = app.input.len();
                                    }
                                    Ok(None) => {
                                        // User cancelled, keep original input
                                    }
                                    Err(e) => {
                                        app.messages
                                            .push(Message::Error(format!("Editor failed: {}", e)));
                                        app.layout_cache.invalidate();
                                    }
                                }
                                // Force full terminal redraw after returning from editor
                                needs_terminal_clear = true;
                            }
                            (KeyCode::Char('m'), KeyModifiers::CONTROL) => {
                                // Open model selection menu
                                app.open_model_menu();
                            }
                            (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
                                // Cycle security mode
                                app.cycle_security_mode();
                            }
                            (KeyCode::BackTab, mods)
                                if mods.is_empty() || mods == KeyModifiers::SHIFT =>
                            {
                                // Check if completion is active
                                if app.completion_active() {
                                    app.move_completion(-1);
                                } else {
                                    // Cycle through favorite models
                                    app.cycle_favorite_model();
                                }
                            }
                            (KeyCode::Esc, mods) => {
                                // Clear completion menu if active, otherwise normal escape handling
                                if app.completion_active() {
                                    app.file_completer.clear();
                                } else {
                                    handle_escape(&mut app, &mut last_escape, mods);
                                }
                            }
                            (KeyCode::Enter, mods) if mods.contains(KeyModifiers::CONTROL) => {
                                // Ctrl+Enter intentionally no-op per requirements.
                            }
                            (KeyCode::Enter, mods)
                                if mods.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                            {
                                app.insert_str_at_cursor("\n");
                                app.reset_scroll();
                            }
                            (KeyCode::Char('j'), mods) | (KeyCode::Char('m'), mods)
                                if mods.contains(KeyModifiers::CONTROL) =>
                            {
                                app.insert_str_at_cursor("\n");
                                app.reset_scroll();
                            }
                            (KeyCode::Enter, _) => {
                                // If completion menu is active, apply selection and close menu
                                if app.completion_active() {
                                    app.apply_completion();
                                } else {
                                    app.submit_message();
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                app.file_completer.clear();
                                app.backspace();
                            }
                            (KeyCode::Delete, _) => {
                                app.file_completer.clear();
                                app.delete_forward();
                            }
                            (KeyCode::Left, _) => {
                                app.file_completer.clear();
                                app.move_left();
                            }
                            (KeyCode::Right, _) => {
                                app.file_completer.clear();
                                app.move_right();
                            }
                            (KeyCode::Up, mods)
                                if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                            {
                                if app.completion_active() {
                                    app.move_completion(-1);
                                } else if !app.move_cursor_up() {
                                    app.history_up();
                                }
                            }
                            (KeyCode::Down, mods)
                                if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                            {
                                if app.completion_active() {
                                    app.move_completion(1);
                                } else if !app.move_cursor_down() {
                                    app.history_down();
                                }
                            }
                            (KeyCode::Home, _) => {
                                app.file_completer.clear();
                                app.move_to_start();
                            }
                            (KeyCode::End, _) => {
                                app.file_completer.clear();
                                app.move_to_end();
                            }
                            (KeyCode::Char('a'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                                app.file_completer.clear();
                                // Check for double Ctrl+A within 2 seconds
                                let now = Instant::now();
                                let threshold = Duration::from_secs(2);

                                if let Some(prev) = last_ctrl_a
                                    && now.duration_since(prev) <= threshold
                                {
                                    app.move_to_start();
                                    last_ctrl_a = None;
                                } else {
                                    app.move_to_line_start();
                                    last_ctrl_a = Some(now);
                                }
                            }
                            (KeyCode::Char('e'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                                app.file_completer.clear();
                                // Check for double Ctrl+E within 2 seconds
                                let now = Instant::now();
                                let threshold = Duration::from_secs(2);

                                if let Some(prev) = last_ctrl_e
                                    && now.duration_since(prev) <= threshold
                                {
                                    app.move_to_end();
                                    last_ctrl_e = None;
                                } else {
                                    app.move_to_line_end();
                                    last_ctrl_e = Some(now);
                                }
                            }
                            (KeyCode::Char('b'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                                app.file_completer.clear();
                                app.move_left()
                            }
                            (KeyCode::Char('f'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                                app.file_completer.clear();
                                app.move_right()
                            }
                            (KeyCode::Char('b'), mods) if mods.contains(KeyModifiers::ALT) => {
                                app.file_completer.clear();
                                app.move_word_left()
                            }
                            (KeyCode::Char('f'), mods) if mods.contains(KeyModifiers::ALT) => {
                                app.file_completer.clear();
                                app.move_word_right()
                            }
                            (KeyCode::Char('w'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                                app.file_completer.clear();
                                app.delete_word_back()
                            }
                            (KeyCode::Char('d'), mods) if mods.contains(KeyModifiers::ALT) => {
                                app.file_completer.clear();
                                app.delete_word_forward()
                            }
                            (KeyCode::Char('u'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                                app.file_completer.clear();
                                app.kill_to_start()
                            }
                            (KeyCode::Char('k'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                                app.file_completer.clear();
                                app.kill_to_end()
                            }
                            (KeyCode::Char('r'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                                app.open_history_search();
                            }
                            (KeyCode::PageUp, mods)
                                if mods.is_empty() || mods.contains(KeyModifiers::SHIFT) =>
                            {
                                let page = app.last_viewport_height.max(1) as usize;
                                app.scroll_up_by(page);
                            }
                            (KeyCode::PageDown, mods)
                                if mods.is_empty() || mods.contains(KeyModifiers::SHIFT) =>
                            {
                                let page = app.last_viewport_height.max(1) as usize;
                                app.scroll_down_by(page);
                            }
                            (KeyCode::Char(ch), mods)
                                if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                            {
                                let mut buf = [0u8; 4];
                                let s = ch.encode_utf8(&mut buf);
                                app.file_completer.clear();
                                app.insert_str_at_cursor(s);
                            }
                            (KeyCode::Tab, mods) if mods.is_empty() => {
                                // Check if slash menu is active first
                                if app.slash_menu_active() {
                                    app.apply_slash_selection();
                                } else if app.completion_active() {
                                    // Cycle to next completion
                                    app.move_completion(1);
                                } else {
                                    // Initialize file completion
                                    app.init_completion();
                                    // If only one match, apply immediately and clear
                                    // If multiple matches, apply first one but keep menu open
                                    if app.file_completer.matches.len() == 1 {
                                        app.apply_completion();
                                    } else if !app.file_completer.matches.is_empty() {
                                        // Apply first match to input (user sees it)
                                        app.apply_completion_preview();
                                    }
                                }
                            }
                            _ => {}
                        }

                        // Clear terminal buffer if needed (e.g., after external editor)
                        if needs_terminal_clear {
                            terminal.clear()?;
                        }

                        needs_redraw = true;
                    }
                    Event::Mouse(mouse) => {
                        // Check if mouse is in or near input area (with margin for easier selection).
                        // Left margin covers the prompt area plus a small buffer.
                        let margin_left = INPUT_PROMPT_WIDTH + INPUT_PROMPT_GAP + 1;
                        let margin_right = 2;
                        let in_input_area = mouse.column
                            >= app.input_text_area.x.saturating_sub(margin_left)
                            && mouse.column
                                < app.input_text_area.x + app.input_text_area.width + margin_right
                            && mouse.row >= app.input_text_area.y
                            && mouse.row < app.input_text_area.y + app.input_text_area.height;

                        // Get the display input (without ! prefix for shell mode)
                        let (display_input, cursor_offset) = match app.input.strip_prefix('!') {
                            Some(rest) => (rest.to_string(), 1),
                            None => (app.input.clone(), 0),
                        };

                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                if !in_input_area {
                                    app.scroll_up();
                                }
                            }
                            MouseEventKind::ScrollDown => {
                                if !in_input_area {
                                    app.scroll_down();
                                }
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                let now = Instant::now();
                                // Determine click count: increment if within 2s and same position
                                let click_count = if let Some((time, x, y, count)) = app.last_click
                                    && now.duration_since(time).as_secs() < 2
                                    && x == mouse.column
                                    && y == mouse.row
                                {
                                    if count >= 3 { 1 } else { count + 1 }
                                } else {
                                    1
                                };

                                if in_input_area {
                                    app.show_cursor = true;
                                    if let Some(offset) = screen_to_input_offset(
                                        mouse.column,
                                        mouse.row,
                                        app.input_text_area,
                                        &display_input,
                                    ) {
                                        let actual_offset = offset + cursor_offset;

                                        match click_count {
                                            3 => {
                                                // Triple-click: select line
                                                let (start, end) =
                                                    find_line_boundaries(&app.input, actual_offset);
                                                let start = snap_to_char_boundary(&app.input, start);
                                                let end = snap_to_char_boundary(&app.input, end);
                                                app.cursor = end;
                                                app.input_selection.anchor = Some(start);
                                                app.input_selection.cursor = Some(end);
                                            }
                                            2 => {
                                                // Double-click: select word (copy handled by mouse-up)
                                                let (start, end) =
                                                    find_word_boundaries(&app.input, actual_offset);
                                                let start = snap_to_char_boundary(&app.input, start);
                                                let end = snap_to_char_boundary(&app.input, end);
                                                app.cursor = end;
                                                app.input_selection.anchor = Some(start);
                                                app.input_selection.cursor = Some(end);
                                            }
                                            _ => {
                                                // Single click: position cursor
                                                let offset = snap_to_char_boundary(&app.input, actual_offset);
                                                app.cursor = offset;
                                                app.input_selection.anchor = Some(offset);
                                                app.input_selection.cursor = Some(offset);
                                            }
                                        }
                                    }
                                    app.selection.clear();
                                } else {
                                    // Click in chat area - keep cursor visible in input box
                                    app.show_cursor = true;
                                    if let Some(pos) =
                                        app.position_map.lookup_nearest(mouse.column, mouse.row)
                                    {
                                        match click_count {
                                            3 => {
                                                // Triple-click: select line in message
                                                if let Some(text) = app.message_display_text(pos.message_idx) {
                                                    let (start, end) =
                                                        find_line_boundaries(&text, pos.byte_offset);
                                                    app.selection.anchor = Some(ContentPosition::new(
                                                        pos.message_idx,
                                                        start,
                                                    ));
                                                    // Use prev_char_boundary to find start of last char
                                                    let cursor_pos = prev_char_boundary(&text, end).max(start);
                                                    app.selection.cursor = Some(ContentPosition::new(
                                                        pos.message_idx,
                                                        cursor_pos,
                                                    ));
                                                    app.show_cursor = true;
                                                }
                                            }
                                            2 => {
                                                // Double-click: select word in message (copy handled by mouse-up)
                                                if let Some(text) = app.message_display_text(pos.message_idx)
                                                {
                                                    let (start, end) =
                                                        find_word_boundaries(&text, pos.byte_offset);
                                                    app.selection.anchor = Some(ContentPosition::new(
                                                        pos.message_idx,
                                                        start,
                                                    ));
                                                    // Use prev_char_boundary to find start of last char
                                                    let cursor_pos = prev_char_boundary(&text, end).max(start);
                                                    app.selection.cursor = Some(ContentPosition::new(
                                                        pos.message_idx,
                                                        cursor_pos,
                                                    ));
                                                    app.show_cursor = true;
                                                }
                                            }
                                            _ => {
                                                // Single click: start potential drag selection
                                                app.selection.anchor = Some(pos);
                                                app.selection.cursor = Some(pos);
                                            }
                                        }
                                    } else {
                                        // Click on empty space - clear selection, keep cursor
                                        app.selection.clear();
                                        app.show_cursor = true;
                                    }
                                    app.input_selection.clear();
                                }

                                // Update last click for multi-click detection
                                app.last_click = Some((now, mouse.column, mouse.row, click_count));
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                if app.input_selection.anchor.is_some() {
                                    // Dragging input selection
                                    if let Some(offset) = screen_to_input_offset(
                                        mouse.column,
                                        mouse.row,
                                        app.input_text_area,
                                        &display_input,
                                    ) {
                                        let actual_offset = offset + cursor_offset;
                                        let snapped = snap_to_char_boundary(&app.input, actual_offset);
                                        app.cursor = snapped;
                                        app.input_selection.cursor = Some(snapped);
                                    }
                                } else if app.selection.anchor.is_some() {
                                    // Dragging message selection
                                    if let Some(pos) =
                                        app.position_map.lookup_nearest(mouse.column, mouse.row)
                                    {
                                        app.selection.cursor = Some(pos);
                                        // Show cursor if selection spans multiple chars
                                        if app.selection.spans_multiple_chars() {
                                            app.show_cursor = true;
                                        }
                                    }
                                }
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                // Finalize selection and copy to clipboard
                                if let Some((start, end)) = app.input_selection.is_active()
                                    .then(|| app.input_selection.ordered())
                                    .flatten()
                                {
                                    let text = app.input
                                        [start.min(app.input.len())..end.min(app.input.len())]
                                        .to_string();
                                    if !text.is_empty()
                                        && let Err(e) = copy_selection(&text) {
                                            app.messages.push(Message::Error(format!(
                                                "Copy failed: {}",
                                                e
                                            )));
                                            app.layout_cache.invalidate();
                                        }
                                } else if app.selection.is_active()
                                    && let Some(text) = app.get_selected_text()
                                        && !text.is_empty()
                                            && let Err(e) = copy_selection(&text) {
                                                app.messages.push(Message::Error(format!(
                                                    "Copy failed: {}",
                                                    e
                                                )));
                                                app.layout_cache.invalidate();
                                            }
                            }
                            MouseEventKind::Down(MouseButton::Middle) => {
                                // Paste from primary selection on middle click
                                if in_input_area {
                                    // Position cursor at click location if desired
                                    if let Some(offset) = screen_to_input_offset(
                                        mouse.column,
                                        mouse.row,
                                        app.input_text_area,
                                        &display_input,
                                    ) {
                                        let actual_offset = offset + cursor_offset;
                                        app.cursor = snap_to_char_boundary(&app.input, actual_offset);
                                    }
                                }
                                // Paste primary selection
                                if let Err(err) = app.handle_primary_paste() {
                                    app.messages
                                        .push(Message::Error(format!("Middle-click paste failed: {err}")));
                                    app.layout_cache.invalidate();
                                    app.reset_scroll();
                                }
                            }
                            _ => {}
                        }
                        needs_redraw = true;
                    }
                    Event::Paste(paste) => {
                        app.insert_str_at_cursor(&paste);
                        app.reset_scroll();
                        needs_redraw = true;
                    }
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }
            }
        }
    }

    if app.should_switch_to_cli {
        // Build session transfer if we have a model and messages
        let session = app.current_model.as_ref().map(|model| ModeTransferSession {
            messages: app.chat_messages.clone(),
            provider: model.provider,
            model_id: model.model_id.clone(),
            thinking_enabled: app.thinking_enabled,
            read_only: app.read_only,
            session_id: app.current_session_id.clone(),
        });
        Ok(ExitStatus::SwitchToCli(session))
    } else {
        Ok(ExitStatus::Quit)
    }
}

fn handle_escape(app: &mut App, last_escape: &mut Option<Instant>, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) {
        return;
    }

    // Interrupt chat if running
    if app.is_chatting {
        app.chat_interrupted.store(true, Ordering::SeqCst);
        return;
    }

    let now = Instant::now();
    let threshold = Duration::from_millis(1000);

    if let Some(prev) = *last_escape
        && now.duration_since(prev) <= threshold
    {
        app.input.clear();
        app.reset_scroll();
        *last_escape = None;
        return;
    }

    *last_escape = Some(now);
}

fn handle_exit_request(app: &mut App, key: ExitKey, now: Instant) -> bool {
    let timeout = Duration::from_secs(2);
    if let Some(prompt) = app.exit_prompt
        && prompt.key == key
        && now <= prompt.expires_at
    {
        return true;
    }

    app.exit_prompt = Some(ExitPrompt {
        key,
        expires_at: now + timeout,
    });
    false
}
