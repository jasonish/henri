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
mod utils;

use chat::ChatConversation;
use config::Config as AppConfig;
use display::{DisplayStyle, clear_lines, print_full_width_message};
use history::History;
use llm::get_github_copilot_models;

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
            "/quit", "/exit", "/help", "/clear", "/model", "/models", "/verbose", "/json",
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
                "/quit", "/exit", "/help", "/clear", "/model", "/models", "/verbose",
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
}

#[derive(Subcommand)]
enum Commands {
    /// Login to model providers
    Login,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Login) => {
            auth::login().await?;
        }
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

    // Initialize chat conversation
    let mut conversation = ChatConversation::default();

    println!("Welcome to the LLM Chat! Type '/quit' or '/exit' to quit.");
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
        // Display token usage before the prompt if available
        if let Some(usage) = last_usage.take() {
            llm::LLMClient::log_token_usage(&usage);
        }

        let prompt = if multi_line_buffer.is_empty() {
            ">> "
        } else {
            ".. "
        };
        let readline = rl.readline(prompt);
        match readline {
            Ok(line) => {
                // Count this line
                input_line_count += 1;
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
                    println!("  /model [model_name] - Show interactive model menu or set a model");
                    println!("  /models  - Query available models from the API");
                    println!(
                        "  /json    - Enter JSON mode to send raw JSON to the LLM API (use /exit to leave)"
                    );
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
                } else if line.trim() == "/models" {
                    handle_models_query(&mut llm_client).await;
                    input_line_count = 0;
                    continue;
                } else if line.trim().starts_with("/model") {
                    handle_model_command(line.trim()).await;
                    input_line_count = 0;
                    continue;
                } else if line.trim() == "/json" {
                    println!("📝 JSON mode enabled. Send raw JSON to the LLM.");
                    println!(
                        "   Example: {{\"messages\": [{{\"role\": \"user\", \"content\": \"Hello\"}}], \"model\": \"gpt-4o\"}}"
                    );
                    println!("   Type '/exit' or press Ctrl+C to exit JSON mode.");
                    println!();

                    // Enter JSON mode loop
                    'json_mode: loop {
                        let mut json_input = String::new();
                        let mut empty_line_count = 0;

                        // Collect multi-line JSON input
                        loop {
                            let json_line = rl.readline("JSON> ");
                            match json_line {
                                Ok(line) => {
                                    // Check for exit command
                                    if line.trim() == "/exit" {
                                        println!("Exiting JSON mode.");
                                        break 'json_mode;
                                    }

                                    // Add to history
                                    if !line.trim().is_empty() {
                                        history.add_entry(line.clone(), false);
                                    }

                                    if line.trim().is_empty() {
                                        empty_line_count += 1;
                                        if empty_line_count >= 2 {
                                            break;
                                        }
                                    } else {
                                        empty_line_count = 0;
                                    }
                                    json_input.push_str(&line);
                                    json_input.push('\n');
                                }
                                Err(ReadlineError::Interrupted) => {
                                    println!("Exiting JSON mode.");
                                    break 'json_mode;
                                }
                                Err(ReadlineError::Eof) => {
                                    println!("Exiting JSON mode.");
                                    break 'json_mode;
                                }
                                Err(err) => {
                                    eprintln!("Error reading JSON: {err}");
                                    continue;
                                }
                            }
                        }

                        if json_input.trim().is_empty() {
                            continue;
                        }

                        // Send raw JSON to LLM
                        if let Some(ref mut client) = llm_client {
                            match client.send_raw_json_request(&json_input, verbose).await {
                                Ok((response, usage)) => {
                                    println!();
                                    print_full_width_message(
                                        "Assistant",
                                        &response,
                                        DisplayStyle::Assistant,
                                    );
                                    if let Some(usage) = usage {
                                        last_usage = Some(usage);
                                    }
                                    println!();
                                }
                                Err(e) => {
                                    print_full_width_message(
                                        "Error",
                                        &format!("Failed to send JSON request: {e}"),
                                        DisplayStyle::Error,
                                    );
                                }
                            }
                        } else {
                            print_full_width_message(
                                "Error",
                                "No LLM provider configured. Run 'henri login' to authenticate.",
                                DisplayStyle::Error,
                            );
                        }
                    }
                    input_line_count = 0;
                    continue;
                } else if line.trim().starts_with('/') && !line.trim().is_empty() {
                    let command = line.trim();
                    if ![
                        "/quit", "/exit", "/help", "/clear", "/verbose", "/models", "/json",
                    ]
                    .contains(&command)
                        && !command.starts_with("/model")
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
    _client: &mut llm::LLMClient,
    conversation: &mut ChatConversation,
    response: String,
    _verbose: bool,
) -> Option<llm::CopilotUsage> {
    // Just add the response to conversation and display it
    conversation.add_assistant_message(response.clone());
    print_wrapped_response("assistant>", &response);
    None
}

async fn handle_user_input(
    llm_client: &mut Option<llm::LLMClient>,
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
            Ok((response, usage)) => {
                handle_llm_response(client, conversation, response, verbose).await;
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

    match parts.len() {
        1 => {
            // Show interactive model selection menu
            match show_model_selection_menu().await {
                Ok(()) => {
                    // Model was selected and saved
                }
                Err(e) => {
                    eprintln!("{} Model selection failed: {e}", "error>".red().bold());
                }
            }
        }
        2 => {
            // Set model directly
            let model_name = parts[1];
            match set_model(model_name).await {
                Ok(()) => {
                    println!("{} {}", "Model set to:".green().bold(), model_name);
                }
                Err(e) => {
                    eprintln!("{} Failed to set model: {e}", "error>".red().bold());
                }
            }
        }
        _ => {
            println!("{}", "Usage: /model [model_name]".yellow());
            println!("  /model          - Show interactive model selection");
            println!("  /model <name>   - Set model to <name>");
        }
    }
}

async fn show_model_selection_menu() -> Result<()> {
    // Load current config to see what's selected
    let config = AppConfig::load()?;
    let current_model = config.get_selected_model();

    // Check if we have GitHub Copilot configured
    let has_github_copilot = config.providers.github_copilot.is_some();

    if !has_github_copilot {
        println!(
            "{}",
            "No LLM providers configured. Run 'henri login' to authenticate.".yellow()
        );
        return Ok(());
    }

    // Create a temporary LLM client to query models
    let mut temp_client = config
        .providers
        .github_copilot
        .clone()
        .map(|github_config| llm::LLMClient::new(github_config, false));

    // Get available models (will try API first, then fall back to defaults)
    let models = get_github_copilot_models(temp_client.as_mut()).await;

    // Create display options with current selection indicator
    let options: Vec<String> = models
        .iter()
        .map(|model| {
            let indicator = if current_model == Some(&model.id) {
                "● "
            } else {
                "  "
            };
            format!("{}{}", indicator, model.name)
        })
        .collect();

    // Show current selection
    if let Some(current) = current_model {
        if let Some(current_model_info) = models.iter().find(|m| m.id == *current) {
            println!(
                "{} {}",
                "Current model:".green().bold(),
                current_model_info.name,
            );
        } else {
            println!("{} {} (unknown)", "Current model:".green().bold(), current);
        }
        println!();
    }

    // Show the selection menu
    let selection = Select::new("Select a model:", options)
        .with_page_size(10)
        .prompt();

    match selection {
        Ok(selected_display) => {
            // Find the selected model by matching the display string
            if let Some(selected_model) = models.iter().find(|model| {
                let display = format!("  {}", model.name);
                selected_display.ends_with(&display[2..]) // Skip the indicator part
            }) {
                set_model(&selected_model.id).await?;
                println!("{} {}", "Model set to:".green().bold(), selected_model.name,);
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

async fn handle_models_query(llm_client: &mut Option<llm::LLMClient>) {
    // Get the list of available models
    let models = llm::get_github_copilot_models(llm_client.as_mut()).await;

    println!("{}", "Available models:".green().bold());
    for model in models {
        println!("  - {}", model.id.cyan());
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
