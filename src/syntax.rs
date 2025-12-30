// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Shared syntax highlighting utilities.
//!
//! This module provides syntax highlighting for code using syntect (and tree-sitter
//! when enabled). It uses a generic RGB color representation that can be converted
//! to platform-specific color types (ratatui::Color, colored's truecolor, etc.).

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
/// Uses tree-sitter when available, falls back to syntect.
pub(crate) fn highlight_code(code: &str, language: Option<&str>) -> Vec<HighlightSpan> {
    // Try tree-sitter first if available and language is specified
    #[cfg(feature = "tree-sitter")]
    if let Some(lang) = language
        && let Some(spans) = highlight_code_treesitter(code, lang)
    {
        return spans;
    }

    // Fall back to syntect
    highlight_code_syntect(code, language)
}

/// Highlight code using syntect (TextMate grammars)
fn highlight_code_syntect(code: &str, language: Option<&str>) -> Vec<HighlightSpan> {
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

/// Tree-sitter based syntax highlighting
#[cfg(feature = "tree-sitter")]
fn highlight_code_treesitter(code: &str, language: &str) -> Option<Vec<HighlightSpan>> {
    use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

    // Map language names to tree-sitter configurations
    let (lang, highlights_query) = match language {
        "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        ),
        "python" => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY,
        ),
        "javascript" | "jsx" => (
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        ),
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ),
        "json" => (
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
        ),
        "bash" | "sh" => (
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY,
        ),
        "go" => (
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::HIGHLIGHTS_QUERY,
        ),
        "toml" => (
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        ),
        "c" => (
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY,
        ),
        "cpp" | "c++" => (
            tree_sitter_cpp::LANGUAGE.into(),
            tree_sitter_cpp::HIGHLIGHT_QUERY,
        ),
        "ruby" => (
            tree_sitter_ruby::LANGUAGE.into(),
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
        ),
        "java" => (
            tree_sitter_java::LANGUAGE.into(),
            tree_sitter_java::HIGHLIGHTS_QUERY,
        ),
        "html" => (
            tree_sitter_html::LANGUAGE.into(),
            tree_sitter_html::HIGHLIGHTS_QUERY,
        ),
        "css" => (
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY,
        ),
        "yaml" | "yml" => (
            tree_sitter_yaml::LANGUAGE.into(),
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
        ),
        _ => return None,
    };

    let mut config = HighlightConfiguration::new(lang, language, highlights_query, "", "").ok()?;

    // Define highlight names that map to colors
    let highlight_names = [
        "attribute",
        "comment",
        "constant",
        "constant.builtin",
        "constructor",
        "embedded",
        "function",
        "function.builtin",
        "function.macro",
        "function.method",
        "keyword",
        "label",
        "module",
        "number",
        "operator",
        "property",
        "property.builtin",
        "punctuation",
        "punctuation.bracket",
        "punctuation.delimiter",
        "punctuation.special",
        "string",
        "string.escape",
        "string.special",
        "tag",
        "type",
        "type.builtin",
        "variable",
        "variable.builtin",
        "variable.parameter",
    ];
    config.configure(&highlight_names);

    let mut highlighter = Highlighter::new();
    let highlights = highlighter
        .highlight(&config, code.as_bytes(), None, |_| None)
        .ok()?;

    let mut spans = Vec::new();
    let mut current_color: Option<Rgb> = None;

    for event in highlights {
        match event.ok()? {
            HighlightEvent::Source { start, end } => {
                if let Some(color) = current_color {
                    spans.push(HighlightSpan { start, end, color });
                }
            }
            HighlightEvent::HighlightStart(highlight) => {
                current_color = Some(highlight_to_color(highlight.0, &highlight_names));
            }
            HighlightEvent::HighlightEnd => {
                current_color = None;
            }
        }
    }

    Some(spans)
}

/// Map tree-sitter highlight index to RGB color
#[cfg(feature = "tree-sitter")]
fn highlight_to_color(index: usize, names: &[&str]) -> Rgb {
    let name = names.get(index).copied().unwrap_or("");

    // Base16 Mocha-inspired colors
    match name {
        "comment" => Rgb::new(117, 113, 94),                    // gray
        "string" | "string.special" => Rgb::new(152, 195, 121), // green
        "string.escape" => Rgb::new(86, 182, 194),              // cyan
        "number" | "constant" | "constant.builtin" => Rgb::new(209, 154, 102), // orange
        "keyword" => Rgb::new(198, 120, 221),                   // purple
        "function" | "function.builtin" | "function.macro" | "function.method" => {
            Rgb::new(97, 175, 239) // blue
        }
        "type" | "type.builtin" | "constructor" => Rgb::new(229, 192, 123), // yellow
        "variable.builtin" => Rgb::new(224, 108, 117),                      // red
        "property" | "property.builtin" => Rgb::new(224, 108, 117),         // red
        "operator" => Rgb::new(171, 178, 191),                              // light gray
        "punctuation" | "punctuation.bracket" | "punctuation.delimiter" | "punctuation.special" => {
            Rgb::new(171, 178, 191) // light gray
        }
        "attribute" | "tag" => Rgb::new(224, 108, 117), // red
        "embedded" => Rgb::new(171, 178, 191),          // light gray
        "module" => Rgb::new(229, 192, 123),            // yellow
        "label" => Rgb::new(224, 108, 117),             // red
        "variable" | "variable.parameter" => Rgb::new(171, 178, 191), // light gray (default)
        _ => Rgb::new(171, 178, 191),                   // default: light gray
    }
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
