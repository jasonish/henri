// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! System prompts and guidelines for Henri.

use std::path::PathBuf;

use chrono::Local;

/// Git guidelines embedded at compile time.
const GIT_GUIDELINES: &str = include_str!("git.md");

/// Prompt for building/updating AGENTS.md files.
pub const BUILD_AGENTS_MD_PROMPT: &str = include_str!("build-agents-md.md");

/// Default system prompt for AI assistants.
const DEFAULT_SYSTEM_PROMPT: &str = include_str!("system.md");

/// Returns the default system prompt.
pub(crate) fn default_system_prompt() -> &'static str {
    DEFAULT_SYSTEM_PROMPT
}

/// Filenames to search for agent configuration files (searched up directory tree).
const AGENT_FILENAMES: &[&str] = &["AGENTS.md", "CLAUDE.md"];

/// README filenames to include (only from current directory).
const README_FILENAMES: &[&str] = &["README.md", "README.txt", "README"];

/// Returns the git guidelines if the current directory is a git repository.
/// Uses `git rev-parse --is-inside-work-tree` to detect git repos.
pub(crate) fn git_guidelines_if_in_repo() -> Option<&'static str> {
    if is_git_directory() {
        Some(GIT_GUIDELINES)
    } else {
        None
    }
}

/// Check if the current directory is inside a git repository.
fn is_git_directory() -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub(crate) fn system_prompt() -> Vec<String> {
    let mut prompt = vec![];

    prompt.push(default_system_prompt().to_string());

    prompt.push(format!(
        "Current date and time is {}",
        Local::now().format("%Y-%m-%d %H:%M:%S %Z")
    ));

    if let Ok(cwd) = std::env::current_dir() {
        prompt.push(format!("Current working directory: {}", cwd.display()));
    }

    if let Some(git_guidelines) = git_guidelines_if_in_repo() {
        prompt.push(format!("<GitGuidelines>{}</GitGuidelines>", git_guidelines));
    }

    for agent_file in discover_agent_files() {
        let instruction = format!(
            "<Instructions filename=\"{}\">{}</Instructions>",
            agent_file.path.display(),
            agent_file.contents
        );
        prompt.push(instruction);
    }

    prompt
}

/// A discovered agent configuration file.
pub(crate) struct AgentFile {
    pub path: PathBuf,
    pub contents: String,
}

/// Discover AGENTS.md and CLAUDE.md files from current directory up to home/root,
/// plus README files from the current directory only.
/// Returns files in order from closest (current dir) to farthest (home/root).
/// Prefers AGENTS.md over CLAUDE.md - only uses CLAUDE.md as fallback if AGENTS.md
/// doesn't exist in a given directory.
/// This function reads files fresh each time to pick up any changes.
pub(crate) fn discover_agent_files() -> Vec<AgentFile> {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    let home = home_directory();
    let stop_at = determine_stop_directory(&cwd, home.as_deref());

    let mut results = Vec::new();

    // Check for README files in current directory only (first found wins)
    for filename in README_FILENAMES {
        let file_path = cwd.join(filename);
        if file_path.is_file()
            && let Ok(contents) = std::fs::read_to_string(&file_path)
        {
            results.push(AgentFile {
                path: file_path,
                contents,
            });
            break; // Only use first found README
        }
    }

    // Search for AGENTS.md/CLAUDE.md up the directory tree
    let mut current = Some(cwd.as_path());

    while let Some(dir) = current {
        // Prefer AGENTS.md, fall back to CLAUDE.md if not found
        for filename in AGENT_FILENAMES {
            let file_path = dir.join(filename);
            if file_path.is_file()
                && let Ok(contents) = std::fs::read_to_string(&file_path)
            {
                results.push(AgentFile {
                    path: file_path,
                    contents,
                });
                break; // Only use first found file in each directory
            }
        }

        // Stop at the designated boundary
        if Some(dir) == stop_at.as_deref() {
            break;
        }

        current = dir.parent();
    }

    results
}

fn home_directory() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

fn determine_stop_directory(
    cwd: &std::path::Path,
    home: Option<&std::path::Path>,
) -> Option<PathBuf> {
    match home {
        Some(home_path) if cwd.starts_with(home_path) => Some(home_path.to_path_buf()),
        _ => None,
    }
}
