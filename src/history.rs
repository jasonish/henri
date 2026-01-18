// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use base64::Engine;
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const HISTORY_FILE: &str = "history.json";
const MAX_HISTORY: usize = 5000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryEntry {
    prompt: String,
    images: Vec<HistoryImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HistoryImage {
    pub marker: String,
    pub mime_type: String,
    pub data: String, // Base64 encoded image data
}

impl HistoryImage {
    /// Create a HistoryImage from raw image data (encodes to base64)
    pub(crate) fn from_raw_data(
        marker: String,
        mime_type: String,
        data: Vec<u8>,
    ) -> Result<Self, base64::EncodeSliceError> {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
        Ok(Self {
            marker,
            mime_type,
            data: encoded,
        })
    }

    /// Get the raw image data (decodes from base64)
    pub(crate) fn to_raw_data(&self) -> Result<Vec<u8>, base64::DecodeError> {
        base64::engine::general_purpose::STANDARD.decode(&self.data)
    }

    /// Convert to PastedImage for use in prompts
    pub(crate) fn to_pasted_image(&self) -> Result<crate::cli::PastedImage, base64::DecodeError> {
        let raw_data = self.to_raw_data()?;
        Ok(crate::cli::PastedImage {
            marker: self.marker.clone(),
            mime_type: self.mime_type.clone(),
            data: raw_data,
        })
    }
}

pub(crate) struct FileHistory {
    entries: Vec<String>,
    path: PathBuf,
    max_len: usize,
    ignore_dups: bool,
    ignore_space: bool,
}

impl FileHistory {
    pub(crate) fn new() -> Self {
        let path = Self::history_path();
        Self::new_with_path(path)
    }

    fn new_with_path(path: PathBuf) -> Self {
        let mut entries = Self::load_from_file(&path);

        // Keep only the most recent entries in memory
        if entries.len() > MAX_HISTORY {
            entries = entries.split_off(entries.len() - MAX_HISTORY);
        }

        Self {
            entries,
            path,
            max_len: MAX_HISTORY,
            ignore_dups: true,
            ignore_space: true,
        }
    }

    fn history_path() -> PathBuf {
        home_dir()
            .map(|home| home.join(".cache").join("henri"))
            .unwrap_or_else(|| PathBuf::from(".cache/henri"))
            .join(HISTORY_FILE)
    }

    fn load_from_file(path: &Path) -> Vec<String> {
        let Ok(file) = File::open(path) else {
            return Vec::new();
        };

        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let Ok(line) = line else {
                continue;
            };
            if let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line) {
                entries.push(entry.prompt);
            }
        }

        entries
    }

    fn append_to_file_with_images(&self, prompt: &str, images: Vec<HistoryImage>) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let entry = HistoryEntry {
            prompt: prompt.to_string(),
            images,
        };

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            && let Ok(json) = serde_json::to_string(&entry)
        {
            let _ = writeln!(file, "{}", json);
        }
    }

    pub(crate) fn add_with_images(&mut self, line: &str, images: Vec<HistoryImage>) -> bool {
        if self.ignore_space && line.starts_with(' ') {
            return false;
        }
        if line.is_empty() && images.is_empty() {
            return false;
        }
        if self.ignore_dups
            && let Some(last) = self.entries.last()
            && last == line
        {
            return false;
        }

        self.entries.push(line.to_string());
        self.append_to_file_with_images(line, images);

        // Just trim in-memory; file compaction happens on next load
        if self.entries.len() > self.max_len {
            self.entries.remove(0);
        }

        true
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn get(&self, index: usize) -> Option<&String> {
        self.entries.get(index)
    }

    pub(crate) fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Get images for a specific history entry by index
    pub(crate) fn get_images_for_entry(&self, index: usize) -> Option<Vec<HistoryImage>> {
        if index >= self.entries.len() {
            return None;
        }

        let prompt = &self.entries[index];
        self.load_images_for_prompt(prompt)
    }

    /// Load images for a specific prompt from the history file
    fn load_images_for_prompt(&self, target_prompt: &str) -> Option<Vec<HistoryImage>> {
        let Ok(file) = File::open(&self.path) else {
            return None;
        };

        let reader = BufReader::new(file);
        for line in reader.lines() {
            let Ok(line) = line else {
                continue;
            };
            if let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line)
                && entry.prompt == target_prompt
            {
                return Some(entry.images);
            }
        }
        None
    }
}

impl Default for FileHistory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Barrier};
    use std::thread;

    use tempfile::tempdir;

    #[test]
    fn test_concurrent_appends_no_panic() {
        // Concurrent appends without file locking may lose entries due to write
        // races (interleaved bytes can corrupt JSON lines, which are skipped on load).
        // This is acceptable for command history - we just verify no panics occur.
        let dir = tempdir().unwrap();
        let history_path = dir.path().join("history.json");

        let barrier = Arc::new(Barrier::new(2));
        let b1 = barrier.clone();
        let path1 = history_path.clone();
        let path2 = history_path.clone();

        let t1 = thread::spawn(move || {
            let mut history = FileHistory::new_with_path(path1);
            b1.wait();
            for i in 0..10 {
                history.add_with_images(&format!("A{}", i), vec![]);
            }
        });

        let t2 = thread::spawn(move || {
            let mut history = FileHistory::new_with_path(path2);
            barrier.wait();
            for i in 0..10 {
                history.add_with_images(&format!("B{}", i), vec![]);
            }
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // Loading the history file should not panic, even if entries are corrupted
        let _history = FileHistory::new_with_path(dir.path().join("history.json"));
    }
}
