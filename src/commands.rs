// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Unified slash command definitions for CLI mode.

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
pub(crate) enum Command {
    BuildAgentsMd,
    ClaudeCountTokens,
    Clear,
    Compact,
    Custom { name: String, args: String },
    DumpPrompt,
    Echo { text: String },
    Help,
    Lsp,
    Mcp,
    Model,
    Provider,
    Quit,
    ReadOnly,
    ReadWrite,
    Yolo,
    Sessions,
    Settings,
    Skills,
    StartTransactionLogging,
    StopTransactionLogging,
    Tools,
    Truncate,
    Undo,
    Forget,
    Usage,
}

/// Defines when a command should be visible in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Availability {
    /// Always available
    Always,
    /// Only available when using Claude (Anthropic) provider
    ClaudeOnly,
    /// Only available when a Claude provider with OAuth is configured
    ClaudeOAuthConfigured,
}

#[derive(Debug, Clone)]
pub(crate) struct SlashCommand {
    pub command: Command,
    pub name: &'static str,
    pub description: &'static str,
    pub availability: Availability,
}

/// Owned version of SlashCommand for dynamic commands.
#[derive(Debug, Clone)]
pub(crate) struct DynamicSlashCommand {
    pub command: Command,
    pub name: String,
    pub description: String,
}

pub(crate) const COMMANDS: &[SlashCommand] = &[
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
        command: Command::Clear,
        name: "new",
        description: "Start a new conversation (alias for /clear)",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Compact,
        name: "compact",
        description: "Summarize older messages to reduce context",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::DumpPrompt,
        name: "dump-prompt",
        description: "Dump the full API request",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Echo {
            text: String::new(),
        },
        name: "echo",
        description: "Echo text to the output area",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Help,
        name: "help",
        description: "Show available commands",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Lsp,
        name: "lsp",
        description: "Show LSP server status",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Mcp,
        name: "mcp",
        description: "Manage MCP server connections",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Model,
        name: "model",
        description: "Select a model",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Provider,
        name: "provider",
        description: "Manage AI providers (add/remove)",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Quit,
        name: "quit",
        description: "Exit the application",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::ReadOnly,
        name: "read-only",
        description: "Switch to Read-Only mode",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::ReadWrite,
        name: "read-write",
        description: "Switch to Read-Write mode (Sandbox enabled)",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Yolo,
        name: "yolo",
        description: "Switch to YOLO mode (Sandbox disabled)",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Sessions,
        name: "sessions",
        description: "List and select previous sessions",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Settings,
        name: "settings",
        description: "Configure application settings",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Skills,
        name: "skills",
        description: "List available skills",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::StartTransactionLogging,
        name: "start-transaction-logging",
        description: "Enable transaction logging to NDJSON file",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::StopTransactionLogging,
        name: "stop-transaction-logging",
        description: "Disable transaction logging",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Tools,
        name: "tools",
        description: "Enable/disable built-in tools",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Truncate,
        name: "truncate",
        description: "Keep only the last message and clear the rest",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Undo,
        name: "undo",
        description: "Remove the most recent turn (user message and response)",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Forget,
        name: "forget",
        description: "Remove the oldest turn from conversation history",
        availability: Availability::Always,
    },
    SlashCommand {
        command: Command::Usage,
        name: "claude-usage",
        description: "Show Anthropic rate limits",
        availability: Availability::ClaudeOAuthConfigured,
    },
];

/// Filter commands based on context.
pub(crate) fn filter_commands(
    query: &str,
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

    // Handle echo specially since it takes arguments
    if cmd_name == "echo" {
        return Some(Command::Echo { text: args });
    }

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
