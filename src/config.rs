// SPDX-License-Identifier: AGPL-3.0-only

use anyhow::{Context, Result};
use dirs::home_dir;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Provider {
    GitHubCopilot,
    OpenRouter,
    Anthropic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum SystemMode {
    #[default]
    Developer,
    SysAdmin,
}

impl SystemMode {
    pub fn toggle(&self) -> Self {
        match self {
            SystemMode::Developer => SystemMode::SysAdmin,
            SystemMode::SysAdmin => SystemMode::Developer,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SystemMode::Developer => "Developer",
            SystemMode::SysAdmin => "SysAdmin",
        }
    }

    pub fn system_prompt(&self) -> &'static str {
        match self {
            SystemMode::Developer => {
                "You are an expert software developer assistant with deep knowledge across multiple programming languages, frameworks, and architectural patterns. You excel at:
- Writing clean, efficient, and maintainable code
- Debugging complex issues and suggesting optimal solutions
- Explaining technical concepts clearly and concisely
- Following established coding conventions and best practices
- Identifying potential bugs, security issues, and performance bottlenecks
- Suggesting improvements and refactoring opportunities
- Understanding the broader context and architectural implications of code changes

Always strive to understand the user's specific needs, follow their coding style, and provide practical, actionable solutions. Be concise but thorough, and proactively point out important considerations."
            }
            SystemMode::SysAdmin => {
                "You are an expert Linux system administrator with comprehensive knowledge of system administration, security, and infrastructure management. You excel at:
- Configuring and optimizing Linux systems for performance and reliability
- Troubleshooting complex system issues and network problems
- Implementing security best practices and hardening systems
- Automating tasks with shell scripts and configuration management tools
- Managing services, processes, and system resources efficiently
- Understanding various Linux distributions and their specific tools
- Providing clear explanations of system behavior and root causes

Always prioritize security and stability, suggest automation where appropriate, and provide clear, step-by-step guidance. Be concise but thorough, and proactively identify potential risks or improvements."
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub providers: Providers,
    pub selected_model: Option<String>,
    #[serde(default)]
    pub system_mode: SystemMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Providers {
    pub github_copilot: Option<GitHubCopilotConfig>,
    pub open_router: Option<OpenRouterConfig>,
    pub anthropic: Option<AnthropicConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRouterConfig {
    pub api_key: String,
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
#[serde(tag = "type")]
pub enum AnthropicConfig {
    #[serde(rename = "oauth")]
    OAuth {
        refresh: String,
        access: String,
        expires: u64, // timestamp in milliseconds
    },
    #[serde(rename = "api")]
    ApiKey { key: String },
}

impl Default for Config {
    fn default() -> Self {
        Self {
            providers: Providers {
                github_copilot: None,
                open_router: None,
                anthropic: None,
            },
            selected_model: None,
            system_mode: SystemMode::default(),
        }
    }
}

impl Config {
    pub fn config_dir() -> Result<PathBuf> {
        // Check for local .henri directory first
        let current_dir = env::current_dir().context("Could not get current directory")?;
        let local_henri = current_dir.join(".henri");

        if local_henri.exists() && local_henri.is_dir() {
            return Ok(local_henri);
        }

        // Otherwise, use the home directory
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

    pub fn set_open_router(&mut self, config: OpenRouterConfig) {
        self.providers.open_router = Some(config);
    }

    pub fn set_anthropic(&mut self, config: AnthropicConfig) {
        self.providers.anthropic = Some(config);
    }

    pub fn set_selected_model(&mut self, model: String) {
        self.selected_model = Some(model);
    }

    pub fn get_selected_model(&self) -> Option<&String> {
        self.selected_model.as_ref()
    }

    pub fn set_system_mode(&mut self, mode: SystemMode) {
        self.system_mode = mode;
    }

    pub fn get_system_mode(&self) -> SystemMode {
        self.system_mode
    }
}
