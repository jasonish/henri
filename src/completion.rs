// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! File path completion for CLI prompts.

use std::path::PathBuf;

/// File path completer with match state
pub(crate) struct FileCompleter {
    /// Current completion matches
    pub(crate) matches: Vec<String>,
    /// Currently selected match index
    pub(crate) index: usize,
    /// Working directory for relative path resolution
    working_dir: PathBuf,
}

impl FileCompleter {
    /// Create a new FileCompleter with the given working directory
    pub(crate) fn new(working_dir: PathBuf) -> Self {
        Self {
            matches: Vec::new(),
            index: 0,
            working_dir,
        }
    }

    /// Check if the completion menu is active (has matches)
    pub(crate) fn is_active(&self) -> bool {
        !self.matches.is_empty()
    }

    /// Clear all matches
    pub(crate) fn clear(&mut self) {
        self.matches.clear();
        self.index = 0;
    }

    /// Check if a word should trigger completion (starts with `.`, `/`, or `~`)
    pub(crate) fn should_complete(word: &str) -> bool {
        word.starts_with('/') || word.starts_with('.') || word.starts_with('~')
    }

    /// Initialize completion matches for the given prefix
    pub(crate) fn init(&mut self, prefix: &str) {
        self.matches = self.get_matches(prefix);
        self.index = 0;
    }

    /// Get the currently selected match, if any
    pub(crate) fn current(&self) -> Option<&str> {
        self.matches.get(self.index).map(|s| s.as_str())
    }

    /// Move selection by delta (positive = forward, negative = backward)
    pub(crate) fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let len = self.matches.len() as isize;
        let new_index = (self.index as isize + delta).rem_euclid(len);
        self.index = new_index as usize;
    }

    /// Get file completion matches for a prefix
    pub(crate) fn get_matches(&self, prefix: &str) -> Vec<String> {
        let working_dir = &self.working_dir;

        // Determine the prefix to prepend to results (preserve what the user typed)
        let (search_dir, partial_name, result_prefix) = if prefix.starts_with('/') {
            // Absolute path
            let base_path = PathBuf::from(prefix);
            if base_path.as_os_str() == "/" {
                (PathBuf::from("/"), String::new(), "/".to_string())
            } else if prefix.ends_with('/') {
                // Path ends with "/" - list contents of that directory
                if base_path.is_dir() {
                    (base_path, String::new(), prefix.to_string())
                } else {
                    return Vec::new();
                }
            } else {
                let parent = base_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("/"));
                let file_name = base_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                let prefix_str = if parent.as_os_str() == "/" {
                    "/".to_string()
                } else {
                    format!("{}/", parent.display())
                };
                (parent, file_name, prefix_str)
            }
        } else if prefix.starts_with('~') {
            // Tilde expansion - expand ~ to home directory
            let Some(home_dir) = dirs::home_dir() else {
                return Vec::new();
            };

            if prefix == "~" {
                // Just "~" - list home directory contents with "~/" prefix
                (home_dir, String::new(), "~/".to_string())
            } else if let Some(rel_path) = prefix.strip_prefix("~/") {
                // Path starting with ~/
                if rel_path.is_empty() {
                    // Just "~/" - list home directory
                    (home_dir, String::new(), "~/".to_string())
                } else if prefix.ends_with('/') {
                    // Path ends with "/" - list contents of that directory
                    let dir_path = home_dir.join(rel_path);
                    if dir_path.is_dir() {
                        (dir_path, String::new(), prefix.to_string())
                    } else {
                        return Vec::new();
                    }
                } else {
                    // Completing a partial filename
                    let base_path = home_dir.join(rel_path);
                    let parent = base_path.parent().unwrap_or(&home_dir);
                    let file_name = base_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();

                    // Build the prefix to prepend to results
                    let rel_parent = parent.strip_prefix(&home_dir).unwrap_or(parent);
                    let prefix_str = if rel_parent.as_os_str().is_empty() {
                        "~/".to_string()
                    } else {
                        format!("~/{}/", rel_parent.display())
                    };
                    (parent.to_path_buf(), file_name, prefix_str)
                }
            } else {
                return Vec::new();
            }
        } else if prefix == "." {
            // Just "." - list current directory contents with "./" prefix
            (working_dir.clone(), String::new(), "./".to_string())
        } else if prefix == ".." {
            // Just ".." - list parent directory contents with "../" prefix
            let parent = working_dir.parent().unwrap_or(working_dir);
            (parent.to_path_buf(), String::new(), "../".to_string())
        } else if let Some(rel_path) = prefix.strip_prefix("./") {
            // Relative path starting with ./
            if rel_path.is_empty() {
                // Just "./" - list current directory
                (working_dir.clone(), String::new(), "./".to_string())
            } else if prefix.ends_with('/') {
                // Path ends with "/" - list contents of that directory
                let dir_path = working_dir.join(rel_path);
                if dir_path.is_dir() {
                    (dir_path, String::new(), prefix.to_string())
                } else {
                    return Vec::new();
                }
            } else {
                // Completing a partial filename
                let base_path = working_dir.join(rel_path);
                let parent = base_path.parent().unwrap_or(working_dir);
                let file_name = base_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                // Build the prefix to prepend to results
                let rel_parent = parent.strip_prefix(working_dir).unwrap_or(parent);
                let prefix_str = if rel_parent.as_os_str().is_empty() {
                    "./".to_string()
                } else {
                    format!("./{}/", rel_parent.display())
                };
                (parent.to_path_buf(), file_name, prefix_str)
            }
        } else if prefix.starts_with("../") {
            // Relative path starting with ../
            if prefix.ends_with('/') {
                // Path ends with "/" - list contents of that directory
                let dir_path = working_dir.join(prefix);
                if dir_path.is_dir() {
                    (dir_path, String::new(), prefix.to_string())
                } else {
                    return Vec::new();
                }
            } else {
                let base_path = working_dir.join(prefix);
                let parent = base_path.parent().unwrap_or(working_dir);
                let file_name = base_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                // Preserve the ../ prefix structure
                let prefix_without_file = if let Some(idx) = prefix.rfind('/') {
                    format!("{}/", &prefix[..idx])
                } else {
                    "../".to_string()
                };
                (parent.to_path_buf(), file_name, prefix_without_file)
            }
        } else {
            return Vec::new();
        };

        // Try to read directory
        let mut matches = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&search_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();

                // Skip hidden files unless the partial starts with '.'
                if name_str.starts_with('.') && !partial_name.starts_with('.') {
                    continue;
                }

                // Check if name matches partial
                if name_str.starts_with(&partial_name) {
                    let is_dir = entry.path().is_dir();

                    // Build the display path with the original prefix
                    let display_path = if is_dir {
                        format!("{}{}/", result_prefix, name_str)
                    } else {
                        format!("{}{}", result_prefix, name_str)
                    };

                    matches.push(display_path);
                }
            }
        }

        // Sort: directories first, then alphabetically
        matches.sort_by(|a, b| {
            let a_is_dir = a.ends_with('/');
            let b_is_dir = b.ends_with('/');
            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.cmp(b),
            }
        });

        matches
    }
}

/// Get the word at cursor position in a buffer
/// Returns (start_index, end_index, word)
pub(crate) fn get_word_at_cursor(buffer: &str, cursor: usize) -> Option<(usize, usize, String)> {
    // Find the start of the word (go backwards until whitespace or start)
    let word_start = buffer[..cursor]
        .rfind(|c: char| c.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);

    // Find the end of the word (go forwards until whitespace or end)
    let word_end = buffer[word_start..]
        .find(|c: char| c.is_whitespace())
        .map(|i| word_start + i)
        .unwrap_or(buffer.len());

    let word = buffer[word_start..word_end].to_string();
    Some((word_start, word_end, word))
}
