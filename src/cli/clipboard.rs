// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Clipboard operations for the CLI

use std::io;
use std::process::Command;

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
