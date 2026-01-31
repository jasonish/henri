// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! System prompts and guidelines for Henri.

use std::path::PathBuf;

use chrono::Local;

use crate::services::Services;
use crate::skills;

/// Git guidelines embedded at compile time.
const GIT_GUIDELINES: &str = include_str!("git.md");

/// Prompt for building/updating AGENTS.md files.
pub(crate) const BUILD_AGENTS_MD_PROMPT: &str = include_str!("build-agents-md.md");

/// Default system prompt for AI assistants.
const DEFAULT_SYSTEM_PROMPT: &str = include_str!("system.md");

/// Maximum depth for project structure tree.
const MAX_DEPTH: usize = 2;

/// Maximum number of entries in project structure.
const MAX_ENTRIES: usize = 500;

const READ_ONLY_NOTICE: &str = "Read-only mode is enabled. Do not attempt to modify files; write-capable tools are disabled and filesystem writes are blocked.";

/// Directories to skip in project structure.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".github",
    ".idea",
    "node_modules",
    "target",
    "vendor",
    "venv",
    ".venv",
    "__pycache__",
    ".vscode",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "coverage",
];

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

pub(crate) fn system_prompt_with_services(services: Option<&Services>) -> Vec<String> {
    let mut prompt = vec![];

    prompt.push(default_system_prompt().to_string());

    if let Ok(cwd) = std::env::current_dir() {
        prompt.push(format!("Current working directory: {}", cwd.display()));
    }

    if let Some(project_structure) = project_structure() {
        prompt.push(project_structure);
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

    // Add skill prompts (agentskills.io format)
    if let Some(skills_block) = skills::get_skill_prompts() {
        prompt.push(skills_block);
    }

    // If provided, append read-only mode notice.
    if services.is_some_and(|s| s.is_read_only()) {
        prompt.push(READ_ONLY_NOTICE.to_string());
    }

    // Date goes last so it doesn't invalidate prompt caching too frequently.
    // The cache_control marker is placed on the previous block (agent files),
    // so the static content gets cached while the dynamic date comes after.
    prompt.push(format!(
        "Current date: {}",
        Local::now().format("%Y-%m-%d %Z")
    ));

    prompt
}

/// Generates a project structure overview for the current directory.
/// Returns None if no files found or on error.
fn project_structure() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;

    let entries = if is_git_directory() {
        project_structure_from_git(&cwd)
    } else {
        project_structure_from_filesystem(&cwd)
    }?;

    if entries.is_empty() {
        return None;
    }

    Some(format!(
        "<ProjectStructure>\n{}\n</ProjectStructure>",
        entries
    ))
}

/// Gets project structure from git repository.
fn project_structure_from_git(_cwd: &PathBuf) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["ls-tree", "-r", "--name-only", "HEAD"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut tree = TreeBuilder::new(MAX_DEPTH, MAX_ENTRIES);

    for line in stdout.lines() {
        if let Some(depth) = path_depth(line)
            && depth <= MAX_DEPTH
            && !should_skip_path(line)
        {
            tree.add(line, depth);
        }
    }

    tree.build()
}

/// Gets project structure from filesystem.
fn project_structure_from_filesystem(cwd: &PathBuf) -> Option<String> {
    use walkdir::WalkDir;

    let mut tree = TreeBuilder::new(MAX_DEPTH, MAX_ENTRIES);

    let walker = WalkDir::new(cwd)
        .min_depth(1)
        .max_depth(MAX_DEPTH + 1)
        .into_iter();

    for entry in walker.filter_map(|e| e.ok()) {
        let path = entry.path();
        let relative = path.strip_prefix(cwd).ok()?;
        let path_str = relative.to_str()?;

        if should_skip_path(path_str) {
            continue;
        }

        let depth = path_depth(path_str).unwrap_or(MAX_DEPTH + 1);
        if depth <= MAX_DEPTH {
            tree.add(path_str, depth);
        }
    }

    tree.build()
}

/// Returns the depth of a path (number of path separators).
/// Returns None for empty paths.
fn path_depth(path: &str) -> Option<usize> {
    if path.is_empty() {
        return None;
    }
    Some(path.chars().filter(|&c| c == '/' || c == '\\').count())
}

/// Checks if a path should be skipped based on its components.
fn should_skip_path(path: &str) -> bool {
    path.split(['/', '\\'])
        .any(|component| SKIP_DIRS.contains(&component))
}

/// Builder for creating a tree representation of project structure.
/// Prioritizes shallower entries over deeper ones when trimming.
struct TreeBuilder {
    max_depth: usize,
    max_entries: usize,
    /// Entries grouped by depth: entries_by_depth[0] = depth-0 entries, etc.
    entries_by_depth: Vec<Vec<String>>,
}

impl TreeBuilder {
    fn new(max_depth: usize, max_entries: usize) -> Self {
        Self {
            max_depth,
            max_entries,
            entries_by_depth: vec![Vec::new(); max_depth + 1],
        }
    }

    fn add(&mut self, path: &str, depth: usize) {
        if depth <= self.max_depth {
            self.entries_by_depth[depth].push(path.to_string());
        }
    }

    fn build(self) -> Option<String> {
        let total: usize = self.entries_by_depth.iter().map(|v| v.len()).sum();
        if total == 0 {
            return None;
        }

        // Calculate how many entries to take from each depth level.
        // Priority: depth 0 > depth 1 > depth 2, etc.
        // Always take all entries from shallower depths before deeper ones.
        let mut budget = self.max_entries;
        let mut limits = vec![0usize; self.max_depth + 1];

        for (depth, entries) in self.entries_by_depth.iter().enumerate() {
            let take = entries.len().min(budget);
            limits[depth] = take;
            budget = budget.saturating_sub(take);
        }

        // Collect entries with their paths for sorting
        let mut all_entries: Vec<(String, usize)> = Vec::new();

        for (depth, entries) in self.entries_by_depth.iter().enumerate() {
            for path in entries.iter().take(limits[depth]) {
                all_entries.push((path.clone(), depth));
            }
        }

        // Sort entries alphabetically by path for consistent tree-like output
        all_entries.sort_by(|a, b| a.0.cmp(&b.0));

        // Format with indentation
        let formatted: Vec<String> = all_entries
            .iter()
            .map(|(path, depth)| {
                let indent = "  ".repeat(*depth);
                format!("{}{}", indent, path)
            })
            .collect();

        let mut result = formatted.join("\n");

        // Add truncation notice if we didn't show everything
        let shown: usize = limits.iter().sum();
        if shown < total {
            result.push_str(&format!("\n... ({} of {} entries shown)", shown, total));
        }

        Some(result)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_depth() {
        assert_eq!(path_depth(""), None);
        assert_eq!(path_depth("file.txt"), Some(0));
        assert_eq!(path_depth("dir/file.txt"), Some(1));
        assert_eq!(path_depth("dir/subdir/file.txt"), Some(2));
        assert_eq!(path_depth("a/b/c/d/e"), Some(4));
    }

    #[test]
    fn test_should_skip_path() {
        assert!(should_skip_path("node_modules/index.js"));
        assert!(should_skip_path("target/debug/test"));
        assert!(should_skip_path(".git/config"));
        assert!(should_skip_path("vendor/lib.rs"));
        assert!(should_skip_path("src/node_modules/test"));
        assert!(!should_skip_path("src/main.rs"));
        assert!(!should_skip_path("Cargo.toml"));
        assert!(!should_skip_path("tests/it_works.rs"));
    }

    #[test]
    fn test_tree_builder() {
        let mut tree = TreeBuilder::new(2, 5);

        // Add entries at different depths
        tree.add("Cargo.toml", 0);
        tree.add("README.md", 0);
        tree.add("src/main.rs", 1);
        tree.add("src/lib.rs", 1);
        tree.add("src/tools/bash.rs", 2);
        tree.add("src/tools/file_read.rs", 2);
        tree.add("src/tools/grep.rs", 2);

        let result = tree.build().unwrap();

        // Depth 0 and 1 should always be included (4 entries)
        assert!(result.contains("Cargo.toml"));
        assert!(result.contains("README.md"));
        assert!(result.contains("src/main.rs"));
        assert!(result.contains("src/lib.rs"));

        // Only 1 depth-2 entry fits in budget of 5
        assert!(result.contains("src/tools/bash.rs"));

        // These should be trimmed
        assert!(!result.contains("file_read.rs"));
        assert!(!result.contains("grep.rs"));

        // Truncation message
        assert!(result.contains("5 of 7 entries shown"));
    }

    #[test]
    fn test_tree_builder_priority() {
        // Test that shallower entries are always prioritized
        let mut tree = TreeBuilder::new(2, 3);

        // Add many deep entries first
        tree.add("a/b/file1.rs", 2);
        tree.add("a/b/file2.rs", 2);
        tree.add("a/b/file3.rs", 2);

        // Then add shallow entries
        tree.add("Cargo.toml", 0);
        tree.add("src/main.rs", 1);

        let result = tree.build().unwrap();

        // Shallow entries must be included despite being added last
        assert!(result.contains("Cargo.toml"));
        assert!(result.contains("src/main.rs"));

        // Only 1 deep entry fits
        assert!(result.contains("a/b/file1.rs"));
        assert!(!result.contains("file2.rs"));
        assert!(!result.contains("file3.rs"));
    }

    #[test]
    fn test_tree_builder_empty() {
        let tree = TreeBuilder::new(2, 100);
        assert!(tree.build().is_none());
    }
}
