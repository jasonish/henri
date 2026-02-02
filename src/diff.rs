// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::path::Path;

pub(crate) struct DiffResult {
    pub unified_diff: String,
    pub _lines_added: usize,
    pub _lines_removed: usize,
    pub has_changes: bool,
}

pub(crate) fn unified_diff(_path: &Path, old: &str, new: &str, context_lines: usize) -> DiffResult {
    let diff = similar::TextDiff::from_lines(old, new);

    let unified_diff = diff
        .unified_diff()
        .context_radius(context_lines)
        .to_string();

    let mut lines_added = 0;
    let mut lines_removed = 0;

    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => lines_added += 1,
            similar::ChangeTag::Delete => lines_removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }

    let has_changes = lines_added > 0 || lines_removed > 0;

    DiffResult {
        unified_diff,
        _lines_added: lines_added,
        _lines_removed: lines_removed,
        has_changes,
    }
}

fn format_line_count(count: usize) -> String {
    if count == 1 {
        "1 line".to_string()
    } else {
        format!("{} lines", count)
    }
}

pub(crate) fn format_diff_summary(lines_added: usize, lines_removed: usize) -> Option<String> {
    if lines_added == 0 && lines_removed == 0 {
        return None;
    }

    Some(if lines_added > 0 && lines_removed > 0 {
        format!(
            "Added {}, removed {}",
            format_line_count(lines_added),
            format_line_count(lines_removed)
        )
    } else if lines_added > 0 {
        format!("Added {}", format_line_count(lines_added))
    } else {
        format!("Removed {}", format_line_count(lines_removed))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_diff_add_lines() {
        let old = "line1\nline2\n";
        let new = "line1\nline2\nline3\n";
        let path = Path::new("test.txt");

        let result = unified_diff(path, old, new, 3);

        assert!(result.has_changes);
        assert_eq!(result._lines_added, 1);
        assert_eq!(result._lines_removed, 0);
    }

    #[test]
    fn test_unified_diff_remove_lines() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nline2\n";
        let path = Path::new("test.txt");

        let result = unified_diff(path, old, new, 3);

        assert!(result.has_changes);
        assert_eq!(result._lines_added, 0);
        assert_eq!(result._lines_removed, 1);
    }

    #[test]
    fn test_unified_diff_no_changes() {
        let old = "line1\nline2\n";
        let new = "line1\nline2\n";
        let path = Path::new("test.txt");

        let result = unified_diff(path, old, new, 3);

        assert!(!result.has_changes);
        assert_eq!(result._lines_added, 0);
        assert_eq!(result._lines_removed, 0);
    }

    #[test]
    fn test_unified_diff_modify_lines() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nmodified\nline3\n";
        let path = Path::new("test.txt");

        let result = unified_diff(path, old, new, 3);

        assert!(result.has_changes);
        assert_eq!(result._lines_added, 1);
        assert_eq!(result._lines_removed, 1);
    }

    #[test]
    fn test_unified_diff_duplicate_lines() {
        // Test adding duplicate lines (would fail with HashSet approach)
        let old = "a\nb\n";
        let new = "a\nb\nb\n";
        let path = Path::new("test.txt");

        let result = unified_diff(path, old, new, 3);

        assert!(result.has_changes);
        assert_eq!(result._lines_added, 1);
        assert_eq!(result._lines_removed, 0);
    }
}
