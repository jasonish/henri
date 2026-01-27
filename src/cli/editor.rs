// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use tempfile::Builder;

pub(super) fn edit_text_in_external_editor(initial: &str) -> io::Result<Option<String>> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|s| !s.trim().is_empty())
        });

    let Some(editor) = editor else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "$VISUAL/$EDITOR is not set",
        ));
    };

    let mut file = Builder::new()
        .prefix("henri-prompt-")
        .suffix(".md")
        .tempfile()?;
    std::io::Write::write_all(&mut file, initial.as_bytes())?;
    file.flush()?;

    let path: PathBuf = file.path().to_path_buf();

    let status = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "{} {}",
            shell_escape(&editor),
            shell_escape(path.to_string_lossy().as_ref())
        ))
        .status()?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "editor exited with status: {}",
            status
        )));
    }

    let edited = std::fs::read_to_string(&path)?;

    if normalize_newlines(initial).trim_end() == normalize_newlines(&edited).trim_end() {
        return Ok(None);
    }

    Ok(Some(normalize_newlines(&edited)))
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

fn shell_escape(s: &str) -> String {
    // Minimal POSIX sh escaping: wrap in single quotes and escape embedded single quotes.
    // Example: abc'def -> 'abc'"'"'def'
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\"'\"'");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}
