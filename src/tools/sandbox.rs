// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Landlock sandboxing for tool execution.
//!
//! This module provides filesystem sandboxing using Linux Landlock LSM.
//! Sandboxing is best-effort - if Landlock isn't available (older kernels,
//! non-Linux systems), operations proceed without restriction.
//!
//! Only write operations are restricted - reads are allowed everywhere.

use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreated, RulesetCreatedAttr,
    path_beneath_rules,
};

/// If cwd is a git worktree, return the path to the main .git directory.
///
/// Git worktrees have a `.git` file (not directory) containing:
/// `gitdir: /path/to/main/repo/.git/worktrees/name`
///
/// We need write access to the entire `.git` directory (not just the worktree
/// subdirectory) because git writes to `.git/objects`, `.git/refs`, etc.
fn get_git_worktree_dir(cwd: &Path) -> Option<PathBuf> {
    let git_path = cwd.join(".git");
    if git_path.is_file() {
        let content = std::fs::read_to_string(&git_path).ok()?;
        let gitdir = content
            .lines()
            .find(|line| line.starts_with("gitdir:"))?
            .strip_prefix("gitdir:")?
            .trim();
        let path = PathBuf::from(gitdir);
        // Navigate up from .git/worktrees/name to .git
        // The path typically ends in .git/worktrees/<name>
        if let Some(git_dir) = path.ancestors().find(|p| p.ends_with(".git"))
            && git_dir.exists()
        {
            return Some(git_dir.to_path_buf());
        }
    }
    None
}

/// Paths that should be writable beyond the working directory.
const WRITE_PATHS: &[&str] = &["/tmp", "/var/tmp", "/dev/null", "/dev/tty"];

/// Create a Landlock ruleset for bash command execution.
///
/// The ruleset restricts only write operations:
/// - Write access allowed to the working directory and subdirectories
/// - Write access allowed to git worktree directory (if in a worktree)
/// - Write access allowed to /tmp, /var/tmp, /dev/null, /dev/tty
/// - Read access is unrestricted
///
/// Returns `None` if Landlock is not supported or paths can't be accessed.
pub(crate) fn create_bash_ruleset(cwd: &Path) -> Option<RulesetCreated> {
    let abi = ABI::V5;

    // Only restrict write operations - reads are unrestricted
    let write_access = AccessFs::from_all(abi) & !AccessFs::from_read(abi);

    let ruleset = Ruleset::default()
        .handle_access(write_access)
        .ok()?
        .create()
        .ok()?;

    // Allow writes to cwd and subdirectories
    let ruleset = ruleset
        .add_rules(path_beneath_rules([cwd], write_access))
        .ok()?;

    // Allow writes to git worktree directory if applicable
    let ruleset = if let Some(git_dir) = get_git_worktree_dir(cwd) {
        ruleset
            .add_rules(path_beneath_rules([git_dir], write_access))
            .ok()?
    } else {
        ruleset
    };

    // Allow writes to standard writable paths
    let ruleset = ruleset
        .add_rules(path_beneath_rules(WRITE_PATHS, write_access))
        .ok()?;

    Some(ruleset)
}

/// Apply a Landlock ruleset to the current thread.
///
/// This should be called in a `pre_exec` hook before executing a command.
/// Returns `Ok(())` if restrictions were applied or if Landlock is unavailable.
pub(crate) fn apply_ruleset(ruleset: RulesetCreated) -> std::io::Result<()> {
    // restrict_self() returns an error only if the syscall fails,
    // not if enforcement is partial (e.g., older kernel ABI)
    ruleset.restrict_self().map(|_| ()).or(Ok(()))
}

/// Check if Landlock sandboxing is available on this system.
///
/// Returns true if Landlock is supported by the kernel and can be used.
pub(crate) fn is_available() -> bool {
    // Try to create a minimal ruleset to check availability
    let abi = ABI::V5;
    let write_access = AccessFs::from_all(abi) & !AccessFs::from_read(abi);
    Ruleset::default()
        .handle_access(write_access)
        .ok()
        .and_then(|r| r.create().ok())
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_create_ruleset() {
        let cwd = env::current_dir().unwrap();
        // This may return None on systems without Landlock support
        let _ruleset = create_bash_ruleset(&cwd);
        // Just verify it doesn't panic
    }
}
