// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! CLI interface for Henri - the traditional REPL-style interface.

mod input;
pub(crate) mod listener;

pub(crate) use input::{PastedImage, PromptInfo, PromptOutcome, PromptUi};

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use colored::Colorize;
use inquire::Select;

use crate::commands::{self, Command, ExitStatus, ModeTransferSession};
use crate::config::{self, Config, ConfigFile, DefaultModel, UiDefault};
use crate::custom_commands;
use crate::error;
use crate::history::FileHistory;
use crate::output;
use crate::provider::zen::ZenProvider;
use crate::provider::{ContentBlock, Message, MessageContent, Role};
use crate::providers::{ModelProvider, ProviderManager, ThinkingState, build_model_choices};
use crate::session;
use crate::tools::todo::clear_todos;
use crate::usage;

enum CommandResult {
    Quit,
    SwitchToTui,
    Continue,
    ClearHistory,
    SelectModel,
    Status,
    Usage,
    DumpPrompt,
    StartTransactionLogging,
    StopTransactionLogging,
    Compact,
    CountTokens,
    CustomPrompt(String),
    Sessions,
    Settings,
    Mcp,
    Tools,
    SandboxToggle,
}

fn handle_command(
    input: &str,
    current_provider: ModelProvider,
    custom_commands: &[custom_commands::CustomCommand],
    output: &output::OutputContext,
) -> CommandResult {
    let input = input.trim();

    // Handle /exit as an alias for /quit
    if input == "/exit" {
        return CommandResult::Quit;
    }

    let Some(cmd_str) = input.strip_prefix('/') else {
        output.emit(output::OutputEvent::Error(format!(
            "Unknown command: {}",
            input
        )));
        output.emit(output::OutputEvent::Info(
            "Type /help for available commands.".into(),
        ));
        return CommandResult::Continue;
    };

    let Some(cmd) = commands::parse(cmd_str, custom_commands) else {
        output.emit(output::OutputEvent::Error(format!(
            "Unknown command: {}",
            input
        )));
        output.emit(output::OutputEvent::Info(
            "Type /help for available commands.".into(),
        ));
        return CommandResult::Continue;
    };

    match cmd {
        Command::BuildAgentsMd => {
            CommandResult::CustomPrompt(crate::prompts::BUILD_AGENTS_MD_PROMPT.to_string())
        }
        Command::Quit => CommandResult::Quit,
        Command::Tui => CommandResult::SwitchToTui,
        Command::Cli => {
            output.emit(output::OutputEvent::Error("Already in CLI mode.".into()));
            CommandResult::Continue
        }
        Command::Help => {
            output.emit(output::OutputEvent::Info(
                "Available commands:".cyan().bold().to_string(),
            ));
            for slash_cmd in commands::COMMANDS {
                match slash_cmd.availability {
                    commands::Availability::TuiOnly => continue,
                    commands::Availability::ClaudeOnly
                        if !matches!(current_provider, ModelProvider::Claude) =>
                    {
                        continue;
                    }
                    _ => {}
                }
                let cmd_name = format!("/{:<32}", slash_cmd.name);
                output.emit(output::OutputEvent::Info(format!(
                    "  {} - {}",
                    cmd_name.green(),
                    slash_cmd.description
                )));
            }
            if !custom_commands.is_empty() {
                output.emit(output::OutputEvent::Info(
                    "\nCustom commands:".cyan().bold().to_string(),
                ));
                for custom_cmd in custom_commands {
                    let cmd_name = format!("/{:<32}", custom_cmd.name);
                    output.emit(output::OutputEvent::Info(format!(
                        "  {} - {}",
                        cmd_name.green(),
                        custom_cmd.description
                    )));
                }
            }
            output.emit(output::OutputEvent::Info(
                "\nShell commands:".cyan().bold().to_string(),
            ));
            let shell_cmd = format!("{:<33}", "!<cmd>");
            output.emit(output::OutputEvent::Info(format!(
                "  {} - Run a shell command (e.g., !ls -la)",
                shell_cmd.green()
            )));
            output.emit(output::OutputEvent::Info(
                "\nKeyboard shortcuts:".cyan().bold().to_string(),
            ));
            let shortcut = format!("{:<33}", "Ctrl+M");
            output.emit(output::OutputEvent::Info(format!(
                "  {} - Switch model",
                shortcut.yellow()
            )));
            let shortcut = format!("{:<33}", "Ctrl+T");
            output.emit(output::OutputEvent::Info(format!(
                "  {} - Toggle thinking",
                shortcut.yellow()
            )));
            let shortcut = format!("{:<33}", "Shift+Tab");
            output.emit(output::OutputEvent::Info(format!(
                "  {} - Cycle favorite models",
                shortcut.yellow()
            )));
            let shortcut = format!("{:<33}", "Ctrl+R");
            output.emit(output::OutputEvent::Info(format!(
                "  {} - Search history",
                shortcut.yellow()
            )));
            output.emit(output::OutputEvent::Info(
                "\nType any other text to chat with the AI.".into(),
            ));
            CommandResult::Continue
        }
        Command::Clear => {
            output.emit(output::OutputEvent::Info(
                "Conversation history cleared.".into(),
            ));
            CommandResult::ClearHistory
        }
        Command::StartTransactionLogging => CommandResult::StartTransactionLogging,
        Command::StopTransactionLogging => CommandResult::StopTransactionLogging,
        Command::Model => CommandResult::SelectModel,
        Command::Status => CommandResult::Status,
        Command::DumpPrompt => CommandResult::DumpPrompt,
        Command::Compact => CommandResult::Compact,
        Command::Usage => {
            if crate::commands::has_claude_oauth_provider() {
                CommandResult::Usage
            } else {
                output.emit(output::OutputEvent::Error(
                    "/claude-usage requires a Claude provider with OAuth authentication configured."
                        .into(),
                ));
                CommandResult::Continue
            }
        }
        Command::ClaudeCountTokens => {
            if matches!(current_provider, ModelProvider::Claude) {
                CommandResult::CountTokens
            } else {
                output.emit(output::OutputEvent::Error(
                    "/claude-count-tokens is only available when using Claude (Anthropic) provider."
                        .into(),
                ));
                CommandResult::Continue
            }
        }
        // TUI-only commands - should not be reachable in CLI mode
        Command::DumpConversation => {
            output.emit(output::OutputEvent::Error(
                "This command is only available in TUI mode.".into(),
            ));
            CommandResult::Continue
        }
        Command::Sessions => CommandResult::Sessions,
        Command::Settings => CommandResult::Settings,
        Command::Sandbox => CommandResult::SandboxToggle,
        Command::Mcp => CommandResult::Mcp,
        Command::Tools => CommandResult::Tools,
        Command::Custom { name, args } => {
            if let Some(custom) = custom_commands.iter().find(|c| c.name == name) {
                let prompt = custom_commands::substitute_variables(&custom.prompt, &args);
                CommandResult::CustomPrompt(prompt)
            } else {
                output.emit(output::OutputEvent::Error(format!(
                    "Custom command '{}' not found",
                    name
                )));
                CommandResult::Continue
            }
        }
    }
}

/// Interactive model selection using inquire.
/// Returns `true` if the provider changed (requiring thinking block cleanup).
fn select_model(provider_manager: &mut ProviderManager) -> bool {
    let choices = build_model_choices();

    if choices.is_empty() {
        println!("No models available.");
        return false;
    }

    // Find current selection index
    let current_model_id = provider_manager.current_model_id();
    let current_provider = provider_manager.current_provider();
    let start_idx = choices
        .iter()
        .position(|m| m.model_id == current_model_id && m.provider == current_provider)
        .unwrap_or(0);

    match Select::new("Select a model:", choices)
        .with_starting_cursor(start_idx)
        .with_page_size(output::menu_page_size())
        .prompt()
    {
        Ok(choice) => {
            let model_str = choice.short_display();
            let provider_changed = provider_manager.set_model(
                choice.provider,
                choice.model_id.clone(),
                choice.custom_provider.clone(),
            );

            // Save selection to config
            let _ = Config::save_state_model(&model_str);

            println!(
                "Model set to: {} ({})",
                model_str,
                choice.provider.display_name()
            );
            provider_changed
        }
        Err(_) => {
            println!("Selection cancelled.");
            false
        }
    }
}

/// Cycle through favorite models (triggered by Shift+Tab)
/// Returns (provider_display, model_id, provider_enum, provider_changed, thinking_mode) if successful
fn cycle_favorite_model(
    provider_manager: &mut ProviderManager,
) -> Option<(String, String, ModelProvider, bool, Option<String>)> {
    let choices = build_model_choices();
    let favorites: Vec<_> = choices.iter().filter(|c| c.is_favorite).collect();

    if favorites.is_empty() {
        return None;
    }

    // Find current model's position in favorites
    let current_model_id = provider_manager.current_model_id();
    let current_provider = provider_manager.current_provider();
    let current_idx = favorites
        .iter()
        .position(|c| c.provider == current_provider && c.model_id == current_model_id);

    // Cycle to next favorite (or first if not found)
    let next_idx = match current_idx {
        Some(idx) => (idx + 1) % favorites.len(),
        None => 0,
    };

    let next_model = favorites[next_idx];
    let model_str = next_model.short_display();

    // Update provider manager
    let provider_changed = provider_manager.set_model(
        next_model.provider,
        next_model.model_id.clone(),
        next_model.custom_provider.clone(),
    );

    // Save selection to config
    let _ = Config::save_state_model(&model_str);

    // Return new display values
    let provider_display = next_model
        .custom_provider
        .clone()
        .unwrap_or_else(|| next_model.provider.display_name().to_string());
    let mut thinking_mode = None;
    if provider_changed {
        thinking_mode = provider_manager.default_thinking().mode;
    }

    Some((
        provider_display,
        next_model.model_id.clone(),
        next_model.provider,
        provider_changed,
        thinking_mode,
    ))
}

/// Settings menu options
#[derive(Clone)]
enum SettingChoice {
    NetworkStats(bool),
    ShowDiffs(bool),
    DefaultModel(DefaultModel),
    DefaultUi(UiDefault),
    LspEnabled(bool),
}

impl std::fmt::Display for SettingChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingChoice::NetworkStats(enabled) => write!(
                f,
                "Network Stats: {}",
                if *enabled { "Enabled" } else { "Disabled" }
            ),
            SettingChoice::ShowDiffs(enabled) => {
                write!(
                    f,
                    "Show Diffs: {}",
                    if *enabled { "Enabled" } else { "Disabled" }
                )
            }
            SettingChoice::DefaultModel(dm) => match dm {
                DefaultModel::LastUsed => write!(f, "Default Model: :last-used"),
                DefaultModel::Specific(m) => write!(f, "Default Model: {}", m),
            },
            SettingChoice::DefaultUi(ui) => match ui {
                UiDefault::Tui => write!(f, "Default UI: tui"),
                UiDefault::Cli => write!(f, "Default UI: cli"),
            },
            SettingChoice::LspEnabled(enabled) => write!(
                f,
                "LSP Integration: {}",
                if *enabled { "Enabled" } else { "Disabled" }
            ),
        }
    }
}

/// Interactive settings menu using inquire
async fn show_settings_menu(working_dir: &std::path::Path) {
    let config = match ConfigFile::load() {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to load config: {}", e);
            return;
        }
    };

    let choices = vec![
        SettingChoice::NetworkStats(config.show_network_stats),
        SettingChoice::ShowDiffs(config.show_diffs),
        SettingChoice::LspEnabled(config.lsp_enabled),
        SettingChoice::DefaultModel(config.default_model.clone()),
        SettingChoice::DefaultUi(config.ui.default),
    ];

    let selection = match Select::new("Settings:", choices)
        .with_page_size(output::menu_page_size())
        .prompt()
    {
        Ok(choice) => choice,
        Err(_) => {
            println!("Selection cancelled.");
            return;
        }
    };

    match selection {
        SettingChoice::NetworkStats(current) => {
            let toggle_choices = vec!["Enabled", "Disabled"];
            let start_idx = if current { 0 } else { 1 };
            match Select::new("Network Stats:", toggle_choices)
                .with_starting_cursor(start_idx)
                .prompt()
            {
                Ok(choice) => {
                    let enabled = choice == "Enabled";
                    if let Ok(mut cfg) = ConfigFile::load() {
                        cfg.show_network_stats = enabled;
                        if cfg.save().is_ok() {
                            println!(
                                "Network Stats: {}",
                                if enabled { "Enabled" } else { "Disabled" }
                            );
                        }
                    }
                }
                Err(_) => println!("Selection cancelled."),
            }
        }
        SettingChoice::ShowDiffs(current) => {
            let toggle_choices = vec!["Enabled", "Disabled"];
            let start_idx = if current { 0 } else { 1 };
            match Select::new("Show Diffs:", toggle_choices)
                .with_starting_cursor(start_idx)
                .prompt()
            {
                Ok(choice) => {
                    let enabled = choice == "Enabled";
                    if let Ok(mut cfg) = ConfigFile::load() {
                        cfg.show_diffs = enabled;
                        if cfg.save().is_ok() {
                            println!(
                                "Show Diffs: {}",
                                if enabled { "Enabled" } else { "Disabled" }
                            );
                        }
                    }
                }
                Err(_) => println!("Selection cancelled."),
            }
        }
        SettingChoice::DefaultModel(current) => {
            show_default_model_menu(&current);
        }
        SettingChoice::DefaultUi(current) => {
            let choices = vec!["tui", "cli"];
            let start_idx = match current {
                UiDefault::Tui => 0,
                UiDefault::Cli => 1,
            };
            match Select::new("Default UI:", choices)
                .with_starting_cursor(start_idx)
                .prompt()
            {
                Ok(choice) => {
                    let ui = if choice == "tui" {
                        UiDefault::Tui
                    } else {
                        UiDefault::Cli
                    };
                    if let Ok(mut cfg) = ConfigFile::load() {
                        cfg.ui.default = ui;
                        if cfg.save().is_ok() {
                            println!("Default UI: {}", choice);
                        }
                    }
                }
                Err(_) => println!("Selection cancelled."),
            }
        }
        SettingChoice::LspEnabled(current) => {
            let toggle_choices = vec!["Enabled", "Disabled"];
            let start_idx = if current { 0 } else { 1 };
            match Select::new("LSP Integration:", toggle_choices)
                .with_starting_cursor(start_idx)
                .prompt()
            {
                Ok(choice) => {
                    let enabled = choice == "Enabled";
                    if let Ok(mut cfg) = ConfigFile::load() {
                        cfg.lsp_enabled = enabled;
                        if cfg.save().is_ok() {
                            println!(
                                "LSP Integration: {}",
                                if enabled { "Enabled" } else { "Disabled" }
                            );

                            // Reload LSP servers in real-time
                            match crate::lsp::reload_from_config(working_dir).await {
                                Ok(count) if enabled => {
                                    println!("Started {} LSP server(s)", count);
                                }
                                Ok(_) => {
                                    println!("LSP servers stopped");
                                }
                                Err(e) => {
                                    println!("Warning: Failed to reload LSP: {}", e);
                                }
                            }
                        }
                    }
                }
                Err(_) => println!("Selection cancelled."),
            }
        }
    }
}

/// Interactive MCP server management menu using inquire
async fn show_mcp_menu() {
    use inquire::MultiSelect;

    let statuses = crate::mcp::manager().server_statuses().await;

    if statuses.is_empty() {
        println!("No MCP servers configured.");
        println!("\nTo add MCP servers, run:");
        println!("  henri mcp add <name> <command...>");
        println!("\nExample:");
        println!("  henri mcp add my-server npx -y @anthropic/mcp-server");
        return;
    }

    // Build choices - just server names
    let choices: Vec<&str> = statuses.iter().map(|s| s.name.as_str()).collect();

    // Pre-select currently running servers
    let defaults: Vec<usize> = statuses
        .iter()
        .enumerate()
        .filter(|(_, s)| s.is_running)
        .map(|(i, _)| i)
        .collect();

    let selected = match MultiSelect::new("MCP Servers:", choices.clone())
        .with_default(&defaults)
        .with_page_size(output::menu_page_size())
        .prompt()
    {
        Ok(sel) => sel,
        Err(_) => {
            println!("Cancelled.");
            return;
        }
    };

    // Determine which servers to start/stop
    let selected_names: std::collections::HashSet<&str> = selected.into_iter().collect();

    for status in &statuses {
        let is_selected = selected_names.contains(status.name.as_str());
        let name = &status.name;

        if is_selected && !status.is_running {
            // Start server
            match crate::mcp::start_server(name).await {
                Ok(()) => println!("Started '{}'", name),
                Err(e) => println!("Failed to start '{}': {}", name, e),
            }
        } else if !is_selected && status.is_running {
            // Stop server
            match crate::mcp::stop_server(name).await {
                Ok(()) => println!("Stopped '{}'", name),
                Err(e) => println!("Failed to stop '{}': {}", name, e),
            }
        }
    }
}

/// Interactive tools management menu using inquire
fn show_tools_menu() {
    use crate::tools::TOOL_INFO;
    use inquire::MultiSelect;

    let config = config::ConfigFile::load().unwrap_or_default();
    let disabled_tools = &config.disabled_tools;

    // Build choices with descriptions
    let choices: Vec<String> = TOOL_INFO
        .iter()
        .map(|(name, desc)| format!("{:<12} - {}", name, desc))
        .collect();

    // Pre-select enabled tools (those NOT in disabled list)
    let defaults: Vec<usize> = TOOL_INFO
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| !disabled_tools.iter().any(|t| t == *name))
        .map(|(i, _)| i)
        .collect();

    let selected = match MultiSelect::new("Tools:", choices.clone())
        .with_default(&defaults)
        .with_page_size(output::menu_page_size())
        .prompt()
    {
        Ok(sel) => sel,
        Err(_) => {
            println!("Cancelled.");
            return;
        }
    };

    // Determine which tools to enable/disable
    let selected_indices: std::collections::HashSet<usize> = selected
        .iter()
        .filter_map(|s| choices.iter().position(|c| c == s))
        .collect();

    let mut new_disabled_tools: Vec<String> = Vec::new();

    for (i, (name, _)) in TOOL_INFO.iter().enumerate() {
        if !selected_indices.contains(&i) {
            new_disabled_tools.push(name.to_string());
        }
    }

    // Save to config
    if let Ok(mut config) = config::ConfigFile::load() {
        config.disabled_tools = new_disabled_tools;
        if let Err(e) = config.save() {
            println!("Failed to save config: {}", e);
        } else {
            println!("Tool settings saved.");
        }
    }
}

/// Interactive session selection menu using inquire.
/// Returns the selected SessionInfo if one was chosen.
fn show_sessions_menu(working_dir: &std::path::Path) -> Option<session::SessionInfo> {
    let sessions = session::list_sessions(working_dir);

    if sessions.is_empty() {
        println!("No previous sessions found for this directory.");
        return None;
    }

    // Build display strings for each session
    let choices: Vec<String> = sessions.iter().map(|s| s.display_string()).collect();

    match Select::new("Select a session:", choices)
        .with_page_size(output::menu_page_size())
        .prompt()
    {
        Ok(choice) => {
            // Find the corresponding session by matching display string
            let idx = sessions
                .iter()
                .position(|s| s.display_string() == choice)
                .unwrap_or(0);
            Some(sessions[idx].clone())
        }
        Err(_) => {
            println!("Selection cancelled.");
            None
        }
    }
}

/// A choice in the default model submenu
#[derive(Clone)]
enum DefaultModelChoice {
    LastUsed,
    Specific(crate::providers::ModelChoice),
}

impl std::fmt::Display for DefaultModelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DefaultModelChoice::LastUsed => write!(f, ":last-used"),
            DefaultModelChoice::Specific(model) => {
                write!(f, "{}", model.display())?;
                if let Some(suffix) = model.display_suffix() {
                    write!(f, "{}", suffix)?;
                }
                Ok(())
            }
        }
    }
}

/// Show the default model selection submenu
fn show_default_model_menu(current: &DefaultModel) {
    let model_choices = build_model_choices();

    let mut choices: Vec<DefaultModelChoice> = vec![DefaultModelChoice::LastUsed];
    choices.extend(model_choices.into_iter().map(DefaultModelChoice::Specific));

    // Find the index of the current default model
    let start_idx = match current {
        DefaultModel::LastUsed => 0,
        DefaultModel::Specific(model_str) => choices
            .iter()
            .position(|c| match c {
                DefaultModelChoice::Specific(m) => m.short_display() == *model_str,
                _ => false,
            })
            .unwrap_or(0),
    };

    match Select::new("Default Model:", choices)
        .with_starting_cursor(start_idx)
        .with_page_size(output::menu_page_size())
        .prompt()
    {
        Ok(choice) => {
            let new_default = match &choice {
                DefaultModelChoice::LastUsed => DefaultModel::LastUsed,
                DefaultModelChoice::Specific(m) => DefaultModel::Specific(m.short_display()),
            };
            if let Ok(mut cfg) = ConfigFile::load() {
                cfg.default_model = new_default;
                if cfg.save().is_ok() {
                    println!("Default Model: {}", choice);
                }
            }
        }
        Err(_) => println!("Selection cancelled."),
    }
}

pub(crate) fn supports_thinking(provider: ModelProvider, model: &str) -> bool {
    match provider {
        ModelProvider::Antigravity => true,
        ModelProvider::OpenCodeZen => ZenProvider::model_thinking_toggleable(model),
        ModelProvider::GitHubCopilot => model.starts_with("gpt-5"),
        ModelProvider::Claude => true,
        ModelProvider::OpenAi => false,
        ModelProvider::OpenAiCompat => true,
        ModelProvider::OpenRouter => true,
    }
}

fn supports_images(provider: ModelProvider, model: &str) -> bool {
    match provider {
        ModelProvider::Antigravity => true,
        ModelProvider::OpenCodeZen => !matches!(model, "big-pickle" | "glm-4.6"),
        ModelProvider::GitHubCopilot => model.starts_with("gpt-5"),
        ModelProvider::Claude => true,
        ModelProvider::OpenAi => true,
        ModelProvider::OpenAiCompat => true,
        ModelProvider::OpenRouter => true,
    }
}

/// Chat with any provider that supports tools, handling tool calls in a loop.
///
/// This is a CLI-specific wrapper around the shared chat::run_chat_loop
/// that toggles the interrupt flag when Ctrl+C arrives so in-flight
/// requests can be cancelled without exiting the process.
async fn run_chat_loop(
    provider_manager: &mut ProviderManager,
    messages: &mut Vec<Message>,
    interrupted: &Arc<AtomicBool>,
    output: &output::OutputContext,
) -> error::Result<()> {
    let ctrl_c_interrupted = Arc::clone(interrupted);
    let ctrl_c_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            ctrl_c_interrupted.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });

    let result = provider_manager.chat(messages, interrupted, output).await;
    ctrl_c_task.abort();
    result
}

fn show_status(provider_manager: &ProviderManager, output: &output::OutputContext) {
    let provider = provider_manager.current_provider();
    let model = provider_manager.current_model_id();

    output.emit(output::OutputEvent::Info(format!(
        "Provider: {} ({})",
        provider.display_name(),
        provider.id()
    )));
    output.emit(output::OutputEvent::Info(format!("Model: {}", model)));

    if matches!(provider, ModelProvider::Claude) {
        let u = usage::anthropic();
        let context_limit = crate::provider::context_limit(provider, model);

        output.emit(output::OutputEvent::Info(String::new()));
        if let Some(limit) = context_limit {
            let pct = (u.last_input() as f64 / limit as f64) * 100.0;
            output.emit(output::OutputEvent::Info(format!(
                "Context: {} / {} tokens ({:.1}%)",
                u.last_input(),
                limit,
                pct
            )));
        } else {
            output.emit(output::OutputEvent::Info(format!(
                "Context: {} tokens",
                u.last_input()
            )));
        }

        output.emit(output::OutputEvent::Info(String::new()));
        output.emit(output::OutputEvent::Info("Session totals:".into()));
        output.emit(output::OutputEvent::Info(format!(
            "  Input:  {}",
            u.total_input()
        )));
        output.emit(output::OutputEvent::Info(format!(
            "  Output: {}",
            u.total_output()
        )));
    }
}

fn get_git_branch() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn build_prompt(provider_manager: &ProviderManager) -> PromptInfo {
    let provider = provider_manager.current_provider();
    let model = provider_manager.current_model_id();
    let custom_provider = provider_manager.current_custom_provider();

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".to_string());

    let home = std::env::var("HOME").unwrap_or_default();
    let path = if !home.is_empty() && cwd.starts_with(&home) {
        cwd.replacen(&home, "~", 1)
    } else {
        cwd
    };

    let provider_name = if provider == ModelProvider::OpenAiCompat {
        custom_provider
            .map(|s| s.to_string())
            .unwrap_or_else(|| provider.id().to_string())
    } else {
        provider.id().to_string()
    };

    PromptInfo {
        provider: provider_name,
        model: model.to_string(),
        provider_enum: provider,
        path,
        git_branch: get_git_branch(),
        thinking_available: supports_thinking(provider, model),
        show_thinking_status: !matches!(provider, ModelProvider::OpenAi),
    }
}

/// Arguments specific to the CLI interface
pub(crate) struct CliArgs {
    pub model: Option<String>,
    pub prompt: Vec<String>,
    pub working_dir: std::path::PathBuf,
    pub restored_session: Option<session::RestoredSession>,
    /// LSP override: Some(true) = force enable, Some(false) = force disable, None = use config
    pub lsp_override: Option<bool>,
    /// Disable sandbox for write-capable tools
    pub no_sandbox: bool,
}

/// Main entry point for the CLI interface
pub(crate) async fn run(args: CliArgs) -> std::io::Result<ExitStatus> {
    // Create output context for CLI
    let output = {
        let listener: Arc<dyn crate::output::OutputListener> =
            Arc::new(listener::CliListener::new());
        output::OutputContext::new_cli(listener)
    };

    // If no model specified on CLI, try to use the one from the restored session
    let model = args.model.clone().or_else(|| {
        args.restored_session
            .as_ref()
            .map(|s| format!("{}/{}", s.provider, s.model_id))
    });

    let config = match Config::load(model) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // Initialize MCP and LSP servers
    config::initialize_servers(&args.working_dir, args.lsp_override).await;

    let services = crate::services::Services::new();

    // Disable sandbox if --no-sandbox was passed
    if args.no_sandbox {
        services.set_sandbox_enabled(false);
    }

    let mut provider_manager = ProviderManager::new(&config, services.clone());

    // Non-interactive mode: run single prompt and exit
    if !args.prompt.is_empty() {
        let prompt = args.prompt.join(" ");
        let mut messages = vec![Message::user(&prompt)];
        let interrupted = Arc::new(AtomicBool::new(false));

        // Set thinking mode for the current model
        let thinking_state = provider_manager.default_thinking();
        if let Some(mode) = thinking_state.mode.clone() {
            provider_manager.set_thinking_mode(Some(mode));
        } else {
            provider_manager.set_thinking_mode(None);
        }
        provider_manager.set_thinking_enabled(thinking_state.enabled);

        match run_chat_loop(&mut provider_manager, &mut messages, &interrupted, &output).await {
            Ok(()) => {
                print_usage_for_provider(&provider_manager);
            }
            Err(error::Error::Interrupted) => {}
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        return Ok(ExitStatus::Quit);
    }

    // Interactive REPL mode
    let config_file = crate::config::ConfigFile::load().unwrap_or_default();
    let has_configured_providers = config_file
        .providers
        .entries
        .iter()
        .any(|(_, p)| !matches!(p.provider_type(), crate::config::ProviderType::Zen));

    // Determine sandbox status message
    let sandbox_status = if !services.is_sandbox_enabled() {
        "Sandbox: disabled"
    } else if crate::tools::sandbox_available() {
        "Sandbox: enabled"
    } else {
        "Sandbox: unavailable (Landlock not supported)"
    };

    if has_configured_providers {
        output.emit(output::OutputEvent::Info(
            "Welcome to Henri, your Golden Retriever coding assistant! 🐕".into(),
        ));
        output.emit(output::OutputEvent::Info(sandbox_status.into()));
        output.emit(output::OutputEvent::Info("Type /help for help.".into()));
    } else {
        output.emit(output::OutputEvent::Info(
            format!("Welcome to Henri! 🐕\n\nYou are currently using the free 'zen/grok-code' model. It's great for getting started!\nFor more powerful models (Claude, GPT-4), try connecting your accounts:\n\n  henri provider add      # Authenticate with GitHub Copilot, etc.\n\n{sandbox_status}\n\nType /help for help."),
        ));
    }

    let custom_commands = custom_commands::load_custom_commands().unwrap_or_else(|e| {
        eprintln!("Warning: Failed to load custom commands: {}", e);
        Vec::new()
    });

    let working_dir = args.working_dir;

    let mut history = FileHistory::new();
    let mut prompt_ui = PromptUi::new(custom_commands.clone(), working_dir.clone());
    let mut messages: Vec<Message> = Vec::new();
    let mut thinking_state = provider_manager.default_thinking();

    // Track current session ID for saving
    let mut current_session_id: Option<String> = None;

    // Apply restored session if provided
    if let Some(restored) = args.restored_session {
        messages = restored.messages;
        thinking_state.enabled = restored.thinking_enabled;
        current_session_id = Some(restored.session_id);
    } else {
        clear_todos();
    }

    loop {
        let mut prompt_info = build_prompt(&provider_manager);
        // Track if provider changed during cycle_favorite so we can strip thinking blocks
        let mut provider_changed_in_cycle = false;
        let outcome = prompt_ui.read(
            &mut prompt_info,
            &mut thinking_state.enabled,
            &mut thinking_state.mode,
            &mut history,
            || {
                let result = cycle_favorite_model(&mut provider_manager);
                if let Some((_, _, _, changed, _)) = &result
                    && *changed
                {
                    provider_changed_in_cycle = true;
                }
                result
            },
        )?;

        // Transform thinking blocks if provider changed during Shift+Tab cycling
        if provider_changed_in_cycle {
            crate::provider::transform_thinking_for_provider_switch(&mut messages);
        }

        match outcome {
            PromptOutcome::Interrupted => {
                output.emit(output::OutputEvent::Info("^C".into()));
                continue;
            }
            PromptOutcome::ContinueWithMode(mode) => {
                // Provider changed - use the provided thinking mode or get new provider's default
                if let Some(mode) = mode {
                    thinking_state = ThinkingState::new(mode != "off", Some(mode));
                } else {
                    thinking_state = provider_manager.default_thinking();
                }
                continue;
            }
            PromptOutcome::Eof => return Ok(ExitStatus::Quit),
            PromptOutcome::SelectModel => {
                if select_model(&mut provider_manager) {
                    // Provider changed - transform thinking blocks as signatures are provider-specific
                    crate::provider::transform_thinking_for_provider_switch(&mut messages);
                }
                thinking_state = provider_manager.default_thinking();
            }

            PromptOutcome::Submitted {
                content,
                pasted_images,
            } => {
                if content.trim().is_empty() && pasted_images.is_empty() {
                    continue;
                }

                let input = content.trim_end_matches('\n').to_string();

                let is_custom_prompt = if input.trim_start().starts_with('/') {
                    let current_provider = provider_manager.current_provider();
                    let cmd_result =
                        handle_command(&input, current_provider, &custom_commands, &output);
                    let is_custom = matches!(cmd_result, CommandResult::CustomPrompt(_));

                    match cmd_result {
                        CommandResult::Quit => return Ok(ExitStatus::Quit),
                        CommandResult::SwitchToTui => {
                            let session = if messages.is_empty() {
                                None
                            } else {
                                Some(ModeTransferSession {
                                    messages: messages.clone(),
                                    provider: provider_manager.current_provider(),
                                    model_id: provider_manager.current_model_id().to_string(),
                                    thinking_enabled: thinking_state.enabled,
                                    session_id: current_session_id.clone(),
                                })
                            };
                            return Ok(ExitStatus::SwitchToTui(session));
                        }
                        CommandResult::ClearHistory => {
                            messages.clear();
                            if let Some(ref session_id) = current_session_id {
                                let _ = session::delete_session(&working_dir, session_id);
                            }
                            current_session_id = None;
                            crate::usage::network_stats().clear();
                        }
                        CommandResult::SelectModel => {
                            if select_model(&mut provider_manager) {
                                // Provider changed - transform thinking blocks as signatures are provider-specific
                                crate::provider::transform_thinking_for_provider_switch(
                                    &mut messages,
                                );
                            }
                            thinking_state = provider_manager.default_thinking();
                        }
                        CommandResult::Continue => {}
                        CommandResult::Status => {
                            show_status(&provider_manager, &output);
                        }
                        CommandResult::Usage => {
                            output::start_spinner(&output, "Fetching rate limits...");
                            match usage::fetch_anthropic_rate_limits().await {
                                Ok(limits) => {
                                    output::stop_spinner(&output);
                                    limits.display();
                                }
                                Err(e) => {
                                    output::stop_spinner(&output);
                                    output.emit(output::OutputEvent::Error(format!(
                                        "Failed to fetch rate limits: {}",
                                        e
                                    )));
                                }
                            }
                        }
                        CommandResult::DumpPrompt => {
                            match provider_manager.prepare_request(messages.clone()).await {
                                Ok(json) => {
                                    let pretty = serde_json::to_string_pretty(&json)
                                        .unwrap_or_else(|_| json.to_string());
                                    println!("{}", pretty);
                                }
                                Err(e) => {
                                    output.emit(output::OutputEvent::Error(format!(
                                        "Failed to prepare request: {}",
                                        e
                                    )));
                                }
                            }
                        }
                        CommandResult::StartTransactionLogging => {
                            let path = crate::provider::transaction_log::start(None);
                            output.emit(output::OutputEvent::Info(format!(
                                "Transaction logging started to: {}",
                                path.display()
                            )));
                        }
                        CommandResult::StopTransactionLogging => {
                            crate::provider::transaction_log::stop();
                            output.emit(output::OutputEvent::Info(
                                "Transaction logging stopped.".into(),
                            ));
                        }
                        CommandResult::Compact => {
                            if messages.is_empty() {
                                output.emit(output::OutputEvent::Info(
                                    "No messages to compact.".into(),
                                ));
                                continue;
                            }

                            output::start_spinner(&output, "Compacting context...");

                            match provider_manager
                                .compact_context(&mut messages, 0, &output)
                                .await
                            {
                                Ok(result) => {
                                    output::stop_spinner(&output);
                                    output.emit(output::OutputEvent::Info(format!(
                                        "Compacted {} messages into summary.",
                                        result.messages_compacted
                                    )));

                                    match session::save_session(
                                        &working_dir,
                                        &messages,
                                        &provider_manager.current_provider(),
                                        provider_manager.current_model_id(),
                                        thinking_state.enabled,
                                        current_session_id.as_deref(),
                                    ) {
                                        Ok(id) => current_session_id = Some(id),
                                        Err(e) => {
                                            eprintln!("Warning: Failed to save session: {}", e)
                                        }
                                    }
                                }
                                Err(e) => {
                                    output::stop_spinner(&output);
                                    output.emit(output::OutputEvent::Error(format!(
                                        "Compaction failed: {}",
                                        e
                                    )));
                                }
                            }
                        }
                        CommandResult::CountTokens => {
                            output::start_spinner(&output, "Counting tokens...");
                            match provider_manager.count_tokens(&messages).await {
                                Ok(json) => {
                                    output::stop_spinner(&output);
                                    let pretty = serde_json::to_string_pretty(&json)
                                        .unwrap_or_else(|_| json.to_string());
                                    println!("{}", pretty);
                                }
                                Err(e) => {
                                    output::stop_spinner(&output);
                                    output.emit(output::OutputEvent::Error(format!(
                                        "Failed to count tokens: {}",
                                        e
                                    )));
                                }
                            }
                        }
                        CommandResult::CustomPrompt(prompt) => {
                            println!("\n{}\n", prompt);
                            let user_msg = Message::user(&prompt);
                            messages.push(user_msg);
                        }
                        CommandResult::Sessions => {
                            if let Some(selected) = show_sessions_menu(&working_dir) {
                                // Load the selected session
                                if let Some(state) =
                                    session::load_session_by_id(&working_dir, &selected.id)
                                {
                                    let restored = session::RestoredSession::from_state(&state);
                                    messages = restored.messages;
                                    thinking_state.enabled = restored.thinking_enabled;
                                    // Use the ID we loaded by, not from metadata (may be empty for v1)
                                    current_session_id = Some(selected.id.clone());

                                    // Replay session
                                    session::replay_session(&state);
                                }
                            }
                        }
                        CommandResult::Settings => {
                            show_settings_menu(&working_dir).await;
                        }
                        CommandResult::Mcp => {
                            show_mcp_menu().await;
                        }
                        CommandResult::Tools => {
                            show_tools_menu();
                        }
                        CommandResult::SandboxToggle => {
                            let enabled = !services.is_sandbox_enabled();
                            services.set_sandbox_enabled(enabled);
                            if enabled {
                                output.emit(output::OutputEvent::Info(
                                    "Sandbox enabled for bash commands.".into(),
                                ));
                            } else {
                                output.emit(output::OutputEvent::Info(
                                    "⚠ Sandbox disabled for bash commands.".into(),
                                ));
                            }
                        }
                    }

                    is_custom
                } else {
                    false
                };

                // Skip chat processing for regular commands
                if input.trim_start().starts_with('/') && !is_custom_prompt {
                    continue;
                }

                // Add regular input to messages if not already added
                if !input.trim_start().starts_with('/') && !input.trim_start().starts_with('!') {
                    let user_msg = Message::user(&input);
                    messages.push(user_msg);
                }

                // Handle shell commands starting with '!'
                if input.trim_start().starts_with('!') {
                    let cmd = input.trim_start().strip_prefix('!').unwrap_or("");
                    if !cmd.is_empty() {
                        let status = std::process::Command::new("sh").arg("-c").arg(cmd).status();
                        match status {
                            Ok(exit_status) => {
                                if !exit_status.success()
                                    && let Some(code) = exit_status.code()
                                {
                                    eprintln!("[Command exited with status {}]", code);
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to execute command: {}", e);
                            }
                        }
                    }
                    continue;
                }

                output::start_spinner(&output, "Waiting...");

                let messages_count = messages.len();
                let current_provider = provider_manager.current_provider();
                let model_id = provider_manager.current_model_id().to_string();

                // Validate image support for current model
                if !pasted_images.is_empty() && !supports_images(current_provider, &model_id) {
                    output::stop_spinner(&output);
                    eprintln!(
                        "Error: Model {}/{} does not support images.",
                        current_provider.id(),
                        model_id
                    );
                    continue;
                }

                // Create message with text and images
                let message = if pasted_images.is_empty() {
                    Message::user(&input)
                } else {
                    let mut blocks = Vec::new();

                    if !input.trim().is_empty() {
                        blocks.push(ContentBlock::Text { text: input });
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

                // Set thinking state
                provider_manager.set_thinking_enabled(thinking_state.enabled);
                provider_manager.set_thinking_mode(thinking_state.mode.clone());

                let interrupted = Arc::new(AtomicBool::new(false));

                let result =
                    run_chat_loop(&mut provider_manager, &mut messages, &interrupted, &output)
                        .await;

                match result {
                    Err(error::Error::Interrupted) => {
                        output::stop_spinner(&output);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        print!("\x1b[2K\rCancelled\n");
                        std::io::Write::flush(&mut std::io::stdout()).ok();
                        messages.truncate(messages_count);
                    }
                    Err(e) => {
                        output::stop_spinner(&output);
                        eprintln!("Error: {}", e);
                        messages.truncate(messages_count);
                    }
                    Ok(()) => {
                        print_usage_for_provider(&provider_manager);

                        match session::save_session(
                            &working_dir,
                            &messages,
                            &provider_manager.current_provider(),
                            provider_manager.current_model_id(),
                            thinking_state.enabled,
                            current_session_id.as_deref(),
                        ) {
                            Ok(id) => current_session_id = Some(id),
                            Err(e) => eprintln!("Warning: Failed to save session: {}", e),
                        }
                    }
                }
            }
        }
    }

    // Unreachable due to loop structure
    #[allow(unreachable_code)]
    Ok(ExitStatus::Quit)
}

/// Print usage statistics for the current provider
fn print_usage_for_provider(provider_manager: &ProviderManager) {
    let provider = provider_manager.current_provider();
    let model_id = provider_manager.current_model_id();

    match provider {
        ModelProvider::Antigravity => {
            let limit = crate::provider::context_limit(provider, model_id);
            usage::antigravity().print_last_usage(limit);
        }
        ModelProvider::Claude => {
            let limit = crate::provider::context_limit(provider, model_id);
            usage::anthropic().print_last_usage(limit);
        }
        ModelProvider::OpenCodeZen => {
            let limit = crate::provider::context_limit(provider, model_id);
            usage::zen().print_last_usage(limit);
        }
        ModelProvider::OpenRouter => {
            usage::openrouter().print_last_usage(None);
        }
        ModelProvider::OpenAiCompat => {
            usage::openai_compat().print_last_usage(None);
        }
        ModelProvider::OpenAi => {
            usage::openai().print_last_usage(None);
        }
        ModelProvider::GitHubCopilot => {
            // Copilot doesn't have usage tracking
        }
    }
}
