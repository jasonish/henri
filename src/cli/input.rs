// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crossterm::cursor::{self, MoveToColumn, MoveUp};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType, disable_raw_mode, enable_raw_mode};
use unicode_width::UnicodeWidthChar;

use crate::commands::{self, DynamicSlashCommand};
use crate::custom_commands::CustomCommand;
use crate::history::FileHistory;

const PROMPT: &str = "› ";

pub(crate) struct PromptInfo {
    pub provider: String,
    pub model: String,
    pub path: String,
    pub git_branch: Option<String>,
    pub thinking_available: bool,
    pub show_thinking_status: bool,
}

pub(crate) enum PromptOutcome {
    Submitted {
        content: String,
        pasted_images: Vec<PastedImage>,
    },
    Interrupted,
    Eof,
    SelectModel,
}

pub(crate) struct PastedImage {
    pub marker: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

enum ClipboardPayload {
    Image { mime_type: String, data: Vec<u8> },
    Text(String),
}

pub(crate) struct PromptUi {
    buffer: String,
    cursor: usize,
    history_index: Option<usize>,
    draft: String,
    rendered_lines: u16,
    last_cursor_row: u16,
    rendered_menu_lines: u16,
    menu_index: usize,
    last_esc: Option<Instant>,
    pasted_images: Vec<PastedImage>,
    next_image_id: usize,
    custom_commands: Vec<CustomCommand>,
    // File completion state
    file_completer: crate::completion::FileCompleter,
    working_dir: std::path::PathBuf,
}

impl PromptUi {
    pub(crate) fn new(
        custom_commands: Vec<CustomCommand>,
        working_dir: std::path::PathBuf,
    ) -> Self {
        let working_dir = if working_dir.as_os_str().is_empty() {
            std::env::current_dir().unwrap_or_default()
        } else {
            working_dir
        };
        Self {
            buffer: String::new(),
            cursor: 0,
            history_index: None,
            draft: String::new(),
            rendered_lines: 1,
            last_cursor_row: 0,
            rendered_menu_lines: 0,
            menu_index: 0,
            last_esc: None,
            pasted_images: Vec::new(),
            next_image_id: 1,
            custom_commands,
            file_completer: crate::completion::FileCompleter::new(working_dir.clone()),
            working_dir,
        }
    }

    pub(crate) fn read<F>(
        &mut self,
        info: &mut PromptInfo,
        thinking_enabled: &mut bool,
        history: &mut FileHistory,
        mut cycle_favorite: F,
    ) -> io::Result<PromptOutcome>
    where
        F: FnMut() -> Option<(String, String, bool)>,
    {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
        self.rendered_lines = 1;
        self.last_cursor_row = 0;
        self.rendered_menu_lines = 0;
        self.menu_index = 0;
        self.pasted_images.clear();
        self.next_image_id = 1;
        self.file_completer = crate::completion::FileCompleter::new(self.working_dir.clone());

        let _raw = RawModeGuard::new()?;

        self.render_status_bar(info, *thinking_enabled)?;
        crossterm::execute!(io::stdout(), Print("\r\n"))?;

        self.render(info)?;

        loop {
            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        if let Some(outcome) = self.handle_key(
                            key,
                            history,
                            info,
                            thinking_enabled,
                            &mut cycle_favorite,
                        )? {
                            self.finalize_display()?;
                            return Ok(outcome);
                        }
                        self.render(info)?;
                    }
                    Event::Paste(pasted) => {
                        let normalized = normalize_paste_text(&pasted);
                        self.insert_str(&normalized);
                        self.render(info)?;
                    }
                    Event::Resize(_, _) => {
                        // Full redraw on resize to avoid rendering artifacts
                        let mut stdout = io::stdout();
                        crossterm::execute!(stdout, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
                        self.render_status_bar(info, *thinking_enabled)?;
                        crossterm::execute!(stdout, Print("\r\n"))?;
                        self.rendered_lines = 1;
                        self.last_cursor_row = 0;
                        self.rendered_menu_lines = 0;
                        self.render(info)?;
                    }
                    _ => {}
                }
            }
        }
    }

    fn render_status_bar(&self, info: &PromptInfo, thinking_enabled: bool) -> io::Result<()> {
        let mut stdout = io::stdout();

        crossterm::execute!(
            stdout,
            SetForegroundColor(Color::Magenta),
            Print(&info.provider),
            SetForegroundColor(Color::DarkGrey),
            Print("/"),
            SetForegroundColor(Color::Cyan),
            Print(&info.model),
            Print(" "),
            SetForegroundColor(Color::Blue),
            Print(&info.path),
            ResetColor
        )?;
        if let Some(branch) = &info.git_branch {
            crossterm::execute!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print(" on "),
                SetForegroundColor(Color::Green),
                Print(""),
                Print(branch),
                ResetColor
            )?;
        }

        // Display thinking status
        if info.show_thinking_status {
            let (thinking_status, thinking_color) = if !info.thinking_available {
                ("thinking n/a", Color::DarkGrey)
            } else if thinking_enabled {
                ("thinking on", Color::Green)
            } else {
                ("thinking off", Color::Yellow)
            };
            crossterm::execute!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print(" ["),
                SetForegroundColor(thinking_color),
                Print(thinking_status),
                SetForegroundColor(Color::DarkGrey),
                Print("]"),
                ResetColor
            )?;
        }

        // Clear to end of line to remove any stale content
        crossterm::execute!(stdout, Clear(ClearType::UntilNewLine))?;

        stdout.flush()?;
        Ok(())
    }

    fn filtered_commands(&mut self, info: &PromptInfo) -> Vec<DynamicSlashCommand> {
        if !self.buffer.starts_with('/') {
            return vec![];
        }

        // Reload custom commands to pick up any newly added files
        self.custom_commands =
            crate::custom_commands::load_custom_commands().unwrap_or_else(|_| Vec::new());

        let query = self.buffer.trim_start_matches('/');
        let is_claude = info.provider == "claude";
        let has_claude_oauth = crate::commands::has_claude_oauth_provider();
        commands::filter_commands(
            query,
            false,
            is_claude,
            has_claude_oauth,
            &self.custom_commands,
        )
    }

    fn menu_visible(&mut self, info: &PromptInfo) -> bool {
        self.buffer.starts_with('/')
            && !self.buffer.contains(' ')
            && !self.buffer.contains('\n')
            && !self.filtered_commands(info).is_empty()
    }

    fn render(&mut self, info: &PromptInfo) -> io::Result<()> {
        let mut stdout = io::stdout();
        let (term_width, _) = terminal::size()?;

        let prompt_width = display_width(PROMPT);
        let margin: String = " ".repeat(prompt_width);

        // Build the entire output string first to minimize screen updates
        let mut cursor_col = 0u16;
        let mut cursor_row = 0u16;
        let mut current_col = prompt_width as u16;
        let mut current_row = 0u16;

        // Pre-calculate input content with line tracking and word-wrapping
        let mut input_content = String::new();
        let chars: Vec<(usize, char)> = self.buffer.char_indices().collect();
        let mut i = 0;

        while i < chars.len() {
            let (byte_idx, ch) = chars[i];

            if byte_idx == self.cursor {
                cursor_col = current_col;
                cursor_row = current_row;
            }

            if ch == '\n' {
                // Pad to end of line to overwrite stale characters
                while current_col < term_width {
                    input_content.push(' ');
                    current_col += 1;
                }
                input_content.push_str("\r\n");
                input_content.push_str(&margin);
                current_row += 1;
                current_col = prompt_width as u16;
                i += 1;
            } else if ch.is_whitespace() {
                // Output whitespace characters directly
                let char_width = UnicodeWidthChar::width(ch).unwrap_or(1) as u16;

                if current_col + char_width > term_width {
                    input_content.push_str("\r\n");
                    input_content.push_str(&margin);
                    current_row += 1;
                    current_col = prompt_width as u16;
                }

                input_content.push(ch);
                current_col += char_width;
                i += 1;
            } else {
                // Non-whitespace: calculate word width for word-wrapping
                let mut word_width = 0u16;
                let mut j = i;
                while j < chars.len() && !chars[j].1.is_whitespace() && chars[j].1 != '\n' {
                    word_width += UnicodeWidthChar::width(chars[j].1).unwrap_or(1) as u16;
                    j += 1;
                }

                // Check if word fits on current line
                let available_width = term_width.saturating_sub(prompt_width as u16);
                let should_wrap = current_col + word_width > term_width
                    && current_col > prompt_width as u16
                    && word_width <= available_width;

                if should_wrap {
                    // Wrap before the word
                    while current_col < term_width {
                        input_content.push(' ');
                        current_col += 1;
                    }
                    input_content.push_str("\r\n");
                    input_content.push_str(&margin);
                    current_row += 1;
                    current_col = prompt_width as u16;

                    // Update cursor position if it's at current character
                    if byte_idx == self.cursor {
                        cursor_col = current_col;
                        cursor_row = current_row;
                    }
                }

                // Output the character
                let char_width = UnicodeWidthChar::width(ch).unwrap_or(1) as u16;

                // Handle very long words that don't fit on a line
                if current_col + char_width > term_width {
                    input_content.push_str("\r\n");
                    input_content.push_str(&margin);
                    current_row += 1;
                    current_col = prompt_width as u16;
                }

                input_content.push(ch);
                current_col += char_width;
                i += 1;
            }
        }

        // Handle cursor at end of buffer
        if self.cursor >= self.buffer.len() {
            cursor_col = current_col;
            cursor_row = current_row;
        }

        // Pad the final input line to overwrite any stale text
        while current_col < term_width {
            input_content.push(' ');
            current_col += 1;
        }

        let input_lines = current_row + 1;

        // Determine menu visibility and size
        let menu_is_visible = self.menu_visible(info);
        let filtered = self.filtered_commands(info);

        // Determine completion menu visibility (only show if slash menu is not visible)
        let completion_is_visible = !menu_is_visible && self.completion_active();
        let completion_items: Vec<&String> = if completion_is_visible {
            let total = self.file_completer.matches.len();
            let visible = total.min(crate::completion::COMPLETION_MENU_MAX_VISIBLE);
            let selected = self.file_completer.index.min(total.saturating_sub(1));
            let max_start = total.saturating_sub(visible);
            let start = selected
                .saturating_sub(visible.saturating_sub(1))
                .min(max_start);
            let end = (start + visible).min(total);
            self.file_completer.matches[start..end].iter().collect()
        } else {
            Vec::new()
        };
        let completion_selected_in_view = if completion_is_visible {
            let total = self.file_completer.matches.len();
            let visible = total.min(crate::completion::COMPLETION_MENU_MAX_VISIBLE);
            let selected = self.file_completer.index.min(total.saturating_sub(1));
            let max_start = total.saturating_sub(visible);
            let start = selected
                .saturating_sub(visible.saturating_sub(1))
                .min(max_start);
            selected.saturating_sub(start)
        } else {
            0
        };

        let new_menu_lines = if menu_is_visible {
            filtered.len() as u16
        } else if completion_is_visible {
            completion_items.len() as u16
        } else {
            0
        };

        // Hide cursor during update to reduce flicker
        crossterm::queue!(stdout, cursor::Hide)?;

        // Move back to the start of the previously rendered input
        // The cursor is always left in the input area at last_cursor_row
        if self.last_cursor_row > 0 {
            crossterm::queue!(stdout, MoveUp(self.last_cursor_row))?;
        }
        crossterm::queue!(stdout, MoveToColumn(0))?;

        // Render input line (already padded to full width)
        crossterm::queue!(
            stdout,
            SetForegroundColor(Color::Green),
            Print(PROMPT),
            ResetColor,
            Print(&input_content)
        )?;

        // Clear any leftover lines from a previously larger input
        let input_lines_cleared = if self.rendered_lines > input_lines {
            let lines_to_clear = self.rendered_lines - input_lines;
            for _ in 0..lines_to_clear {
                crossterm::queue!(stdout, Print("\r\n"), Clear(ClearType::CurrentLine))?;
            }
            lines_to_clear
        } else {
            0
        };

        // Render menu if visible
        if menu_is_visible {
            // Calculate alignment - find the longest command name (add 1 for "/" prefix)
            let max_name_width = filtered
                .iter()
                .map(|cmd| display_width(&cmd.name) + 1)
                .max()
                .unwrap_or(0);
            let name_padding = max_name_width + 2;

            for (i, cmd) in filtered.iter().enumerate() {
                crossterm::queue!(stdout, Print("\r\n"))?;

                let (bg_color, name_color, desc_color) = if i == self.menu_index {
                    (
                        Color::Rgb {
                            r: 30,
                            g: 30,
                            b: 30,
                        },
                        Color::Rgb {
                            r: 137,
                            g: 180,
                            b: 250,
                        },
                        Color::Rgb {
                            r: 200,
                            g: 200,
                            b: 200,
                        },
                    )
                } else {
                    (
                        Color::Rgb {
                            r: 20,
                            g: 20,
                            b: 20,
                        },
                        Color::Rgb {
                            r: 120,
                            g: 120,
                            b: 120,
                        },
                        Color::Rgb {
                            r: 150,
                            g: 150,
                            b: 150,
                        },
                    )
                };

                // Add 1 for "/" prefix
                let name_width = display_width(&cmd.name) + 1;
                let padding_needed = name_padding.saturating_sub(name_width);
                let desc_width = display_width(&cmd.description);
                let content_width = 1 + name_width + padding_needed + desc_width;
                let remaining_space = term_width.saturating_sub(content_width as u16) as usize;

                // Build the entire menu line as a single string for efficiency
                let padding_str: String = " ".repeat(padding_needed);
                let trailing_str: String = " ".repeat(remaining_space);

                crossterm::queue!(
                    stdout,
                    SetBackgroundColor(bg_color),
                    Print(" /"),
                    SetForegroundColor(name_color),
                    Print(&cmd.name),
                    Print(&padding_str),
                    SetForegroundColor(desc_color),
                    Print(&cmd.description),
                    Print(&trailing_str),
                    ResetColor
                )?;
            }
        }

        // Render completion menu if visible (and slash menu is not)
        if completion_is_visible {
            for (i, path) in completion_items.iter().enumerate() {
                crossterm::queue!(stdout, Print("\r\n"))?;

                let is_selected = i == completion_selected_in_view;
                let is_dir = path.ends_with('/');

                let (bg_color, text_color) = if is_selected {
                    (
                        Color::Rgb {
                            r: 30,
                            g: 30,
                            b: 30,
                        },
                        Color::Cyan,
                    )
                } else if is_dir {
                    (
                        Color::Rgb {
                            r: 20,
                            g: 20,
                            b: 20,
                        },
                        Color::Blue,
                    )
                } else {
                    (
                        Color::Rgb {
                            r: 20,
                            g: 20,
                            b: 20,
                        },
                        Color::Rgb {
                            r: 150,
                            g: 150,
                            b: 150,
                        },
                    )
                };

                let prefix = if is_selected { ">" } else { " " };
                let path_width = display_width(path);
                let content_width = 2 + path_width; // prefix + space + path
                let remaining_space = term_width.saturating_sub(content_width as u16) as usize;
                let trailing_str: String = " ".repeat(remaining_space);

                crossterm::queue!(
                    stdout,
                    SetBackgroundColor(bg_color),
                    SetForegroundColor(text_color),
                    Print(prefix),
                    Print(" "),
                    Print(*path),
                    Print(&trailing_str),
                    ResetColor
                )?;
            }
        }

        // Clear any leftover lines from a previously larger menu
        let lines_cleared = if self.rendered_menu_lines > new_menu_lines {
            let lines_to_clear = self.rendered_menu_lines - new_menu_lines;
            for _ in 0..lines_to_clear {
                crossterm::queue!(stdout, Print("\r\n"), Clear(ClearType::CurrentLine))?;
            }
            lines_to_clear
        } else {
            0
        };

        // Store rendered state
        self.rendered_lines = input_lines;
        self.last_cursor_row = cursor_row;
        self.rendered_menu_lines = new_menu_lines;

        // Position cursor back to input area
        // We're currently at end of: input content + cleared input lines + menu lines + cleared menu lines
        let lines_to_go_up =
            (input_lines - 1 - cursor_row) + input_lines_cleared + new_menu_lines + lines_cleared;
        if lines_to_go_up > 0 {
            crossterm::queue!(stdout, MoveUp(lines_to_go_up))?;
        }
        crossterm::queue!(stdout, MoveToColumn(cursor_col), cursor::Show)?;

        stdout.flush()?;
        Ok(())
    }

    fn finalize_display(&self) -> io::Result<()> {
        let mut stdout = io::stdout();

        // Move to the last line of the input to ensure we don't clear content when printing \r\n
        let lines_down = self
            .rendered_lines
            .saturating_sub(1)
            .saturating_sub(self.last_cursor_row);
        if lines_down > 0 {
            crossterm::execute!(stdout, cursor::MoveDown(lines_down))?;
        }

        // Clear any menu below the input before printing newline
        crossterm::execute!(stdout, Clear(ClearType::FromCursorDown), Print("\r\n"))?;
        stdout.flush()?;
        Ok(())
    }

    fn handle_key<F>(
        &mut self,
        key: crossterm::event::KeyEvent,
        history: &mut FileHistory,
        info: &mut PromptInfo,
        thinking_enabled: &mut bool,
        cycle_favorite: &mut F,
    ) -> io::Result<Option<PromptOutcome>>
    where
        F: FnMut() -> Option<(String, String, bool)>,
    {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if self.buffer.is_empty() && self.pasted_images.is_empty() {
                return Ok(Some(PromptOutcome::Eof));
            }
            return Ok(Some(PromptOutcome::Interrupted));
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
            if self.buffer.is_empty() {
                return Ok(Some(PromptOutcome::Eof));
            }
            self.delete_forward();
            return Ok(None);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('v') {
            self.handle_clipboard_paste();
            return Ok(None);
        }

        let menu_visible = self.menu_visible(info);
        let completion_visible = self.completion_active();

        match key.code {
            KeyCode::Enter => {
                if menu_visible {
                    // Complete the selected command and submit
                    let _ = self.complete_command(info);
                    let submission = self.build_command_submission(history);
                    return Ok(Some(submission));
                } else if completion_visible {
                    // Apply completion and close menu
                    self.apply_completion();
                } else if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::CONTROL)
                {
                    self.insert_newline_with_indent();
                } else if !self.buffer.trim().is_empty() {
                    let submission = self.build_submission(history);
                    return Ok(Some(submission));
                }
            }
            KeyCode::Tab => {
                if menu_visible {
                    let auto_execute = self.complete_command(info);
                    if auto_execute {
                        let submission = self.build_command_submission(history);
                        return Ok(Some(submission));
                    }
                } else if completion_visible {
                    // Cycle to next completion
                    self.move_completion(1);
                } else {
                    // Try file completion
                    self.init_completion();
                    if self.file_completer.matches.len() == 1 {
                        // Only one match - apply immediately
                        self.apply_completion();
                    } else if !self.file_completer.matches.is_empty() {
                        // Multiple matches - show menu and apply first
                        self.apply_completion_preview();
                    } else {
                        // No completions - insert spaces
                        self.insert_str("    ");
                    }
                }
            }
            KeyCode::BackTab => {
                if completion_visible {
                    self.move_completion(-1);
                } else if let Some((provider, model, _provider_changed)) = cycle_favorite() {
                    info.provider = provider;
                    info.model = model;
                    // Redraw status bar in place
                    let mut stdout = io::stdout();
                    let lines_up = self.rendered_lines + self.rendered_menu_lines;
                    crossterm::execute!(stdout, MoveUp(lines_up), MoveToColumn(0))?;
                    self.render_status_bar(info, *thinking_enabled)?;
                    crossterm::execute!(stdout, MoveToColumn(0), cursor::MoveDown(lines_up))?;
                }
            }
            KeyCode::Esc => {
                if menu_visible {
                    self.buffer.clear();
                    self.cursor = 0;
                    self.menu_index = 0;
                } else if completion_visible {
                    // Clear completion menu
                    self.file_completer.clear();
                } else if let Some(last) = self.last_esc {
                    // Double-ESC within 500ms clears the buffer
                    if last.elapsed() < Duration::from_millis(500) {
                        self.buffer.clear();
                        self.cursor = 0;
                        self.history_index = None;
                    }
                }
                self.last_esc = Some(Instant::now());
            }
            KeyCode::Backspace => {
                self.file_completer.clear();
                if key.modifiers.contains(KeyModifiers::ALT) {
                    self.delete_prev_word();
                } else {
                    self.delete_backward();
                }
                self.menu_index = 0;
            }
            KeyCode::Delete => {
                self.file_completer.clear();
                self.delete_forward();
                self.menu_index = 0;
            }
            KeyCode::Left => {
                self.file_completer.clear();
                if key.modifiers.contains(KeyModifiers::ALT) {
                    self.move_prev_word();
                } else {
                    self.move_left();
                }
            }
            KeyCode::Right => {
                self.file_completer.clear();
                if key.modifiers.contains(KeyModifiers::ALT) {
                    self.move_next_word();
                } else {
                    self.move_right();
                }
            }
            KeyCode::Home => {
                self.file_completer.clear();
                self.move_line_start();
            }
            KeyCode::End => {
                self.file_completer.clear();
                self.move_line_end();
            }
            KeyCode::Up => {
                if menu_visible {
                    let count = self.filtered_commands(info).len();
                    if count > 0 {
                        self.menu_index = self.menu_index.checked_sub(1).unwrap_or(count - 1);
                    }
                } else if completion_visible {
                    self.move_completion(-1);
                } else if self.cursor == 0 || !self.buffer.contains('\n') {
                    self.apply_history_up(history)
                } else {
                    self.move_line_up()
                }
            }
            KeyCode::Down => {
                if menu_visible {
                    let count = self.filtered_commands(info).len();
                    if count > 0 {
                        self.menu_index = (self.menu_index + 1) % count;
                    }
                } else if completion_visible {
                    self.move_completion(1);
                } else {
                    let at_last_line = !self.buffer[self.cursor..].contains('\n');
                    if at_last_line {
                        self.apply_history_down(history)
                    } else {
                        self.move_line_down()
                    }
                }
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if menu_visible {
                    let count = self.filtered_commands(info).len();
                    if count > 0 {
                        self.menu_index = self.menu_index.checked_sub(1).unwrap_or(count - 1);
                    }
                } else if completion_visible {
                    self.move_completion(-1);
                } else {
                    self.apply_history_up(history)
                }
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if menu_visible {
                    let count = self.filtered_commands(info).len();
                    if count > 0 {
                        self.menu_index = (self.menu_index + 1) % count;
                    }
                } else if completion_visible {
                    self.move_completion(1);
                } else {
                    self.apply_history_down(history)
                }
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.file_completer.clear();
                self.move_line_start()
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.file_completer.clear();
                self.move_line_end()
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.file_completer.clear();
                self.kill_line_end()
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.file_completer.clear();
                self.kill_line_start()
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.file_completer.clear();
                self.delete_prev_word();
                self.menu_index = 0;
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.file_completer.clear();
                self.move_right()
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.file_completer.clear();
                self.move_left();
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Clear screen and redraw
                let mut stdout = io::stdout();
                crossterm::execute!(stdout, Clear(ClearType::All), cursor::MoveTo(0, 0))?;
                self.render_status_bar(info, *thinking_enabled)?;
                crossterm::execute!(stdout, Print("\r\n"))?;
                self.rendered_lines = 1;
                self.last_cursor_row = 0;
                self.rendered_menu_lines = 0;
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if info.thinking_available && info.show_thinking_status {
                    *thinking_enabled = !*thinking_enabled;
                    // Redraw status bar in place
                    let mut stdout = io::stdout();
                    // Move up past input lines to status bar line
                    let lines_up = self.rendered_lines + self.rendered_menu_lines;
                    crossterm::execute!(stdout, MoveUp(lines_up), MoveToColumn(0))?;
                    self.render_status_bar(info, *thinking_enabled)?;
                    // Move back down to input area
                    crossterm::execute!(stdout, MoveToColumn(0), cursor::MoveDown(lines_up))?;
                }
            }
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(Some(PromptOutcome::SelectModel));
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Run fzf history search
                // Temporarily exit raw mode for fzf to take over the terminal
                let mut stdout = io::stdout();

                // Clear current input display and move to start
                let lines_up = self.rendered_lines + self.rendered_menu_lines;
                if lines_up > 0 {
                    crossterm::execute!(stdout, MoveUp(lines_up))?;
                }
                crossterm::execute!(
                    stdout,
                    MoveToColumn(0),
                    Clear(ClearType::FromCursorDown),
                    cursor::Show
                )?;

                // Exit raw mode
                let _ = crossterm::execute!(stdout, PopKeyboardEnhancementFlags);
                let _ = crossterm::execute!(stdout, DisableBracketedPaste);
                disable_raw_mode().ok();
                stdout.flush()?;

                // Run fzf
                let selected = run_fzf_history(history);

                // Re-enter raw mode
                enable_raw_mode().map_err(io::Error::other)?;
                crossterm::execute!(stdout, EnableBracketedPaste).ok();
                let keyboard_flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
                crossterm::execute!(stdout, PushKeyboardEnhancementFlags(keyboard_flags)).ok();

                // Redraw status bar
                self.render_status_bar(info, *thinking_enabled)?;
                crossterm::execute!(stdout, Print("\r\n"))?;

                // Apply selected entry if any
                if let Some(entry) = selected {
                    self.buffer = entry;
                    self.cursor = self.buffer.len();
                    self.history_index = None;
                }

                // Reset rendered state since we cleared the screen
                self.rendered_lines = 1;
                self.last_cursor_row = 0;
                self.rendered_menu_lines = 0;
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.file_completer.clear();
                self.move_prev_word()
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.file_completer.clear();
                self.move_next_word()
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.file_completer.clear();
                self.delete_next_word();
                self.menu_index = 0;
            }
            KeyCode::Char(ch) => {
                self.file_completer.clear();
                if key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && matches!(ch, 'j' | 'm')
                {
                    self.insert_newline_with_indent();
                } else {
                    self.insert_char(ch);
                    self.menu_index = 0;
                }
            }
            _ => {}
        }

        Ok(None)
    }

    /// Check if a command should be saved to history.
    /// Returns true for regular input and custom slash commands, false for built-in commands.
    fn should_save_to_history(&self, content: &str) -> bool {
        if let Some(cmd_str) = content.trim_start().strip_prefix('/') {
            // It's a slash command - only save if it's custom
            if let Some(cmd) = crate::commands::parse(cmd_str, &self.custom_commands) {
                matches!(cmd, crate::commands::Command::Custom { .. })
            } else {
                false
            }
        } else {
            // Not a slash command, save to history
            true
        }
    }

    /// Save content and images to history.
    fn save_to_history(
        &self,
        history: &mut FileHistory,
        content: &str,
        pasted_images: &[PastedImage],
    ) {
        let history_images: Vec<crate::history::HistoryImage> = pasted_images
            .iter()
            .filter_map(|img| {
                crate::history::HistoryImage::from_raw_data(
                    img.marker.clone(),
                    img.mime_type.clone(),
                    img.data.clone(),
                )
                .ok()
            })
            .collect();

        let _ = history.add_with_images(content, history_images);
    }

    fn build_submission(&mut self, history: &mut FileHistory) -> PromptOutcome {
        let content = self.buffer.clone();
        let pasted_images = self.active_images();

        if self.should_save_to_history(&content) {
            self.save_to_history(history, &content, &pasted_images);
        }

        PromptOutcome::Submitted {
            content,
            pasted_images,
        }
    }

    fn build_command_submission(&mut self, history: &mut FileHistory) -> PromptOutcome {
        let content = self.buffer.clone();
        let pasted_images = self.active_images();

        if self.should_save_to_history(&content) {
            self.save_to_history(history, &content, &pasted_images);
        }

        PromptOutcome::Submitted {
            content,
            pasted_images,
        }
    }

    fn complete_command(&mut self, info: &PromptInfo) -> bool {
        let filtered = self.filtered_commands(info);
        if let Some(cmd) = filtered.get(self.menu_index) {
            self.buffer = format!("/{}", cmd.name);
            self.cursor = self.buffer.len();
            self.menu_index = 0;
            // Auto-execute /model and /settings commands
            matches!(
                cmd.command,
                crate::commands::Command::Model | crate::commands::Command::Settings
            )
        } else {
            false
        }
    }

    fn insert_newline_with_indent(&mut self) {
        let indent = self.current_line_indent();
        self.insert_char('\n');
        if !indent.is_empty() {
            self.insert_str(&indent);
        }
    }

    fn current_line_indent(&self) -> String {
        let line_start = self.current_line_start();
        self.buffer[line_start..self.cursor]
            .chars()
            .take_while(|ch| ch.is_whitespace() && *ch != '\n')
            .collect()
    }

    fn current_line_start(&self) -> usize {
        self.buffer[..self.cursor]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0)
    }

    fn insert_char(&mut self, ch: char) {
        self.buffer.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    fn insert_str(&mut self, s: &str) {
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some((idx, ch)) = self.buffer[..self.cursor].char_indices().last() {
            self.buffer.drain(idx..idx + ch.len_utf8());
            self.cursor = idx;
        }
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        if let Some((_, ch)) = self.buffer[self.cursor..].char_indices().next() {
            self.buffer.drain(self.cursor..self.cursor + ch.len_utf8());
        }
    }

    fn move_left(&mut self) {
        if let Some((idx, _)) = self.buffer[..self.cursor].char_indices().last() {
            self.cursor = idx;
        }
    }

    fn move_right(&mut self) {
        if let Some((_, ch)) = self.buffer[self.cursor..].char_indices().next() {
            self.cursor += ch.len_utf8();
        }
    }

    fn move_line_start(&mut self) {
        self.cursor = self.current_line_start();
    }

    fn move_line_end(&mut self) {
        if let Some(rest) = self.buffer[self.cursor..].find('\n') {
            self.cursor += rest;
        } else {
            self.cursor = self.buffer.len();
        }
    }

    fn move_line_up(&mut self) {
        let line_start = self.current_line_start();
        if line_start == 0 {
            return;
        }
        let target_col = self.cursor.saturating_sub(line_start);
        let prev_line_end = line_start.saturating_sub(1);
        let prev_line_start = self.buffer[..prev_line_end]
            .rfind('\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);
        let prev_len = prev_line_end.saturating_sub(prev_line_start);
        let new_col = target_col.min(prev_len);
        self.cursor = prev_line_start + new_col;
    }

    fn move_line_down(&mut self) {
        let line_start = self.current_line_start();
        let current_line_end = self.buffer[line_start..]
            .find('\n')
            .map(|idx| line_start + idx)
            .unwrap_or_else(|| self.buffer.len());
        if current_line_end >= self.buffer.len() {
            return;
        }
        let next_line_start = current_line_end + 1;
        let target_col = self.cursor.saturating_sub(line_start);
        let next_line_end = self.buffer[next_line_start..]
            .find('\n')
            .map(|idx| next_line_start + idx)
            .unwrap_or_else(|| self.buffer.len());
        let next_len = next_line_end.saturating_sub(next_line_start);
        let new_col = target_col.min(next_len);
        self.cursor = next_line_start + new_col;
    }

    fn move_prev_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let mut idx = self.cursor;
        // Skip whitespace
        while let Some((pos, ch)) = self.buffer[..idx].char_indices().last() {
            if !ch.is_whitespace() {
                idx = pos + ch.len_utf8();
                break;
            }
            idx = pos;
        }
        // Skip word characters
        while let Some((pos, ch)) = self.buffer[..idx].char_indices().last() {
            if ch.is_whitespace() {
                idx = pos + ch.len_utf8();
                break;
            }
            idx = pos;
        }
        self.cursor = idx;
    }

    fn move_next_word(&mut self) {
        let mut idx = self.cursor;
        // Skip current word
        while let Some((pos, ch)) = self.buffer[idx..].char_indices().next() {
            idx += pos + ch.len_utf8();
            if ch.is_whitespace() {
                break;
            }
        }
        // Skip whitespace
        while let Some((pos, ch)) = self.buffer[idx..].char_indices().next() {
            if !ch.is_whitespace() {
                break;
            }
            idx += pos + ch.len_utf8();
        }
        self.cursor = idx.min(self.buffer.len());
    }

    fn delete_prev_word(&mut self) {
        let start = self.cursor;
        self.move_prev_word();
        self.buffer.drain(self.cursor..start);
    }

    fn delete_next_word(&mut self) {
        let start = self.cursor;
        self.move_next_word();
        self.buffer.drain(start..self.cursor);
        self.cursor = start;
    }

    fn kill_line_end(&mut self) {
        let line_end = self.buffer[self.cursor..]
            .find('\n')
            .map(|idx| self.cursor + idx)
            .unwrap_or(self.buffer.len());

        if self.cursor < line_end {
            self.buffer.drain(self.cursor..line_end);
        } else if self.cursor < self.buffer.len() {
            self.buffer.drain(self.cursor..self.cursor + 1);
        }
    }

    fn kill_line_start(&mut self) {
        let start = self.current_line_start();
        if start < self.cursor {
            self.buffer.drain(start..self.cursor);
            self.cursor = start;
        }
    }

    fn apply_history_up(&mut self, history: &FileHistory) {
        if history.is_empty() {
            return;
        }
        if self.history_index.is_none() {
            self.draft = self.buffer.clone();
        }

        let mut next_idx = match self.history_index {
            None => Some(history.len().saturating_sub(1)),
            Some(0) => Some(0),
            Some(i) => Some(i.saturating_sub(1)),
        };

        // Skip entries that are built-in slash commands (but allow custom commands)
        while let Some(idx) = next_idx {
            if let Some(entry) = history.get(idx) {
                // Use helper to determine if entry should be included in history
                if !self.should_save_to_history(entry) {
                    next_idx = if idx == 0 {
                        None
                    } else {
                        Some(idx.saturating_sub(1))
                    };
                } else {
                    break;
                }
            } else {
                next_idx = None;
            }
        }

        if let Some(idx) = next_idx
            && let Some(entry) = history.get(idx)
        {
            self.buffer = entry.clone();
            self.cursor = self.buffer.len();
            self.history_index = Some(idx);

            // Load and restore images for this history entry
            if let Some(history_images) = history.get_images_for_entry(idx) {
                self.pasted_images.clear();
                for history_image in history_images {
                    if let Ok(pasted_image) = history_image.to_pasted_image() {
                        self.pasted_images.push(pasted_image);
                    }
                }
            }
        }
    }

    fn apply_history_down(&mut self, history: &FileHistory) {
        if history.is_empty() {
            return;
        }
        if let Some(idx) = self.history_index {
            let mut next = idx + 1;

            // Skip entries that are built-in slash commands (but allow custom commands)
            while next < history.len() {
                if let Some(entry) = history.get(next) {
                    // Use helper to determine if entry should be included in history
                    if !self.should_save_to_history(entry) {
                        next += 1;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }

            if next >= history.len() {
                self.buffer = self.draft.clone();
                self.cursor = self.buffer.len();
                self.history_index = None;
            } else if let Some(entry) = history.get(next) {
                self.buffer = entry.clone();
                self.cursor = self.buffer.len();
                self.history_index = Some(next);

                // Load and restore images for this history entry
                if let Some(history_images) = history.get_images_for_entry(next) {
                    self.pasted_images.clear();
                    for history_image in history_images {
                        if let Ok(pasted_image) = history_image.to_pasted_image() {
                            self.pasted_images.push(pasted_image);
                        }
                    }
                }
            }
        }
    }

    fn handle_clipboard_paste(&mut self) {
        match capture_wl_paste() {
            Some(ClipboardPayload::Image { mime_type, data }) => {
                let marker_id = self.next_image_id;
                self.next_image_id += 1;

                let marker = format!("img#{}", marker_id);
                let indicator =
                    format!("[[{} {} {}]]", marker, mime_type, format_bytes(data.len()));

                self.insert_str(&indicator);
                self.pasted_images.push(PastedImage {
                    marker,
                    mime_type,
                    data,
                });
                self.menu_index = 0;
            }
            Some(ClipboardPayload::Text(text)) => {
                let normalized = normalize_paste_text(&text);
                self.insert_str(&normalized);
                self.menu_index = 0;
            }
            None => {}
        }
    }

    fn active_images(&mut self) -> Vec<PastedImage> {
        let buffer_snapshot = self.buffer.clone();
        let mut images = std::mem::take(&mut self.pasted_images);
        images.retain(|img| buffer_snapshot.contains(&img.marker));
        images
    }

    // File completion methods

    /// Check if completion menu is active
    fn completion_active(&self) -> bool {
        self.file_completer.is_active()
    }

    /// Initialize completion matches
    fn init_completion(&mut self) {
        if let Some((_start, _end, word)) =
            crate::completion::get_word_at_cursor(&self.buffer, self.cursor)
        {
            if crate::completion::FileCompleter::should_complete(&word) {
                self.file_completer.init(&word);
            } else {
                self.file_completer.clear();
            }
        } else {
            self.file_completer.clear();
        }
    }

    /// Apply the selected completion and close menu
    fn apply_completion(&mut self) -> bool {
        if !self.completion_active() {
            return false;
        }

        if let Some(selected) = self.file_completer.current()
            && let Some((word_start, word_end, _word)) =
                crate::completion::get_word_at_cursor(&self.buffer, self.cursor)
        {
            // Replace the word with the completion
            self.buffer.drain(word_start..word_end);
            self.buffer.insert_str(word_start, selected);
            self.cursor = word_start + selected.len();

            // Clear completion after applying
            self.file_completer.clear();
            return true;
        }

        self.file_completer.clear();
        false
    }

    /// Apply current completion to input but keep menu open (for previewing)
    fn apply_completion_preview(&mut self) -> bool {
        if !self.completion_active() {
            return false;
        }

        if let Some(selected) = self.file_completer.current()
            && let Some((word_start, word_end, _word)) =
                crate::completion::get_word_at_cursor(&self.buffer, self.cursor)
        {
            // Replace the word with the completion (don't clear matches)
            self.buffer.drain(word_start..word_end);
            self.buffer.insert_str(word_start, selected);
            self.cursor = word_start + selected.len();
            return true;
        }

        false
    }

    /// Move through completion options
    fn move_completion(&mut self, delta: isize) -> bool {
        if !self.completion_active() {
            return false;
        }

        self.file_completer.move_selection(delta);

        // Apply the new selection immediately (like bash completion)
        if let Some(selected) = self.file_completer.current()
            && let Some((word_start, word_end, _word)) =
                crate::completion::get_word_at_cursor(&self.buffer, self.cursor)
        {
            self.buffer.drain(word_start..word_end);
            self.buffer.insert_str(word_start, selected);
            self.cursor = word_start + selected.len();
            return true;
        }

        false
    }
}

fn display_width(s: &str) -> usize {
    s.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(1))
        .sum()
}

fn normalize_paste_text(pasted: &str) -> String {
    pasted.replace("\r\n", "\n").replace('\r', "\n")
}

fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;

    let bytes_f = bytes as f64;
    if bytes_f >= MB {
        format!("{:.1}MB", bytes_f / MB)
    } else if bytes_f >= KB {
        format!("{:.1}KB", bytes_f / KB)
    } else {
        format!("{}B", bytes)
    }
}

/// Run fzf with history entries for interactive selection.
/// Returns the selected entry if one was chosen, None otherwise.
fn run_fzf_history(history: &FileHistory) -> Option<String> {
    if history.is_empty() {
        return None;
    }

    let mut child = Command::new("fzf")
        .args([
            "--read0",
            "--print0",
            "--no-sort",
            "--tac",
            "--prompt=history> ",
            "--height=50%",
            "--layout=reverse",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;

    // Write history entries to fzf's stdin, null-separated, most recent last (for --tac)
    if let Some(mut stdin) = child.stdin.take() {
        for entry in history.entries() {
            let _ = stdin.write_all(entry.as_bytes());
            let _ = stdin.write_all(b"\0");
        }
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }

    // Parse the null-terminated output
    let result = String::from_utf8(output.stdout).ok()?;
    let selected = result.trim_end_matches('\0');
    if selected.is_empty() {
        None
    } else {
        Some(selected.to_string())
    }
}

fn capture_wl_paste() -> Option<ClipboardPayload> {
    let types_output = Command::new("wl-paste").arg("--list-types").output().ok()?;

    if !types_output.status.success() {
        return None;
    }

    let types = String::from_utf8_lossy(&types_output.stdout);
    if let Some(mime_type) = types
        .lines()
        .filter_map(|line| {
            let mime = line.trim();
            if mime.to_ascii_lowercase().starts_with("image/") {
                Some(mime.to_string())
            } else {
                None
            }
        })
        .next()
    {
        let image_output = Command::new("wl-paste")
            .args(["--type", &mime_type])
            .output()
            .ok()?;

        if !image_output.status.success() || image_output.stdout.is_empty() {
            return None;
        }

        return Some(ClipboardPayload::Image {
            mime_type,
            data: image_output.stdout,
        });
    }

    let text_output = Command::new("wl-paste").arg("--no-newline").output().ok()?;

    if !text_output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&text_output.stdout).to_string();
    if text.is_empty() {
        None
    } else {
        Some(ClipboardPayload::Text(text))
    }
}

struct RawModeGuard {
    keyboard_flags_enabled: bool,
}

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        enable_raw_mode().map_err(io::Error::other)?;

        crossterm::execute!(io::stdout(), EnableBracketedPaste).ok();

        let mut keyboard_flags_enabled = false;

        let keyboard_flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            | KeyboardEnhancementFlags::REPORT_EVENT_TYPES;

        if crossterm::execute!(io::stdout(), PushKeyboardEnhancementFlags(keyboard_flags)).is_ok() {
            keyboard_flags_enabled = true;
        }

        Ok(Self {
            keyboard_flags_enabled,
        })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.keyboard_flags_enabled {
            let _ = crossterm::execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste);
        disable_raw_mode().ok();
        let _ = crossterm::execute!(io::stdout(), cursor::Show);
    }
}
