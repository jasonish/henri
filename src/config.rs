use anyhow::{Context, Result};
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub providers: Providers,
    pub selected_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Providers {
    pub github_copilot: Option<GitHubCopilotConfig>,
    pub claude: Option<ClaudeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubCopilotConfig {
    pub access_token: String, // GitHub OAuth token
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub copilot_token: Option<String>, // Copilot API token
    pub copilot_expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeConfig {
    pub api_key: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            providers: Providers {
                github_copilot: None,
                claude: None,
            },
            selected_model: None,
        }
    }
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        let home = home_dir().context("Could not find home directory")?;
        Ok(home.join(".henri"))
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(Self::config_dir()?.join("config.json"))
    }

    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;

        if !config_path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

        // Try to parse as new format first
        if let Ok(config) = serde_json::from_str::<Config>(&content) {
            return Ok(config);
        }

        // If parsing fails, it might be an old format - start fresh
        eprintln!("Warning: Config file format has changed. Creating new config file.");
        let config = Self::default();
        config.save()?;

        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let config_dir = Self::config_dir()?;
        let config_path = Self::config_path()?;

        // Create config directory if it doesn't exist
        if !config_dir.exists() {
            fs::create_dir_all(&config_dir).with_context(|| {
                format!(
                    "Failed to create config directory: {}",
                    config_dir.display()
                )
            })?;
        }

        let content = serde_json::to_string_pretty(self).context("Failed to serialize config")?;

        fs::write(&config_path, content)
            .with_context(|| format!("Failed to write config file: {}", config_path.display()))?;

        Ok(())
    }

    pub fn set_github_copilot(&mut self, config: GitHubCopilotConfig) {
        self.providers.github_copilot = Some(config);
    }

    #[allow(dead_code)]
    pub fn get_github_copilot(&self) -> Option<&GitHubCopilotConfig> {
        self.providers.github_copilot.as_ref()
    }

    pub fn set_selected_model(&mut self, model: String) {
        self.selected_model = Some(model);
    }

    pub fn get_selected_model(&self) -> Option<&String> {
        self.selected_model.as_ref()
    }
}
