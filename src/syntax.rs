// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Shared syntax highlighting utilities.
//!
//! This module provides syntax highlighting for code using syntect. It uses a generic
//! RGB color representation that can be converted to platform-specific color types
//! (colored's truecolor, etc.).

use std::path::Path;
use std::sync::OnceLock;

use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

/// Global syntax set - loaded once on first use
static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();

/// Global theme - loaded once on first use
static THEME: OnceLock<Theme> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes
            .get("base16-mocha.dark")
            .cloned()
            .unwrap_or_else(|| ts.themes.values().next().cloned().unwrap())
    })
}

/// RGB color for syntax highlighting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub(crate) const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// A highlighted span with byte range and color
#[derive(Debug, Clone)]
pub(crate) struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub color: Rgb,
}

/// Highlight code and return spans with RGB colors.
pub(crate) fn highlight_code(code: &str, language: Option<&str>) -> Vec<HighlightSpan> {
    let ps = syntax_set();
    let theme = theme();

    // Find syntax definition
    let syntax = language
        .and_then(|lang| ps.find_syntax_by_token(lang))
        .or_else(|| ps.find_syntax_by_extension("txt"))
        .unwrap_or_else(|| ps.find_syntax_plain_text());

    let mut highlighter = syntect::easy::HighlightLines::new(syntax, theme);
    let mut spans = Vec::new();
    let mut byte_offset = 0;

    for line in syntect::util::LinesWithEndings::from(code) {
        match highlighter.highlight_line(line, ps) {
            Ok(ranges) => {
                for (style, text) in ranges {
                    let fg = style.foreground;
                    let color = Rgb::new(fg.r, fg.g, fg.b);
                    let start = byte_offset;
                    let end = byte_offset + text.len();
                    spans.push(HighlightSpan { start, end, color });
                    byte_offset = end;
                }
            }
            Err(_) => {
                // Fallback: no highlighting for this line
                byte_offset += line.len();
            }
        }
    }

    spans
}

/// Extract language identifier from file path extension
pub(crate) fn language_from_path(path: &str) -> Option<String> {
    let ext = Path::new(path).extension()?.to_str()?;
    // Map extensions to language identifiers
    let lang = match ext.to_lowercase().as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "javascript",
        "sh" | "bash" | "zsh" => "bash",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "go" => "go",
        "rb" => "ruby",
        "java" => "java",
        "json" => "json",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "html" | "htm" => "html",
        "css" => "css",
        _ => return None,
    };
    Some(lang.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_from_path() {
        assert_eq!(language_from_path("foo.rs"), Some("rust".to_string()));
        assert_eq!(language_from_path("bar.py"), Some("python".to_string()));
        assert_eq!(
            language_from_path("test.js"),
            Some("javascript".to_string())
        );
        assert_eq!(language_from_path("config.toml"), Some("toml".to_string()));
        assert_eq!(language_from_path("unknown.xyz"), None);
    }

    #[test]
    fn test_highlight_code_rust() {
        let code = "let x = 42;";
        let spans = highlight_code(code, Some("rust"));
        assert!(!spans.is_empty(), "Rust code should have highlights");
    }

    #[test]
    fn test_highlight_code_python() {
        let code = "def hello():\n    pass";
        let spans = highlight_code(code, Some("python"));
        assert!(!spans.is_empty(), "Python code should have highlights");
    }
}
