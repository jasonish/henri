// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use base64::Engine;
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

const HISTORY_FILE: &str = "history.json";
const MAX_HISTORY: usize = 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryEntry {
    prompt: String,
    images: Vec<HistoryImage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryImage {
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

pub struct FileHistory {
    entries: Vec<String>,
    path: PathBuf,
    max_len: usize,
    ignore_dups: bool,
    ignore_space: bool,
}

impl FileHistory {
    pub(crate) fn new() -> Self {
        let path = Self::history_path();
        let entries = Self::load_from_file(&path);
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

    fn load_from_file(path: &PathBuf) -> Vec<String> {
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

    fn rewrite_file(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        if let Ok(mut file) = File::create(&self.path) {
            for prompt in &self.entries {
                let entry = HistoryEntry {
                    prompt: prompt.clone(),
                    images: Vec::new(),
                };
                if let Ok(json) = serde_json::to_string(&entry) {
                    let _ = writeln!(file, "{}", json);
                }
            }
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

        if self.entries.len() > self.max_len {
            self.entries.remove(0);
            self.rewrite_file();
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
