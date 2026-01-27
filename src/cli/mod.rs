// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! CLI interface for Henri.
//!
//! Uses an event-driven architecture with a unified event loop that handles
//! keyboard input, resize events, and chat streaming concurrently.

mod clipboard;
pub(crate) mod history;
mod input;
pub(crate) mod listener;
mod markdown;
mod menus;
mod prompt;
pub(crate) mod render;
mod slash_menu;
pub(crate) mod terminal;

mod editor;

pub(crate) const TOOL_OUTPUT_VIEWPORT_LINES: usize = 10;
pub(crate) const TOOL_OUTPUT_VIEWPORT_SPACER_LINES: u16 = 1;

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use colored::Colorize;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal as crossterm_terminal;
use futures::FutureExt;
use tokio::sync::oneshot;

/// An image pasted from the clipboard
#[derive(Debug, Clone)]
pub(crate) struct PastedImage {
    pub marker: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

use crate::commands::Command;
use crate::config::Config;
use crate::custom_commands::{self, CustomCommand};
use crate::history::FileHistory;
use crate::output::{self, OutputContext};
use crate::provider::zen::ZenProvider;
use crate::provider::{ContentBlock, Message, MessageContent, Role, remove_pending_tool_turn};
use crate::providers::{
    ModelChoice, ModelProvider, ProviderManager, cycle_model_variant, cycle_thinking_state,
    default_thinking_state, get_model_variant, uses_model_variants,
};
use crate::services::Services;
use crate::session;
use crate::tools::todo::clear_todos;

use crate::cli::terminal::update_terminal_title;

use input::{InputAction, InputState};
use menus::{
    HistorySearchState, McpMenuState, ModelMenuState, SessionMenuState, SettingsMenuAction,
    SettingsMenuState, ToolsMenuState,
};
use prompt::{PromptBox, SecurityStatus, ThinkingStatus};

fn echo_user_prompt_to_output(prompt: &str, pasted_images: &[PastedImage]) {
    if history::has_events() {
        if terminal::output_has_output() {
            terminal::ensure_trailing_newlines(2);
        } else {
            terminal::print_above("\n");
        }
    } else {
        terminal::ensure_line_break();
    }

    let image_metas: Vec<history::ImageMeta> = pasted_images
        .iter()
        .map(|img| history::ImageMeta {
            _marker: img.marker.clone(),
            _mime_type: img.mime_type.clone(),
            _size_bytes: img.data.len(),
        })
        .collect();

    history::push_user_prompt(prompt, image_metas.clone());

    let rendered = render::render_event(
        &history::HistoryEvent::UserPrompt {
            text: prompt.to_string(),
            images: image_metas,
        },
        terminal::term_width() as usize,
    );
    terminal::print_above(&rendered);
    listener::CliListener::note_user_prompt_printed();
}

struct PendingPrompt {
    /// The prompt text to send
    input: String,
    /// Images pasted with this prompt
    images: Vec<PastedImage>,
}

#[derive(Debug)]
enum ChatTaskStatus {
    Ok,
    Interrupted,
    Error(String),
    Panic(String),
}

struct ChatTaskResult {
    provider_manager: ProviderManager,
    messages: Vec<Message>,
    status: ChatTaskStatus,
    can_retry_prompt: bool,
}

/// State for an active chat task
struct ChatTask {
    result_rx: oneshot::Receiver<ChatTaskResult>,
    interrupted: Arc<AtomicBool>,
    /// Cached provider info for shortcuts during streaming
    provider: crate::providers::ModelProvider,
    model_id: String,
    /// Custom provider name (if any)
    custom_provider: Option<String>,
    /// Compaction state, if this is a compaction chat
    compaction: Option<CompactionState>,
}

/// State for an in-progress compaction operation
struct CompactionState {
    /// Messages to preserve (not compacted)
    preserved: Vec<Message>,
    /// Number of messages being compacted
    messages_compacted: usize,
    /// Original messages (for rollback)
    original: Vec<Message>,
}

/// Arguments for the CLI interface
pub(crate) struct CliArgs {
    pub model: Option<String>,
    pub prompt: Vec<String>,
    pub working_dir: PathBuf,
    pub restored_session: Option<session::RestoredSession>,
    /// LSP override: Some(true) = force enable, Some(false) = force disable, None = use config
    pub lsp_override: Option<bool>,
    /// Enable read-only mode (disables file editing tools)
    pub read_only: bool,
    /// Exit after processing the prompt (batch mode)
    pub batch: bool,
}

/// Events from chat completion
enum ChatOutcome {
    /// Chat completed successfully
    Complete,
    /// Chat was interrupted
    Interrupted,
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Shorten a path by replacing the home directory with ~
fn shorten_path(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(stripped) = path.strip_prefix(&home)
    {
        return format!("~/{}", stripped.display());
    }
    path.display().to_string()
}

/// Check if thinking is available and toggleable for a given provider/model
fn supports_thinking(provider: ModelProvider, model: &str) -> bool {
    match provider {
        ModelProvider::Antigravity => true,
        ModelProvider::OpenCodeZen => ZenProvider::model_thinking_toggleable(model),
        ModelProvider::GitHubCopilot => {
            let base = model.split_once('#').map(|(b, _)| b).unwrap_or(model);
            uses_model_variants(provider, model)
                || base.starts_with("gpt-5")
                || base.starts_with("o1")
                || base.starts_with("o3")
        }
        ModelProvider::OpenAi => uses_model_variants(provider, model),
        ModelProvider::Claude => uses_model_variants(provider, model),
        ModelProvider::OpenAiCompat => false, // Thinking is config-only, not UI toggleable
        ModelProvider::OpenRouter => true,
    }
}

/// Get the LSP server count, respecting the lsp_enabled setting
async fn get_lsp_server_count() -> usize {
    let config = crate::config::ConfigFile::load().unwrap_or_default();
    if config.lsp_enabled {
        crate::lsp::manager().server_count().await
    } else {
        0
    }
}

/// Get the count of running MCP servers
async fn get_mcp_server_count(services: &Services) -> usize {
    services
        .mcp
        .server_statuses()
        .await
        .iter()
        .filter(|s| s.is_running)
        .count()
}

/// Update the prompt box with current provider/model, cwd, and thinking info
async fn update_prompt_status(
    prompt_box: &mut PromptBox,
    provider_manager: &ProviderManager,
    working_dir: &std::path::Path,
    thinking_state: &crate::providers::ThinkingState,
    services: &Services,
) {
    let provider = provider_manager
        .current_custom_provider()
        .unwrap_or_else(|| provider_manager.current_provider().id())
        .to_string();
    let model = provider_manager.current_model_id().to_string();
    let cwd = shorten_path(working_dir);

    let thinking = ThinkingStatus {
        available: supports_thinking(provider_manager.current_provider(), &model),
        enabled: thinking_state.enabled,
        mode: thinking_state.mode.clone(),
    };

    let security = SecurityStatus {
        read_only: services.is_read_only(),
        sandbox_enabled: services.is_sandbox_enabled(),
    };

    let lsp_server_count = get_lsp_server_count().await;
    let mcp_server_count = get_mcp_server_count(services).await;

    prompt_box.set_status(
        provider,
        model,
        cwd,
        thinking,
        security,
        lsp_server_count,
        mcp_server_count,
    );
}

async fn update_prompt_status_from_task(
    prompt_box: &mut PromptBox,
    task: &ChatTask,
    working_dir: &std::path::Path,
    thinking_state: &crate::providers::ThinkingState,
    services: &Services,
) {
    let provider_name = task
        .custom_provider
        .as_deref()
        .unwrap_or_else(|| task.provider.id())
        .to_string();
    let cwd = shorten_path(working_dir);
    let thinking = ThinkingStatus {
        available: supports_thinking(task.provider, &task.model_id),
        enabled: thinking_state.enabled,
        mode: thinking_state.mode.clone(),
    };
    let security = SecurityStatus {
        read_only: services.is_read_only(),
        sandbox_enabled: services.is_sandbox_enabled(),
    };
    let lsp_server_count = get_lsp_server_count().await;
    let mcp_server_count = get_mcp_server_count(services).await;
    prompt_box.set_status(
        provider_name,
        task.model_id.clone(),
        cwd,
        thinking,
        security,
        lsp_server_count,
        mcp_server_count,
    );
}

async fn refresh_prompt_status(
    prompt_box: &mut PromptBox,
    provider_manager: &Option<ProviderManager>,
    chat_task: &Option<ChatTask>,
    working_dir: &std::path::Path,
    thinking_state: &crate::providers::ThinkingState,
    services: &Services,
) {
    if let Some(pm) = provider_manager {
        update_prompt_status(prompt_box, pm, working_dir, thinking_state, services).await;
    } else if let Some(task) = chat_task {
        update_prompt_status_from_task(prompt_box, task, working_dir, thinking_state, services)
            .await;
    }
}

const NO_MODEL_CONFIGURED_MESSAGE: &str =
    "No model configured. Enter `/provider` to add a provider/model.";

fn show_no_model_configured() {
    terminal::println_above(&NO_MODEL_CONFIGURED_MESSAGE.red().to_string());
    history::push(history::HistoryEvent::Error(
        NO_MODEL_CONFIGURED_MESSAGE.to_string(),
    ));
}

fn build_ephemeral_config_for_model(model: &str) -> crate::config::Config {
    let config = crate::config::ConfigFile::load().unwrap_or_default();

    // Try to get API key from a zen provider if one exists.
    let api_key = config
        .providers
        .entries
        .values()
        .find_map(|p| p.as_zen())
        .map(|c| c.api_key.clone())
        .unwrap_or_default();

    crate::config::Config {
        api_key,
        model: model.to_string(),
    }
}
/// Main entry point for the CLI interface
pub(crate) async fn run(args: CliArgs) -> std::io::Result<()> {
    // Create output context for CLI
    let output = {
        let listener = Box::leak(Box::new(listener::CliListener::new()));
        listener.register_active();
        let proxy: Arc<dyn output::OutputListener> =
            Arc::new(listener::CliListenerProxy::new(listener));
        OutputContext::new_cli(proxy)
    };

    let services = Services::new();

    // Enable read-only mode if --read-only was passed
    if args.read_only {
        services.set_read_only(true);
    }

    // Initialize MCP and LSP servers
    crate::config::initialize_servers(&args.working_dir, args.lsp_override).await;

    // If no model specified on CLI, try to use the one from the restored session
    let model = args.model.clone().or_else(|| {
        args.restored_session
            .as_ref()
            .map(|s| format!("{}/{}", s.provider, s.model_id))
    });

    let (provider_manager, thinking_state, welcome_message) = match Config::load(model) {
        Ok(config) => {
            let provider_manager = ProviderManager::new(&config, services.clone());
            let thinking_state = provider_manager.default_thinking();
            (Some(provider_manager), thinking_state, None)
        }
        Err(crate::error::Error::NoModelConfigured) => {
            let thinking_state = crate::providers::ThinkingState::new(false, None);
            (
                None,
                thinking_state,
                Some(NO_MODEL_CONFIGURED_MESSAGE.red().to_string()),
            )
        }
        Err(e) => {
            terminal::println_above(&format!("Error: {}", e).red().to_string());
            std::process::exit(1);
        }
    };

    let mut messages: Vec<Message> = Vec::new();

    // NOTE: When modifying current_session_id, also call services.set_session_id()
    // to keep the provider's cache key in sync.
    let mut current_session_id: Option<String> = None;
    let read_only = args.read_only;
    let working_dir = args.working_dir;

    // Apply restored session if provided
    let mut thinking_state = thinking_state;
    if let Some(restored) = args.restored_session {
        messages = restored.messages;
        thinking_state.enabled = restored.thinking_enabled;
        current_session_id = Some(restored.session_id);
    } else {
        clear_todos();
    }

    if current_session_id.is_none() {
        current_session_id = Some(session::generate_session_id());
    }
    services.set_session_id(current_session_id.clone());

    // Convert prompt args to initial prompt for interactive mode
    let initial_prompt = if args.prompt.is_empty() {
        None
    } else {
        Some(args.prompt.join(" "))
    };

    // Load custom commands
    let custom_commands = custom_commands::load_custom_commands().unwrap_or_default();

    // Load prompt history
    let mut prompt_history = FileHistory::new();

    // Run the event-driven main loop
    run_event_loop(
        &output,
        provider_manager,
        messages,
        &mut thinking_state,
        &mut current_session_id,
        read_only,
        &working_dir,
        &services,
        &custom_commands,
        &mut prompt_history,
        initial_prompt,
        welcome_message,
        args.batch,
    )
    .await
}

/// Run the main event loop
#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
    output: &OutputContext,
    provider_manager: Option<ProviderManager>,
    messages: Vec<Message>,
    thinking_state: &mut crate::providers::ThinkingState,
    current_session_id: &mut Option<String>,
    read_only: bool,
    working_dir: &std::path::Path,
    services: &Services,
    custom_commands: &[CustomCommand],
    prompt_history: &mut FileHistory,
    initial_prompt: Option<String>,
    welcome_message: Option<String>,
    batch: bool,
) -> std::io::Result<()> {
    let mut prompt_box = PromptBox::new();
    let mut input_state = InputState::new(working_dir.to_path_buf());

    // Wrap in Option for ownership transfer during chat
    let mut provider_manager = provider_manager;
    let mut messages: Vec<Message> = messages;

    // Set initial is_claude state for slash menu filtering
    if let Some(ref pm) = provider_manager {
        input_state.set_is_claude(pm.current_provider() == ModelProvider::Claude);
    }

    // Active chat task state
    let mut chat_task: Option<ChatTask> = None;

    // Model menu state (active when Some)
    let mut model_menu: Option<ModelMenuState> = None;

    // Session menu state (active when Some)
    let mut session_menu: Option<SessionMenuState> = None;

    // Settings menu state (active when Some)
    let mut settings_menu: Option<SettingsMenuState> = None;

    // MCP menu state (active when Some)
    let mut mcp_menu: Option<McpMenuState> = None;

    // Tools menu state (active when Some)
    let mut tools_menu: Option<ToolsMenuState> = None;

    // History search menu state (active when Some)
    let mut history_search: Option<HistorySearchState> = None;

    // Pending model change to apply when chat completes
    let mut pending_model_change: Option<ModelChoice> = None;

    // Pending prompts queue - prompts entered during an active chat
    let mut pending_prompts: VecDeque<PendingPrompt> = VecDeque::new();
    // Only support editing/deleting the most recently queued prompt.
    // When editing, we pop it out of the queue and put it back into the input buffer.
    let mut editing_pending_prompt: Option<PendingPrompt> = None;

    // Exit prompt state
    let mut exit_prompt: Option<std::time::Instant> = None;
    prompt_box.set_exit_hint(exit_prompt);

    // Track if we're processing the initial prompt (for batch mode)
    let mut processing_initial_prompt = false;

    // Track LSP generation to detect when servers start during streaming
    let mut last_lsp_generation = crate::lsp::generation();

    // Enable raw mode for the entire session (skip in batch mode)
    if !batch {
        let cwd_for_title = shorten_path(working_dir);
        update_terminal_title(&format!("🐕 {}", cwd_for_title));

        crossterm_terminal::enable_raw_mode()?;
        // Enable keyboard enhancement for Ctrl+M support (to distinguish from Enter)
        // Enable bracketed paste to handle multi-line paste properly
        execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            EnableBracketedPaste
        )?;
    }

    // Initial draw (skip in batch mode - no interactive prompt needed)
    if !batch {
        let show_welcome_hint = initial_prompt.is_none();
        prompt_box.set_welcome_hint(show_welcome_hint);
        if let Some(ref pm) = provider_manager {
            update_prompt_status(&mut prompt_box, pm, working_dir, thinking_state, services).await;
        }

        // Print welcome message before drawing the prompt so it appears below the shell prompt.
        // Make sure we're on a new line first so we don't overwrite the command that launched henri.
        terminal::ensure_cursor_on_new_line();
        if let Some(msg) = welcome_message {
            terminal::println_above(&msg);
        }

        // Don't reserve the streaming status line area until we actually start streaming.
        // This avoids showing a blank status area on startup.
        prompt_box.draw(&input_state, true)?;

        // If we started with a restored session, replay it into the output history now that
        // the prompt is visible and output can be rendered above it.
        if let Some(session_id) = current_session_id.as_deref()
            && let Some(state) = session::load_session_by_id(working_dir, session_id)
        {
            session::replay_session_into_output(&state);
            prompt_box.redraw_history().ok();
            // Restore user draft after redraw (redraw_history draws a fresh prompt state).
            prompt_box.draw(&input_state, false)?;
        }

        // Start spinner/bandwidth updates.
        listener::reload_show_network_stats();
        listener::init_spinner();
    }

    // Load show_diffs setting (needed for both interactive and batch mode)
    listener::reload_show_diffs();

    // Submit initial prompt if provided
    if let Some(prompt) = initial_prompt {
        if provider_manager.is_none() {
            show_no_model_configured();
            if batch {
                return Ok(());
            }
        } else {
            let result = process_input(
                &prompt,
                &mut messages,
                current_session_id,
                working_dir,
                &mut prompt_box,
                &mut input_state,
                services,
                custom_commands,
                &mut provider_manager,
                thinking_state,
            )
            .await;

            match result {
                ProcessResult::Continue => {
                    if batch {
                        return Ok(());
                    }
                }
                ProcessResult::Quit => return Ok(()),
                ProcessResult::StartChat(prompt, history_entry) => {
                    let history_text = history_entry.as_ref().unwrap_or(&prompt);
                    let _ = prompt_history.add_with_images(history_text, vec![]);

                    processing_initial_prompt = batch;

                    if let Some(pm) = provider_manager.take() {
                        chat_task = Some(spawn_chat_task(
                            prompt,
                            vec![],
                            &mut messages,
                            pm,
                            thinking_state,
                            output,
                        ));
                    }
                }
                ProcessResult::OpenModelMenu => {
                    if batch {
                        return Ok(());
                    }
                    if let Some(pm) = provider_manager.as_mut() {
                        model_menu = Some(ModelMenuState::with_current_model(format!(
                            "{}/{}",
                            pm.current_custom_provider()
                                .unwrap_or_else(|| pm.current_provider().id()),
                            pm.current_model_id(),
                        )));
                        prompt_box
                            .draw_with_model_menu(&input_state, model_menu.as_ref().unwrap())?;
                    }
                }
                ProcessResult::OpenSessionsMenu => {
                    if batch {
                        return Ok(());
                    }
                    session_menu = Some(SessionMenuState::new(
                        working_dir,
                        current_session_id.as_deref(),
                    ));
                    prompt_box
                        .draw_with_sessions_menu(&input_state, session_menu.as_ref().unwrap())?;
                }
                ProcessResult::OpenSettings => {
                    if batch {
                        return Ok(());
                    }
                    settings_menu = Some(SettingsMenuState::new());
                    prompt_box
                        .draw_with_settings_menu(&input_state, settings_menu.as_ref().unwrap())?;
                }
                ProcessResult::OpenMcpMenu => {
                    if batch {
                        return Ok(());
                    }
                    let statuses = services.mcp.server_statuses().await;
                    mcp_menu = Some(McpMenuState::new(statuses));
                    prompt_box.draw_with_mcp_menu(&input_state, mcp_menu.as_ref().unwrap())?;
                }
                ProcessResult::OpenLspMenu => {
                    if batch {
                        return Ok(());
                    }
                    // /lsp behaves like /settings: show status immediately, even during streaming.
                    let config = crate::config::ConfigFile::load().unwrap_or_default();
                    if !config.lsp_enabled {
                        let msg =
                            "LSP integration is disabled. Enable it in /settings.".to_string();
                        terminal::println_above(&msg);
                        history::push(history::HistoryEvent::Info(msg));
                    } else {
                        let servers = crate::lsp::manager().server_info().await;
                        if servers.is_empty() {
                            let msg = "No LSP servers connected.".to_string();
                            terminal::println_above(&msg);
                            history::push(history::HistoryEvent::Info(msg));
                        } else {
                            let msg = format!("LSP servers connected: {}", servers.len());
                            terminal::println_above(&msg);
                            history::push(history::HistoryEvent::Info(msg));
                            for server in servers {
                                let extensions = if server.file_extensions.is_empty() {
                                    String::new()
                                } else {
                                    format!(" ({})", server.file_extensions.join(", "))
                                };
                                let msg = format!("  • {}{}", server.name, extensions);
                                terminal::println_above(&msg);
                                history::push(history::HistoryEvent::Info(msg));
                            }
                        }
                    }
                    // Ensure the prompt/status reflects any changes in server count.
                    refresh_prompt_status(
                        &mut prompt_box,
                        &provider_manager,
                        &chat_task,
                        working_dir,
                        thinking_state,
                        services,
                    )
                    .await;
                    prompt_box.draw(&input_state, false)?;
                }
                ProcessResult::OpenToolsMenu => {
                    if batch {
                        return Ok(());
                    }
                    tools_menu = Some(ToolsMenuState::new(services.is_read_only()));
                    prompt_box.draw_with_tools_menu(&input_state, tools_menu.as_ref().unwrap())?;
                }
                ProcessResult::StartCompaction(data) => {
                    processing_initial_prompt = batch;
                    if let Some(pm) = provider_manager.take() {
                        chat_task = Some(spawn_compaction_chat(data, &mut messages, pm, output));
                    }
                }
                ProcessResult::RunProviderFlow => {
                    if batch {
                        return Ok(());
                    }
                    // Run provider flow in hybrid mode:
                    // - disable raw mode (Henri prompt)
                    // - disable bracketed paste / keyboard enhancement (inquire doesn't expect them)
                    // - run inquire prompts
                    // - restore terminal modes
                    let _ = crossterm_terminal::disable_raw_mode();
                    let _ = execute!(
                        std::io::stdout(),
                        PopKeyboardEnhancementFlags,
                        DisableBracketedPaste
                    );
                    let _ = prompt_box.hide_and_clear();

                    // Run the provider management flow
                    match crate::auth::manage_providers().await {
                        Ok(crate::auth::ProviderAction::Added)
                        | Ok(crate::auth::ProviderAction::Removed) => {
                            // Reinitialize provider manager with new config
                            if let Ok(new_config) = Config::load(None) {
                                let new_pm = ProviderManager::new(&new_config, services.clone());
                                provider_manager = Some(new_pm);
                            }
                        }
                        Ok(crate::auth::ProviderAction::Cancelled) => {
                            // User cancelled
                        }
                        Err(e) => {
                            eprintln!("{}", format!("Error: {}", e).red());
                        }
                    }

                    // Restore terminal state without full redraw
                    println!();
                    let _ = execute!(
                        std::io::stdout(),
                        PushKeyboardEnhancementFlags(
                            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        ),
                        EnableBracketedPaste
                    );
                    let _ = crossterm_terminal::enable_raw_mode();

                    // Refresh prompt status with potentially new provider
                    refresh_prompt_status(
                        &mut prompt_box,
                        &provider_manager,
                        &chat_task,
                        working_dir,
                        thinking_state,
                        services,
                    )
                    .await;
                    prompt_box.draw(&input_state, true)?;
                }
            }
        }
    }

    loop {
        // Poll for chat completion if a task is running
        if let Some(ref mut task) = chat_task {
            match task.result_rx.try_recv() {
                Ok(task_result) => {
                    // Chat completed - restore state
                    provider_manager = Some(task_result.provider_manager);

                    let was_interrupted = task.interrupted.load(Ordering::SeqCst)
                        || matches!(task_result.status, ChatTaskStatus::Interrupted);

                    // Handle compaction completion
                    if let Some(compaction_state) = task.compaction.take() {
                        if was_interrupted || !matches!(task_result.status, ChatTaskStatus::Ok) {
                            // Rollback on interrupt
                            messages = compaction_state.original;
                            terminal::println_above(&match task_result.status {
                                ChatTaskStatus::Ok | ChatTaskStatus::Interrupted => {
                                    "Compaction cancelled.".yellow().to_string()
                                }
                                ChatTaskStatus::Error(msg) | ChatTaskStatus::Panic(msg) => {
                                    format!("Compaction failed: {}", msg).red().to_string()
                                }
                            });
                            pending_prompts.clear();
                        } else {
                            // Finalize compaction
                            messages = finalize_compaction(task_result.messages, compaction_state);
                            terminal::ensure_line_break();
                            terminal::println_above("");
                            // Avoid printing a trailing newline so we don't leave an extra blank
                            // row above the reserved streaming status line.
                            terminal::print_above(
                                &format!("[Compacted {} messages into summary.]", messages.len())
                                    .green()
                                    .to_string(),
                            );
                        }
                        chat_task = None;
                    } else {
                        // Normal chat completion
                        messages = task_result.messages;

                        // Apply pending model change if any
                        if let Some(choice) = pending_model_change.take()
                            && let Some(ref mut pm) = provider_manager
                        {
                            let provider_changed = pm.set_model(
                                choice.provider,
                                choice.model_id.clone(),
                                choice.custom_provider.clone(),
                            );
                            if provider_changed {
                                crate::provider::transform_thinking_for_provider_switch(
                                    &mut messages,
                                );
                            }
                            // Update thinking state to new model's default
                            let new_thinking =
                                default_thinking_state(choice.provider, &choice.model_id);
                            thinking_state.enabled = new_thinking.enabled;
                            thinking_state.mode = new_thinking.mode;
                            // Update is_claude for slash menu filtering
                            input_state.set_is_claude(choice.provider == ModelProvider::Claude);
                        }

                        chat_task = None;

                        match task_result.status {
                            ChatTaskStatus::Ok | ChatTaskStatus::Interrupted => {
                                let outcome = if was_interrupted {
                                    // Clear pending prompts on interrupt
                                    pending_prompts.clear();
                                    ChatOutcome::Interrupted
                                } else {
                                    ChatOutcome::Complete
                                };

                                if let Some(ref pm) = provider_manager {
                                    handle_chat_outcome(
                                        outcome,
                                        &mut messages,
                                        pm,
                                        thinking_state,
                                        current_session_id,
                                        read_only,
                                        working_dir,
                                        &mut prompt_box,
                                    )?;

                                    update_prompt_status(
                                        &mut prompt_box,
                                        pm,
                                        working_dir,
                                        thinking_state,
                                        services,
                                    )
                                    .await;
                                }

                                // Exit in batch mode after initial prompt completes
                                if processing_initial_prompt {
                                    break;
                                }
                            }
                            ChatTaskStatus::Error(msg) | ChatTaskStatus::Panic(msg) => {
                                pending_prompts.clear();

                                if batch {
                                    return Err(std::io::Error::other(msg));
                                }

                                terminal::println_above(
                                    &if task_result.can_retry_prompt {
                                        "Request failed. Press ↑ then Enter to retry."
                                    } else {
                                        "Request failed. Send a new prompt to continue."
                                    }
                                    .yellow()
                                    .to_string(),
                                );

                                prompt_box.draw(&input_state, true)?;

                                if let Some(ref pm) = provider_manager {
                                    update_prompt_status(
                                        &mut prompt_box,
                                        pm,
                                        working_dir,
                                        thinking_state,
                                        services,
                                    )
                                    .await;
                                }
                            }
                        }
                    }

                    // Pop next prompt from queue and start it
                    if let Some(next) = pending_prompts.pop_front()
                        && let Some(pm) = provider_manager.take()
                    {
                        // Save to history
                        let _ = prompt_history.add_with_images(&next.input, vec![]);

                        // Draw prompt first so it's visible for spawn_chat_task's println_above
                        prompt_box.draw_with_pending(&input_state, &pending_prompts)?;

                        chat_task = Some(spawn_chat_task(
                            next.input,
                            next.images,
                            &mut messages,
                            pm,
                            thinking_state,
                            output,
                        ));
                    } else {
                        prompt_box.draw(&input_state, true)?;
                    }
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    // Task panicked or was dropped - treat as error
                    chat_task = None;
                    pending_prompts.clear(); // Clear queue on error
                    output::emit_error(
                        output,
                        "Chat task failed unexpectedly and did not return. Please retry.",
                    );
                    prompt_box.draw(&input_state, true)?;
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    // Still running, continue
                }
            }
        }

        // Check if LSP generation changed (a new server started during streaming).
        // If so, redraw the prompt to show the updated LSP count.
        if let Some(ref task) = chat_task {
            let current_lsp_gen = crate::lsp::generation();
            if current_lsp_gen != last_lsp_generation {
                last_lsp_generation = current_lsp_gen;
                let provider_name = task
                    .custom_provider
                    .as_deref()
                    .unwrap_or_else(|| task.provider.id())
                    .to_string();
                let cwd = shorten_path(working_dir);
                let thinking = ThinkingStatus {
                    available: supports_thinking(task.provider, &task.model_id),
                    enabled: thinking_state.enabled,
                    mode: thinking_state.mode.clone(),
                };
                let security = SecurityStatus {
                    read_only: services.is_read_only(),
                    sandbox_enabled: services.is_sandbox_enabled(),
                };
                let lsp_server_count = get_lsp_server_count().await;
                let mcp_server_count = get_mcp_server_count(services).await;
                prompt_box.set_status(
                    provider_name,
                    task.model_id.clone(),
                    cwd,
                    thinking,
                    security,
                    lsp_server_count,
                    mcp_server_count,
                );
                prompt_box.draw(&input_state, false)?;
            }
        }

        let event = if crossterm::event::poll(Duration::from_millis(50))? {
            Some(crossterm::event::read()?)
        } else {
            None
        };

        if let Some(evt) = event {
            match evt {
                Event::Resize(cols, rows) => {
                    // In batch mode the prompt is never shown; ignore resize events.
                    // Handling resizes here can force a prompt redraw and make it visible.
                    if !batch {
                        prompt_box
                            .handle_resize(&input_state, &pending_prompts, cols, rows)
                            .await?;
                    }
                }
                Event::Paste(text) => {
                    // Handle bracketed paste - insert the full text with newlines
                    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                    input_state.insert_str(&normalized);
                    prompt_box.draw(&input_state, false)?;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // Handle ESC during chat to interrupt (cancel agent loop)
                    // But if a menu is open, just close the menu instead
                    if chat_task.is_some() && key.code == KeyCode::Esc {
                        if input_state.slash_menu_active() || input_state.completion_active() {
                            // Let the input handler close the menu
                            let action = input_state.handle_key(key);
                            if matches!(action, InputAction::Redraw) {
                                prompt_box.draw(&input_state, false)?;
                            }
                            continue;
                        }
                        // Close model menu if open, don't interrupt work
                        if model_menu.is_some() {
                            model_menu = None;
                            prompt_box.draw(&input_state, false)?;
                            continue;
                        }
                        // Close settings menu if open, don't interrupt work
                        if settings_menu.is_some() {
                            settings_menu = None;
                            prompt_box.draw(&input_state, false)?;
                            continue;
                        }
                        // Close MCP menu if open, don't interrupt work
                        if mcp_menu.is_some() {
                            mcp_menu = None;
                            prompt_box.draw(&input_state, false)?;
                            continue;
                        }
                        // LSP menu is output-only; don't interrupt work if invoked
                        // Close tools menu if open, don't interrupt work
                        if tools_menu.is_some() {
                            tools_menu = None;
                            prompt_box.draw(&input_state, false)?;
                            continue;
                        }
                        if let Some(ref task) = chat_task {
                            task.interrupted.store(true, Ordering::SeqCst);
                        }
                        continue;
                    }

                    // Handle Ctrl+T during chat using cached provider info
                    if let Some(ref task) = chat_task
                        && key.code == KeyCode::Char('t')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        // Toggle thinking for next request using cached provider/model
                        // Note: For variant providers (like OpenAI), model changes only apply
                        // when idle since we can't change the model mid-chat.
                        if supports_thinking(task.provider, &task.model_id) {
                            if uses_model_variants(task.provider, &task.model_id) {
                                let variant =
                                    get_model_variant(&task.model_id).map(|s| s.to_string());
                                thinking_state.enabled = variant.as_deref() != Some("off");
                                thinking_state.mode = variant;
                            } else {
                                let next = cycle_thinking_state(
                                    task.provider,
                                    &task.model_id,
                                    thinking_state,
                                );
                                thinking_state.enabled = next.enabled;
                                thinking_state.mode = next.mode;
                            }

                            // Update status bar with cached provider info
                            let provider_name = task
                                .custom_provider
                                .as_deref()
                                .unwrap_or_else(|| task.provider.id())
                                .to_string();
                            let cwd = shorten_path(working_dir);
                            let thinking = ThinkingStatus {
                                available: true, // We already checked supports_thinking
                                enabled: thinking_state.enabled,
                                mode: thinking_state.mode.clone(),
                            };
                            let security = SecurityStatus {
                                read_only: services.is_read_only(),
                                sandbox_enabled: services.is_sandbox_enabled(),
                            };
                            let lsp_server_count = get_lsp_server_count().await;
                            let mcp_server_count = get_mcp_server_count(services).await;
                            prompt_box.set_status(
                                provider_name,
                                task.model_id.clone(),
                                cwd,
                                thinking,
                                security,
                                lsp_server_count,
                                mcp_server_count,
                            );
                            prompt_box.draw(&input_state, false)?;
                        }
                        continue;
                    }

                    // Handle Ctrl+X during chat to toggle sandbox mode
                    if let Some(ref task) = chat_task
                        && key.code == KeyCode::Char('x')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        services.cycle_sandbox_mode();
                        // Update status bar with cached provider info
                        let provider_name = task
                            .custom_provider
                            .as_deref()
                            .unwrap_or_else(|| task.provider.id())
                            .to_string();
                        let cwd = shorten_path(working_dir);
                        let thinking = ThinkingStatus {
                            available: supports_thinking(task.provider, &task.model_id),
                            enabled: thinking_state.enabled,
                            mode: thinking_state.mode.clone(),
                        };
                        let security = SecurityStatus {
                            read_only: services.is_read_only(),
                            sandbox_enabled: services.is_sandbox_enabled(),
                        };
                        let lsp_server_count = get_lsp_server_count().await;
                        let mcp_server_count = get_mcp_server_count(services).await;
                        prompt_box.set_status(
                            provider_name,
                            task.model_id.clone(),
                            cwd,
                            thinking,
                            security,
                            lsp_server_count,
                            mcp_server_count,
                        );
                        prompt_box.draw(&input_state, false)?;
                        continue;
                    }

                    // Handle Ctrl+X when no model is configured.
                    if chat_task.is_none()
                        && key.code == KeyCode::Char('x')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        services.cycle_sandbox_mode();

                        if let Some(ref pm) = provider_manager {
                            update_prompt_status(
                                &mut prompt_box,
                                pm,
                                working_dir,
                                thinking_state,
                                services,
                            )
                            .await;
                        } else {
                            let security = SecurityStatus {
                                read_only: services.is_read_only(),
                                sandbox_enabled: services.is_sandbox_enabled(),
                            };
                            let lsp_server_count = get_lsp_server_count().await;
                            let mcp_server_count = get_mcp_server_count(services).await;

                            prompt_box.set_status(
                                String::new(),
                                String::new(),
                                shorten_path(working_dir),
                                ThinkingStatus::default(),
                                security,
                                lsp_server_count,
                                mcp_server_count,
                            );
                        }

                        prompt_box.draw(&input_state, false)?;
                        continue;
                    }

                    // Handle Ctrl+M (via keyboard enhancement) or Ctrl+O to open model menu
                    if ((key.code == KeyCode::Char('m')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                        || (key.code == KeyCode::Char('o')
                            && key.modifiers.contains(KeyModifiers::CONTROL)))
                        && model_menu.is_none()
                    {
                        // Build current model string from available sources
                        let current_model = if let Some(ref task) = chat_task {
                            format!(
                                "{}/{}",
                                task.custom_provider
                                    .as_deref()
                                    .unwrap_or_else(|| task.provider.id()),
                                task.model_id
                            )
                        } else if let Some(ref pm) = provider_manager {
                            format!(
                                "{}/{}",
                                pm.current_custom_provider()
                                    .unwrap_or_else(|| pm.current_provider().id()),
                                pm.current_model_id()
                            )
                        } else {
                            String::new()
                        };
                        model_menu = Some(ModelMenuState::with_current_model(current_model));
                        prompt_box
                            .draw_with_model_menu(&input_state, model_menu.as_ref().unwrap())?;
                        continue;
                    }

                    // During chat: allow typing but block submission and other shortcuts
                    let chatting = chat_task.is_some();

                    if chatting
                        && key.code == KeyCode::Up
                        && key.modifiers.contains(KeyModifiers::SHIFT)
                        && editing_pending_prompt.is_none()
                        && !pending_prompts.is_empty()
                        && input_state.can_edit_pending_prompt()
                    {
                        // Pop most recent queued prompt into input for editing.
                        if let Some(pending) = pending_prompts.pop_back() {
                            let prompt_text = pending.input.clone();
                            let images = pending.images.clone();

                            editing_pending_prompt = Some(pending);
                            input_state.set_content_with_images(&prompt_text, images);

                            terminal::write_status_line(
                                &"Editing queued message (Enter to re-queue, Esc to cancel, Shift+Delete to discard)"
                                    .bright_black()
                                    .to_string(),
                            );
                            prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                            continue;
                        }
                    }

                    if chatting
                        && key.code == KeyCode::Delete
                        && key.modifiers.contains(KeyModifiers::SHIFT)
                        && (editing_pending_prompt.is_some()
                            || (!pending_prompts.is_empty()
                                && input_state.can_delete_pending_prompt()))
                    {
                        // If we're editing a queued prompt, discard it (do not re-queue).
                        if editing_pending_prompt.is_some() {
                            editing_pending_prompt = None;
                            input_state.clear();
                            terminal::write_status_line(
                                &"Discarded queued message".bright_black().to_string(),
                            );
                            prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                            continue;
                        }

                        // Otherwise delete the most recently queued prompt.
                        if pending_prompts.pop_back().is_some() {
                            terminal::write_status_line(
                                &"Deleted queued message".bright_black().to_string(),
                            );
                            prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                            continue;
                        }
                    }

                    // Handle global shortcuts only when not chatting
                    if !chatting
                        && let Some(ref mut pm) = provider_manager
                        && handle_global_shortcuts(
                            key,
                            thinking_state,
                            pm,
                            services,
                            &mut prompt_box,
                            &input_state,
                            working_dir,
                        )
                        .await?
                    {
                        continue;
                    }

                    // Handle model menu if active
                    if let Some(ref mut menu) = model_menu {
                        use menus::ModelMenuAction;
                        match menu.handle_key(key) {
                            ModelMenuAction::None => {}
                            ModelMenuAction::Redraw => {
                                prompt_box.draw_with_model_menu(&input_state, menu)?;
                            }
                            ModelMenuAction::Cancel => {
                                model_menu = None;
                                prompt_box.draw(&input_state, false)?;
                            }
                            ModelMenuAction::Select(choice) => {
                                model_menu = None;

                                if chat_task.is_some() {
                                    // Chat is running - queue the model change and update status bar
                                    // Update status bar to show the queued model
                                    let cwd = shorten_path(working_dir);
                                    let thinking = ThinkingStatus {
                                        available: supports_thinking(
                                            choice.provider,
                                            &choice.model_id,
                                        ),
                                        enabled: thinking_state.enabled,
                                        mode: thinking_state.mode.clone(),
                                    };
                                    let security = SecurityStatus {
                                        read_only: services.is_read_only(),
                                        sandbox_enabled: services.is_sandbox_enabled(),
                                    };
                                    let lsp_server_count = get_lsp_server_count().await;
                                    let mcp_server_count = get_mcp_server_count(services).await;
                                    prompt_box.set_status(
                                        choice
                                            .custom_provider
                                            .clone()
                                            .unwrap_or_else(|| choice.provider.id().to_string()),
                                        choice.model_id.clone(),
                                        cwd,
                                        thinking,
                                        security,
                                        lsp_server_count,
                                        mcp_server_count,
                                    );
                                    pending_model_change = Some(choice);
                                } else if let Some(ref mut pm) = provider_manager {
                                    // Chat is not running - apply immediately
                                    let provider_changed = pm.set_model(
                                        choice.provider,
                                        choice.model_id.clone(),
                                        choice.custom_provider.clone(),
                                    );
                                    if provider_changed {
                                        crate::provider::transform_thinking_for_provider_switch(
                                            &mut messages,
                                        );
                                    }

                                    // Update thinking state to new model's default
                                    let new_thinking =
                                        default_thinking_state(choice.provider, &choice.model_id);
                                    thinking_state.enabled = new_thinking.enabled;
                                    thinking_state.mode = new_thinking.mode;

                                    // Update is_claude for slash menu filtering
                                    input_state
                                        .set_is_claude(choice.provider == ModelProvider::Claude);

                                    update_prompt_status(
                                        &mut prompt_box,
                                        pm,
                                        working_dir,
                                        thinking_state,
                                        services,
                                    )
                                    .await;
                                } else {
                                    // No provider_manager yet (no model configured at startup)
                                    let model_spec = format!(
                                        "{}/{}",
                                        choice
                                            .custom_provider
                                            .clone()
                                            .unwrap_or_else(|| choice.provider.id().to_string()),
                                        choice.model_id
                                    );
                                    let config = build_ephemeral_config_for_model(&model_spec);
                                    provider_manager =
                                        Some(ProviderManager::new(&config, services.clone()));

                                    let new_thinking =
                                        default_thinking_state(choice.provider, &choice.model_id);
                                    thinking_state.enabled = new_thinking.enabled;
                                    thinking_state.mode = new_thinking.mode;

                                    update_prompt_status(
                                        &mut prompt_box,
                                        provider_manager.as_ref().expect("provider set"),
                                        working_dir,
                                        thinking_state,
                                        services,
                                    )
                                    .await;
                                }
                                prompt_box.draw(&input_state, false)?;
                            }
                        }
                        continue;
                    }

                    // Handle session menu if active
                    if let Some(ref mut menu) = session_menu {
                        use menus::SessionMenuAction;
                        match menu.handle_key(key) {
                            SessionMenuAction::None => {}
                            SessionMenuAction::Redraw => {
                                prompt_box.draw_with_sessions_menu(&input_state, menu)?;
                            }
                            SessionMenuAction::Cancel => {
                                session_menu = None;
                                input_state.clear();
                                prompt_box.draw(&input_state, false)?;
                            }
                            SessionMenuAction::Select(selected_session) => {
                                session_menu = None;
                                input_state.clear();

                                // Load and apply the selected session
                                if let Some(state) =
                                    session::load_session_by_id(working_dir, &selected_session.id)
                                {
                                    let restored = session::RestoredSession::from_state(&state);
                                    messages = restored.messages;
                                    thinking_state.enabled = restored.thinking_enabled;
                                    services.set_read_only(restored.read_only);
                                    // Use the ID we loaded by
                                    *current_session_id = Some(selected_session.id.clone());
                                    services.set_session_id(current_session_id.clone());

                                    // Clear history and replay session
                                    session::replay_session_into_output(&state);
                                    prompt_box.redraw_history().ok();

                                    terminal::println_above(&format!(
                                        "Loaded session: {} ({} messages)",
                                        selected_session
                                            .preview
                                            .as_deref()
                                            .unwrap_or("(no preview)"),
                                        state.messages.len()
                                    ));

                                    // Update provider/model from session
                                    if let Some(ref mut pm) = provider_manager {
                                        let (provider, model_id, custom_provider) =
                                            crate::providers::parse_model_spec(&format!(
                                                "{}/{}",
                                                restored.provider, restored.model_id
                                            ));
                                        let provider_changed =
                                            pm.set_model(provider, model_id, custom_provider);
                                        if provider_changed {
                                            crate::provider::transform_thinking_for_provider_switch(
                                                &mut messages,
                                            );
                                        }
                                        pm.set_thinking_enabled(thinking_state.enabled);

                                        // Update is_claude for slash menu filtering
                                        input_state
                                            .set_is_claude(provider == ModelProvider::Claude);

                                        update_prompt_status(
                                            &mut prompt_box,
                                            pm,
                                            working_dir,
                                            thinking_state,
                                            services,
                                        )
                                        .await;
                                    }
                                } else {
                                    terminal::println_above(
                                        &"Failed to load session".red().to_string(),
                                    );
                                }
                                prompt_box.draw(&input_state, false)?;
                            }
                        }
                        continue;
                    }

                    // Handle settings menu if active
                    if let Some(ref mut menu) = settings_menu {
                        match menu.handle_key(key) {
                            SettingsMenuAction::None => {}
                            SettingsMenuAction::Redraw => {
                                refresh_prompt_status(
                                    &mut prompt_box,
                                    &provider_manager,
                                    &chat_task,
                                    working_dir,
                                    thinking_state,
                                    services,
                                )
                                .await;
                                prompt_box.draw_with_settings_menu(&input_state, menu)?;
                            }
                            SettingsMenuAction::Close => {
                                settings_menu = None;
                                input_state.clear();
                                // Reload settings after changes
                                prompt_box.reload_settings();
                                listener::reload_show_network_stats();
                                listener::reload_show_diffs();
                                refresh_prompt_status(
                                    &mut prompt_box,
                                    &provider_manager,
                                    &chat_task,
                                    working_dir,
                                    thinking_state,
                                    services,
                                )
                                .await;
                                prompt_box.draw(&input_state, false)?;
                            }
                        }
                        continue;
                    }

                    // Handle MCP menu if active
                    if let Some(ref mut menu) = mcp_menu {
                        use menus::McpMenuAction;
                        match menu.handle_key(key) {
                            McpMenuAction::None => {}
                            McpMenuAction::Redraw => {
                                prompt_box.draw_with_mcp_menu(&input_state, menu)?;
                            }
                            McpMenuAction::Close => {
                                mcp_menu = None;
                                input_state.clear();
                                prompt_box.draw(&input_state, false)?;
                            }
                            McpMenuAction::ToggleServer(_index) => {
                                // Get the server name and current state
                                if let Some(name) = menu.selected_server_name().map(String::from) {
                                    // If disabled, show "Starting" immediately before async call
                                    if menu.is_server_disabled(&name) {
                                        menu.set_server_starting(&name);
                                        prompt_box.draw_with_mcp_menu(&input_state, menu)?;
                                    }

                                    // Toggle the server
                                    let result = services.mcp.toggle_server(&name).await;
                                    match result {
                                        Ok((is_running, tool_count)) => {
                                            menu.update_server_status(
                                                &name, is_running, tool_count,
                                            );
                                        }
                                        Err(e) => {
                                            // On error, reset to disabled
                                            menu.update_server_status(&name, false, 0);
                                            terminal::println_above(
                                                &format!("Failed to toggle MCP server: {}", e)
                                                    .red()
                                                    .to_string(),
                                            );
                                        }
                                    }

                                    // Update prompt status to reflect new MCP count
                                    refresh_prompt_status(
                                        &mut prompt_box,
                                        &provider_manager,
                                        &chat_task,
                                        working_dir,
                                        thinking_state,
                                        services,
                                    )
                                    .await;

                                    prompt_box.draw_with_mcp_menu(&input_state, menu)?;
                                }
                            }
                        }
                        continue;
                    }

                    // Handle tools menu if active
                    if let Some(ref mut menu) = tools_menu {
                        use menus::ToolsMenuAction;
                        match menu.handle_key(key) {
                            ToolsMenuAction::None => {}
                            ToolsMenuAction::Redraw => {
                                prompt_box.draw_with_tools_menu(&input_state, menu)?;
                            }
                            ToolsMenuAction::Close => {
                                tools_menu = None;
                                input_state.clear();
                                prompt_box.draw(&input_state, false)?;
                            }
                        }
                        continue;
                    }

                    // Handle history search menu if active
                    if let Some(ref mut menu) = history_search {
                        use menus::HistorySearchAction;
                        match menu.handle_key(key) {
                            HistorySearchAction::None => {}
                            HistorySearchAction::Redraw => {
                                prompt_box.draw_with_history_search(&input_state, menu)?;
                            }
                            HistorySearchAction::Cancel => {
                                history_search = None;
                                prompt_box.draw(&input_state, false)?;
                            }
                            HistorySearchAction::Select(entry) => {
                                history_search = None;
                                input_state.set_content(&entry);
                                prompt_box.draw(&input_state, false)?;
                            }
                        }
                        continue;
                    }

                    let action = input_state.handle_key(key);

                    // Clear exit prompt if action is not ClearOrExit
                    if !matches!(action, InputAction::ClearOrExit) {
                        exit_prompt = None;
                        prompt_box.set_exit_hint(exit_prompt);
                    }

                    if !matches!(action, InputAction::None) {
                        prompt_box.set_welcome_hint(false);
                    }

                    match action {
                        InputAction::None => {}
                        InputAction::RedrawLine => {
                            // Always do a full redraw to ensure correct cursor positioning
                            // with word wrapping and viewport scrolling
                            if pending_prompts.is_empty() {
                                prompt_box.draw(&input_state, false)?;
                            } else {
                                prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                            }
                        }
                        InputAction::Redraw => {
                            if pending_prompts.is_empty() {
                                prompt_box.draw(&input_state, false)?;
                            } else {
                                prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                            }
                        }
                        InputAction::MoveCursor => {
                            prompt_box.position_cursor(&input_state)?;
                        }
                        InputAction::HistoryUp => {
                            input_state.apply_history_up(prompt_history);
                            if pending_prompts.is_empty() {
                                prompt_box.draw(&input_state, false)?;
                            } else {
                                prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                            }
                        }
                        InputAction::HistoryDown => {
                            input_state.apply_history_down(prompt_history);
                            if pending_prompts.is_empty() {
                                prompt_box.draw(&input_state, false)?;
                            } else {
                                prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                            }
                        }
                        InputAction::ClipboardPaste => {
                            // Try image first, then text
                            if let Ok((bytes, mime)) = clipboard::paste_image() {
                                input_state.add_pasted_image(mime, bytes);
                            } else if let Ok(text) = clipboard::paste_text() {
                                // Normalize newlines and insert
                                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                                input_state.insert_str(&normalized);
                            }
                            if pending_prompts.is_empty() {
                                prompt_box.draw(&input_state, false)?;
                            } else {
                                prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                            }
                        }
                        InputAction::HistorySearch => {
                            if history_search.is_none() {
                                history_search = Some(HistorySearchState::new(prompt_history));
                                prompt_box.draw_with_history_search(
                                    &input_state,
                                    history_search.as_ref().unwrap(),
                                )?;
                            }
                        }
                        InputAction::EditInEditor => {
                            if chatting {
                                continue;
                            }

                            let initial = input_state.content();

                            let _ = crossterm_terminal::disable_raw_mode();
                            let _ = prompt_box.hide_and_clear();

                            let edited = editor::edit_text_in_external_editor(&initial);

                            let _ = crossterm_terminal::enable_raw_mode();

                            match edited {
                                Ok(Some(text)) => {
                                    input_state.set_content(text.trim_end_matches('\n'));
                                    input_state.prune_unused_images();
                                    prompt_box.draw(&input_state, true)?;
                                }
                                Ok(None) => {
                                    prompt_box.draw(&input_state, true)?;
                                }
                                Err(e) => {
                                    terminal::println_above(
                                        &format!("Failed to open editor: {}", e).red().to_string(),
                                    );
                                    prompt_box.draw(&input_state, true)?;
                                }
                            }
                        }
                        InputAction::OpenModelMenu => {
                            // Open model menu - works during streaming too
                            if model_menu.is_none() {
                                let current_model = if let Some(ref task) = chat_task {
                                    format!(
                                        "{}/{}",
                                        task.custom_provider
                                            .as_deref()
                                            .unwrap_or_else(|| task.provider.id()),
                                        task.model_id
                                    )
                                } else if let Some(ref pm) = provider_manager {
                                    format!(
                                        "{}/{}",
                                        pm.current_custom_provider()
                                            .unwrap_or_else(|| pm.current_provider().id()),
                                        pm.current_model_id()
                                    )
                                } else {
                                    String::new()
                                };
                                model_menu =
                                    Some(ModelMenuState::with_current_model(current_model));
                                prompt_box.draw_with_model_menu(
                                    &input_state,
                                    model_menu.as_ref().unwrap(),
                                )?;
                            }
                        }
                        InputAction::OpenSettingsMenu => {
                            if settings_menu.is_none() {
                                settings_menu = Some(SettingsMenuState::new());
                                prompt_box.draw_with_settings_menu(
                                    &input_state,
                                    settings_menu.as_ref().unwrap(),
                                )?;
                            }
                        }
                        InputAction::OpenToolsMenu => {
                            if tools_menu.is_none() {
                                tools_menu = Some(ToolsMenuState::new(services.is_read_only()));
                                prompt_box.draw_with_tools_menu(
                                    &input_state,
                                    tools_menu.as_ref().unwrap(),
                                )?;
                            }
                        }
                        InputAction::OpenMcpMenu => {
                            if mcp_menu.is_none() {
                                let statuses = services.mcp.server_statuses().await;
                                mcp_menu = Some(McpMenuState::new(statuses));
                                prompt_box
                                    .draw_with_mcp_menu(&input_state, mcp_menu.as_ref().unwrap())?;
                            }
                        }
                        InputAction::OpenLspMenu => {
                            // /lsp behaves like /settings: show status immediately, even during streaming.
                            let config = crate::config::ConfigFile::load().unwrap_or_default();
                            if !config.lsp_enabled {
                                let msg = "LSP integration is disabled. Enable it in /settings."
                                    .to_string();
                                terminal::println_above(&msg);
                                history::push(history::HistoryEvent::Info(msg));
                            } else {
                                let servers = crate::lsp::manager().server_info().await;
                                if servers.is_empty() {
                                    let msg = "No LSP servers connected.".to_string();
                                    terminal::println_above(&msg);
                                    history::push(history::HistoryEvent::Info(msg));
                                } else {
                                    let msg = format!("LSP servers connected: {}", servers.len());
                                    terminal::println_above(&msg);
                                    history::push(history::HistoryEvent::Info(msg));
                                    for server in servers {
                                        let extensions = if server.file_extensions.is_empty() {
                                            String::new()
                                        } else {
                                            format!(" ({})", server.file_extensions.join(", "))
                                        };
                                        let msg = format!("  • {}{}", server.name, extensions);
                                        terminal::println_above(&msg);
                                        history::push(history::HistoryEvent::Info(msg));
                                    }
                                }
                            }
                            refresh_prompt_status(
                                &mut prompt_box,
                                &provider_manager,
                                &chat_task,
                                working_dir,
                                thinking_state,
                                services,
                            )
                            .await;
                            prompt_box.draw(&input_state, false)?;
                        }
                        InputAction::TriggerReadOnly => {
                            // Allow toggling while chatting
                            services.set_read_only(true);
                            terminal::println_above("Switched to Read-Only mode.");
                            if let Some(ref pm) = provider_manager {
                                update_prompt_status(
                                    &mut prompt_box,
                                    pm,
                                    working_dir,
                                    thinking_state,
                                    services,
                                )
                                .await;
                            }
                            prompt_box.draw(&input_state, false)?;
                        }
                        InputAction::TriggerReadWrite => {
                            // Allow toggling while chatting
                            services.set_read_only(false);
                            services.set_sandbox_enabled(true);
                            terminal::println_above(
                                "Switched to Read-Write mode (Sandbox enabled).",
                            );
                            if let Some(ref pm) = provider_manager {
                                update_prompt_status(
                                    &mut prompt_box,
                                    pm,
                                    working_dir,
                                    thinking_state,
                                    services,
                                )
                                .await;
                            }
                            prompt_box.draw(&input_state, false)?;
                        }
                        InputAction::TriggerClear => {
                            // Don't allow /clear during streaming
                            if chatting {
                                continue;
                            }
                            // Start a new session id, but keep prior sessions for /sessions restore.
                            messages.clear();
                            *current_session_id = Some(session::generate_session_id());
                            services.set_session_id(current_session_id.clone());
                            crate::usage::reset_last_context_usage();
                            history::clear();
                            clear_todos();
                            terminal::redraw_from_history(prompt_box.height());
                        }
                        InputAction::Submit => {
                            let content = input_state.content();
                            if content.trim().is_empty() {
                                continue;
                            }

                            // Editing a queued prompt: hitting Enter re-queues it.
                            if chatting && editing_pending_prompt.is_some() {
                                // Replace the original with the edited version.
                                editing_pending_prompt = None;
                                pending_prompts.push_back(PendingPrompt {
                                    input: content,
                                    images: input_state.active_images(),
                                });
                                input_state.clear();
                                prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                                continue;
                            }

                            // During chat: handle some commands immediately, queue other prompts
                            if chatting {
                                // Check if this is a /settings command - handle it immediately
                                if content.trim() == "/settings" {
                                    input_state.clear();
                                    settings_menu = Some(SettingsMenuState::new());
                                    prompt_box.draw_with_settings_menu(
                                        &input_state,
                                        settings_menu.as_ref().unwrap(),
                                    )?;
                                    continue;
                                }

                                // Allow transaction logging commands to run immediately while the model is working.
                                if let Some(cmd_input) = content.trim().strip_prefix('/')
                                    && let Some(command) =
                                        crate::commands::parse(cmd_input, custom_commands)
                                    && matches!(
                                        command,
                                        Command::StartTransactionLogging
                                            | Command::StopTransactionLogging
                                    )
                                {
                                    match command {
                                        Command::StartTransactionLogging => {
                                            let path =
                                                crate::provider::transaction_log::start(None);
                                            terminal::println_above(&format!(
                                                "Transaction logging started: {}",
                                                path.display()
                                            ));
                                        }
                                        Command::StopTransactionLogging => {
                                            crate::provider::transaction_log::stop();
                                            terminal::println_above("Transaction logging stopped.");
                                        }
                                        _ => {}
                                    }

                                    input_state.clear();
                                    if pending_prompts.is_empty() {
                                        prompt_box.draw(&input_state, false)?;
                                    } else {
                                        prompt_box
                                            .draw_with_pending(&input_state, &pending_prompts)?;
                                    }
                                    continue;
                                }

                                let pasted_images = input_state.active_images();

                                pending_prompts.push_back(PendingPrompt {
                                    input: content,
                                    images: pasted_images,
                                });

                                input_state.clear();

                                // Redraw with pending prompts visible
                                prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                                continue;
                            }

                            // No model configured: allow /quit (and other non-model commands handled in
                            // process_input) to work; otherwise echo like a normal submit then error.
                            if provider_manager.is_none() && !content.trim_start().starts_with('/')
                            {
                                let images = input_state.active_images();
                                let prompt = input_state.content();

                                input_state.clear();
                                prompt_box.draw(&input_state, false)?;

                                echo_user_prompt_to_output(&prompt, &images);
                                show_no_model_configured();

                                prompt_box.draw(&input_state, true)?;
                                continue;
                            }

                            let content = input_state.content();
                            if !content.trim().is_empty() {
                                let result = process_input(
                                    &content,
                                    &mut messages,
                                    current_session_id,
                                    working_dir,
                                    &mut prompt_box,
                                    &mut input_state,
                                    services,
                                    custom_commands,
                                    &mut provider_manager,
                                    thinking_state,
                                )
                                .await;

                                match result {
                                    ProcessResult::Continue => {
                                        input_state.clear();
                                        if let Some(ref pm) = provider_manager {
                                            update_prompt_status(
                                                &mut prompt_box,
                                                pm,
                                                working_dir,
                                                thinking_state,
                                                services,
                                            )
                                            .await;
                                        }
                                        prompt_box.draw(&input_state, true)?;
                                    }
                                    ProcessResult::Quit => {
                                        break;
                                    }
                                    ProcessResult::StartChat(prompt, history_entry) => {
                                        // Get active images before clearing
                                        let pasted_images = input_state.active_images();

                                        // Save to history before clearing.
                                        // Use history_entry if provided (for custom commands),
                                        // otherwise use the prompt.
                                        let history_text =
                                            history_entry.as_ref().unwrap_or(&prompt);
                                        let _ =
                                            prompt_history.add_with_images(history_text, vec![]);

                                        // Clear input and redraw prompt BEFORE spawning chat task.
                                        // This ensures the prompt box is at the correct position
                                        // when spawn_chat_task echoes the user's input to the
                                        // output area.
                                        input_state.clear();
                                        prompt_box.draw(&input_state, false)?;

                                        // Spawn chat task (takes ownership of provider_manager)
                                        if let Some(pm) = provider_manager.take() {
                                            chat_task = Some(spawn_chat_task(
                                                prompt,
                                                pasted_images,
                                                &mut messages,
                                                pm,
                                                thinking_state,
                                                output,
                                            ));
                                        }
                                    }
                                    ProcessResult::OpenModelMenu => {
                                        input_state.clear();
                                        // Build current model string from available sources
                                        let current_model = if let Some(ref task) = chat_task {
                                            format!(
                                                "{}/{}",
                                                task.custom_provider
                                                    .as_deref()
                                                    .unwrap_or_else(|| task.provider.id()),
                                                task.model_id
                                            )
                                        } else if let Some(ref pm) = provider_manager {
                                            format!(
                                                "{}/{}",
                                                pm.current_custom_provider()
                                                    .unwrap_or_else(|| pm.current_provider().id()),
                                                pm.current_model_id()
                                            )
                                        } else {
                                            String::new()
                                        };

                                        model_menu =
                                            Some(ModelMenuState::with_current_model(current_model));
                                        prompt_box.draw_with_model_menu(
                                            &input_state,
                                            model_menu.as_ref().unwrap(),
                                        )?;
                                    }
                                    ProcessResult::OpenSessionsMenu => {
                                        input_state.clear();
                                        session_menu = Some(SessionMenuState::new(
                                            working_dir,
                                            current_session_id.as_deref(),
                                        ));
                                        prompt_box.draw_with_sessions_menu(
                                            &input_state,
                                            session_menu.as_ref().unwrap(),
                                        )?;
                                    }
                                    ProcessResult::OpenSettings => {
                                        input_state.clear();
                                        settings_menu = Some(SettingsMenuState::new());
                                        prompt_box.draw_with_settings_menu(
                                            &input_state,
                                            settings_menu.as_ref().unwrap(),
                                        )?;
                                    }
                                    ProcessResult::OpenMcpMenu => {
                                        input_state.clear();
                                        let statuses = services.mcp.server_statuses().await;
                                        mcp_menu = Some(McpMenuState::new(statuses));
                                        prompt_box.draw_with_mcp_menu(
                                            &input_state,
                                            mcp_menu.as_ref().unwrap(),
                                        )?;
                                    }
                                    ProcessResult::OpenLspMenu => {
                                        input_state.clear();
                                        let config =
                                            crate::config::ConfigFile::load().unwrap_or_default();
                                        if !config.lsp_enabled {
                                            let msg = "LSP integration is disabled. Enable it in /settings."
                                                .to_string();
                                            terminal::println_above(&msg);
                                            history::push(history::HistoryEvent::Info(msg));
                                        } else {
                                            let servers = crate::lsp::manager().server_info().await;
                                            if servers.is_empty() {
                                                let msg = "No LSP servers connected.".to_string();
                                                terminal::println_above(&msg);
                                                history::push(history::HistoryEvent::Info(msg));
                                            } else {
                                                let msg = format!(
                                                    "LSP servers connected: {}",
                                                    servers.len()
                                                );
                                                terminal::println_above(&msg);
                                                history::push(history::HistoryEvent::Info(msg));
                                                for server in servers {
                                                    let extensions =
                                                        if server.file_extensions.is_empty() {
                                                            String::new()
                                                        } else {
                                                            format!(
                                                                " ({})",
                                                                server.file_extensions.join(", ")
                                                            )
                                                        };
                                                    let msg = format!(
                                                        "  • {}{}",
                                                        server.name, extensions
                                                    );
                                                    terminal::println_above(&msg);
                                                    history::push(history::HistoryEvent::Info(msg));
                                                }
                                            }
                                        }
                                        refresh_prompt_status(
                                            &mut prompt_box,
                                            &provider_manager,
                                            &chat_task,
                                            working_dir,
                                            thinking_state,
                                            services,
                                        )
                                        .await;
                                        prompt_box.draw(&input_state, false)?;
                                    }
                                    ProcessResult::OpenToolsMenu => {
                                        input_state.clear();
                                        tools_menu =
                                            Some(ToolsMenuState::new(services.is_read_only()));
                                        prompt_box.draw_with_tools_menu(
                                            &input_state,
                                            tools_menu.as_ref().unwrap(),
                                        )?;
                                    }
                                    ProcessResult::StartCompaction(data) => {
                                        input_state.clear();
                                        prompt_box.draw(&input_state, false)?;

                                        // Spawn compaction chat task
                                        if let Some(pm) = provider_manager.take() {
                                            chat_task = Some(spawn_compaction_chat(
                                                data,
                                                &mut messages,
                                                pm,
                                                output,
                                            ));
                                        }
                                    }
                                    ProcessResult::RunProviderFlow => {
                                        input_state.clear();

                                        // Run provider flow in hybrid mode:
                                        // - disable raw mode (Henri prompt)
                                        // - disable bracketed paste / keyboard enhancement (inquire doesn't expect them)
                                        // - run inquire prompts
                                        // - restore terminal modes
                                        let _ = crossterm_terminal::disable_raw_mode();
                                        let _ = execute!(
                                            std::io::stdout(),
                                            PopKeyboardEnhancementFlags,
                                            DisableBracketedPaste
                                        );
                                        let _ = prompt_box.hide_and_clear();

                                        // Run the provider management flow
                                        match crate::auth::manage_providers().await {
                                            Ok(crate::auth::ProviderAction::Added)
                                            | Ok(crate::auth::ProviderAction::Removed) => {
                                                // Reinitialize provider manager with new config
                                                if let Ok(new_config) = Config::load(None) {
                                                    let new_pm = ProviderManager::new(
                                                        &new_config,
                                                        services.clone(),
                                                    );
                                                    provider_manager = Some(new_pm);
                                                }
                                            }
                                            Ok(crate::auth::ProviderAction::Cancelled) => {
                                                // User cancelled
                                            }
                                            Err(e) => {
                                                eprintln!("{}", format!("Error: {}", e).red());
                                            }
                                        }

                                        // Restore terminal state without full redraw
                                        println!();
                                        let _ = execute!(
                                            std::io::stdout(),
                                            PushKeyboardEnhancementFlags(
                                                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                                            ),
                                            EnableBracketedPaste
                                        );
                                        let _ = crossterm_terminal::enable_raw_mode();

                                        // Refresh prompt status with potentially new provider
                                        refresh_prompt_status(
                                            &mut prompt_box,
                                            &provider_manager,
                                            &chat_task,
                                            working_dir,
                                            thinking_state,
                                            services,
                                        )
                                        .await;
                                        prompt_box.draw(&input_state, true)?;
                                    }
                                }
                            }
                        }
                        InputAction::CancelAgentLoop => {
                            if chatting && editing_pending_prompt.is_some() {
                                if let Some(pending) = editing_pending_prompt.take() {
                                    pending_prompts.push_back(pending);
                                }
                                input_state.clear();
                                prompt_box.draw_with_pending(&input_state, &pending_prompts)?;
                            }
                            // ESC when not chatting - no-op (agent interruption handled above)
                            // Could add "clear input" behavior here if desired
                        }
                        InputAction::ClearOrExit => {
                            // This triggers when Ctrl+C is pressed and input is empty

                            let now = std::time::Instant::now();
                            if let Some(expires) = exit_prompt
                                && now < expires
                            {
                                break;
                            }

                            exit_prompt = Some(now + Duration::from_secs(2));
                            prompt_box.set_exit_hint(exit_prompt);
                            prompt_box.draw(&input_state, false)?;
                        }
                        InputAction::CycleFavoritesForward
                        | InputAction::CycleFavoritesBackward => {
                            let reverse = matches!(action, InputAction::CycleFavoritesBackward);

                            // Get current model string
                            let current_model = if let Some(pending) = pending_model_change.as_ref()
                                && chatting
                            {
                                pending.short_display()
                            } else if let Some(ref task) = chat_task {
                                format!(
                                    "{}/{}",
                                    task.custom_provider
                                        .as_deref()
                                        .unwrap_or_else(|| task.provider.id()),
                                    task.model_id
                                )
                            } else if let Some(ref pm) = provider_manager {
                                format!(
                                    "{}/{}",
                                    pm.current_custom_provider()
                                        .unwrap_or_else(|| pm.current_provider().id()),
                                    pm.current_model_id()
                                )
                            } else {
                                String::new()
                            };

                            if let Some(next) =
                                crate::providers::cycle_favorite_model(&current_model, reverse)
                            {
                                let display_name = next
                                    .custom_provider
                                    .as_deref()
                                    .unwrap_or_else(|| next.provider.display_name());

                                if chatting {
                                    // Chat is running - queue the model change and update status bar
                                    // Update status bar to show the queued model
                                    let cwd = shorten_path(working_dir);
                                    let thinking = ThinkingStatus {
                                        available: supports_thinking(next.provider, &next.model_id),
                                        enabled: thinking_state.enabled,
                                        mode: thinking_state.mode.clone(),
                                    };
                                    let security = SecurityStatus {
                                        read_only: services.is_read_only(),
                                        sandbox_enabled: services.is_sandbox_enabled(),
                                    };
                                    let lsp_server_count = get_lsp_server_count().await;
                                    let mcp_server_count = get_mcp_server_count(services).await;
                                    prompt_box.set_status(
                                        next.custom_provider
                                            .clone()
                                            .unwrap_or_else(|| next.provider.id().to_string()),
                                        next.model_id.clone(),
                                        cwd,
                                        thinking,
                                        security,
                                        lsp_server_count,
                                        mcp_server_count,
                                    );
                                    pending_model_change = Some(next);
                                } else if let Some(ref mut pm) = provider_manager {
                                    // Chat is not running - apply immediately
                                    let provider_changed = pm.set_model(
                                        next.provider,
                                        next.model_id.clone(),
                                        next.custom_provider.clone(),
                                    );
                                    if provider_changed {
                                        crate::provider::transform_thinking_for_provider_switch(
                                            &mut messages,
                                        );
                                    }
                                    // Update thinking state to new model's default
                                    let new_thinking =
                                        default_thinking_state(next.provider, &next.model_id);
                                    thinking_state.enabled = new_thinking.enabled;
                                    thinking_state.mode = new_thinking.mode;

                                    // Update is_claude for slash menu filtering
                                    input_state
                                        .set_is_claude(next.provider == ModelProvider::Claude);

                                    terminal::println_above(&format!(
                                        "Switched to {} / {}",
                                        display_name, next.model_id
                                    ));

                                    update_prompt_status(
                                        &mut prompt_box,
                                        pm,
                                        working_dir,
                                        thinking_state,
                                        services,
                                    )
                                    .await;
                                } else {
                                    let provider_spec = next
                                        .custom_provider
                                        .clone()
                                        .unwrap_or_else(|| next.provider.id().to_string());
                                    let model_spec = format!("{}/{}", provider_spec, next.model_id);
                                    let config = build_ephemeral_config_for_model(&model_spec);
                                    provider_manager =
                                        Some(ProviderManager::new(&config, services.clone()));

                                    let new_thinking =
                                        default_thinking_state(next.provider, &next.model_id);
                                    thinking_state.enabled = new_thinking.enabled;
                                    thinking_state.mode = new_thinking.mode;

                                    terminal::println_above(&format!(
                                        "Switched to {} / {}",
                                        display_name, next.model_id
                                    ));
                                }
                                prompt_box.draw(&input_state, false)?;
                            } else {
                                terminal::println_above(
                                    &"No favorite models configured. Use ^F in /model menu to add favorites."
                                        .yellow()
                                        .to_string(),
                                );
                                prompt_box.draw(&input_state, false)?;
                            }
                        }
                        InputAction::Quit => {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            tokio::task::yield_now().await;
        }
    }

    // Restore terminal state (skip in batch mode - we never enabled raw mode)
    if !batch {
        execute!(
            std::io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste
        )?;
        crossterm_terminal::disable_raw_mode()?;
        prompt_box.hide_and_exit()?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_chat_outcome(
    outcome: ChatOutcome,
    messages: &mut Vec<Message>,
    provider_manager: &ProviderManager,
    thinking_state: &crate::providers::ThinkingState,
    current_session_id: &mut Option<String>,
    read_only: bool,
    working_dir: &std::path::Path,
    prompt_box: &mut PromptBox,
) -> std::io::Result<()> {
    match outcome {
        ChatOutcome::Complete => {
            match session::save_session(
                working_dir,
                messages,
                &provider_manager.current_provider(),
                provider_manager.current_model_id(),
                thinking_state.enabled,
                read_only,
                current_session_id.as_deref(),
            ) {
                Ok(id) => *current_session_id = Some(id),
                Err(e) => {
                    terminal::println_above(&format!("Warning: Failed to save session: {}", e));
                }
            }
            prompt_box.hide()?;
        }
        ChatOutcome::Interrupted => {
            terminal::println_above("Cancelled");
            remove_pending_tool_turn(messages);
            prompt_box.hide()?;
        }
    }

    Ok(())
}

async fn handle_global_shortcuts(
    key: crossterm::event::KeyEvent,
    thinking_state: &mut crate::providers::ThinkingState,
    provider_manager: &mut ProviderManager,
    services: &Services,
    prompt_box: &mut PromptBox,
    input_state: &InputState,
    working_dir: &std::path::Path,
) -> std::io::Result<bool> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
            let provider = provider_manager.current_provider();
            let model = provider_manager.current_model_id();

            // Only cycle if thinking is available for this model
            if supports_thinking(provider, model) {
                // Check if this provider uses model variants for reasoning levels
                if uses_model_variants(provider, model) {
                    // Cycle to next model variant
                    let new_model = cycle_model_variant(provider, model);
                    let custom_provider = provider_manager
                        .current_custom_provider()
                        .map(|s| s.to_string());
                    provider_manager.set_model(provider, new_model.clone(), custom_provider);

                    // Update thinking state mode to reflect the variant
                    let variant = get_model_variant(&new_model).map(|s| s.to_string());
                    thinking_state.enabled = variant.as_deref() != Some("off");
                    thinking_state.mode = variant;
                } else {
                    let next = cycle_thinking_state(provider, model, thinking_state);
                    thinking_state.enabled = next.enabled;
                    thinking_state.mode = next.mode;
                }

                // Update prompt status to show new thinking state
                update_prompt_status(
                    prompt_box,
                    provider_manager,
                    working_dir,
                    thinking_state,
                    services,
                )
                .await;
            }
            prompt_box.draw(input_state, false)?;
            Ok(true)
        }
        (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
            services.cycle_sandbox_mode();
            update_prompt_status(
                prompt_box,
                provider_manager,
                working_dir,
                thinking_state,
                services,
            )
            .await;
            prompt_box.draw(input_state, false)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Spawn a chat task that runs asynchronously
fn spawn_chat_task(
    prompt: String,
    pasted_images: Vec<PastedImage>,
    messages: &mut Vec<Message>,
    mut provider_manager: ProviderManager,
    thinking_state: &crate::providers::ThinkingState,
    output: &OutputContext,
) -> ChatTask {
    // Reset streaming stats for this new turn
    listener::reset_turn_stats();

    // Ensure the streaming status-line rows are reserved before streaming begins.
    // This prevents late activation from scrolling/clearing over the prompt block.
    terminal::set_streaming_status_line_active(true);
    // Prime the status line immediately so the newly-reserved rows aren't blank.
    listener::spinner_working();

    // Capture provider info before moving provider_manager
    let provider = provider_manager.current_provider();
    let model_id = provider_manager.current_model_id().to_string();
    let custom_provider = provider_manager
        .current_custom_provider()
        .map(|s| s.to_string());

    // Show the submitted input above the prompt with grey background
    // The prompt text contains inline image markers like Image#1, Image#2, etc.
    //
    // Ensure we have a blank line before the user prompt if there was previous output.
    // This keeps the conversation visually separated between turns.
    //
    // Note: `PromptBox::hide()` resets the output cursor state, so we can't rely solely on
    // `terminal::output_has_output()` to know whether there's previous on-screen content.
    if history::has_events() {
        if terminal::output_has_output() {
            terminal::ensure_trailing_newlines(2);
        } else {
            // First output after a prompt hide/show. A single newline here creates exactly one
            // blank row before the prompt line when the prompt is printed next.
            terminal::print_above("\n");
        }
    } else {
        terminal::ensure_line_break();
    }

    let display_prompt = prompt.strip_prefix('!').unwrap_or(&prompt);

    // Use a slightly reduced width to avoid terminals auto-wrapping when the last column
    // is filled. We'll still paint the remainder of the line using `\x1b[K`.
    let term_width = terminal::term_width() as usize;
    let safe_width = term_width.saturating_sub(1).max(1);
    let prompt_prefix = if prompt.trim_start().starts_with('!') {
        input::SHELL_PROMPT
    } else {
        input::PROMPT
    };
    let content_width = safe_width.saturating_sub(2); // prefix is 2 chars
    let mut is_first_row = true;

    // Build all prompt lines first so we can avoid printing a trailing newline on the
    // final line. Leaving the cursor mid-line prevents a visible blank row above the
    // streaming status line while we're waiting for the model.
    let mut prompt_lines: Vec<String> = Vec::new();

    // Add a padding row above the prompt block (same background as prompt).
    //
    // Include a single space so the output cursor is mid-line; this avoids leaving the
    // cursor sitting on an empty trailing row in the reserved status area.
    let prompt_padding_line = format!("{} \x1b[K\x1b[0m", render::BG_GREY_ANSI);
    terminal::print_above(&format!("{}\n", prompt_padding_line));

    for (i, line) in display_prompt.lines().enumerate() {
        // Wrap each logical line to fit terminal width
        let wrapped = render::wrap_text(line, content_width);
        for wrapped_line in &wrapped {
            let prefix = if is_first_row {
                is_first_row = false;
                prompt_prefix
            } else if i == 0 {
                // Continuation of first line (wrapped)
                input::CONTINUATION
            } else {
                // Continuation of subsequent lines
                input::CONTINUATION
            };
            // Colorize image markers
            let styled_line = render::colorize_image_markers(wrapped_line);

            // Full-width grey background without printing trailing spaces.
            //
            // Important: `colorize_image_markers()` may emit ANSI reset codes; re-assert
            // the prompt background before `\x1b[K` so the erase uses the grey background.
            let line = format!(
                "{}{}{}{}\x1b[K\x1b[0m",
                render::BG_GREY_ANSI,
                prefix,
                styled_line,
                render::BG_GREY_ANSI,
            );
            prompt_lines.push(line);
        }
    }

    for (idx, line) in prompt_lines.iter().enumerate() {
        if idx + 1 < prompt_lines.len() {
            terminal::print_above(&format!("{}\n", line));
        } else {
            terminal::print_above(line);
        }
    }

    // Add a padding row below the prompt block (same background as prompt).
    // Do not print a trailing newline so the cursor stays mid-line (see comment above).
    if !prompt_lines.is_empty() {
        terminal::ensure_line_break();
    }
    terminal::print_above(&prompt_padding_line);

    // Tell the CLI listener that the most recent output is now the user prompt.
    // This keeps spacing decisions between streaming blocks correct.
    listener::CliListener::note_user_prompt_printed();

    // Add to history for resize redraw
    let image_metas: Vec<history::ImageMeta> = pasted_images
        .iter()
        .map(|img| history::ImageMeta {
            _marker: img.marker.clone(),
            _mime_type: img.mime_type.clone(),
            _size_bytes: img.data.len(),
        })
        .collect();
    history::push_user_prompt(&prompt, image_metas);

    // Create message with text and images
    let message = if pasted_images.is_empty() {
        Message::user(&prompt)
    } else {
        let mut blocks = Vec::new();

        if !prompt.trim().is_empty() {
            blocks.push(ContentBlock::Text {
                text: prompt.clone(),
            });
        }

        for image in pasted_images {
            blocks.push(ContentBlock::Image {
                mime_type: image.mime_type,
                data: image.data,
            });
        }

        Message {
            role: Role::User,
            content: MessageContent::Blocks(blocks),
        }
    };

    messages.push(message);

    // Set up thinking state
    provider_manager.set_thinking_enabled(thinking_state.enabled);
    provider_manager.set_thinking_mode(thinking_state.mode.clone());

    // Create interrupt flag and result channel
    let interrupted = Arc::new(AtomicBool::new(false));
    let (result_tx, result_rx) = oneshot::channel();

    // Take messages for the async task
    let mut task_messages = std::mem::take(messages);
    let pre_prompt_len = task_messages.len().saturating_sub(1);
    let task_interrupted = Arc::clone(&interrupted);
    let task_output = output.clone();

    tokio::spawn(async move {
        let initial_len = task_messages.len();

        let result = AssertUnwindSafe(provider_manager.chat(
            &mut task_messages,
            &task_interrupted,
            &task_output,
        ))
        .catch_unwind()
        .await;

        let status = match result {
            Ok(Ok(())) => ChatTaskStatus::Ok,
            Ok(Err(crate::error::Error::Interrupted)) => ChatTaskStatus::Interrupted,
            Ok(Err(e)) => ChatTaskStatus::Error(e.display_message()),
            Err(panic) => {
                let msg = panic_payload_to_string(panic);
                output::emit_error(
                    &task_output,
                    &format!(
                        "Internal error: chat task panicked ({}) — try again, and consider running with RUST_BACKTRACE=1.",
                        msg
                    ),
                );
                ChatTaskStatus::Panic(msg)
            }
        };

        let can_retry_prompt =
            matches!(status, ChatTaskStatus::Error(_) | ChatTaskStatus::Panic(_))
                && task_messages.len() == initial_len;

        if can_retry_prompt {
            task_messages.truncate(pre_prompt_len);
        }

        let _ = result_tx.send(ChatTaskResult {
            provider_manager,
            messages: task_messages,
            status,
            can_retry_prompt,
        });
    });

    ChatTask {
        result_rx,
        interrupted,
        provider,
        model_id,
        custom_provider,
        compaction: None,
    }
}

/// Spawn a compaction chat task that summarizes old messages
fn spawn_compaction_chat(
    data: CompactionData,
    messages: &mut Vec<Message>,
    mut provider_manager: ProviderManager,
    output: &OutputContext,
) -> ChatTask {
    use crate::compaction;

    // Reset streaming stats for this turn
    listener::reset_turn_stats();

    // Capture provider info
    let provider = provider_manager.current_provider();
    let model_id = provider_manager.current_model_id().to_string();
    let custom_provider = provider_manager
        .current_custom_provider()
        .map(|s| s.to_string());

    // Show summarization request in history
    history::push_user_prompt(&data.request_text, vec![]);

    // Set up compaction messages: system prompt + user request
    let compaction_messages = vec![
        Message::system(compaction::summarization_system_prompt()),
        Message::user(&data.request_text),
    ];

    // Store original messages for rollback, but set up the compaction messages for the task
    *messages = compaction_messages;

    // Disable thinking for compaction (simpler, faster)
    provider_manager.set_thinking_enabled(false);

    // Create interrupt flag and result channel
    let interrupted = Arc::new(AtomicBool::new(false));
    let (result_tx, result_rx) = oneshot::channel();

    // Take messages for the async task
    let mut task_messages = std::mem::take(messages);
    let task_interrupted = Arc::clone(&interrupted);
    let task_output = output.clone();

    tokio::spawn(async move {
        let result = AssertUnwindSafe(provider_manager.chat(
            &mut task_messages,
            &task_interrupted,
            &task_output,
        ))
        .catch_unwind()
        .await;

        let status = match result {
            Ok(Ok(())) => ChatTaskStatus::Ok,
            Ok(Err(crate::error::Error::Interrupted)) => ChatTaskStatus::Interrupted,
            Ok(Err(e)) => ChatTaskStatus::Error(e.display_message()),
            Err(panic) => {
                let msg = panic_payload_to_string(panic);
                output::emit_error(
                    &task_output,
                    &format!(
                        "Internal error: compaction task panicked ({}) — try again, and consider running with RUST_BACKTRACE=1.",
                        msg
                    ),
                );
                ChatTaskStatus::Panic(msg)
            }
        };

        let _ = result_tx.send(ChatTaskResult {
            provider_manager,
            messages: task_messages,
            status,
            can_retry_prompt: false,
        });
    });

    ChatTask {
        result_rx,
        interrupted,
        provider,
        model_id,
        custom_provider,
        compaction: Some(CompactionState {
            preserved: data.preserved,
            messages_compacted: data.messages_compacted,
            original: data.original,
        }),
    }
}

/// Finalize compaction: extract summary from response and rebuild messages
fn finalize_compaction(chat_messages: Vec<Message>, state: CompactionState) -> Vec<Message> {
    // Extract summary from the last assistant message
    let summary = chat_messages
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

    // Build new messages: summary block + preserved messages
    let summary_message = Message {
        role: Role::User,
        content: MessageContent::Blocks(vec![ContentBlock::Summary {
            summary,
            messages_compacted: state.messages_compacted,
        }]),
    };

    let mut new_messages = vec![summary_message];
    new_messages.extend(state.preserved);
    new_messages
}

enum ProcessResult {
    /// Continue to next input
    Continue,
    /// Quit the application
    Quit,
    /// Start a chat with the model.
    /// Contains (prompt_to_send, history_entry).
    /// If history_entry is None, use prompt_to_send for history.
    StartChat(String, Option<String>),
    /// Open the model selection menu
    OpenModelMenu,
    /// Open the session selection menu
    OpenSessionsMenu,
    /// Open the MCP server menu
    OpenMcpMenu,
    /// Open the LSP menu
    OpenLspMenu,
    /// Open the settings panel
    OpenSettings,
    /// Open the tools menu
    OpenToolsMenu,
    /// Run the provider management flow (add/remove)
    RunProviderFlow,
    /// Start compaction
    StartCompaction(CompactionData),
}

/// Process user input and return what to do next
#[allow(clippy::too_many_arguments)]
async fn process_input(
    input: &str,
    messages: &mut Vec<Message>,
    current_session_id: &mut Option<String>,
    working_dir: &std::path::Path,
    prompt_box: &mut PromptBox,
    input_state: &mut InputState,
    services: &Services,
    custom_commands: &[CustomCommand],
    provider_manager: &mut Option<ProviderManager>,
    _thinking_state: &mut crate::providers::ThinkingState,
) -> ProcessResult {
    let input = input.trim();

    // Handle shell commands starting with '!'
    if input.starts_with('!') {
        // Disable raw mode for shell command
        let _ = crossterm_terminal::disable_raw_mode();
        let _ = prompt_box.hide_and_clear();

        let cmd = input.strip_prefix('!').unwrap_or("");
        if !cmd.is_empty() {
            history::push_user_prompt(&format!("!{}", cmd), vec![]);
            let output = std::process::Command::new("sh").arg("-c").arg(cmd).output();
            match output {
                Ok(result) => {
                    let stdout = String::from_utf8_lossy(&result.stdout);
                    let stderr = String::from_utf8_lossy(&result.stderr);
                    let mut combined = String::new();
                    if !stdout.is_empty() {
                        combined.push_str(&stdout);
                    }
                    if !stderr.is_empty() {
                        if !combined.is_empty() && !combined.ends_with('\n') {
                            combined.push('\n');
                        }
                        combined.push_str(&stderr);
                    }
                    if !combined.is_empty() {
                        let mut cleaned = combined.clone();
                        if cleaned.ends_with('\n') {
                            cleaned.pop();
                        }
                        history::push(history::HistoryEvent::Info(cleaned));
                        print!("{}", combined);
                    }
                    if !result.status.success()
                        && let Some(code) = result.status.code()
                    {
                        terminal::println_above(
                            &format!("[Command exited with status {}]", code)
                                .bright_black()
                                .to_string(),
                        );
                    }
                }
                Err(e) => {
                    terminal::println_above(
                        &format!("Failed to execute command: {}", e)
                            .red()
                            .to_string(),
                    );
                    history::push(history::HistoryEvent::Error(format!(
                        "Failed to execute command: {}",
                        e
                    )));
                }
            }
        }

        // Re-enable raw mode
        let _ = crossterm_terminal::enable_raw_mode();
        return ProcessResult::Continue;
    }

    // Handle slash commands
    if input.starts_with('/') {
        let cmd_input = input.strip_prefix('/').unwrap_or("");
        if let Some(command) = crate::commands::parse(cmd_input, custom_commands) {
            // Track if this is a custom command for history
            let is_custom = matches!(command, Command::Custom { .. });

            if provider_manager.is_none()
                && !matches!(
                    command,
                    Command::Help
                        | Command::Echo { .. }
                        | Command::Quit
                        | Command::ReadOnly
                        | Command::ReadWrite
                        | Command::Yolo
                        | Command::Model
                        | Command::Provider
                        | Command::Sessions
                        | Command::Settings
                        | Command::Mcp
                        | Command::Lsp
                        | Command::Tools
                )
            {
                show_no_model_configured();
                return ProcessResult::Continue;
            }

            let result = match handle_command(
                command,
                messages,
                current_session_id,
                working_dir,
                prompt_box,
                input_state,
                services,
                custom_commands,
                provider_manager,
            )
            .await
            {
                Some(r) => r,
                None => return ProcessResult::Continue,
            };

            match result {
                CommandResult::Continue => return ProcessResult::Continue,
                CommandResult::Quit => return ProcessResult::Quit,
                CommandResult::SendToModel(prompt) => {
                    // For custom commands, save original command to history instead of expanded prompt
                    let history_entry = if is_custom {
                        Some(input.to_string())
                    } else {
                        None
                    };
                    return ProcessResult::StartChat(prompt, history_entry);
                }
                CommandResult::OpenModelMenu => return ProcessResult::OpenModelMenu,
                CommandResult::OpenSessionsMenu => return ProcessResult::OpenSessionsMenu,
                CommandResult::OpenMcpMenu => return ProcessResult::OpenMcpMenu,
                CommandResult::OpenLspMenu => return ProcessResult::OpenLspMenu,
                CommandResult::OpenSettings => return ProcessResult::OpenSettings,
                CommandResult::OpenToolsMenu => return ProcessResult::OpenToolsMenu,
                CommandResult::RunProviderFlow => return ProcessResult::RunProviderFlow,
                CommandResult::StartCompaction(data) => {
                    return ProcessResult::StartCompaction(data);
                }
            }
        } else {
            // Unknown command
            let cmd_name = cmd_input.split_whitespace().next().unwrap_or(cmd_input);
            terminal::println_above(&format!("Unknown command: /{}", cmd_name).red().to_string());
            return ProcessResult::Continue;
        }
    }

    // Regular user message - spawn_chat_task will display the prompt
    ProcessResult::StartChat(input.to_string(), None)
}

/// Result of handling a slash command.
enum CommandResult {
    /// Command handled, continue to next input
    Continue,
    /// Quit the application
    Quit,
    /// Send the given prompt to the model
    SendToModel(String),
    /// Open the model selection menu
    OpenModelMenu,
    /// Open the session selection menu
    OpenSessionsMenu,
    /// Open the MCP server menu
    OpenMcpMenu,
    /// Open the LSP menu
    OpenLspMenu,
    /// Open the settings panel
    OpenSettings,
    /// Open the tools menu
    OpenToolsMenu,
    /// Run the provider management flow (add/remove)
    RunProviderFlow,
    /// Start compaction with the given data
    StartCompaction(CompactionData),
}

/// Data needed to perform compaction
struct CompactionData {
    /// Messages to preserve (not compacted)
    preserved: Vec<Message>,
    /// Number of messages being compacted
    messages_compacted: usize,
    /// Original messages (for rollback)
    original: Vec<Message>,
    /// Summarization request text
    request_text: String,
}

/// Handle a parsed slash command.
#[allow(clippy::too_many_arguments)]
async fn handle_command(
    command: Command,
    messages: &mut Vec<Message>,
    current_session_id: &mut Option<String>,
    _working_dir: &std::path::Path,
    prompt_box: &mut PromptBox,
    input_state: &mut InputState,
    services: &Services,
    custom_commands: &[CustomCommand],
    provider_manager: &mut Option<ProviderManager>,
) -> Option<CommandResult> {
    Some(match command {
        Command::Quit => CommandResult::Quit,

        Command::Clear => {
            // Start a new session by dropping the current session id, but keep the
            // previous session on disk so it can be restored via /sessions.
            messages.clear();
            *current_session_id = Some(session::generate_session_id());
            services.set_session_id(current_session_id.clone());
            crate::usage::reset_last_context_usage();
            history::clear();
            clear_todos();
            terminal::redraw_from_history(prompt_box.height());
            CommandResult::Continue
        }

        Command::Echo { text } => {
            terminal::println_above(&text);
            CommandResult::Continue
        }

        Command::Help => {
            show_help(custom_commands);
            CommandResult::Continue
        }

        Command::Lsp => {
            // Keep /lsp as a menu-style action so it can run while streaming.
            CommandResult::OpenLspMenu
        }

        Command::ReadOnly => {
            services.set_read_only(true);
            terminal::println_above("Switched to Read-Only mode.");
            CommandResult::Continue
        }

        Command::ReadWrite => {
            services.set_read_only(false);
            services.set_sandbox_enabled(true);
            terminal::println_above("Switched to Read-Write mode (Sandbox enabled).");
            CommandResult::Continue
        }

        Command::Yolo => {
            services.set_read_only(false);
            services.set_sandbox_enabled(false);
            terminal::println_above("Switched to YOLO mode (Sandbox disabled).");
            CommandResult::Continue
        }

        Command::Undo => {
            if crate::provider::remove_last_turn(messages) > 0 {
                terminal::println_above("Removed the most recent turn.");
            } else {
                terminal::println_above("No turns to undo.");
            }
            CommandResult::Continue
        }

        Command::Forget => {
            if crate::provider::remove_first_turn(messages) > 0 {
                terminal::println_above("Removed the oldest turn.");
            } else {
                terminal::println_above("No turns to forget.");
            }
            CommandResult::Continue
        }

        Command::Truncate => {
            if messages.len() > 1 {
                let last = messages.pop();
                messages.clear();
                if let Some(msg) = last {
                    messages.push(msg);
                }
                // Clear display history and repopulate with the last message
                history::clear();
                clear_todos();
                if let Some(msg) = messages.last() {
                    history::push_message(msg);
                }
                prompt_box.redraw_history().ok();
            } else {
                terminal::println_above("Nothing to truncate.");
            }
            CommandResult::Continue
        }

        Command::Custom { name, args } => {
            if let Some(custom) = custom_commands.iter().find(|c| c.name == name) {
                let prompt = custom_commands::substitute_variables(&custom.prompt, &args);
                CommandResult::SendToModel(prompt)
            } else {
                terminal::println_above(
                    &format!("Custom command not found: {}", name)
                        .red()
                        .to_string(),
                );
                CommandResult::Continue
            }
        }

        Command::Model => {
            // Return to event loop to open the model menu
            CommandResult::OpenModelMenu
        }

        Command::Provider => {
            // Run the provider management flow
            CommandResult::RunProviderFlow
        }

        Command::Tools => {
            // Open tools menu for interactive toggling
            CommandResult::OpenToolsMenu
        }

        Command::Mcp => {
            // Open MCP servers menu for interactive toggling
            CommandResult::OpenMcpMenu
        }

        Command::Sessions => {
            // Open sessions menu for interactive selection
            CommandResult::OpenSessionsMenu
        }

        Command::BuildAgentsMd => {
            // Send the build-agents-md prompt to the model
            let prompt = crate::prompts::BUILD_AGENTS_MD_PROMPT.to_string();
            CommandResult::SendToModel(prompt)
        }

        Command::StartTransactionLogging => {
            let path = crate::provider::transaction_log::start(None);
            terminal::println_above(&format!("Transaction logging started: {}", path.display()));
            CommandResult::Continue
        }

        Command::StopTransactionLogging => {
            crate::provider::transaction_log::stop();
            terminal::println_above("Transaction logging stopped.");
            CommandResult::Continue
        }

        Command::DumpPrompt => {
            let Some(provider_manager) = provider_manager.as_mut() else {
                show_no_model_configured();
                return None;
            };

            // Dump the full API request as JSON
            terminal::println_above("Preparing request...");
            let msgs = messages.clone();
            match provider_manager.prepare_request(msgs).await {
                Ok(json) => {
                    let pretty =
                        serde_json::to_string_pretty(&json).unwrap_or_else(|_| json.to_string());
                    for line in pretty.lines() {
                        terminal::println_above(line);
                    }
                }
                Err(e) => {
                    terminal::println_above(&format!("Error: {}", e).red().to_string());
                }
            }
            CommandResult::Continue
        }

        Command::ClaudeCountTokens => {
            let Some(provider_manager) = provider_manager.as_mut() else {
                show_no_model_configured();
                return None;
            };

            let is_claude = matches!(
                provider_manager.current_provider(),
                crate::providers::ModelProvider::Claude
            );

            if is_claude {
                terminal::println_above("Counting tokens...");
                match provider_manager.count_tokens(messages).await {
                    Ok(json) => {
                        let pretty = serde_json::to_string_pretty(&json)
                            .unwrap_or_else(|_| json.to_string());
                        for line in pretty.lines() {
                            terminal::println_above(line);
                        }
                    }
                    Err(e) => {
                        terminal::println_above(&format!("Error: {}", e).red().to_string());
                    }
                }
            } else {
                terminal::println_above(
                    &"/claude-count-tokens is only available with Claude provider."
                        .red()
                        .to_string(),
                );
            }
            CommandResult::Continue
        }

        Command::Usage => {
            let has_claude_oauth = crate::commands::has_claude_oauth_provider();

            if has_claude_oauth {
                // Clear the prompt before the async fetch
                input_state.clear();
                prompt_box.draw(input_state, true).ok();

                terminal::println_above("Fetching rate limits...");
                match crate::usage::fetch_anthropic_rate_limits().await {
                    Ok(limits) => {
                        for line in limits.format_lines() {
                            terminal::println_above(&line);
                        }
                    }
                    Err(e) => {
                        terminal::println_above(&format!("Error: {}", e).red().to_string());
                    }
                }
            } else {
                terminal::println_above(
                    &"/claude-usage requires a Claude provider with OAuth."
                        .red()
                        .to_string(),
                );
            }
            CommandResult::Continue
        }

        Command::Compact => {
            use crate::compaction;

            if messages.is_empty() {
                terminal::println_above(&"No messages to compact.".yellow().to_string());
                return None;
            }

            // Segment messages (compact everything, preserve nothing)
            let (to_compact, to_preserve) = compaction::segment_messages(messages, 0);

            if to_compact.is_empty() {
                terminal::println_above(&"No messages to compact.".yellow().to_string());
                return None;
            }

            let messages_compacted = to_compact.len();
            terminal::ensure_line_break();
            terminal::println_above("");
            // Keep one blank line between the status message and the model output.
            terminal::println_above(
                &format!("[Compacting {} messages...]", messages_compacted)
                    .cyan()
                    .to_string(),
            );

            // Ensure model output starts after a blank line.
            crate::cli::listener::CliListener::note_user_prompt_printed();

            // Build the summarization request
            let request_text = compaction::build_summarization_request_text(&to_compact);

            CommandResult::StartCompaction(CompactionData {
                preserved: to_preserve,
                messages_compacted,
                original: messages.clone(),
                request_text,
            })
        }

        Command::Settings => {
            // Open the settings panel on the alternate screen
            CommandResult::OpenSettings
        }

        // Internal: redraw is handled separately
        #[allow(unreachable_patterns)]
        _ => {
            prompt_box.redraw_history().ok();
            CommandResult::Continue
        }
    })
}

/// Show help with available commands.
fn show_help(_custom_commands: &[CustomCommand]) {
    let has_claude_oauth = crate::commands::has_claude_oauth_provider();

    terminal::println_above(&"Available commands:".cyan().bold().to_string());

    for cmd in crate::commands::COMMANDS {
        // Skip Claude OAuth commands if not configured
        if cmd.availability == crate::commands::Availability::ClaudeOAuthConfigured
            && !has_claude_oauth
        {
            continue;
        }
        let cmd_name = format!("/{:<20}", cmd.name);
        terminal::println_above(&format!("  {} {}", cmd_name.green(), cmd.description));
    }

    terminal::println_above(&"Shell commands:".cyan().bold().to_string());
    let shell_cmd = format!("{:<21}", "!<cmd>");
    terminal::println_above(&format!(
        "  {} Run a shell command (e.g., !ls -la)",
        shell_cmd.green()
    ));

    terminal::println_above(&"Keyboard shortcuts:".cyan().bold().to_string());
    let shortcut = format!("{:<21}", "Ctrl+M");
    terminal::println_above(&format!("  {} Switch model", shortcut.yellow()));
    let shortcut = format!("{:<21}", "Ctrl+T");
    terminal::println_above(&format!("  {} Toggle thinking", shortcut.yellow()));
    let shortcut = format!("{:<21}", "Ctrl+X");
    terminal::println_above(&format!(
        "  {} Cycle security mode (Read-Write -> Read-Only -> YOLO)",
        shortcut.yellow()
    ));
    let shortcut = format!("{:<21}", "Ctrl+Y");
    terminal::println_above(&format!(
        "  {} Cycle favorite models (Shift+Ctrl+Y reverse)",
        shortcut.yellow()
    ));
    let shortcut = format!("{:<21}", "Shift+Up");
    terminal::println_above(&format!(
        "  {} Edit most recent queued message (during response)",
        shortcut.yellow()
    ));
    let shortcut = format!("{:<21}", "Shift+Delete");
    terminal::println_above(&format!(
        "  {} Delete most recent queued message (during response)",
        shortcut.yellow()
    ));
    let shortcut = format!("{:<21}", "Ctrl+R");
    terminal::println_above(&format!("  {} Search history", shortcut.yellow()));
    let shortcut = format!("{:<21}", "Ctrl+G");
    terminal::println_above(&format!(
        "  {} Edit prompt in $VISUAL/$EDITOR",
        shortcut.yellow()
    ));
}

#[cfg(test)]
mod tests {
    use super::panic_payload_to_string;

    #[test]
    fn test_panic_payload_to_string_static_str() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(panic_payload_to_string(payload), "boom");
    }

    #[test]
    fn test_panic_payload_to_string_string() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("boom".to_string());
        assert_eq!(panic_payload_to_string(payload), "boom");
    }
}
