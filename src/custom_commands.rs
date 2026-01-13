// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Custom slash commands loaded from .claude/commands/*.md files.
//! Recursively scans subdirectories, so commands can be organized hierarchically.
//! For example, `suricata/review-pr.md` becomes the command `suricata/review-pr`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value as JsonValue;
use walkdir::WalkDir;

/// A custom command loaded from a markdown file.
#[derive(Debug, Clone)]
pub(crate) struct CustomCommand {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub _model: Option<String>,
}

#[derive(Debug, Clone)]
struct CommandFrontmatter {
    description: Option<String>,
    model: Option<String>,
}

/// Load all custom commands from multiple directories.
/// Checks in order:
/// 1. .claude/commands/ (current directory)
/// 2. .henri/commands/ (current directory)
/// 3. ~/.claude/commands/ (user's home)
/// 4. ~/.config/henri/commands/ (henri config)
pub(crate) fn load_custom_commands() -> io::Result<Vec<CustomCommand>> {
    let mut all_commands = Vec::new();

    // Define command directories to check with their labels
    let mut search_dirs = Vec::new();

    // 1. Current directory .claude/commands
    search_dirs.push((Path::new(".claude/commands").to_path_buf(), "(.claude)"));

    // 2. Current directory .henri/commands
    search_dirs.push((Path::new(".henri/commands").to_path_buf(), "(.henri)"));

    // 3. Home directory ~/.claude/commands
    if let Some(home) = std::env::var_os("HOME") {
        let mut home_claude = std::path::PathBuf::from(home);
        home_claude.push(".claude/commands");
        search_dirs.push((home_claude, "(~/.claude)"));
    }

    // 4. Config directory ~/.config/henri/commands
    if let Some(home) = std::env::var_os("HOME") {
        let mut config_dir = std::path::PathBuf::from(home);
        config_dir.push(".config/henri/commands");
        search_dirs.push((config_dir, "(~/.config/henri)"));
    }

    // Load commands from each directory
    for (commands_dir, label) in search_dirs {
        if !commands_dir.exists() || !commands_dir.is_dir() {
            continue;
        }

        // Recursively walk through all subdirectories
        for entry in WalkDir::new(&commands_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            // Skip directories, only process files
            if !path.is_file() {
                continue;
            }

            // Only process .md files
            if let Some(ext) = path.extension() {
                if ext != "md" {
                    continue;
                }
            } else {
                continue;
            }

            // Compute the command name from the relative path
            // e.g., "suricata/review-pr.md" -> "suricata/review-pr"
            let relative_path = match path.strip_prefix(&commands_dir) {
                Ok(rel) => rel,
                Err(_) => continue,
            };

            let name = relative_path
                .with_extension("")
                .to_str()
                .map(|s| s.to_string());

            if let Some(name) = name
                && let Ok(content) = fs::read_to_string(path)
            {
                let (description, prompt, model) = parse_command_file(&content);
                all_commands.push(CustomCommand {
                    name,
                    description: format!("{} {}", description, label),
                    prompt,
                    _model: model,
                });
            }
        }
    }

    Ok(all_commands)
}

/// Parse a command file into description and prompt.
/// Handles front-matter (YAML, TOML) if present.
/// If front-matter has a "description" field, uses that.
/// Otherwise, first line after front-matter is the description.
fn parse_command_file(content: &str) -> (String, String, Option<String>) {
    let content = content.trim();

    // Try to parse and extract front-matter
    if let Some((frontmatter, body)) = parse_frontmatter(content) {
        if let Some(desc) = frontmatter.description.as_ref() {
            return (desc.clone(), body.to_string(), frontmatter.model);
        }
        if let Some(first_line_end) = body.find('\n') {
            let description = body[..first_line_end].trim().to_string();
            return (description, body.to_string(), frontmatter.model);
        } else {
            return (body.to_string(), body.to_string(), frontmatter.model);
        }
    }

    // No front-matter, use original logic: first line is description
    if let Some(first_line_end) = content.find('\n') {
        let description = content[..first_line_end].trim().to_string();
        let prompt = content.to_string();
        (description, prompt, None)
    } else {
        (content.to_string(), content.to_string(), None)
    }
}

/// Parse front-matter from content.
/// Returns front-matter and body content without front-matter.
fn parse_frontmatter(content: &str) -> Option<(CommandFrontmatter, &str)> {
    let content = content.trim_start();

    // YAML front-matter (---...---)
    if let Some(after_open) = content.strip_prefix("---\n") {
        // Handle empty front-matter (---\n---\n)
        if let Some(body) = after_open.strip_prefix("---\n") {
            let frontmatter = CommandFrontmatter {
                description: None,
                model: None,
            };
            return Some((frontmatter, body.trim_start()));
        }
        // Handle non-empty front-matter
        if let Some(end_pos) = after_open.find("\n---\n") {
            let frontmatter_str = &after_open[..end_pos];
            let body = after_open[end_pos + 5..].trim_start();

            let frontmatter = extract_frontmatter_from_yaml(frontmatter_str);
            return Some((frontmatter, body));
        }
    }

    // TOML front-matter (+++...+++)
    if let Some(after_open) = content.strip_prefix("+++\n") {
        // Handle empty front-matter (+++\n+++\n)
        if let Some(body) = after_open.strip_prefix("+++\n") {
            let frontmatter = CommandFrontmatter {
                description: None,
                model: None,
            };
            return Some((frontmatter, body.trim_start()));
        }
        // Handle non-empty front-matter
        if let Some(end_pos) = after_open.find("\n+++\n") {
            let frontmatter_str = &after_open[..end_pos];
            let body = after_open[end_pos + 5..].trim_start();

            let frontmatter = extract_frontmatter_from_toml(frontmatter_str);
            return Some((frontmatter, body));
        }
    }

    None
}

/// Extract description and model fields from YAML front-matter.
fn extract_frontmatter_from_yaml(yaml: &str) -> CommandFrontmatter {
    let mut frontmatter = CommandFrontmatter {
        description: None,
        model: None,
    };

    if let Ok(value) = serde_yaml_ng::from_str::<HashMap<String, JsonValue>>(yaml) {
        frontmatter.description = value
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        frontmatter.model = value
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    frontmatter
}

/// Extract description and model fields from TOML front-matter.
fn extract_frontmatter_from_toml(toml_str: &str) -> CommandFrontmatter {
    let mut frontmatter = CommandFrontmatter {
        description: None,
        model: None,
    };

    if let Ok(value) = toml::from_str::<HashMap<String, toml::Value>>(toml_str) {
        frontmatter.description = value
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        frontmatter.model = value
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }

    frontmatter
}

/// Parse arguments respecting quotes (single and double).
/// Quoted strings are treated as a single argument, even if they contain spaces.
/// Examples:
/// - `foo bar` -> ["foo", "bar"]
/// - `"foo bar" baz` -> ["foo bar", "baz"]
/// - `'foo bar' baz` -> ["foo bar", "baz"]
/// - `foo "bar baz"` -> ["foo", "bar baz"]
fn parse_arguments(args: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_quote = None; // Track which quote character we're in (None, Some('"'), or Some('\''))

    for c in args.chars() {
        match c {
            '"' | '\'' if in_quote.is_none() => {
                // Start of quoted string
                in_quote = Some(c);
            }
            '"' | '\'' if in_quote == Some(c) => {
                // End of quoted string
                in_quote = None;
            }
            ' ' | '\t' if in_quote.is_none() => {
                // Whitespace outside quotes - end current argument
                if !current.is_empty() {
                    result.push(current.clone());
                    current.clear();
                }
            }
            _ => {
                // Regular character or whitespace inside quotes
                current.push(c);
            }
        }
    }

    // Don't forget the last argument
    if !current.is_empty() {
        result.push(current);
    }

    result
}

/// Substitute variables in the prompt with provided values.
/// Supports:
/// - $ARGUMENTS - all arguments as-is
/// - $1, $2, $3, ... - individual positional arguments
/// - $0 - the entire argument string (same as $ARGUMENTS)
pub(crate) fn substitute_variables(prompt: &str, args: &str) -> String {
    let parsed_args = parse_arguments(args);
    let mut result = prompt.to_string();

    // Replace $ARGUMENTS and $0 with the entire argument string
    result = result.replace("$ARGUMENTS", args);
    result = result.replace("$0", args);

    // Replace positional arguments $1, $2, etc.
    // We need to replace higher numbers first to avoid $10 being replaced as $1 + "0"
    for i in (1..=parsed_args.len()).rev() {
        let placeholder = format!("${}", i);
        if let Some(value) = parsed_args.get(i - 1) {
            result = result.replace(&placeholder, value);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_recursive_command_loading() {
        // Create a temporary directory structure
        let temp_dir = TempDir::new().unwrap();
        let commands_dir = temp_dir.path().join("commands");

        // Create subdirectories
        std::fs::create_dir_all(commands_dir.join("suricata")).unwrap();
        std::fs::create_dir_all(commands_dir.join("project/subdir")).unwrap();

        // Create test command files
        std::fs::write(
            commands_dir.join("simple.md"),
            "Simple command\nThis is a simple command",
        )
        .unwrap();

        std::fs::write(
            commands_dir.join("suricata/review-pr.md"),
            "Review PR\nReview the pull request",
        )
        .unwrap();

        std::fs::write(
            commands_dir.join("project/subdir/deep.md"),
            "Deep command\nThis is deeply nested",
        )
        .unwrap();

        // Load commands from this directory
        let mut all_commands = Vec::new();
        for entry in WalkDir::new(&commands_dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            if let Some(ext) = path.extension() {
                if ext != "md" {
                    continue;
                }
            } else {
                continue;
            }

            let relative_path = path.strip_prefix(&commands_dir).unwrap();
            let name = relative_path
                .with_extension("")
                .to_str()
                .map(|s| s.to_string())
                .unwrap();

            let content = std::fs::read_to_string(path).unwrap();
            let (description, prompt, model) = parse_command_file(&content);
            all_commands.push(CustomCommand {
                name,
                description,
                prompt,
                _model: model,
            });
        }

        // Verify all commands were loaded
        assert_eq!(all_commands.len(), 3);

        // Check names are correct (with forward slashes for nested commands)
        let names: Vec<_> = all_commands.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"simple"));
        assert!(names.contains(&"suricata/review-pr"));
        assert!(names.contains(&"project/subdir/deep"));

        // Verify descriptions
        let review_cmd = all_commands
            .iter()
            .find(|c| c.name == "suricata/review-pr")
            .unwrap();
        assert_eq!(review_cmd.description, "Review PR");
    }

    #[test]
    fn test_parse_yaml_frontmatter() {
        let content = r#"---
description: "Test command from YAML"
author: "Test Author"
---

# Main Content
This is the actual command content."#;

        let (description, prompt, model) = parse_command_file(content);
        assert_eq!(description, "Test command from YAML");
        assert!(prompt.starts_with("# Main Content"));
        assert!(!prompt.contains("---"));
        assert!(model.is_none());
    }

    #[test]
    fn test_parse_yaml_frontmatter_with_model() {
        let content = r#"---
description: "Test command from YAML"
model: "claude/claude-haiku-4-5"
---

# Main Content
This is the actual command content."#;

        let (description, prompt, model) = parse_command_file(content);
        assert_eq!(description, "Test command from YAML");
        assert_eq!(model.as_deref(), Some("claude/claude-haiku-4-5"));
        assert!(prompt.starts_with("# Main Content"));
        assert!(!prompt.contains("---"));
    }

    #[test]
    fn test_parse_toml_frontmatter() {
        let content = r#"+++
description = "Test command from TOML"
author = "Test Author"
+++

# Main Content
This is the actual command content."#;

        let (description, prompt, model) = parse_command_file(content);
        assert_eq!(description, "Test command from TOML");
        assert!(prompt.starts_with("# Main Content"));
        assert!(!prompt.contains("+++"));
        assert!(model.is_none());
    }

    #[test]
    fn test_parse_toml_frontmatter_with_model() {
        let content = r#"+++
description = "Test command from TOML"
model = "openrouter/anthropic/claude-3.5-haiku"
+++

# Main Content
This is the actual command content."#;

        let (description, prompt, model) = parse_command_file(content);
        assert_eq!(description, "Test command from TOML");
        assert_eq!(
            model.as_deref(),
            Some("openrouter/anthropic/claude-3.5-haiku")
        );
        assert!(prompt.starts_with("# Main Content"));
        assert!(!prompt.contains("+++"));
    }

    #[test]
    fn test_parse_yaml_frontmatter_no_description() {
        let content = r#"---
author: "Test Author"
date: "2025-01-15"
---

First line is description
This is the actual command content."#;

        let (description, prompt, model) = parse_command_file(content);
        assert_eq!(description, "First line is description");
        assert!(prompt.starts_with("First line is description"));
        assert!(!prompt.contains("---"));
        assert!(model.is_none());
    }

    #[test]
    fn test_parse_no_frontmatter() {
        let content = "Simple description\nCommand content here";

        let (description, prompt, model) = parse_command_file(content);
        assert_eq!(description, "Simple description");
        assert_eq!(prompt, content);
        assert!(model.is_none());
    }

    #[test]
    fn test_parse_empty_yaml_frontmatter() {
        let content = "---\n---\n\nActual prompt content.\nMore content here.";

        let (description, prompt, model) = parse_command_file(content);
        assert_eq!(description, "Actual prompt content.");
        assert!(prompt.starts_with("Actual prompt content."));
        assert!(!prompt.contains("---"));
        assert!(model.is_none());
    }

    #[test]
    fn test_parse_empty_toml_frontmatter() {
        let content = "+++\n+++\n\nActual prompt content.\nMore content here.";

        let (description, prompt, model) = parse_command_file(content);
        assert_eq!(description, "Actual prompt content.");
        assert!(prompt.starts_with("Actual prompt content."));
        assert!(!prompt.contains("+++"));
        assert!(model.is_none());
    }

    #[test]
    fn test_parse_arguments_simple() {
        let args = "foo bar baz";
        let parsed = parse_arguments(args);
        assert_eq!(parsed, vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn test_parse_arguments_double_quotes() {
        let args = r#"foo "bar baz" qux"#;
        let parsed = parse_arguments(args);
        assert_eq!(parsed, vec!["foo", "bar baz", "qux"]);
    }

    #[test]
    fn test_parse_arguments_single_quotes() {
        let args = "foo 'bar baz' qux";
        let parsed = parse_arguments(args);
        assert_eq!(parsed, vec!["foo", "bar baz", "qux"]);
    }

    #[test]
    fn test_parse_arguments_mixed_quotes() {
        let args = r#"'first arg' "second arg" third"#;
        let parsed = parse_arguments(args);
        assert_eq!(parsed, vec!["first arg", "second arg", "third"]);
    }

    #[test]
    fn test_parse_arguments_empty() {
        let args = "";
        let parsed = parse_arguments(args);
        assert_eq!(parsed, Vec::<String>::new());
    }

    #[test]
    fn test_parse_arguments_only_whitespace() {
        let args = "   ";
        let parsed = parse_arguments(args);
        assert_eq!(parsed, Vec::<String>::new());
    }

    #[test]
    fn test_substitute_positional_arguments() {
        let prompt = "PR number: $1, Branch: $2";
        let args = "1234 feature-branch";
        let result = substitute_variables(prompt, args);
        assert_eq!(result, "PR number: 1234, Branch: feature-branch");
    }

    #[test]
    fn test_substitute_positional_with_quotes() {
        let prompt = "First: $1, Second: $2";
        let args = r#""multi word arg" simple"#;
        let result = substitute_variables(prompt, args);
        assert_eq!(result, "First: multi word arg, Second: simple");
    }

    #[test]
    fn test_substitute_arguments_variable() {
        let prompt = "All args: $ARGUMENTS";
        let args = "foo bar baz";
        let result = substitute_variables(prompt, args);
        assert_eq!(result, "All args: foo bar baz");
    }

    #[test]
    fn test_substitute_mixed_variables() {
        let prompt = "First: $1, All: $ARGUMENTS, Second: $2";
        let args = "alpha beta gamma";
        let result = substitute_variables(prompt, args);
        assert_eq!(result, "First: alpha, All: alpha beta gamma, Second: beta");
    }

    #[test]
    fn test_substitute_higher_numbers_first() {
        // Regression test: ensure $10 isn't replaced as $1 + "0"
        let prompt = "Args: $1 $10 $2";
        let args = "1 2 3 4 5 6 7 8 9 10 11";
        let result = substitute_variables(prompt, args);
        assert_eq!(result, "Args: 1 10 2");
    }

    #[test]
    fn test_substitute_no_args() {
        let prompt = "PR: $1, Branch: $2";
        let args = "";
        let result = substitute_variables(prompt, args);
        // Unused placeholders remain unchanged
        assert_eq!(result, "PR: $1, Branch: $2");
    }

    #[test]
    fn test_substitute_dollar_zero() {
        let prompt = "All args via $0";
        let args = "foo bar baz";
        let result = substitute_variables(prompt, args);
        assert_eq!(result, "All args via foo bar baz");
    }
}
