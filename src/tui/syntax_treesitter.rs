// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Tree-sitter based syntax highlighting.
//!
//! This module provides syntax highlighting using tree-sitter grammars,
//! which are AST-based and more accurate than TextMate grammars.

use std::collections::HashMap;
use std::sync::OnceLock;

use ratatui::style::Color;
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

use super::syntax::HighlightSpan;

/// Standard highlight names used by tree-sitter grammars.
/// The order matters - it determines the index used in HighlightEvent.
const HIGHLIGHT_NAMES: &[&str] = &[
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

/// Map highlight names to colors (One Dark theme inspired)
fn highlight_color(name: &str) -> Color {
    match name {
        "keyword" => Color::Rgb(198, 120, 221), // purple
        "string" | "string.special" => Color::Rgb(152, 195, 121), // green
        "string.escape" => Color::Rgb(86, 182, 194), // cyan
        "comment" => Color::Rgb(92, 99, 112),   // gray
        "function" | "function.builtin" | "function.method" | "function.macro" => {
            Color::Rgb(97, 175, 239) // blue
        }
        "type" | "type.builtin" | "constructor" => Color::Rgb(229, 192, 123), // yellow
        "number" | "constant" | "constant.builtin" => Color::Rgb(209, 154, 102), // orange
        "operator" => Color::Rgb(171, 178, 191),                              // light gray
        "variable.builtin" => Color::Rgb(224, 108, 117),                      // red
        "property" | "property.builtin" => Color::Rgb(224, 108, 117),         // red
        "attribute" => Color::Rgb(198, 120, 221),                             // purple
        "tag" => Color::Rgb(224, 108, 117),                                   // red (for HTML tags)
        "punctuation" | "punctuation.bracket" | "punctuation.delimiter" | "punctuation.special" => {
            Color::Rgb(171, 178, 191) // light gray
        }
        "module" => Color::Rgb(229, 192, 123),   // yellow
        "label" => Color::Rgb(224, 108, 117),    // red
        "embedded" => Color::Rgb(171, 178, 191), // light gray
        "variable" | "variable.parameter" => Color::Rgb(171, 178, 191), // light gray (default)
        _ => Color::Rgb(171, 178, 191),          // default light gray
    }
}

/// Language configuration with its tree-sitter setup
struct LangConfig {
    config: HighlightConfiguration,
}

/// Global registry of language configurations
static REGISTRY: OnceLock<HashMap<&'static str, LangConfig>> = OnceLock::new();

/// Initialize and return the language registry
fn registry() -> &'static HashMap<&'static str, LangConfig> {
    REGISTRY.get_or_init(|| {
        let mut map = HashMap::new();

        // Helper macro to reduce boilerplate
        macro_rules! register_lang {
            ($map:expr, $name:expr, $lang:ident, $highlights:expr, $injections:expr, $locals:expr) => {
                if let Ok(mut config) = HighlightConfiguration::new(
                    $lang::LANGUAGE.into(),
                    $name,
                    $highlights,
                    $injections,
                    $locals,
                ) {
                    config.configure(HIGHLIGHT_NAMES);
                    $map.insert($name, LangConfig { config });
                }
            };
            ($map:expr, $name:expr, $lang:ident, $highlights:expr) => {
                register_lang!($map, $name, $lang, $highlights, "", "");
            };
        }

        // Rust
        register_lang!(
            map,
            "rust",
            tree_sitter_rust,
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            ""
        );

        // Python
        register_lang!(
            map,
            "python",
            tree_sitter_python,
            tree_sitter_python::HIGHLIGHTS_QUERY
        );

        // JavaScript
        register_lang!(
            map,
            "javascript",
            tree_sitter_javascript,
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY
        );

        // TypeScript (uses JavaScript grammar with TypeScript additions)
        #[cfg(feature = "tree-sitter")]
        if let Ok(mut config) = HighlightConfiguration::new(
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "typescript",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ) {
            config.configure(HIGHLIGHT_NAMES);
            map.insert("typescript", LangConfig { config });
        }

        // TSX
        #[cfg(feature = "tree-sitter")]
        if let Ok(mut config) = HighlightConfiguration::new(
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            "tsx",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ) {
            config.configure(HIGHLIGHT_NAMES);
            map.insert("tsx", LangConfig { config });
        }

        // JSON
        register_lang!(
            map,
            "json",
            tree_sitter_json,
            tree_sitter_json::HIGHLIGHTS_QUERY
        );

        // Bash
        register_lang!(
            map,
            "bash",
            tree_sitter_bash,
            tree_sitter_bash::HIGHLIGHT_QUERY
        );

        // Go
        register_lang!(
            map,
            "go",
            tree_sitter_go,
            tree_sitter_go::HIGHLIGHTS_QUERY
        );

        // TOML
        register_lang!(
            map,
            "toml",
            tree_sitter_toml_ng,
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY
        );

        // C
        register_lang!(map, "c", tree_sitter_c, tree_sitter_c::HIGHLIGHT_QUERY);

        // C++
        register_lang!(
            map,
            "cpp",
            tree_sitter_cpp,
            tree_sitter_cpp::HIGHLIGHT_QUERY
        );

        // Ruby
        register_lang!(
            map,
            "ruby",
            tree_sitter_ruby,
            tree_sitter_ruby::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_ruby::LOCALS_QUERY
        );

        // Java
        register_lang!(
            map,
            "java",
            tree_sitter_java,
            tree_sitter_java::HIGHLIGHTS_QUERY
        );

        // HTML
        register_lang!(
            map,
            "html",
            tree_sitter_html,
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
            ""
        );

        // CSS
        register_lang!(map, "css", tree_sitter_css, tree_sitter_css::HIGHLIGHTS_QUERY);

        // YAML
        register_lang!(
            map,
            "yaml",
            tree_sitter_yaml,
            tree_sitter_yaml::HIGHLIGHTS_QUERY
        );

        // Note: Markdown has a different API (function vs constant) and complex
        // block/inline handling, so we skip it and let syntect handle it.

        map
    })
}

/// Map language aliases to canonical names
fn normalize_language(lang: &str) -> Option<&'static str> {
    // First check for common aliases
    let normalized = match lang.to_lowercase().as_str() {
        "rs" | "rust" => "rust",
        "py" | "python" => "python",
        "js" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "tsx" => "tsx",
        "sh" | "shell" | "zsh" | "bash" => "bash",
        "c" => "c",
        "c++" | "cpp" => "cpp",
        "go" | "golang" => "go",
        "rb" | "ruby" => "ruby",
        "java" => "java",
        "json" => "json",
        "toml" => "toml",
        "yml" | "yaml" => "yaml",
        "htm" | "html" => "html",
        "css" => "css",
        _ => return None,
    };
    Some(normalized)
}

/// Highlight code using tree-sitter.
/// Returns None if the language is not supported.
pub(crate) fn highlight_code(code: &str, language: &str) -> Option<Vec<HighlightSpan>> {
    let normalized = normalize_language(language)?;
    let lang_config = registry().get(normalized)?;

    let mut highlighter = Highlighter::new();
    let mut spans = Vec::new();
    let mut highlight_stack: Vec<usize> = Vec::new();

    let highlights = highlighter
        .highlight(&lang_config.config, code.as_bytes(), None, |_| None)
        .ok()?;

    for event in highlights {
        match event {
            Ok(HighlightEvent::Source { start, end }) => {
                if let Some(&highlight_idx) = highlight_stack.last()
                    && highlight_idx < HIGHLIGHT_NAMES.len()
                {
                    let name = HIGHLIGHT_NAMES[highlight_idx];
                    let color = highlight_color(name);
                    spans.push(HighlightSpan { start, end, color });
                }
            }
            Ok(HighlightEvent::HighlightStart(highlight)) => {
                highlight_stack.push(highlight.0);
            }
            Ok(HighlightEvent::HighlightEnd) => {
                highlight_stack.pop();
            }
            Err(_) => {
                // Continue on error
            }
        }
    }

    Some(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_highlighting() {
        let code = r#"fn main() {
    let x = 42;
    println!("{}", x);
}"#;
        let spans = highlight_code(code, "rust").expect("Should highlight Rust");
        assert!(!spans.is_empty(), "Should have highlight spans");

        // Check that "fn" is highlighted as keyword (purple)
        let fn_span = spans.iter().find(|s| &code[s.start..s.end] == "fn");
        assert!(fn_span.is_some(), "Should find 'fn' keyword");
        assert_eq!(fn_span.unwrap().color, Color::Rgb(198, 120, 221));
    }

    #[test]
    fn test_python_highlighting() {
        let code = "def hello():\n    return 42";
        let spans = highlight_code(code, "python").expect("Should highlight Python");
        assert!(!spans.is_empty());

        // Check that "def" is highlighted as keyword
        let def_span = spans.iter().find(|s| &code[s.start..s.end] == "def");
        assert!(def_span.is_some(), "Should find 'def' keyword");
    }

    #[test]
    fn test_language_aliases() {
        // Test common aliases work
        assert!(highlight_code("x", "rs").is_some());
        assert!(highlight_code("x", "py").is_some());
        assert!(highlight_code("x", "js").is_some());
        assert!(highlight_code("x", "sh").is_some());
    }

    #[test]
    fn test_unsupported_language() {
        assert!(highlight_code("code", "brainfuck").is_none());
    }

    #[test]
    fn test_all_registered_languages() {
        let languages = [
            "rust",
            "python",
            "javascript",
            "json",
            "bash",
            "go",
            "toml",
            "c",
            "cpp",
            "ruby",
            "java",
            "html",
            "css",
            "yaml",
        ];

        for lang in languages {
            assert!(
                highlight_code("x", lang).is_some(),
                "Language '{}' should be supported",
                lang
            );
        }
    }
}
