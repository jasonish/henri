// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(target_os = "linux")]
use anyhow::Context;
use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;
use inquire::Select;
use rustyline::Helper;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Cmd, KeyEvent, Modifiers};
use rustyline::{Config, Editor};

mod auth;
mod chat;
mod config;
mod display;
mod history;
mod llm;
mod prompt;
mod utils;

#[cfg(target_os = "linux")]
mod landlock;

use chat::ChatConversation;
use config::Config as AppConfig;
use display::{DisplayStyle, clear_lines, print_full_width_message};
use history::History;
use llm::{LLM, ProviderClient, get_available_models};

struct ReplHelper;

impl Helper for ReplHelper {}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let commands = vec![
            "/quit", "/exit", "/help", "/clear", "/model", "/verbose", "/system",
        ];
        let mut candidates = Vec::new();

        if line.starts_with('/') {
            let partial = &line[..pos];
            for cmd in commands {
                if cmd.starts_with(partial) {
                    candidates.push(Pair {
                        display: cmd.to_string(),
                        replacement: cmd.to_string(),
                    });
                }
            }
        }

        Ok((0, candidates))
    }
}

impl Hinter for ReplHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &rustyline::Context<'_>) -> Option<String> {
        if line.starts_with('/') && pos == line.len() {
            let commands = vec![
                "/quit", "/exit", "/help", "/clear", "/model", "/verbose", "/system",
            ];
            for cmd in commands {
                if cmd.starts_with(line) && cmd.len() > line.len() {
                    return Some(cmd[line.len()..].to_string());
                }
            }
        }
        None
    }
}

impl Highlighter for ReplHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
        // Make hints cyan and dim
        format!("\x1b[36;2m{hint}\x1b[0m").into()
    }
}

impl Validator for ReplHelper {}

#[derive(Parser)]
#[command(name = "henri")]
#[command(about = "A coding assistant REPL")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Enable verbose mode with debug output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Enable landlock sandboxing (Linux only)
    #[cfg(target_os = "linux")]
    #[arg(long, global = true)]
    landlock: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Login to model providers
    Login,
    /// Test authentication for configured providers
    #[command(name = "auth")]
    Auth {
        #[command(subcommand)]
        subcommand: AuthCommands,
    },
}

#[derive(Subcommand)]
enum AuthCommands {
    /// Test authentication for a provider
    Test,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Apply landlock restrictions if requested (Linux only)
    #[cfg(target_os = "linux")]
    if cli.landlock {
        landlock::apply_landlock_restrictions().context("Failed to apply landlock restrictions")?;
        if cli.verbose {
            eprintln!("Landlock restrictions applied successfully");
        }
    }

    match cli.command {
        Some(Commands::Login) => {
            auth::login().await?;
        }
        Some(Commands::Auth { subcommand }) => match subcommand {
            AuthCommands::Test => {
                auth::test_auth_interactive(cli.verbose).await?;
            }
        },
        None => {
            start_repl(cli.verbose).await?;
        }
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn start_repl(mut verbose: bool) -> Result<()> {
    // Initialize LLM client
    let mut llm_client = match llm::create_llm_client(verbose) {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Warning: Failed to initialize LLM client: {e}");
            eprintln!("You can still use the REPL, but responses will be echoed back.");
            eprintln!("Run 'henri login' to authenticate with a provider.");
            None
        }
    };

    let mut rl = Editor::with_config(Config::default()).expect("Failed to create readline editor");
    rl.set_helper(Some(ReplHelper {}));

    rl.bind_sequence(KeyEvent::new('\r', Modifiers::ALT), Cmd::Newline);
    rl.bind_sequence(KeyEvent::new('\n', Modifiers::ALT), Cmd::Newline);

    // Load history
    let mut history = History::load().unwrap_or_else(|e| {
        eprintln!("Warning: Failed to load history: {e}");
        History::default()
    });

    // Load history into rustyline for arrow key navigation
    if let Err(e) = history.load_into_rustyline(&mut rl) {
        eprintln!("Warning: Failed to load history into editor: {e}");
    }

    // Load config to get system mode
    let mut config = AppConfig::load().unwrap_or_default();
    let system_mode = config.get_system_mode();

    // Initialize chat conversation with system message
    let mut conversation = ChatConversation::default();
    conversation.add_system_message(system_mode.system_prompt().to_string());

    println!("Welcome to Henri, your Golden Retriever coding assistant! 🐕");
    println!("Current mode: {} Assistant", system_mode.as_str());
    println!("Type '/quit' or '/exit' to quit.");
    println!("Use Alt+Enter for multi-line input or end lines with '\\'.");

    if llm_client.is_some() {
        println!(
            "LLM provider configured. Your conversation history will be maintained during this session."
        );
    } else {
        println!("No LLM provider configured. Responses will be echoed back.");
        println!("Run 'henri login' to authenticate with GitHub Copilot.");
    }

    let mut multi_line_buffer = String::new();
    let mut last_usage: Option<llm::CopilotUsage> = None;
    let mut input_line_count = 0;

    loop {
        // Get model info for prompt
        let (model, provider) = prompt::get_model_info(&config);

        // Build fancy prompt
        let prompt = prompt::PromptBuilder::new()
            .with_model(model.clone(), provider.clone())
            .with_tokens(last_usage.take())
            .multi_line(!multi_line_buffer.is_empty())
            .build();

        let readline = rl.readline(&prompt);
        match readline {
            Ok(line) => {
                // Count this line
                input_line_count += 1;

                // Check if user typed just "/" to show command menu
                if line.trim() == "/" {
                    match show_command_menu().await {
                        Ok(Some(command)) => {
                            // Execute the selected command by simulating user input
                            rl.add_history_entry(&command).ok();
                            history.add_entry(command.clone(), false);

                            // Process the command as if the user typed it
                            if command == "/quit" || command == "/exit" {
                                // Save history before exiting
                                if let Err(e) = history.save() {
                                    eprintln!("Warning: Failed to save history: {e}");
                                }
                                break;
                            } else if command == "/help" {
                                println!("Available commands:");
                                println!("  /help    - Show this help message");
                                println!("  /clear   - Clear chat conversation history");
                                println!(
                                    "  /verbose - Toggle verbose mode (show raw API requests/responses)"
                                );
                                println!("  /model   - Show interactive model menu");
                                println!(
                                    "  /system  - Toggle between Developer and SysAdmin modes"
                                );
                                println!("  /quit    - Exit the REPL");
                                println!("  /exit    - Exit the REPL");
                                println!();
                                println!(
                                    "Use Alt+Enter for multi-line input or end lines with '\\\\'."
                                );
                            } else if command == "/clear" {
                                conversation.clear();
                                println!(
                                    "{}",
                                    "Chat history cleared (system prompt preserved).".yellow()
                                );
                            } else if command == "/system" {
                                let config = AppConfig::load().unwrap_or_default();
                                let current_mode = config.get_system_mode();
                                let new_mode = current_mode.toggle();

                                // Save new mode to config
                                let mut config = AppConfig::load().unwrap_or_default();
                                config.set_system_mode(new_mode);
                                if let Err(e) = config.save() {
                                    eprintln!("Warning: Failed to save config: {e}");
                                }

                                // Clear conversation and add new system message
                                conversation.clear();
                                conversation
                                    .add_system_message(new_mode.system_prompt().to_string());

                                println!(
                                    "{}",
                                    format!(
                                        "System mode switched to: {} Assistant",
                                        new_mode.as_str()
                                    )
                                    .green()
                                    .bold()
                                );
                                println!(
                                    "{}",
                                    "Chat history cleared with new system prompt.".yellow()
                                );
                            } else if command == "/verbose" {
                                verbose = !verbose;
                                println!(
                                    "{}",
                                    format!("Verbose mode: {}", if verbose { "ON" } else { "OFF" })
                                        .yellow()
                                );
                                // Update the LLM client's verbose setting if it exists
                                if let Some(ref mut client) = llm_client {
                                    client.set_verbose(verbose);
                                }
                            } else if command.starts_with("/model") {
                                handle_model_command(&command).await;
                                // Reload config to get updated model
                                config = AppConfig::load().unwrap_or_default();
                                // Recreate LLM client with new model/provider
                                llm_client = match llm::create_llm_client(verbose) {
                                    Ok(client) => client,
                                    Err(e) => {
                                        eprintln!(
                                            "{} Failed to create LLM client: {e}",
                                            "error>".red().bold()
                                        );
                                        None
                                    }
                                };
                            }
                        }
                        Ok(None) => {
                            // User cancelled the selection
                        }
                        Err(e) => {
                            eprintln!("Error showing command menu: {e}");
                        }
                    }
                    input_line_count = 0;
                    continue;
                }

                if line.trim() == "/quit" || line.trim() == "/exit" {
                    // Save history before exiting
                    if let Err(e) = history.save() {
                        eprintln!("Warning: Failed to save history: {e}");
                    }
                    break;
                } else if line.trim() == "/help" {
                    println!("Available commands:");
                    println!("  /help    - Show this help message");
                    println!("  /clear   - Clear chat conversation history");
                    println!("  /verbose - Toggle verbose mode (show raw API requests/responses)");
                    println!("  /model   - Show interactive model menu");
                    println!("  /system  - Toggle between Developer and SysAdmin modes");
                    println!("  /quit    - Exit the REPL");
                    println!("  /exit    - Exit the REPL");
                    println!();
                    println!("Use Alt+Enter for multi-line input or end lines with '\\\\'.");
                    input_line_count = 0;
                    continue;
                } else if line.trim() == "/clear" {
                    conversation.clear();
                    println!(
                        "{}",
                        "Chat history cleared (system prompt preserved).".yellow()
                    );
                    input_line_count = 0;
                    continue;
                } else if line.trim() == "/system" {
                    let config = AppConfig::load().unwrap_or_default();
                    let current_mode = config.get_system_mode();
                    let new_mode = current_mode.toggle();

                    // Save new mode to config
                    let mut config = AppConfig::load().unwrap_or_default();
                    config.set_system_mode(new_mode);
                    if let Err(e) = config.save() {
                        eprintln!("Warning: Failed to save config: {e}");
                    }

                    // Clear conversation and add new system message
                    conversation.clear();
                    conversation.add_system_message(new_mode.system_prompt().to_string());

                    println!(
                        "{}",
                        format!("System mode switched to: {} Assistant", new_mode.as_str())
                            .green()
                            .bold()
                    );
                    println!(
                        "{}",
                        "Chat history cleared with new system prompt.".yellow()
                    );
                    input_line_count = 0;
                    continue;
                } else if line.trim() == "/verbose" {
                    verbose = !verbose;
                    println!(
                        "{}",
                        format!("Verbose mode: {}", if verbose { "ON" } else { "OFF" }).yellow()
                    );
                    // Update the LLM client's verbose setting if it exists
                    if let Some(ref mut client) = llm_client {
                        client.set_verbose(verbose);
                    }
                    input_line_count = 0;
                    continue;
                } else if line.trim().starts_with("/model") {
                    handle_model_command(line.trim()).await;
                    // Reload config to get updated model
                    config = AppConfig::load().unwrap_or_default();
                    // Recreate LLM client with new model/provider
                    llm_client = match llm::create_llm_client(verbose) {
                        Ok(client) => client,
                        Err(e) => {
                            eprintln!("{} Failed to create LLM client: {e}", "error>".red().bold());
                            None
                        }
                    };
                    input_line_count = 0;
                    continue;
                } else if line.trim().starts_with('/') && !line.trim().is_empty() {
                    let command = line.trim();
                    if ![
                        "/quit", "/exit", "/help", "/clear", "/verbose", "/model", "/system",
                    ]
                    .contains(&command)
                    {
                        println!(
                            "Unknown command: {command}. Type '/help' for available commands."
                        );
                        input_line_count = 0;
                        continue;
                    }
                }
                if line.trim() == "\x1b" {
                    println!("ESC was caught");
                    continue;
                }

                if line.trim().is_empty() && multi_line_buffer.is_empty() {
                    input_line_count = 0;
                    continue;
                }

                if line.ends_with('\\') {
                    let mut line_without_backslash = line.clone();
                    line_without_backslash.pop();
                    multi_line_buffer.push_str(&line_without_backslash);
                    multi_line_buffer.push('\n');
                    continue;
                }

                if multi_line_buffer.is_empty() {
                    // Add to both rustyline history and our JSON history
                    rl.add_history_entry(&line).ok();
                    history.add_entry(line.clone(), false);

                    last_usage = handle_user_input(
                        &mut llm_client,
                        &mut conversation,
                        line,
                        verbose,
                        input_line_count,
                    )
                    .await;
                    input_line_count = 0;
                } else {
                    multi_line_buffer.push_str(&line);
                    let complete_input = multi_line_buffer.clone();
                    multi_line_buffer.clear();

                    // Add to both rustyline history and our JSON history
                    rl.add_history_entry(&complete_input).ok();
                    history.add_entry(complete_input.clone(), true);

                    last_usage = handle_user_input(
                        &mut llm_client,
                        &mut conversation,
                        complete_input,
                        verbose,
                        input_line_count,
                    )
                    .await;
                    input_line_count = 0;
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("CTRL-C was caught");
            }
            Err(ReadlineError::Eof) => {
                println!("CTRL-D - exiting");
                // Save history before exiting
                if let Err(e) = history.save() {
                    eprintln!("Warning: Failed to save history: {e}");
                }
                break;
            }
            Err(err) => {
                println!("Error: {err:?}");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_llm_response(
    client: &mut ProviderClient,
    conversation: &mut ChatConversation,
    mut choices: Vec<llm::CopilotChoice>,
    verbose: bool,
) -> Option<llm::CopilotUsage> {
    let mut accumulated_usage: Option<llm::CopilotUsage> = None;

    // Process choices in a loop to handle tool calls iteratively
    loop {
        let mut has_tool_calls = false;
        let mut all_tool_results = Vec::new();

        let mut assistant_content = String::new();
        let mut assistant_tool_calls = Vec::new();

        // First pass: collect content and tool calls from all choices
        for choice in &choices {
            // Collect content
            if let Some(content) = &choice.message.content {
                if !content.is_empty() {
                    assistant_content = content.clone();
                    print_wrapped_response("assistant>", content);
                }
            }

            // Collect tool calls
            if let Some(tool_calls) = &choice.message.tool_calls {
                if !tool_calls.is_empty() {
                    has_tool_calls = true;
                    assistant_tool_calls.extend(tool_calls.clone());
                    if verbose {
                        eprintln!("🔧 Found {} tool call(s)", tool_calls.len());
                    }
                }
            }
        }

        // Add the assistant message to conversation (only once, combining content and tool calls)
        if !assistant_content.is_empty() || !assistant_tool_calls.is_empty() {
            if assistant_tool_calls.is_empty() {
                conversation.add_assistant_message(assistant_content);
            } else {
                conversation.add_assistant_message_with_tool_calls(
                    assistant_content,
                    assistant_tool_calls.clone(),
                );
            }
        }

        // Now execute tool calls if we have any
        if has_tool_calls && !assistant_tool_calls.is_empty() {
            if verbose {
                eprintln!("🔧 Executing {} tool call(s)", assistant_tool_calls.len());
            }

            // Execute the tool calls
            match llm::execute_tool_calls(&assistant_tool_calls, verbose).await {
                Ok(tool_results) => {
                    // Display tool results
                    for result in &tool_results {
                        let mut output = format!("Exit code: {}", result.exit_code);

                        if !result.stdout.trim().is_empty() {
                            let truncated_stdout = utils::truncate_stdout(&result.stdout, 4);
                            output.push_str(&format!("\nStdout:\n{truncated_stdout}"));
                        }

                        if !result.stderr.trim().is_empty() {
                            output.push_str(&format!("\nStderr:\n{}", result.stderr));
                        }

                        // Display tool output without prefix
                        println!("{output}");
                    }

                    // Add tool results to conversation
                    for result in &tool_results {
                        let tool_content = serde_json::json!({
                            "stdout": result.stdout,
                            "stderr": result.stderr,
                            "exit_code": result.exit_code
                        });
                        conversation.add_tool_message(
                            serde_json::to_string(&tool_content).unwrap_or_default(),
                            result.tool_call_id.clone(),
                        );
                    }

                    all_tool_results.extend(tool_results);
                }
                Err(e) => {
                    eprintln!(
                        "{} Failed to execute tool calls: {e}",
                        "error>".red().bold()
                    );
                }
            }
        }

        // If we had tool calls, send the results back and continue
        if has_tool_calls && !all_tool_results.is_empty() {
            match client
                .send_tool_results(conversation.get_messages(), &all_tool_results, verbose)
                .await
            {
                Ok((new_choices, new_usage)) => {
                    // Update usage if provided
                    if let Some(usage) = new_usage {
                        accumulated_usage = Some(usage);
                    }

                    // Continue with the new choices
                    choices = new_choices;
                }
                Err(e) => {
                    eprintln!("{} Failed to send tool results: {e}", "error>".red().bold());
                    break;
                }
            }
        } else {
            // No more tool calls, we're done
            break;
        }
    }

    accumulated_usage
}

async fn handle_user_input(
    llm_client: &mut Option<ProviderClient>,
    conversation: &mut ChatConversation,
    user_input: String,
    verbose: bool,
    input_lines: usize,
) -> Option<llm::CopilotUsage> {
    // Add user message to conversation
    conversation.add_user_message(user_input.clone());

    // Clear the lines where the user typed their input
    clear_lines(input_lines);

    // Display user input with colored background
    print_full_width_message("user", &user_input, DisplayStyle::User);

    if let Some(client) = llm_client {
        // Send to LLM and get response
        match client
            .send_chat_request(conversation.get_messages(), verbose)
            .await
        {
            Ok((choices, usage)) => {
                handle_llm_response(client, conversation, choices, verbose).await;
                usage
            }
            Err(e) => {
                eprintln!("{} Failed to get LLM response: {e}", "error>".red().bold());
                // Fallback to echo
                println!("{} {}", "echo>".yellow().bold(), user_input);
                None
            }
        }
    } else {
        // Fallback to echo if no LLM client available
        println!("{} {}", "echo>".yellow().bold(), user_input);
        None
    }
}

async fn handle_model_command(command: &str) {
    let parts: Vec<&str> = command.split_whitespace().collect();

    if parts.len() == 1 {
        // Show interactive model selection menu
        match show_model_selection_menu().await {
            Ok(()) => {
                // Model was selected and saved
            }
            Err(e) => {
                eprintln!("{} Model selection failed: {e}", "error>".red().bold());
            }
        }
    } else {
        println!("{}", "Usage: /model".yellow());
        println!("  /model - Show interactive model selection");
    }
}

async fn show_model_selection_menu() -> Result<()> {
    // Load current config to see what's selected
    let config = AppConfig::load()?;
    let current_model = config.get_selected_model();

    // Check if we have any providers configured
    let has_providers =
        config.providers.github_copilot.is_some() || config.providers.open_router.is_some();

    if !has_providers {
        println!(
            "{}",
            "No LLM providers configured. Run 'henri login' to authenticate.".yellow()
        );
        return Ok(());
    }

    // Get available models based on configured providers
    let models = get_available_models(&config);

    // Create display options with current selection indicator
    let options: Vec<String> = models
        .iter()
        .map(|model| {
            let indicator = if current_model == Some(&model.id) {
                "● "
            } else {
                "  "
            };
            format!("{}{} ({})", indicator, model.id, model.provider)
        })
        .collect();

    // Show current selection
    if let Some(current) = current_model {
        // Find the model in our list to get its provider
        let provider = models
            .iter()
            .find(|m| &m.id == current)
            .map(|m| m.provider.to_string())
            .unwrap_or_else(|| "Unknown".to_string());
        println!(
            "{} {} ({})",
            "Current model:".green().bold(),
            current,
            provider,
        );
        println!();
    }

    // Find the index of the current model for default selection
    let default_index = if let Some(current) = current_model {
        models.iter().position(|m| m.id == *current).unwrap_or(0)
    } else {
        0
    };

    // Show the selection menu
    let selection = Select::new("Select a model:", options)
        .with_page_size(10)
        .with_starting_cursor(default_index)
        .prompt();

    match selection {
        Ok(selected_display) => {
            // Find the selected model by matching the display string
            // Handle the indicator which might be multi-byte (● is 3 bytes)
            let display_without_indicator =
                if let Some(stripped) = selected_display.strip_prefix("● ") {
                    stripped
                } else if let Some(stripped) = selected_display.strip_prefix("  ") {
                    stripped
                } else {
                    &selected_display
                };

            // Extract model ID from display string (format: "model_id (provider)")
            let model_id = if let Some(paren_pos) = display_without_indicator.find(" (") {
                &display_without_indicator[..paren_pos]
            } else {
                display_without_indicator
            };

            // Find model by exact ID match
            if let Some(selected_model) = models.iter().find(|model| model.id == model_id) {
                set_model(&selected_model.id).await?;
                println!("{} {}", "Model set to:".green().bold(), selected_model.id);
            } else {
                anyhow::bail!("Could not find selected model");
            }
        }
        Err(inquire::InquireError::OperationCanceled) => {
            println!("{}", "Model selection canceled.".yellow());
        }
        Err(e) => {
            return Err(anyhow::anyhow!("Selection error: {}", e));
        }
    }

    Ok(())
}

async fn set_model(model_id: &str) -> Result<()> {
    let mut config = AppConfig::load()?;
    config.set_selected_model(model_id.to_string());
    config.save()?;
    Ok(())
}

async fn show_command_menu() -> Result<Option<String>> {
    #[derive(Clone)]
    struct Command {
        name: &'static str,
        description: &'static str,
    }

    let commands = vec![
        Command {
            name: "/help",
            description: "Show help message with available commands",
        },
        Command {
            name: "/clear",
            description: "Clear chat conversation history",
        },
        Command {
            name: "/model",
            description: "Show interactive model menu or set a model",
        },
        Command {
            name: "/verbose",
            description: "Toggle verbose mode (show raw API requests/responses)",
        },
        Command {
            name: "/quit",
            description: "Exit the REPL",
        },
        Command {
            name: "/exit",
            description: "Exit the REPL",
        },
        Command {
            name: "/system",
            description: "Toggle between Developer and SysAdmin modes",
        },
    ];

    let options: Vec<String> = commands
        .iter()
        .map(|cmd| format!("{:<12} - {}", cmd.name, cmd.description))
        .collect();

    let selection = Select::new("Select a command:", options)
        .with_page_size(10)
        .prompt();

    match selection {
        Ok(selected) => {
            // Extract the command name from the selection
            if let Some(cmd) = commands.iter().find(|c| selected.starts_with(c.name)) {
                Ok(Some(cmd.name.to_string()))
            } else {
                Ok(None)
            }
        }
        Err(inquire::InquireError::OperationCanceled) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("Selection error: {}", e)),
    }
}

fn print_wrapped_response(prefix: &str, text: &str) {
    let style = match prefix {
        "assistant>" => DisplayStyle::Assistant,
        "tool>" => DisplayStyle::Tool,
        "error>" => DisplayStyle::Error,
        "system>" => DisplayStyle::System,
        "user>" => DisplayStyle::User,
        _ => DisplayStyle::System,
    };

    // Always use full-width background display
    display::print_full_width_message(&prefix[..prefix.len() - 1], text, style);
}
