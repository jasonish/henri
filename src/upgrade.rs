// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Upgrade checking functionality for Henri.

use serde::Deserialize;

use crate::version::VERSION;

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/jasonish/henri/releases/latest";

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
}

/// Result of checking for an upgrade.
pub(crate) enum UpgradeStatus {
    /// A newer version is available.
    Available { latest: String, url: String },
    /// Already on the latest version.
    UpToDate,
}

/// Check GitHub releases for the latest version.
pub(crate) async fn check_for_upgrade() -> Result<UpgradeStatus, String> {
    let client = reqwest::Client::new();
    let response = client
        .get(GITHUB_RELEASES_API)
        .header("User-Agent", format!("henri/{}", VERSION))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch releases: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("GitHub API returned status: {}", response.status()));
    }

    let release: GitHubRelease = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse release info: {}", e))?;

    // Strip 'v' prefix if present for comparison
    let latest_version = release.tag_name.trim_start_matches('v');
    let current_version = VERSION.trim_start_matches('v');

    if is_newer_version(latest_version, current_version) {
        Ok(UpgradeStatus::Available {
            latest: latest_version.to_string(),
            url: release.html_url,
        })
    } else {
        Ok(UpgradeStatus::UpToDate)
    }
}

/// Compare two semantic version strings.
/// Returns true if `latest` is newer than `current`.
fn is_newer_version(latest: &str, current: &str) -> bool {
    let parse_version = |v: &str| -> Vec<u32> {
        v.split('.')
            .filter_map(|part| part.parse::<u32>().ok())
            .collect()
    };

    let latest_parts = parse_version(latest);
    let current_parts = parse_version(current);

    for (l, c) in latest_parts.iter().zip(current_parts.iter()) {
        match l.cmp(c) {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => continue,
        }
    }

    // If all compared parts are equal, check if latest has more parts
    latest_parts.len() > current_parts.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_version() {
        // Newer versions
        assert!(is_newer_version("1.0.0", "0.1.0"));
        assert!(is_newer_version("0.2.0", "0.1.0"));
        assert!(is_newer_version("0.1.1", "0.1.0"));
        assert!(is_newer_version("1.0.0", "0.9.9"));
        assert!(is_newer_version("2.0.0", "1.99.99"));

        // Same version
        assert!(!is_newer_version("0.1.0", "0.1.0"));
        assert!(!is_newer_version("1.0.0", "1.0.0"));

        // Older versions
        assert!(!is_newer_version("0.1.0", "0.2.0"));
        assert!(!is_newer_version("0.1.0", "1.0.0"));
        assert!(!is_newer_version("0.0.9", "0.1.0"));
    }
}
