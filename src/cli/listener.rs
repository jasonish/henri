// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use colored::Colorize;
use terminal_size::terminal_size;

use crate::output::{OutputEvent, OutputListener};
use crate::tools::TodoStatus;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy, PartialEq)]
enum MarkdownStyle {
    Normal,
    Bold,
}

struct ThinkingState {
    column: usize,
    word_buffer: String,
    pending_newlines: usize,
    has_content: bool,
    style: MarkdownStyle,
    asterisk_count: usize,
    in_code_block: bool,
    line_buffer: String,
}

impl ThinkingState {
    const fn new() -> Self {
        Self {
            column: 0,
            word_buffer: String::new(),
            pending_newlines: 0,
            has_content: false,
            style: MarkdownStyle::Normal,
            asterisk_count: 0,
            in_code_block: false,
            line_buffer: String::new(),
        }
    }

    fn reset(&mut self) {
        self.column = 0;
        self.word_buffer.clear();
        self.pending_newlines = 0;
        self.has_content = false;
        self.style = MarkdownStyle::Normal;
        self.asterisk_count = 0;
        self.in_code_block = false;
        self.line_buffer.clear();
    }
}

struct TextState {
    column: usize,
    word_buffer: String,
    has_content: bool,
    style: MarkdownStyle,
    asterisk_count: usize,
    in_code_block: bool,
    line_buffer: String,
}

impl TextState {
    const fn new() -> Self {
        Self {
            column: 0,
            word_buffer: String::new(),
            has_content: false,
            style: MarkdownStyle::Normal,
            asterisk_count: 0,
            in_code_block: false,
            line_buffer: String::new(),
        }
    }

    fn reset(&mut self) {
        self.column = 0;
        self.word_buffer.clear();
        self.has_content = false;
        self.style = MarkdownStyle::Normal;
        self.asterisk_count = 0;
        self.in_code_block = false;
        self.line_buffer.clear();
    }
}

pub(crate) struct QuietListener;

impl QuietListener {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl OutputListener for QuietListener {
    fn on_event(&self, event: &OutputEvent) {
        // Only print errors to stderr, suppress everything else
        if let OutputEvent::Error(msg) = event {
            eprintln!("{}", msg);
        }
    }
}

pub(crate) struct CliListener {
    thinking: Mutex<ThinkingState>,
    text: Mutex<TextState>,
    spinner_running: Arc<AtomicBool>,
    spinner_message: Arc<Mutex<String>>,
    spinner_generation: Arc<AtomicU64>,
}

impl CliListener {
    pub(crate) fn new() -> Self {
        Self {
            thinking: Mutex::new(ThinkingState::new()),
            text: Mutex::new(TextState::new()),
            spinner_running: Arc::new(AtomicBool::new(false)),
            spinner_message: Arc::new(Mutex::new(String::new())),
            spinner_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn terminal_width() -> usize {
        terminal_size().map(|(w, _)| w.0 as usize).unwrap_or(80)
    }

    fn flush_thinking_word(&self, state: &mut ThinkingState, width: usize) {
        if state.word_buffer.is_empty() {
            return;
        }

        let word_len = state.word_buffer.len();

        // Only word wrap if not in a code block
        if !state.in_code_block && state.column + word_len >= width && state.column > 0 {
            println!();
            state.column = 0;
        }

        let styled = match state.style {
            MarkdownStyle::Bold => state.word_buffer.bright_black().bold(),
            MarkdownStyle::Normal => state.word_buffer.bright_black(),
        };
        print!("{}", styled);

        state.column += word_len;
        state.word_buffer.clear();
    }

    fn flush_text_word(&self, state: &mut TextState, width: usize) {
        if state.word_buffer.is_empty() {
            return;
        }

        let word_len = state.word_buffer.len();

        // Only word wrap if not in a code block
        if !state.in_code_block && state.column + word_len >= width && state.column > 0 {
            println!();
            state.column = 0;
        }

        match state.style {
            MarkdownStyle::Bold => print!("{}", state.word_buffer.bold()),
            MarkdownStyle::Normal => print!("{}", state.word_buffer),
        }
        state.column += word_len;
        state.word_buffer.clear();
    }

    fn stop_spinner(&self) {
        if self.spinner_running.swap(false, Ordering::SeqCst) {
            let msg_len = self
                .spinner_message
                .lock()
                .expect("spinner mutex poisoned")
                .len();
            let clear_len = msg_len + 3;
            let mut stdout = io::stdout();
            print!("\r{}\r", " ".repeat(clear_len));
            let _ = stdout.flush();
        }
    }
}

impl OutputListener for CliListener {
    fn on_event(&self, event: &OutputEvent) {
        match event {
            OutputEvent::ThinkingStart => {
                self.stop_spinner();
                if let Ok(mut state) = self.thinking.lock() {
                    state.reset();
                }
            }

            OutputEvent::Thinking(text) => {
                let width = Self::terminal_width();
                let Ok(mut state) = self.thinking.lock() else {
                    print!("{}", text.bright_black());
                    io::stdout().flush().ok();
                    return;
                };

                for ch in text.chars() {
                    if ch == '*' {
                        if !state.word_buffer.is_empty() {
                            self.flush_thinking_word(&mut state, width);
                        }
                        state.asterisk_count += 1;
                        continue;
                    }

                    if state.asterisk_count > 0 {
                        if state.asterisk_count >= 2 {
                            state.style = if state.style == MarkdownStyle::Bold {
                                MarkdownStyle::Normal
                            } else {
                                MarkdownStyle::Bold
                            };
                        } else {
                            state.word_buffer.push('*');
                        }
                        state.asterisk_count = 0;
                    }

                    if ch == '\r' {
                        continue;
                    } else if ch == ' ' || ch == '\t' {
                        // Skip leading whitespace at start of line - but NOT in code blocks
                        if state.column == 0 && state.word_buffer.is_empty() && !state.in_code_block
                        {
                            continue;
                        }
                        state.line_buffer.push(ch);
                        self.flush_thinking_word(&mut state, width);
                        // In code blocks, always print spaces (even at start of line)
                        // Outside code blocks, only print if not at start of line
                        if state.in_code_block || state.column > 0 {
                            print!(" ");
                            state.column += 1;
                        }
                    } else if ch == '\n' {
                        // Check if this line was a code fence
                        let trimmed = state.line_buffer.trim();
                        if trimmed.starts_with("```") {
                            state.in_code_block = !state.in_code_block;
                        }
                        state.line_buffer.clear();

                        if !state.has_content {
                            continue;
                        }
                        self.flush_thinking_word(&mut state, width);

                        if state.in_code_block {
                            println!();
                            state.column = 0;
                        } else if state.pending_newlines < 2 {
                            state.pending_newlines += 1;
                        }
                    } else {
                        if !state.has_content {
                            state.column = 0;
                        }
                        if !state.in_code_block {
                            for _ in 0..state.pending_newlines {
                                println!();
                                state.column = 0;
                            }
                        }
                        state.pending_newlines = 0;
                        state.has_content = true;
                        state.line_buffer.push(ch);
                        state.word_buffer.push(ch);
                    }
                }

                io::stdout().flush().ok();
            }

            OutputEvent::ThinkingEnd => {
                let needs_newline = if let Ok(mut state) = self.thinking.lock() {
                    let width = Self::terminal_width();
                    self.flush_thinking_word(&mut state, width);
                    let needs = state.has_content && state.column > 0;
                    state.reset();
                    needs
                } else {
                    false
                };
                if needs_newline {
                    println!();
                }
            }

            OutputEvent::Text(text) => {
                self.stop_spinner();
                let width = Self::terminal_width();

                let Ok(mut state) = self.text.lock() else {
                    print!("{}", text);
                    io::stdout().flush().ok();
                    return;
                };

                for ch in text.chars() {
                    if ch == '*' {
                        if !state.word_buffer.is_empty() {
                            self.flush_text_word(&mut state, width);
                        }
                        state.asterisk_count += 1;
                        continue;
                    }

                    if state.asterisk_count > 0 {
                        if state.asterisk_count >= 2 {
                            state.style = if state.style == MarkdownStyle::Bold {
                                MarkdownStyle::Normal
                            } else {
                                MarkdownStyle::Bold
                            };
                        } else {
                            state.word_buffer.push('*');
                        }
                        state.asterisk_count = 0;
                    }

                    if ch == '\r' {
                        continue;
                    } else if ch == ' ' || ch == '\t' {
                        // Skip leading whitespace at start of line - but NOT in code blocks
                        if state.column == 0 && state.word_buffer.is_empty() && !state.in_code_block
                        {
                            continue;
                        }
                        state.line_buffer.push(ch);
                        self.flush_text_word(&mut state, width);
                        // In code blocks, always print spaces (even at start of line)
                        // Outside code blocks, only print if not at start of line
                        if state.in_code_block || state.column > 0 {
                            print!(" ");
                            state.column += 1;
                        }
                    } else if ch == '\n' {
                        // Check if this line was a code fence
                        let trimmed = state.line_buffer.trim();
                        if trimmed.starts_with("```") {
                            state.in_code_block = !state.in_code_block;
                        }
                        state.line_buffer.clear();

                        if !state.has_content {
                            continue;
                        }
                        self.flush_text_word(&mut state, width);
                        println!();
                        state.column = 0;
                    } else {
                        state.has_content = true;
                        state.line_buffer.push(ch);
                        state.word_buffer.push(ch);
                    }
                }

                io::stdout().flush().ok();
            }

            OutputEvent::TextEnd => {
                let needs_newline = if let Ok(mut state) = self.text.lock() {
                    let width = Self::terminal_width();
                    self.flush_text_word(&mut state, width);
                    let needs = state.has_content && state.column > 0;
                    state.reset();
                    needs
                } else {
                    false
                };
                if needs_newline {
                    println!();
                }
            }

            OutputEvent::ToolCall { description, .. } => {
                self.stop_spinner();

                print!("\x1b[2m▶ {}\x1b[0m", description);
                io::stdout().flush().ok();
            }

            OutputEvent::ToolResult {
                is_error,
                error_preview,
            } => {
                let (indicator, color) = if *is_error {
                    ("✗", "31")
                } else {
                    ("✓", "32")
                };

                if *is_error && let Some(preview) = error_preview {
                    println!(
                        "\n\x1b[2m\x1b[{}m{}\x1b[0m\x1b[2m Error: {}\x1b[0m",
                        color, indicator, preview
                    );
                } else {
                    println!(" \x1b[2m\x1b[{}m{}\x1b[0m", color, indicator);
                }
            }

            OutputEvent::SpinnerStart(message) => {
                let generation = self.spinner_generation.fetch_add(1, Ordering::SeqCst) + 1;
                self.spinner_running.store(true, Ordering::SeqCst);
                *self.spinner_message.lock().expect("spinner mutex poisoned") = message.clone();

                let spinner_running = self.spinner_running.clone();
                let spinner_generation = self.spinner_generation.clone();
                let spinner_message = self.spinner_message.clone();

                tokio::spawn(async move {
                    let mut frame = 0;
                    let mut stdout = io::stdout();

                    while spinner_running.load(Ordering::SeqCst)
                        && spinner_generation.load(Ordering::SeqCst) == generation
                    {
                        let spinner_char = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
                        print!(
                            "\r{} {}",
                            spinner_char.to_string().cyan(),
                            spinner_message
                                .lock()
                                .expect("spinner mutex poisoned")
                                .bright_black()
                        );
                        let _ = stdout.flush();
                        frame += 1;
                        tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
                    }
                });
            }

            OutputEvent::SpinnerStop => {
                self.stop_spinner();
            }

            OutputEvent::Info(msg) => {
                println!("{}", msg);
            }

            OutputEvent::Error(msg) => {
                eprintln!("{}", msg);
            }

            OutputEvent::TodoList { todos } => {
                self.stop_spinner();
                if todos.is_empty() {
                    println!("\x1b[2mTodo list cleared.\x1b[0m");
                } else {
                    println!("\x1b[2mTodo List:\x1b[0m");
                    for item in todos {
                        let (indicator, color) = match item.status {
                            TodoStatus::Pending => ("○", "\x1b[2m"),     // dim
                            TodoStatus::InProgress => ("◐", "\x1b[36m"), // cyan
                            TodoStatus::Completed => ("●", "\x1b[32m"),  // green
                        };
                        let text = match item.status {
                            TodoStatus::InProgress => &item.active_form,
                            _ => &item.content,
                        };
                        println!("{}  {} {}\x1b[0m", color, indicator, text);
                    }
                }
            }

            OutputEvent::FileDiff {
                path,
                diff,
                lines_added,
                lines_removed,
            } => {
                self.stop_spinner();
                println!(
                    "{} {} {}",
                    path.bright_black().bold(),
                    format!("+{}", lines_added).green(),
                    format!("-{}", lines_removed).red()
                );
                for line in diff.lines() {
                    if line.starts_with('+') && !line.starts_with("+++") {
                        println!("{}", line.green());
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        println!("{}", line.red());
                    } else if line.starts_with("@@") {
                        println!("{}", line.cyan());
                    } else {
                        println!("{}", line.dimmed());
                    }
                }
            }

            // These events are used by the TUI but not needed for CLI output
            OutputEvent::Waiting
            | OutputEvent::Done
            | OutputEvent::Interrupted
            | OutputEvent::WorkingProgress { .. } => {}
        }
    }
}
