use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub content: String,
    pub timestamp: i64,
    pub is_multiline: bool,
}

#[derive(Debug, Clone)]
pub struct History {
    pub entries: VecDeque<HistoryEntry>,
    pub max_entries: usize,
}

impl Default for History {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            max_entries: 1000,
        }
    }
}

impl History {
    pub fn history_path() -> Result<PathBuf> {
        let config_dir = Config::config_dir()?;
        Ok(config_dir.join("history.json"))
    }

    pub fn load() -> Result<Self> {
        let history_path = Self::history_path()?;

        if !history_path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&history_path)
            .with_context(|| format!("Failed to read history file: {}", history_path.display()))?;

        let entries: Vec<HistoryEntry> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse history file: {}", history_path.display()))?;

        let history = Self {
            entries: entries.into(),
            ..Default::default()
        };

        Ok(history)
    }

    pub fn save(&self) -> Result<()> {
        let config_dir = Config::config_dir()?;
        let history_path = Self::history_path()?;

        // Create config directory if it doesn't exist
        if !config_dir.exists() {
            fs::create_dir_all(&config_dir).with_context(|| {
                format!(
                    "Failed to create config directory: {}",
                    config_dir.display()
                )
            })?;
        }

        // Convert VecDeque to Vec for serialization
        let entries: Vec<HistoryEntry> = self.entries.iter().cloned().collect();
        let content =
            serde_json::to_string_pretty(&entries).context("Failed to serialize history")?;

        fs::write(&history_path, content)
            .with_context(|| format!("Failed to write history file: {}", history_path.display()))?;

        Ok(())
    }

    pub fn add_entry(&mut self, content: String, is_multiline: bool) {
        let entry = HistoryEntry {
            content,
            timestamp: chrono::Utc::now().timestamp(),
            is_multiline,
        };

        // Remove oldest entries if we exceed max_entries
        while self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }

        self.entries.push_back(entry);
    }

    #[allow(dead_code)]
    pub fn get_entries(&self) -> &VecDeque<HistoryEntry> {
        &self.entries
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn load_into_rustyline<H: rustyline::Helper>(
        &self,
        editor: &mut rustyline::Editor<H, rustyline::history::DefaultHistory>,
    ) -> Result<()> {
        for entry in &self.entries {
            editor
                .add_history_entry(&entry.content)
                .map_err(|e| anyhow::anyhow!("Failed to add history entry to rustyline: {}", e))?;
        }
        Ok(())
    }
}
