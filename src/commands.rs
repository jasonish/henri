// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Unified slash command definitions for both CLI and TUI modes.

use crate::config::ConfigFile;

/// Check if any Claude provider is configured (all use OAuth now).
pub(crate) fn has_claude_oauth_provider() -> bool {
    if let Ok(config) = ConfigFile::load() {
        config
            .providers
            .entries
            .values()
            .any(|provider| provider.as_claude().is_some())
    } else {
        false
    }
}

/// Command identifier for dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    BuildAgentsMd,
    ClaudeCountTokens,
    Clear,
    Cli,
    Tui,
    Compact,
    Custom { name: String, args: String },
    DumpConversation,
    DumpPrompt,
    Help,
    Model,
    Quit,
    Settings,
    StartTransactionLogging,
    Status,
    StopTransactionLogging,
    Usage,
}

/// Defines when a command should be visible in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Always available
    Always,
    /// Only available when using Claude (Anthropic) provider
    ClaudeOnly,
    /// Only available when a Claude provider with OAuth is configured
    ClaudeOAuthConfigured,
    /// Only available in TUI mode
    TuiOnly,
    /// Only available in CLI mode
    CliOnly,
}

#[derive(Debug, Clone)]
pub struct SlashCommand {
    pub command: Command,
    pub name: &'static str,
    pub description: &'static str,
    pub availability: Availability,
}

/// Owned version of SlashCommand for dynamic commands.
#[derive(Debug, Clone)]
pub struct DynamicSlashCommand {
    pub command: Command,
    pub name: String,
    pub description: String,
}

/// Session state for mode transitions.
/// This carries the current session directly to avoid reloading from disk.
#[derive(Debug, Clone)]
pub struct ModeTransferSession {
    pub messages: Vec<crate::provider::Message>,
    pub provider: crate::providers::ModelProvider,
    pub model_id: String,
    pub thinking_enabled: bool,
}

impl ModeTransferSession {
    /// Convert to RestoredSession for compatibility with existing mode interfaces.
    pub(crate) fn into_restored_session(
        self,
        working_dir: &std::path::Path,
    ) -> crate::session::RestoredSession {
        use crate::session::{RestoredSession, SerializableMessage, SessionMeta, SessionState};
        use chrono::Utc;

        // Create minimal session state for compatibility
        let meta = SessionMeta {
            version: 1,
            working_directory: working_dir.to_path_buf(),
            saved_at: Utc::now(),
            provider: self.provider.id().to_string(),
            model_id: self.model_id.clone(),
            thinking_enabled: self.thinking_enabled,
            todos: None,
        };

        let serializable_messages: Vec<SerializableMessage> =
            self.messages.iter().map(|m| m.into()).collect();

        let state = SessionState {
            meta,
            messages: serializable_messages,
        };

        RestoredSession {
            messages: self.messages,
            provider: self.provider.id().to_string(),
            model_id: self.model_id,
            thinking_enabled: self.thinking_enabled,
            state,
        }
    }
}

/// Status returned when exiting a mode (CLI or TUI)
#[derive(Debug, Clone)]
pub enum ExitStatus {
    Quit,
    /// Switch to CLI mode, optionally carrying current session state
    SwitchToCli(Option<ModeTransferSession>),
    /// Switch to TUI mode, optionally carrying current session state
    SwitchToTui(Option<ModeTransferSession>),
}

pub const COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        command: Command::BuildAgentsMd,
        name: "build-agents-md",
        description: "Generate/update AGENTS.md file for this project",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::ClaudeCountTokens,
        name: "claude-count-tokens",
        description: "Count tokens in current prompt",
        availability: Availability::ClaudeOnly,
    },
    SlashCommand {
        command: Command::Clear,
        name: "clear",
        description: "Clear conversation history",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Compact,
        name: "compact",
        description: "Summarize older messages to reduce context",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::DumpConversation,
        name: "dump-conversation",
        description: "Display conversation context as JSON",
        availability: Availability::TuiOnly,
    },
    SlashCommand {
        command: Command::DumpPrompt,
        name: "dump-prompt",
        description: "Dump the full API request",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Help,
        name: "help",
        description: "Show available commands",
        availability: Availability::CliOnly,
    },
    SlashCommand {
        command: Command::Cli,
        name: "cli",
        description: "Switch to CLI mode",
        availability: Availability::TuiOnly,
    },
    SlashCommand {
        command: Command::Tui,
        name: "tui",
        description: "Switch to TUI mode",
        availability: Availability::CliOnly,
    },
    SlashCommand {
        command: Command::Model,
        name: "model",
        description: "Select a model",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Quit,
        name: "quit",
        description: "Exit the application",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Settings,
        name: "settings",
        description: "Configure application settings",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::StartTransactionLogging,
        name: "start-transaction-logging",
        description: "Enable transaction logging to NDJSON file",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Status,
        name: "status",
        description: "Show usage statistics",
        availability: Availability::CliOnly,
    },
    SlashCommand {
        command: Command::StopTransactionLogging,
        name: "stop-transaction-logging",
        description: "Disable transaction logging",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Usage,
        name: "claude-usage",
        description: "Show Anthropic rate limits",
        availability: Availability::ClaudeOAuthConfigured,
    },
];

pub const SLASH_MENU_MAX_VISIBLE: usize = 6;

/// Filter commands based on context.
pub(crate) fn filter_commands(
    query: &str,
    is_tui: bool,
    is_claude: bool,
    has_claude_oauth: bool,
    custom_commands: &[crate::custom_commands::CustomCommand],
) -> Vec<DynamicSlashCommand> {
    let query = query.to_lowercase();
    let transaction_logging_active = crate::provider::transaction_log::is_active();

    let mut results: Vec<DynamicSlashCommand> = COMMANDS
        .iter()
        .filter(|cmd| {
            if transaction_logging_active && cmd.name == "start-transaction-logging" {
                return false;
            }
            if !transaction_logging_active && cmd.name == "stop-transaction-logging" {
                return false;
            }

            match cmd.availability {
                Availability::TuiOnly if !is_tui => return false,
                Availability::CliOnly if is_tui => return false,
                Availability::ClaudeOnly if !is_claude => return false,
                Availability::ClaudeOAuthConfigured if !has_claude_oauth => return false,
                _ => {}
            }
            cmd.name.contains(&query)
        })
        .map(|cmd| DynamicSlashCommand {
            command: cmd.command.clone(),
            name: cmd.name.to_string(),
            description: cmd.description.to_string(),
        })
        .collect();

    for custom in custom_commands {
        if custom.name.contains(&query) {
            results.push(DynamicSlashCommand {
                command: Command::Custom {
                    name: custom.name.clone(),
                    args: String::new(),
                },
                name: custom.name.clone(),
                description: custom.description.clone(),
            });
        }
    }

    results
}

/// Parse a command string (without leading "/") into a Command.
/// Splits the input into command name and arguments.
pub(crate) fn parse(
    input: &str,
    custom_commands: &[crate::custom_commands::CustomCommand],
) -> Option<Command> {
    let input = input.trim();

    // Split into command name and arguments
    let (cmd_name, args) = if let Some(space_pos) = input.find(char::is_whitespace) {
        let (name, rest) = input.split_at(space_pos);
        (name.trim().to_lowercase(), rest.trim().to_string())
    } else {
        (input.to_lowercase(), String::new())
    };

    // Check built-in commands (they don't accept arguments currently)
    if let Some(cmd) = COMMANDS.iter().find(|cmd| cmd.name == cmd_name) {
        return Some(cmd.command.clone());
    }

    // Check custom commands (they can accept arguments)
    if let Some(_custom) = custom_commands.iter().find(|cmd| cmd.name == cmd_name) {
        return Some(Command::Custom {
            name: cmd_name,
            args,
        });
    }

    None
}
