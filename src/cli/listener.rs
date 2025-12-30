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
    code_block_buffer: String,
    code_block_language: Option<String>,
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
            code_block_buffer: String::new(),
            code_block_language: None,
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
        self.code_block_buffer.clear();
        self.code_block_language = None;
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
    diff_shown: AtomicBool,
}

impl CliListener {
    pub(crate) fn new() -> Self {
        Self {
            thinking: Mutex::new(ThinkingState::new()),
            text: Mutex::new(TextState::new()),
            spinner_running: Arc::new(AtomicBool::new(false)),
            spinner_message: Arc::new(Mutex::new(String::new())),
            spinner_generation: Arc::new(AtomicU64::new(0)),
            diff_shown: AtomicBool::new(false),
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
                    // If we're inside a code block (after the opening fence),
                    // buffer content instead of printing
                    if state.in_code_block {
                        if ch == '\r' {
                            continue;
                        } else if ch == '\n' {
                            // Check if this line is the closing fence
                            let is_closing_fence = state.line_buffer.trim() == "```";
                            if is_closing_fence {
                                // Exiting code block - highlight and print buffered content
                                print_highlighted_code(
                                    &state.code_block_buffer,
                                    state.code_block_language.as_deref(),
                                );
                                // Print closing fence
                                println!("{}", "```".dimmed());
                                state.in_code_block = false;
                                state.code_block_buffer.clear();
                                state.code_block_language = None;
                                state.column = 0;
                            } else {
                                // Add to buffer
                                let line = state.line_buffer.clone();
                                state.code_block_buffer.push_str(&line);
                                state.code_block_buffer.push('\n');
                            }
                            state.line_buffer.clear();
                        } else {
                            state.line_buffer.push(ch);
                        }
                        continue;
                    }

                    // Normal text processing (outside code blocks)
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
                        // Skip leading whitespace at start of line
                        if state.column == 0 && state.word_buffer.is_empty() {
                            state.line_buffer.push(ch);
                            continue;
                        }
                        state.line_buffer.push(ch);
                        self.flush_text_word(&mut state, width);
                        if state.column > 0 {
                            print!(" ");
                            state.column += 1;
                        }
                    } else if ch == '\n' {
                        // Check if this line is a code fence (opening)
                        let trimmed = state.line_buffer.trim().to_string();
                        if trimmed.starts_with("```") {
                            // Entering code block
                            state.in_code_block = true;
                            // Extract language (everything after ```)
                            let lang = trimmed.strip_prefix("```").unwrap_or("").trim();
                            state.code_block_language = if lang.is_empty() {
                                None
                            } else {
                                Some(lang.to_string())
                            };
                            // Print the opening fence dimmed (clear word_buffer, don't flush it)
                            state.word_buffer.clear();
                            if state.has_content {
                                println!();
                            }
                            println!("{}", trimmed.dimmed());
                            state.column = 0;
                        } else {
                            if !state.has_content {
                                state.line_buffer.clear();
                                continue;
                            }
                            self.flush_text_word(&mut state, width);
                            println!();
                            state.column = 0;
                        }
                        state.line_buffer.clear();
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
                // If a diff was just shown, it already displayed the checkmark
                if self.diff_shown.swap(false, Ordering::SeqCst) && !*is_error {
                    return;
                }

                let (indicator, color) = if *is_error {
                    ("✗", "31")
                } else {
                    ("✓", "32")
                };

                // Only show error details if there's actually a non-empty preview
                if *is_error
                    && let Some(preview) = error_preview
                    && !preview.trim().is_empty()
                {
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
                    println!("\x1b[90mTodo list cleared.\x1b[0m");
                } else {
                    println!("\x1b[1;36mTodo List:\x1b[0m");
                    for item in todos {
                        let (indicator, color) = match item.status {
                            TodoStatus::Pending => ("[ ]", "\x1b[90m"), // dark gray
                            TodoStatus::InProgress => ("[-]", "\x1b[33m"), // yellow
                            TodoStatus::Completed => ("[✓]", "\x1b[90m"), // dark gray
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
                path: _,
                diff,
                lines_added: _,
                lines_removed: _,
                language,
            } => {
                self.stop_spinner();
                // Print checkmark on same line as tool call, then diff on new line
                println!(" \x1b[2m\x1b[32m✓\x1b[0m");
                self.diff_shown.store(true, Ordering::SeqCst);
                render_diff_with_syntax(diff, language.as_deref());
            }

            // These events are used by the TUI but not needed for CLI output
            OutputEvent::Waiting
            | OutputEvent::Done
            | OutputEvent::Interrupted
            | OutputEvent::WorkingProgress { .. } => {}

            OutputEvent::AutoCompactStarting {
                current_usage,
                limit,
            } => {
                let pct = (*current_usage as f64 / *limit as f64) * 100.0;
                println!(
                    "\n\x1b[33mContext at {:.0}% ({}/{}) - auto-compacting...\x1b[0m",
                    pct, current_usage, limit
                );
            }

            OutputEvent::AutoCompactCompleted { messages_compacted } => {
                println!(
                    "\x1b[32mCompacted {} messages into summary.\x1b[0m",
                    messages_compacted
                );
            }
        }
    }
}

/// Render a diff with syntax highlighting and line numbers.
fn render_diff_with_syntax(diff: &str, language: Option<&str>) {
    use crate::syntax::highlight_code;
    use colored::Colorize;

    // Background colors for diff lines (subtle, not too bright)
    const ADD_BG: (u8, u8, u8) = (0, 50, 0); // dark green
    const DEL_BG: (u8, u8, u8) = (50, 0, 0); // dark red

    // Default foreground for code without syntax highlighting
    const DEFAULT_FG: (u8, u8, u8) = (200, 200, 200); // light gray

    // First pass: collect line info and parse hunk headers
    struct LineInfo<'a> {
        line_type: DiffLineType,
        code: &'a str,
        line_num: Option<usize>,
    }

    let mut line_infos: Vec<LineInfo> = Vec::new();
    let mut old_line_num = 0usize;
    let mut new_line_num = 0usize;

    for line in diff.lines() {
        if line.starts_with("@@") {
            // Parse hunk header to get starting line numbers
            if let Some((old_start, new_start)) = parse_hunk_header(line) {
                old_line_num = old_start;
                new_line_num = new_start;
            }
            // Skip rendering hunk headers
            continue;
        } else if line.starts_with("+++") || line.starts_with("---") {
            // Skip file headers
            continue;
        } else if line.starts_with('+') {
            let ln = new_line_num;
            new_line_num += 1;
            line_infos.push(LineInfo {
                line_type: DiffLineType::Add,
                code: line.get(1..).unwrap_or(""),
                line_num: Some(ln),
            });
        } else if line.starts_with('-') {
            let ln = old_line_num;
            old_line_num += 1;
            line_infos.push(LineInfo {
                line_type: DiffLineType::Del,
                code: line.get(1..).unwrap_or(""),
                line_num: Some(ln),
            });
        } else if line.starts_with(' ') {
            let ln = new_line_num; // Use new line number for context
            old_line_num += 1;
            new_line_num += 1;
            line_infos.push(LineInfo {
                line_type: DiffLineType::Context,
                code: line.get(1..).unwrap_or(""),
                line_num: Some(ln),
            });
        } else {
            // Other lines (shouldn't happen in unified diff)
            line_infos.push(LineInfo {
                line_type: DiffLineType::Context,
                code: line,
                line_num: None,
            });
        }
    }

    // Get syntax highlights if language is available
    let highlights: Option<Vec<Vec<(usize, usize, crate::syntax::Rgb)>>> = language.map(|lang| {
        line_infos
            .iter()
            .map(|info| {
                let spans = highlight_code(info.code, Some(lang));
                spans
                    .into_iter()
                    .map(|s| (s.start, s.end, s.color))
                    .collect()
            })
            .collect()
    });

    // Render each line with gutter
    for (i, info) in line_infos.iter().enumerate() {
        let line_highlights = highlights.as_ref().and_then(|h| h.get(i));

        // Print gutter: "  3 + " or "  3 - " or "  3   "
        let line_num_str = info
            .line_num
            .map(|n| format!("{:3}", n))
            .unwrap_or_else(|| "   ".to_string());
        let prefix = match info.line_type {
            DiffLineType::Add => "+",
            DiffLineType::Del => "-",
            _ => " ",
        };
        print!("{} {} ", line_num_str.bright_black(), prefix.bright_black());

        // Print code with syntax highlighting and diff background
        if let Some(spans) = line_highlights
            && !spans.is_empty()
        {
            render_highlighted_code(info.code, spans, info.line_type, ADD_BG, DEL_BG, DEFAULT_FG);
        } else {
            // No syntax highlighting - just apply diff color
            match info.line_type {
                DiffLineType::Add => print!(
                    "{}",
                    info.code
                        .truecolor(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2)
                        .on_truecolor(ADD_BG.0, ADD_BG.1, ADD_BG.2)
                ),
                DiffLineType::Del => print!(
                    "{}",
                    info.code
                        .truecolor(DEFAULT_FG.0, DEFAULT_FG.1, DEFAULT_FG.2)
                        .on_truecolor(DEL_BG.0, DEL_BG.1, DEL_BG.2)
                ),
                _ => print!("{}", info.code.dimmed()),
            }
        }
        println!();
    }
}

/// Parse a unified diff hunk header to extract starting line numbers
/// Format: @@ -old_start[,old_count] +new_start[,new_count] @@
fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let line = line.trim();
    if !line.starts_with("@@") {
        return None;
    }
    let line = line.strip_prefix("@@")?.trim_start();
    let line = line.split(" @@").next()?;

    let mut parts = line.split_whitespace();

    let old_part = parts.next()?.strip_prefix('-')?;
    let old_start: usize = old_part.split(',').next()?.parse().ok()?;

    let new_part = parts.next()?.strip_prefix('+')?;
    let new_start: usize = new_part.split(',').next()?.parse().ok()?;

    Some((old_start, new_start))
}

#[derive(Clone, Copy)]
enum DiffLineType {
    Add,
    Del,
    Context,
}

/// Render code with syntax highlighting and diff background
fn render_highlighted_code(
    code: &str,
    spans: &[(usize, usize, crate::syntax::Rgb)],
    line_type: DiffLineType,
    add_bg: (u8, u8, u8),
    del_bg: (u8, u8, u8),
    default_fg: (u8, u8, u8),
) {
    use colored::Colorize;

    let bg = match line_type {
        DiffLineType::Add => Some(add_bg),
        DiffLineType::Del => Some(del_bg),
        _ => None,
    };

    let mut pos = 0;
    for (start, end, color) in spans {
        // Print any gap before this span
        if pos < *start {
            let gap = &code[pos..*start];
            if let Some((br, bg, bb)) = bg {
                print!(
                    "{}",
                    gap.truecolor(default_fg.0, default_fg.1, default_fg.2)
                        .on_truecolor(br, bg, bb)
                );
            } else {
                print!("{}", gap.dimmed());
            }
        }

        // Print the highlighted span
        let text = &code[*start..*end];
        if let Some((br, bg, bb)) = bg {
            print!(
                "{}",
                text.truecolor(color.r, color.g, color.b)
                    .on_truecolor(br, bg, bb)
            );
        } else {
            print!("{}", text.truecolor(color.r, color.g, color.b));
        }
        pos = *end;
    }

    // Print any remaining text after the last span
    if pos < code.len() {
        let remaining = &code[pos..];
        if let Some((br, bg, bb)) = bg {
            print!(
                "{}",
                remaining
                    .truecolor(default_fg.0, default_fg.1, default_fg.2)
                    .on_truecolor(br, bg, bb)
            );
        } else {
            print!("{}", remaining.dimmed());
        }
    }
}

/// Print code with syntax highlighting (for code blocks in markdown)
fn print_highlighted_code(code: &str, language: Option<&str>) {
    use colored::Colorize;

    // Get syntax highlights
    let highlights: Vec<Vec<(usize, usize, crate::syntax::Rgb)>> = code
        .lines()
        .map(|line| {
            if let Some(lang) = language {
                crate::syntax::highlight_code(line, Some(lang))
                    .into_iter()
                    .map(|s| (s.start, s.end, s.color))
                    .collect()
            } else {
                Vec::new()
            }
        })
        .collect();

    // Print each line with highlighting
    for (i, line) in code.lines().enumerate() {
        let line_highlights = highlights.get(i);

        if let Some(spans) = line_highlights
            && !spans.is_empty()
        {
            // Print with syntax highlighting
            let mut pos = 0;
            for (start, end, color) in spans {
                // Print any gap before this span
                if pos < *start {
                    print!("{}", &line[pos..*start]);
                }
                // Print the highlighted span
                let text = &line[*start..*end];
                print!("{}", text.truecolor(color.r, color.g, color.b));
                pos = *end;
            }
            // Print any remaining text
            if pos < line.len() {
                print!("{}", &line[pos..]);
            }
            println!();
        } else {
            // No highlighting - print plain
            println!("{}", line);
        }
    }
}
