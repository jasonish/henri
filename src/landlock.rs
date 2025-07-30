// SPDX-License-Identifier: AGPL-3.0-only

use anyhow::{Context, Result};
use landlock::{
    ABI, Access, AccessFs, PathBeneath, Ruleset, RulesetAttr, RulesetCreated, RulesetCreatedAttr,
};
use std::env;
use std::fs::File;
use std::path::PathBuf;

pub fn apply_landlock_restrictions() -> Result<()> {
    // Get current working directory
    let cwd = env::current_dir().context("Failed to get current directory")?;

    // Get home directory and construct ~/.henri path
    let home_dir = dirs::home_dir().context("Failed to get home directory")?;
    let henri_config_dir = home_dir.join(".henri");

    // Determine the best available ABI version
    let abi = ABI::V3; // Use V3 for broader compatibility (Linux 6.2+)

    // Create ruleset with all filesystem access handling
    let ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .context("Failed to create landlock ruleset")?;

    // Create the ruleset
    let mut ruleset = ruleset
        .create()
        .context("Failed to create landlock ruleset")?;

    // Add read-write rules for current directory
    ruleset = add_rw_rule(ruleset, &cwd)
        .context("Failed to add read-write rule for current directory")?;

    // Add read-write rules for ~/.henri (create directory if it doesn't exist)
    if !henri_config_dir.exists() {
        std::fs::create_dir_all(&henri_config_dir)
            .context("Failed to create ~/.henri directory")?;
    }
    ruleset = add_rw_rule(ruleset, &henri_config_dir)
        .context("Failed to add read-write rule for ~/.henri")?;

    // Add read-only access to important system directories
    let read_only_paths = vec![
        "/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc", "/proc", "/sys", "/dev",
    ];

    for path in read_only_paths {
        if PathBuf::from(path).exists() {
            // We need to handle the ownership transfer carefully
            let result = add_ro_rule(ruleset, path);
            match result {
                Ok(new_ruleset) => {
                    ruleset = new_ruleset;
                }
                Err(e) => {
                    // Log warning but continue - best effort approach
                    eprintln!("Warning: Failed to add read-only rule for {path}: {e}");
                    // Since add_ro_rule consumed ruleset, we can't continue
                    // This is a design limitation - we should handle this better
                    return Err(e).context("Failed to add read-only rule");
                }
            }
        }
    }

    // Apply the ruleset to the current process
    ruleset
        .restrict_self()
        .context("Failed to apply landlock restrictions")?;

    Ok(())
}

fn add_rw_rule(
    ruleset: RulesetCreated,
    path: impl AsRef<std::path::Path>,
) -> Result<RulesetCreated> {
    let access_all = AccessFs::from_all(ABI::V3);
    let file = File::open(path.as_ref()).context("Failed to open path for landlock rule")?;
    Ok(ruleset.add_rule(PathBeneath::new(file, access_all))?)
}

fn add_ro_rule(
    ruleset: RulesetCreated,
    path: impl AsRef<std::path::Path>,
) -> Result<RulesetCreated> {
    let access_read = AccessFs::from_read(ABI::V3);
    let file = File::open(path.as_ref()).context("Failed to open path for landlock rule")?;
    Ok(ruleset.add_rule(PathBeneath::new(file, access_read))?)
}
