// SPDX-License-Identifier: MIT
// Clipboard operations for the TUI

use std::io::{self, Write};
use std::process::{Command, Stdio};

/// Copy text to clipboard using wl-copy (Wayland) or xclip (X11)
pub(crate) fn copy_selection(text: &str) -> Result<(), String> {
    // Try wl-copy first (Wayland)
    if let Ok(mut child) = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
        return Ok(());
    }

    // Try xclip (X11)
    if let Ok(mut child) = Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
        return Ok(());
    }

    Err("No clipboard tool available (install wl-copy or xclip)".to_string())
}

/// Try to paste text from clipboard
pub(crate) fn paste_text() -> io::Result<String> {
    // Try wl-paste for text first
    if let Ok(output) = Command::new("wl-paste").arg("-n").output()
        && output.status.success()
        && !output.stdout.is_empty()
    {
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        return Ok(text);
    }

    // Try xclip for text
    if let Ok(output) = Command::new("xclip")
        .args(["-selection", "clipboard", "-o"])
        .output()
        && output.status.success()
        && !output.stdout.is_empty()
    {
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        return Ok(text);
    }

    Err(io::Error::other("No text in clipboard"))
}

/// Try to paste text from PRIMARY selection (for middle mouse button)
pub(crate) fn paste_primary() -> io::Result<String> {
    // Try wl-paste with primary selection
    if let Ok(output) = Command::new("wl-paste").args(["-p", "-n"]).output()
        && output.status.success()
        && !output.stdout.is_empty()
    {
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        return Ok(text);
    }

    // Try xclip with primary selection
    if let Ok(output) = Command::new("xclip")
        .args(["-selection", "primary", "-o"])
        .output()
        && output.status.success()
        && !output.stdout.is_empty()
    {
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        return Ok(text);
    }

    Err(io::Error::other("No text in primary selection"))
}

/// Try to paste an image from clipboard
pub(crate) fn paste_image() -> io::Result<(Vec<u8>, String)> {
    wl_paste_image()
}

fn wl_paste_image() -> io::Result<(Vec<u8>, String)> {
    let types = wl_paste_list_types()?;
    let Some(image_type) = types.iter().find(|t| t.starts_with("image/")).cloned() else {
        return Err(io::Error::other(format!(
            "wl-paste returned no image types. Available: {:?}",
            types
        )));
    };

    let output = Command::new("wl-paste")
        .args(["-t", &image_type])
        .output()
        .map_err(|e| io::Error::other(format!("wl-paste not available: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "wl-paste -t {image_type} failed (status {:?}): {}",
            output.status.code(),
            stderr.trim()
        )));
    }

    Ok((output.stdout, image_type))
}

fn wl_paste_list_types() -> io::Result<Vec<String>> {
    let output = Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .map_err(|e| io::Error::other(format!("wl-paste not available: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "wl-paste --list-types failed (status {:?}): {}",
            output.status.code(),
            stderr.trim()
        )));
    }

    let types = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Ok(types)
}
