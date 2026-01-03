// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

mod auth;
mod chat;
mod cli;
mod commands;
mod compaction;
mod completion;
mod config;
mod custom_commands;
mod diff;
mod error;
mod history;
mod lsp;
mod mcp;
mod output;
mod prompts;
mod provider;
mod providers;
mod services;
mod session;
mod sse;
mod syntax;
mod tools;
mod tui;
mod upgrade;
mod usage;
mod version;

use std::path::PathBuf;

use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::{Parser, Subcommand};

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default());

/// Check for existing session and restore if requested.
/// Returns (working_dir, Option<RestoredSession>)
/// Only replays session history to stdout when `replay` is true (CLI mode).
fn handle_session_restore(
    continue_session: bool,
    replay: bool,
) -> (PathBuf, Option<session::RestoredSession>) {
    let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    if !continue_session {
        return (working_dir, None);
    }

    let Some(saved_session) = session::load_session(&working_dir) else {
        println!("No saved session found.\n");
        return (working_dir, None);
    };

    // Replay session history only in CLI mode
    if replay {
        session::replay_session(&saved_session);
    }
    (
        working_dir,
        Some(session::RestoredSession::from_state(&saved_session)),
    )
}

#[derive(Parser, Debug)]
#[command(name = "henri")]
#[command(about = "Your Golden Retriever AI Coding Assistant")]
#[command(version = version::VERSION)]
#[command(styles = STYLES, color = clap::ColorChoice::Always)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(short, long, help = "Model to use (default: claude-sonnet-4)")]
    model: Option<String>,

    #[arg(short = 'c', long = "continue", help = "Continue previous session")]
    continue_session: bool,

    #[arg(long, help = "Enable LSP integration", conflicts_with = "no_lsp")]
    lsp: bool,

    #[arg(long, help = "Disable LSP integration", conflicts_with = "lsp")]
    no_lsp: bool,

    #[arg(long, help = "Enable read-only mode (disables file editing tools)")]
    read_only: bool,

    #[arg(
        trailing_var_arg = true,
        help = "Prompt to send (non-interactive mode)"
    )]
    prompt: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Launch CLI mode (REPL interface)
    Cli {
        #[arg(short, long, help = "Model to use (default: claude-sonnet-4)")]
        model: Option<String>,

        #[arg(short = 'c', long = "continue", help = "Continue previous session")]
        continue_session: bool,

        #[arg(long, help = "Enable LSP integration", conflicts_with = "no_lsp")]
        lsp: bool,

        #[arg(long, help = "Disable LSP integration", conflicts_with = "lsp")]
        no_lsp: bool,

        #[arg(long, help = "Enable read-only mode (disables file editing tools)")]
        read_only: bool,

        #[arg(
            trailing_var_arg = true,
            help = "Prompt to send (non-interactive mode)"
        )]
        prompt: Vec<String>,
    },
    /// Launch TUI mode (terminal UI)
    Tui {
        #[arg(short, long, help = "Model to use (default: claude-sonnet-4)")]
        model: Option<String>,

        #[arg(short = 'c', long = "continue", help = "Continue previous session")]
        continue_session: bool,

        #[arg(long, help = "Enable LSP integration", conflicts_with = "no_lsp")]
        lsp: bool,

        #[arg(long, help = "Disable LSP integration", conflicts_with = "lsp")]
        no_lsp: bool,

        #[arg(long, help = "Enable read-only mode (disables file editing tools)")]
        read_only: bool,

        #[arg(
            trailing_var_arg = true,
            help = "Prompt to send (non-interactive mode)"
        )]
        prompt: Vec<String>,
    },
    /// Manage providers
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Test built-in tools directly (for debugging/learning)
    ToolCall {
        #[command(subcommand)]
        tool: ToolCommand,
    },
    /// Check for available upgrades
    #[command(alias = "update", hide = true)]
    Upgrade,
}

#[derive(Subcommand, Debug)]
enum ProviderCommand {
    /// Add a provider (OAuth/API key setup)
    Add,
    /// Remove a configured provider
    Remove,
}

#[derive(Subcommand, Debug)]
enum McpCommand {
    /// Add an MCP server
    Add {
        /// Name for the MCP server
        name: String,
        /// Command and arguments to run the MCP server
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Remove an MCP server
    Remove {
        /// Name of the MCP server to remove (interactive selection if not provided)
        name: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum ToolCommand {
    /// Test the glob tool to find files using patterns
    Glob {
        /// Glob pattern to match files (e.g., '**/*.rs', 'src/**/*.rs')
        pattern: String,

        /// Base directory for search (default: current directory)
        #[arg(short, long)]
        path: Option<String>,

        /// Maximum number of files to return
        #[arg(short, long)]
        limit: Option<usize>,

        /// Include hidden files/directories
        #[arg(long)]
        include_hidden: bool,
    },
    /// Test the grep tool to search for patterns in files
    Grep {
        /// Regular expression or string to search for
        pattern: String,

        /// Directory or file to search in (default: current directory)
        #[arg(short, long)]
        path: Option<String>,

        /// Ignore case distinctions
        #[arg(short = 'i', long)]
        case_insensitive: bool,

        /// Include hidden files/directories and ignored files
        #[arg(long)]
        include_hidden: bool,
    },
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    // Handle non-interactive subcommands first
    if let Some(command) = &args.command {
        match command {
            Command::Provider { command } => match command {
                ProviderCommand::Add => {
                    return handle_add_command().await;
                }
                ProviderCommand::Remove => {
                    return handle_provider_remove_command().await;
                }
            },
            Command::Mcp { command } => match command {
                McpCommand::Add { name, command } => {
                    return handle_mcp_add_command(name.clone(), command.clone());
                }
                McpCommand::Remove { name } => {
                    return handle_mcp_remove_command(name.clone());
                }
            },
            Command::ToolCall { tool } => match tool {
                ToolCommand::Glob {
                    pattern,
                    path,
                    limit,
                    include_hidden,
                } => {
                    return handle_glob_command(
                        pattern.clone(),
                        path.clone(),
                        *limit,
                        *include_hidden,
                    )
                    .await;
                }
                ToolCommand::Grep {
                    pattern,
                    path,
                    case_insensitive,
                    include_hidden,
                } => {
                    return handle_grep_command(
                        pattern.clone(),
                        path.clone(),
                        *case_insensitive,
                        *include_hidden,
                    )
                    .await;
                }
            },
            Command::Upgrade => {
                return handle_upgrade_command().await;
            }
            _ => {}
        }
    }

    use commands::ExitStatus;

    enum AppMode {
        Cli,
        Tui,
    }

    // Determine initial mode: explicit Cli/Tui subcommand wins, otherwise use config default
    let mut mode = match &args.command {
        Some(Command::Cli { .. }) => AppMode::Cli,
        Some(Command::Tui { .. }) => AppMode::Tui,
        _ => {
            // Load config to get default UI mode (falls back to TUI if config fails or invalid)
            let config_file = config::ConfigFile::load().unwrap_or_default();
            match config_file.ui.default {
                config::UiDefault::Cli => AppMode::Cli,
                config::UiDefault::Tui => AppMode::Tui,
            }
        }
    };

    let mut continue_session = args.continue_session;
    // Check if Cli/Tui subcommand also has continue_session flag set
    match &args.command {
        Some(Command::Cli {
            continue_session: c,
            ..
        })
        | Some(Command::Tui {
            continue_session: c,
            ..
        }) => {
            continue_session = continue_session || *c;
        }
        _ => {}
    }

    // Determine LSP override: subcommand takes precedence over global args
    let lsp_override: Option<bool> = match &args.command {
        Some(Command::Cli { lsp, no_lsp, .. }) | Some(Command::Tui { lsp, no_lsp, .. }) => {
            if *lsp {
                Some(true)
            } else if *no_lsp {
                Some(false)
            } else if args.lsp {
                Some(true)
            } else if args.no_lsp {
                Some(false)
            } else {
                None
            }
        }
        _ => {
            if args.lsp {
                Some(true)
            } else if args.no_lsp {
                Some(false)
            } else {
                None
            }
        }
    };

    // Determine read-only mode: subcommand takes precedence over global args
    let read_only = match &args.command {
        Some(Command::Cli { read_only, .. }) | Some(Command::Tui { read_only, .. }) => {
            *read_only || args.read_only
        }
        _ => args.read_only,
    };

    let mut first_run = true;
    let mut transferred_session: Option<commands::ModeTransferSession> = None;

    loop {
        // Determine effective prompt for this run (only relevant if first_run)
        let prompt_args = if first_run {
            match &args.command {
                Some(Command::Cli { prompt, .. }) | Some(Command::Tui { prompt, .. }) => {
                    prompt.clone()
                }
                _ => args.prompt.clone(),
            }
        } else {
            Vec::new()
        };

        // Determine session: use transferred session from mode switch, or load from disk
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let restored_session = if let Some(transfer) = transferred_session.take() {
            // Use session transferred from mode switch (don't load from disk)
            let restored = transfer.into_restored_session(&working_dir);
            // Replay session history when switching to CLI mode
            if matches!(mode, AppMode::Cli) {
                session::replay_session(&restored.state);
            }
            Some(restored)
        } else if prompt_args.is_empty() && continue_session {
            // Load from disk only on first run with --continue flag
            // Only replay session history in CLI mode (TUI has its own display)
            let replay = matches!(mode, AppMode::Cli);
            let (_, session) = handle_session_restore(continue_session, replay);
            session
        } else {
            None
        };

        // Extract model from subcommand args if present, else global args
        let model = match &args.command {
            Some(Command::Cli { model, .. }) | Some(Command::Tui { model, .. }) => {
                model.clone().or_else(|| args.model.clone())
            }
            _ => args.model.clone(),
        };

        let status = match mode {
            AppMode::Cli => {
                cli::run(cli::CliArgs {
                    model,
                    prompt: prompt_args,
                    working_dir,
                    restored_session,
                    lsp_override,
                    read_only,
                })
                .await?
            }
            AppMode::Tui => {
                let initial_prompt = if !prompt_args.is_empty() {
                    Some(prompt_args.join(" "))
                } else {
                    None
                };

                tui::run(
                    working_dir,
                    restored_session,
                    initial_prompt,
                    model,
                    lsp_override,
                    read_only,
                )
                .await?
            }
        };

        first_run = false;

        match status {
            ExitStatus::Quit => break,
            ExitStatus::SwitchToCli(session) => {
                mode = AppMode::Cli;
                transferred_session = session;
                // Don't set continue_session - we use transferred_session instead
            }
            ExitStatus::SwitchToTui(session) => {
                mode = AppMode::Tui;
                transferred_session = session;
                // Don't set continue_session - we use transferred_session instead
            }
        }
    }

    Ok(())
}

async fn handle_glob_command(
    pattern: String,
    path: Option<String>,
    limit: Option<usize>,
    include_hidden: bool,
) -> std::io::Result<()> {
    use tools::Tool;

    let glob_tool = tools::Glob;
    let input = serde_json::json!({
        "pattern": pattern,
        "path": path,
        "limit": limit,
        "include_hidden": include_hidden,
    });

    let output = output::OutputContext::new_quiet();
    let services = services::Services::new();
    let result = glob_tool
        .execute("glob-test", input, &output, &services)
        .await;

    if result.is_error {
        eprintln!("Error: {}", result.content);
        std::process::exit(1);
    } else {
        print!("{}", result.content);
    }

    Ok(())
}

async fn handle_grep_command(
    pattern: String,
    path: Option<String>,
    case_insensitive: bool,
    include_hidden: bool,
) -> std::io::Result<()> {
    use tools::Tool;

    let grep_tool = tools::Grep;
    let input = serde_json::json!({
        "pattern": pattern,
        "path": path,
        "case_insensitive": case_insensitive,
        "include_hidden": include_hidden,
    });

    let output = output::OutputContext::new_quiet();
    let services = services::Services::new();
    let result = grep_tool
        .execute("grep-test", input, &output, &services)
        .await;

    if result.is_error {
        eprintln!("Error: {}", result.content);
        std::process::exit(1);
    } else {
        print!("{}", result.content);
    }

    Ok(())
}

async fn handle_add_command() -> std::io::Result<()> {
    match auth::login().await {
        Ok(Some(_)) => {
            println!("Provider connected successfully.");
            Ok(())
        }
        Ok(None) => {
            println!("Connection cancelled.");
            Ok(())
        }
        Err(e) => {
            eprintln!("Connection failed: {}", e);
            std::process::exit(1);
        }
    }
}

async fn handle_provider_remove_command() -> std::io::Result<()> {
    use inquire::Select;

    // Load config
    let mut config = match config::ConfigFile::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Failed to load configuration: {}", e);
            std::process::exit(1);
        }
    };

    // Get all providers
    let providers: Vec<(String, String)> = config
        .providers
        .entries
        .iter()
        .map(|(id, provider_config)| {
            let provider_type = provider_config.provider_type();
            let display = format!("{} ({})", id, provider_type.display_name());
            (id.clone(), display)
        })
        .collect();

    if providers.is_empty() {
        println!("No providers configured.");
        return Ok(());
    }

    // Create display list for inquire
    let display_options: Vec<String> = providers
        .iter()
        .map(|(_, display)| display.clone())
        .collect();

    // Let user select a provider
    let selection = match Select::new("Select a provider to remove:", display_options)
        .with_page_size(output::menu_page_size())
        .prompt()
    {
        Ok(selected) => selected,
        Err(inquire::InquireError::OperationCanceled) => {
            println!("Cancelled.");
            return Ok(());
        }
        Err(e) => {
            eprintln!("Selection failed: {}", e);
            std::process::exit(1);
        }
    };

    // Find the provider ID from the selection
    let provider_id = providers
        .iter()
        .find(|(_, display)| display == &selection)
        .map(|(id, _)| id.clone())
        .expect("Selected provider not found in list");

    // Confirm removal
    let confirm = match inquire::Confirm::new(&format!("Remove provider '{}'?", provider_id))
        .with_default(false)
        .prompt()
    {
        Ok(confirmed) => confirmed,
        Err(inquire::InquireError::OperationCanceled) => {
            println!("Cancelled.");
            return Ok(());
        }
        Err(e) => {
            eprintln!("Confirmation failed: {}", e);
            std::process::exit(1);
        }
    };

    if !confirm {
        println!("Cancelled.");
        return Ok(());
    }

    // Remove the provider
    config.remove_provider(&provider_id);

    // Save configuration
    if let Err(e) = config.save() {
        eprintln!("Failed to save configuration: {}", e);
        std::process::exit(1);
    }

    println!("✓ Provider '{}' removed successfully.", provider_id);
    Ok(())
}

async fn handle_upgrade_command() -> std::io::Result<()> {
    println!("Checking for updates...");

    match upgrade::check_for_upgrade().await {
        Ok(upgrade::UpgradeStatus::Available { latest, url }) => {
            println!();
            println!(
                "A new version of Henri is available: {} (current: {})",
                latest,
                version::VERSION
            );
            println!();
            println!("To upgrade, run:");
            println!("  cargo install --git https://github.com/jasonish/henri");
            println!();
            println!("Release notes: {}", url);
        }
        Ok(upgrade::UpgradeStatus::UpToDate) => {
            println!("You're on the latest version ({}).", version::VERSION);
        }
        Err(e) => {
            eprintln!("Failed to check for updates: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}

fn handle_mcp_add_command(name: String, command: Vec<String>) -> std::io::Result<()> {
    if command.is_empty() {
        eprintln!("Error: command is required");
        std::process::exit(1);
    }

    // Load config
    let mut config = match config::ConfigFile::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Failed to load configuration: {}", e);
            std::process::exit(1);
        }
    };

    // Check if server with this name already exists
    let mcp_config = config.mcp.get_or_insert_with(Default::default);
    if mcp_config.servers.iter().any(|s| s.name == name) {
        eprintln!("Error: MCP server '{}' already exists", name);
        std::process::exit(1);
    }

    // Split command into command and args
    let (cmd, args) = command.split_first().unwrap();

    // Add the new server
    mcp_config.servers.push(config::McpServerConfig {
        name: name.clone(),
        command: cmd.clone(),
        args: args.to_vec(),
        env: std::collections::HashMap::new(),
        enabled: true,
    });

    // Save configuration
    if let Err(e) = config.save() {
        eprintln!("Failed to save configuration: {}", e);
        std::process::exit(1);
    }

    println!("✓ MCP server '{}' added successfully.", name);
    Ok(())
}

fn handle_mcp_remove_command(name: Option<String>) -> std::io::Result<()> {
    use inquire::Select;

    // Load config
    let mut config = match config::ConfigFile::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Failed to load configuration: {}", e);
            std::process::exit(1);
        }
    };

    let mcp_config = match &mut config.mcp {
        Some(cfg) if !cfg.servers.is_empty() => cfg,
        _ => {
            println!("No MCP servers configured.");
            return Ok(());
        }
    };

    // Determine which server to remove
    let server_name = match name {
        Some(n) => {
            // Verify the server exists
            if !mcp_config.servers.iter().any(|s| s.name == n) {
                eprintln!("Error: MCP server '{}' not found", n);
                std::process::exit(1);
            }
            n
        }
        None => {
            // Interactive selection
            let server_names: Vec<String> =
                mcp_config.servers.iter().map(|s| s.name.clone()).collect();

            match Select::new("Select an MCP server to remove:", server_names)
                .with_page_size(output::menu_page_size())
                .prompt()
            {
                Ok(selected) => selected,
                Err(inquire::InquireError::OperationCanceled) => {
                    println!("Cancelled.");
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("Selection failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
    };

    // Remove the server
    mcp_config.servers.retain(|s| s.name != server_name);

    // Save configuration
    if let Err(e) = config.save() {
        eprintln!("Failed to save configuration: {}", e);
        std::process::exit(1);
    }

    println!("✓ MCP server '{}' removed successfully.", server_name);
    Ok(())
}
