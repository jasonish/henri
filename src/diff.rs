// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::path::Path;

pub(crate) struct DiffResult {
    pub unified_diff: String,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub has_changes: bool,
}

pub(crate) fn unified_diff(path: &Path, old: &str, new: &str, context_lines: usize) -> DiffResult {
    let diff = similar::TextDiff::from_lines(old, new);

    let unified_diff = diff
        .unified_diff()
        .context_radius(context_lines)
        .header(
            &format!("a/{}", path.display()),
            &format!("b/{}", path.display()),
        )
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
        lines_added,
        lines_removed,
        has_changes,
    }
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
        assert_eq!(result.lines_added, 1);
        assert_eq!(result.lines_removed, 0);
    }

    #[test]
    fn test_unified_diff_remove_lines() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nline2\n";
        let path = Path::new("test.txt");

        let result = unified_diff(path, old, new, 3);

        assert!(result.has_changes);
        assert_eq!(result.lines_added, 0);
        assert_eq!(result.lines_removed, 1);
    }

    #[test]
    fn test_unified_diff_no_changes() {
        let old = "line1\nline2\n";
        let new = "line1\nline2\n";
        let path = Path::new("test.txt");

        let result = unified_diff(path, old, new, 3);

        assert!(!result.has_changes);
        assert_eq!(result.lines_added, 0);
        assert_eq!(result.lines_removed, 0);
    }

    #[test]
    fn test_unified_diff_modify_lines() {
        let old = "line1\nline2\nline3\n";
        let new = "line1\nmodified\nline3\n";
        let path = Path::new("test.txt");

        let result = unified_diff(path, old, new, 3);

        assert!(result.has_changes);
        assert_eq!(result.lines_added, 1);
        assert_eq!(result.lines_removed, 1);
    }

    #[test]
    fn test_unified_diff_duplicate_lines() {
        // Test adding duplicate lines (would fail with HashSet approach)
        let old = "a\nb\n";
        let new = "a\nb\nb\n";
        let path = Path::new("test.txt");

        let result = unified_diff(path, old, new, 3);

        assert!(result.has_changes);
        assert_eq!(result.lines_added, 1);
        assert_eq!(result.lines_removed, 0);
    }
}
