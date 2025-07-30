// SPDX-License-Identifier: AGPL-3.0-only

use crate::config::Config;
use crate::llm::CopilotUsage;
use crossterm::style::{Color, ResetColor, SetBackgroundColor, SetForegroundColor};
use std::fmt::Write;

#[derive(Debug, Clone)]
pub struct PromptTheme {
    pub model_bg: Color,
    pub model_fg: Color,
    pub tokens_bg: Color,
    pub tokens_fg: Color,
    pub mode_bg: Color,
    pub mode_fg: Color,
    pub prompt_bg: Color,
    pub prompt_fg: Color,
    pub separator: char,
}

impl Default for PromptTheme {
    fn default() -> Self {
        // Dark theme suitable for dark backgrounds
        let separator = if supports_unicode() { '\u{e0b0}' } else { '>' };

        Self {
            model_bg: Color::Rgb {
                r: 40,
                g: 42,
                b: 54,
            }, // Dark purple background
            model_fg: Color::Rgb {
                r: 189,
                g: 147,
                b: 249,
            }, // Light purple
            tokens_bg: Color::Rgb {
                r: 68,
                g: 71,
                b: 90,
            }, // Darker purple
            tokens_fg: Color::Rgb {
                r: 80,
                g: 250,
                b: 123,
            }, // Bright green
            mode_bg: Color::Rgb {
                r: 98,
                g: 114,
                b: 164,
            }, // Blue
            mode_fg: Color::Rgb {
                r: 248,
                g: 248,
                b: 242,
            }, // Light foreground
            prompt_bg: Color::Rgb {
                r: 33,
                g: 34,
                b: 44,
            }, // Very dark background
            prompt_fg: Color::Rgb {
                r: 255,
                g: 121,
                b: 198,
            }, // Pink
            separator,
        }
    }
}

fn supports_unicode() -> bool {
    // Check for common environment variables that indicate Unicode support
    if let Ok(term) = std::env::var("TERM") {
        if term.contains("256color") || term.contains("unicode") || term.contains("xterm") {
            return true;
        }
    }

    if let Ok(lang) = std::env::var("LANG") {
        if lang.contains("UTF-8") || lang.contains("utf8") {
            return true;
        }
    }

    // Default to true on most modern systems
    cfg!(not(windows)) || std::env::var("WT_SESSION").is_ok()
}

pub struct PromptBuilder {
    theme: PromptTheme,
    model: Option<String>,
    provider: Option<String>,
    tokens: Option<CopilotUsage>,
    multi_line: bool,
    two_line_prompt: bool,
}

impl PromptBuilder {
    pub fn new() -> Self {
        Self {
            theme: PromptTheme::default(),
            model: None,
            provider: None,
            tokens: None,
            multi_line: false,
            two_line_prompt: std::env::var("HENRI_TWO_LINE_PROMPT").is_ok(),
        }
    }

    pub fn with_model(mut self, model: Option<String>, provider: Option<String>) -> Self {
        self.model = model;
        self.provider = provider;
        self
    }

    pub fn with_tokens(mut self, tokens: Option<CopilotUsage>) -> Self {
        self.tokens = tokens;
        self
    }

    pub fn multi_line(mut self, is_multi: bool) -> Self {
        self.multi_line = is_multi;
        self
    }

    pub fn build(&self) -> String {
        let mut prompt = String::new();

        if self.two_line_prompt && (self.model.is_some() || self.tokens.is_some()) {
            // Two-line prompt: model and tokens on first line
            if let Some(model) = &self.model {
                write!(
                    &mut prompt,
                    "{}{}",
                    SetBackgroundColor(self.theme.model_bg),
                    SetForegroundColor(self.theme.model_fg)
                )
                .unwrap();

                write!(&mut prompt, " {model} ").unwrap();

                if let Some(provider) = &self.provider {
                    write!(&mut prompt, "({provider}) ").unwrap();
                }
            }

            // Tokens segment on same line
            if let Some(tokens) = &self.tokens {
                // Separator between model and tokens
                if self.model.is_some() {
                    write!(
                        &mut prompt,
                        "{}{}{}",
                        SetBackgroundColor(self.theme.tokens_bg),
                        SetForegroundColor(self.theme.model_bg),
                        self.theme.separator
                    )
                    .unwrap();
                } else {
                    write!(&mut prompt, "{}", SetBackgroundColor(self.theme.tokens_bg)).unwrap();
                }

                write!(
                    &mut prompt,
                    "{} 📊 {}/{} ",
                    SetForegroundColor(self.theme.tokens_fg),
                    format_token_count(tokens.prompt_tokens),
                    format_token_count(tokens.total_tokens)
                )
                .unwrap();
            }

            // Reset and add newline
            writeln!(&mut prompt, "{ResetColor}").unwrap();

            // Second line: just the prompt
            write!(
                &mut prompt,
                "{}{}",
                SetBackgroundColor(self.theme.prompt_bg),
                SetForegroundColor(self.theme.prompt_fg)
            )
            .unwrap();

            // Prompt symbol
            let symbol = if self.multi_line {
                ".."
            } else if supports_unicode() {
                "❯"
            } else {
                ">"
            };
            write!(&mut prompt, " {symbol} {ResetColor} ").unwrap();
        } else {
            // Single-line prompt (original behavior)
            // Model segment
            if let Some(model) = &self.model {
                write!(
                    &mut prompt,
                    "{}{}",
                    SetBackgroundColor(self.theme.model_bg),
                    SetForegroundColor(self.theme.model_fg)
                )
                .unwrap();

                write!(&mut prompt, " {model} ").unwrap();

                if let Some(provider) = &self.provider {
                    write!(&mut prompt, "({provider})").unwrap();
                }
            }

            // Tokens segment
            if let Some(tokens) = &self.tokens {
                // Separator between model and tokens
                if self.model.is_some() {
                    write!(
                        &mut prompt,
                        "{}{}{}",
                        SetBackgroundColor(self.theme.tokens_bg),
                        SetForegroundColor(self.theme.model_bg),
                        self.theme.separator
                    )
                    .unwrap();
                } else {
                    write!(&mut prompt, "{}", SetBackgroundColor(self.theme.tokens_bg)).unwrap();
                }

                write!(
                    &mut prompt,
                    "{} 📊 {}/{} ",
                    SetForegroundColor(self.theme.tokens_fg),
                    format_token_count(tokens.prompt_tokens),
                    format_token_count(tokens.total_tokens)
                )
                .unwrap();
            }

            // Mode segment (only for multi-line mode)
            if self.multi_line {
                let prev_bg = if self.tokens.is_some() {
                    self.theme.tokens_bg
                } else if self.model.is_some() {
                    self.theme.model_bg
                } else {
                    self.theme.prompt_bg
                };

                write!(
                    &mut prompt,
                    "{}{}{}",
                    SetBackgroundColor(self.theme.mode_bg),
                    SetForegroundColor(prev_bg),
                    self.theme.separator
                )
                .unwrap();

                let mode_text = " ... ";

                write!(
                    &mut prompt,
                    "{}{}",
                    SetForegroundColor(self.theme.mode_fg),
                    mode_text
                )
                .unwrap();
            }

            // Final prompt segment
            let prev_bg = if self.multi_line {
                self.theme.mode_bg
            } else if self.tokens.is_some() {
                self.theme.tokens_bg
            } else if self.model.is_some() {
                self.theme.model_bg
            } else {
                self.theme.prompt_bg
            };

            write!(
                &mut prompt,
                "{}{}{}",
                SetBackgroundColor(self.theme.prompt_bg),
                SetForegroundColor(prev_bg),
                self.theme.separator
            )
            .unwrap();

            // Prompt symbol
            let symbol = if self.multi_line {
                ".."
            } else if supports_unicode() {
                "❯"
            } else {
                ">"
            };
            write!(
                &mut prompt,
                "{} {} {}",
                SetForegroundColor(self.theme.prompt_fg),
                symbol,
                ResetColor
            )
            .unwrap();
        }

        prompt
    }
}

fn format_token_count(count: u32) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f32 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f32 / 1_000.0)
    } else {
        count.to_string()
    }
}

pub fn get_model_info(config: &Config) -> (Option<String>, Option<String>) {
    if let Some(model) = config.get_selected_model() {
        let provider = if model.starts_with("gpt-") || model.starts_with("o1-") {
            Some("GitHub Copilot".to_string())
        } else if model.starts_with("claude-") {
            Some("Anthropic".to_string())
        } else if model.contains("llama") || model.contains("mistral") || model.contains("gemma") {
            Some("OpenRouter".to_string())
        } else {
            None
        };
        (Some(model.to_string()), provider)
    } else {
        (None, None)
    }
}
