// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use dirs::home_dir;
use serde::{Deserialize, Serialize};

use crate::error::{self, Result};

pub const DEFAULT_MODEL: &str = "zen/grok-code";
const CONFIG_FILE: &str = "config.toml";
const CONFIG_DIR: &str = ".config/henri";

/// Default model selection strategy on startup.
///
/// Can be either ":last-used" to use the most recently selected model,
/// or a specific model string like "claude/claude-sonnet-4-5".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DefaultModel {
    /// Use the most recently selected model
    #[default]
    #[serde(rename = ":last-used")]
    LastUsed,
    /// Use a specific model (stored as untagged to allow plain strings in TOML)
    #[serde(untagged)]
    Specific(String),
}

/// Default UI mode for the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UiDefault {
    /// Terminal UI (ratatui-based)
    #[default]
    Tui,
    /// Command-line REPL interface
    Cli,
}

/// UI configuration section.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiConfig {
    /// Default interface to use (tui or cli). Defaults to tui.
    #[serde(default)]
    pub default: UiDefault,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigFile {
    #[serde(default)]
    pub providers: Providers,
    pub model: Option<String>,
    #[serde(default, rename = "default-model")]
    pub default_model: DefaultModel,
    #[serde(default)]
    pub state: Option<State>,
    #[serde(default)]
    pub mcp: Option<McpConfig>,
    #[serde(default)]
    pub lsp: Option<LspConfig>,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default = "default_show_network_stats")]
    pub show_network_stats: bool,
    #[serde(default = "default_show_diffs")]
    pub show_diffs: bool,
    /// Enable LSP integration for diagnostics (default: true)
    #[serde(default = "default_lsp_enabled", rename = "lsp-enabled")]
    pub lsp_enabled: bool,
}

fn default_show_network_stats() -> bool {
    true
}

fn default_show_diffs() -> bool {
    true
}

fn default_lsp_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LspConfig {
    #[serde(default)]
    pub servers: Vec<LspServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct LspServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub file_extensions: Vec<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Provider type identifier - maps to the `type` field in config
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderType {
    Claude,
    GithubCopilot,
    Openai,
    Zen,
    OpenaiCompat,
    OpenRouter,
}

impl ProviderType {
    /// Default local identifier for this provider type
    pub(crate) fn default_id(&self) -> &'static str {
        match self {
            ProviderType::Claude => "claude",
            ProviderType::GithubCopilot => "copilot",
            ProviderType::Openai => "openai",
            ProviderType::Zen => "zen",
            ProviderType::OpenaiCompat => "openai-compat",
            ProviderType::OpenRouter => "openrouter",
        }
    }

    /// Display name for UI
    pub(crate) fn display_name(&self) -> &'static str {
        match self {
            ProviderType::Claude => "Anthropic Claude",
            ProviderType::GithubCopilot => "GitHub Copilot",
            ProviderType::Openai => "OpenAI",
            ProviderType::Zen => "OpenCode Zen",
            ProviderType::OpenaiCompat => "OpenAI Compatible",
            ProviderType::OpenRouter => "OpenRouter",
        }
    }
}

/// Unified provider configuration with tagged union
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ProviderConfig {
    Claude(ClaudeProviderConfig),
    GithubCopilot(CopilotProviderConfig),
    Openai(OpenAiProviderConfig),
    Zen(ZenProviderConfig),
    OpenaiCompat(OpenAiCompatProviderConfig),
    Openrouter(OpenRouterConfig),
}

impl ProviderConfig {
    pub(crate) fn is_enabled(&self) -> bool {
        match self {
            ProviderConfig::Claude(c) => c.enabled,
            ProviderConfig::GithubCopilot(c) => c.enabled,
            ProviderConfig::Openai(c) => c.enabled,
            ProviderConfig::Zen(c) => c.enabled,
            ProviderConfig::OpenaiCompat(c) => c.enabled,
            ProviderConfig::Openrouter(c) => c.enabled,
        }
    }

    pub(crate) fn provider_type(&self) -> ProviderType {
        match self {
            ProviderConfig::Claude(_) => ProviderType::Claude,
            ProviderConfig::GithubCopilot(_) => ProviderType::GithubCopilot,
            ProviderConfig::Openai(_) => ProviderType::Openai,
            ProviderConfig::Zen(_) => ProviderType::Zen,
            ProviderConfig::OpenaiCompat(_) => ProviderType::OpenaiCompat,
            ProviderConfig::Openrouter(_) => ProviderType::OpenRouter,
        }
    }

    /// Get the OpenAI-compat config if this is an OpenAI-compat provider
    pub(crate) fn as_openai_compat(&self) -> Option<&OpenAiCompatProviderConfig> {
        match self {
            ProviderConfig::OpenaiCompat(c) => Some(c),
            _ => None,
        }
    }

    /// Get the OpenRouter config if this is an OpenRouter provider
    pub(crate) fn as_openrouter(&self) -> Option<&OpenRouterConfig> {
        match self {
            ProviderConfig::Openrouter(c) => Some(c),
            _ => None,
        }
    }

    /// Get the Claude config if this is a Claude provider
    pub(crate) fn as_claude(&self) -> Option<&ClaudeProviderConfig> {
        match self {
            ProviderConfig::Claude(c) => Some(c),
            _ => None,
        }
    }

    /// Get the Copilot config if this is a GitHub Copilot provider
    pub(crate) fn as_copilot(&self) -> Option<&CopilotProviderConfig> {
        match self {
            ProviderConfig::GithubCopilot(c) => Some(c),
            _ => None,
        }
    }

    /// Get the OpenAI config if this is an OpenAI provider
    pub(crate) fn as_openai(&self) -> Option<&OpenAiProviderConfig> {
        match self {
            ProviderConfig::Openai(c) => Some(c),
            _ => None,
        }
    }

    /// Get the Zen config if this is a Zen provider
    pub(crate) fn as_zen(&self) -> Option<&ZenProviderConfig> {
        match self {
            ProviderConfig::Zen(c) => Some(c),
            _ => None,
        }
    }
}

/// New Providers struct using HashMap with flatten
#[derive(Debug, Clone, Serialize, Default)]
pub struct Providers {
    #[serde(flatten)]
    pub entries: HashMap<String, ProviderConfig>,
}

impl<'de> Deserialize<'de> for Providers {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize as a raw map first
        let raw: HashMap<String, toml::Value> = HashMap::deserialize(deserializer)?;
        let mut entries = HashMap::new();

        for (key, value) in raw {
            // Try to deserialize each value into a ProviderConfig
            match ProviderConfig::deserialize(value) {
                Ok(config) => {
                    entries.insert(key, config);
                }
                Err(_) => {
                    // Silently skip unknown provider types - don't fail the entire config load
                }
            }
        }

        Ok(Providers { entries })
    }
}

// Individual provider configs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(flatten)]
    pub auth: ClaudeAuth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeAuth {
    #[serde(rename = "refresh-token")]
    pub refresh_token: String,
    #[serde(rename = "access-token")]
    pub access_token: String,
    #[serde(rename = "expires-at")]
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CopilotProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<i64>,
    pub copilot_token: Option<String>,
    pub copilot_expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OpenAiProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_openai_client_id")]
    pub client_id: String,
    #[serde(default = "default_openai_audience")]
    pub audience: String,
    pub refresh_token: String,
    pub access_token: String,
    pub expires_at: u64,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ZenProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OpenAiCompatProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, rename = "model", skip_serializing_if = "Vec::is_empty")]
    pub model_configs: Vec<ModelConfig>,
}

impl OpenAiCompatProviderConfig {
    /// Get all available model names (from both simple list and detailed configs)
    pub(crate) fn all_models(&self) -> Vec<String> {
        let mut models = self.models.clone();
        models.extend(self.model_configs.iter().map(|m| m.name.clone()));
        models
    }

    /// Get configuration for a specific model
    pub(crate) fn get_model_config(&self, model_name: &str) -> Option<&ModelConfig> {
        self.model_configs.iter().find(|m| m.name == model_name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct OpenRouterConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, rename = "model", skip_serializing_if = "Vec::is_empty")]
    pub model_configs: Vec<ModelConfig>,
}

impl OpenRouterConfig {
    /// Get all available model names (from both simple list and detailed configs)
    pub(crate) fn all_models(&self) -> Vec<String> {
        let mut models = self.models.clone();
        models.extend(self.model_configs.iter().map(|m| m.name.clone()));
        models
    }

    /// Get configuration for a specific model
    pub(crate) fn get_model_config(&self, model_name: &str) -> Option<&ModelConfig> {
        self.model_configs.iter().find(|m| m.name == model_name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ModelConfig {
    /// Model name/identifier
    pub name: String,
    /// Reasoning effort level (low, medium, high)
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Extended thinking configuration (provider-specific JSON)
    /// Example: {"type": "enabled"} or {"type": "enabled", "budget_tokens": 10000}
    #[serde(default)]
    pub thinking: Option<serde_json::Value>,
    /// Temperature for sampling (0.0 - 2.0)
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Maximum tokens to generate
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Custom system prompt for this model
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Stop sequences
    #[serde(default)]
    pub stop_sequences: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub struct State {
    pub last_model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    pub model: String,
}

fn default_openai_client_id() -> String {
    std::env::var("OPENAI_OAUTH_CLIENT_ID")
        .unwrap_or_else(|_| "app_EMoamEEZ73f0CkXaXp7hrann".to_string())
}

fn default_openai_audience() -> String {
    "https://api.openai.com/v1".to_string()
}

impl ConfigFile {
    fn config_dir() -> PathBuf {
        home_dir()
            .map(|home| home.join(CONFIG_DIR))
            .unwrap_or_else(|| PathBuf::from(CONFIG_DIR))
    }

    fn config_file_path() -> PathBuf {
        Self::config_dir().join(CONFIG_FILE)
    }

    pub(crate) fn load() -> Result<Self> {
        let path = Self::config_file_path();

        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path).map_err(|e| error::Error::Config(e.to_string()))?;

        toml::from_str(&content).map_err(|e| error::Error::Config(e.to_string()))
    }

    pub(crate) fn save(&self) -> Result<()> {
        let path = Self::config_file_path();

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| error::Error::Config(e.to_string()))?;
        }

        let content =
            toml::to_string_pretty(self).map_err(|e| error::Error::Config(e.to_string()))?;
        fs::write(path, content).map_err(|e| error::Error::Config(e.to_string()))
    }

    /// Get a provider by local identifier
    pub(crate) fn get_provider(&self, local_id: &str) -> Option<&ProviderConfig> {
        self.providers.entries.get(local_id)
    }

    /// Set/update a provider by local identifier
    pub(crate) fn set_provider(&mut self, local_id: String, config: ProviderConfig) {
        self.providers.entries.insert(local_id, config);
    }

    /// Remove a provider by local identifier
    pub(crate) fn remove_provider(&mut self, local_id: &str) -> Option<ProviderConfig> {
        self.providers.entries.remove(local_id)
    }

    /// Get all providers of a specific type
    pub(crate) fn providers_of_type(
        &self,
        provider_type: ProviderType,
    ) -> Vec<(&String, &ProviderConfig)> {
        self.providers
            .entries
            .iter()
            .filter(|(_, config)| config.provider_type() == provider_type)
            .collect()
    }
}

impl Config {
    pub(crate) fn load(model: Option<String>) -> Result<Self> {
        let config = ConfigFile::load()?;

        // Model resolution priority:
        // 1. CLI --model flag (highest priority)
        // 2. default-model setting (LastUsed -> state.last_model, Specific -> explicit model)
        // 3. Legacy config.model field (for backward compatibility)
        // 4. DEFAULT_MODEL constant (fallback)
        let model = model
            .or_else(|| match &config.default_model {
                DefaultModel::Specific(m) => Some(m.clone()),
                DefaultModel::LastUsed => config.state.as_ref().and_then(|s| s.last_model.clone()),
            })
            .or_else(|| config.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());

        // Try to get API key from a zen provider if one exists
        let api_key = config
            .providers
            .entries
            .values()
            .find_map(|p| p.as_zen())
            .map(|c| c.api_key.clone())
            .unwrap_or_default();

        Ok(Self { api_key, model })
    }

    pub(crate) fn save_state_model(model: &str) -> Result<()> {
        let mut config = ConfigFile::load()?;
        if config.state.is_none() {
            config.state = Some(State::default());
        }
        if let Some(state) = config.state.as_mut() {
            state.last_model = Some(model.to_string());
        }
        config.save()
    }
}

/// Initialize MCP and LSP servers from configuration.
///
/// The `lsp_override` parameter allows command-line options to override the config:
/// - `Some(true)` forces LSP enabled (--lsp)
/// - `Some(false)` forces LSP disabled (--no-lsp)
/// - `None` uses the config file's `lsp_enabled` setting
pub async fn initialize_servers(working_dir: &Path, lsp_override: Option<bool>) {
    let config_file = ConfigFile::load().unwrap_or_default();

    // Initialize MCP servers
    if let Some(mcp_config) = &config_file.mcp {
        let servers: Vec<crate::mcp::McpServerConfig> = mcp_config
            .servers
            .iter()
            .filter(|s| s.enabled)
            .map(|s| crate::mcp::McpServerConfig {
                name: s.name.clone(),
                command: s.command.clone(),
                args: s.args.clone(),
                env: s.env.clone(),
            })
            .collect();
        let _ = crate::mcp::initialize(servers).await;
    }

    // Determine if LSP is enabled: command-line override takes precedence
    let lsp_enabled = lsp_override.unwrap_or(config_file.lsp_enabled);

    // Initialize LSP servers if enabled
    if lsp_enabled && let Some(lsp_config) = &config_file.lsp {
        let servers: Vec<crate::lsp::LspServerConfig> = lsp_config
            .servers
            .iter()
            .filter(|s| s.enabled)
            .map(|s| crate::lsp::LspServerConfig {
                name: s.name.clone(),
                command: s.command.clone(),
                args: s.args.clone(),
                file_extensions: s.file_extensions.clone(),
                root_path: working_dir.to_path_buf(),
            })
            .collect();
        let _ = crate::lsp::initialize(servers).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_serialization_uses_hyphens() {
        // Test that ZenProviderConfig serializes with hyphens
        let zen_config = ZenProviderConfig {
            enabled: true,
            api_key: "test-key".to_string(),
        };
        let toml = toml::to_string(&zen_config).unwrap();
        assert!(
            toml.contains("api-key"),
            "Expected 'api-key' in TOML output, got: {}",
            toml
        );
        assert!(
            !toml.contains("api_key"),
            "Should not contain 'api_key' with underscore"
        );

        // Test CopilotProviderConfig
        let copilot_config = CopilotProviderConfig {
            enabled: true,
            access_token: "token1".to_string(),
            refresh_token: Some("token2".to_string()),
            expires_at: Some(12345),
            copilot_token: Some("token3".to_string()),
            copilot_expires_at: Some(67890),
        };
        let toml = toml::to_string(&copilot_config).unwrap();
        assert!(toml.contains("access-token"), "Expected 'access-token'");
        assert!(toml.contains("refresh-token"), "Expected 'refresh-token'");
        assert!(toml.contains("expires-at"), "Expected 'expires-at'");
        assert!(toml.contains("copilot-token"), "Expected 'copilot-token'");
        assert!(
            toml.contains("copilot-expires-at"),
            "Expected 'copilot-expires-at'"
        );

        // Test State
        let state = State {
            last_model: Some("test-model".to_string()),
        };
        let toml = toml::to_string(&state).unwrap();
        assert!(toml.contains("last-model"), "Expected 'last-model'");
        assert!(
            !toml.contains("last_model"),
            "Should not contain 'last_model' with underscore"
        );

        // Test ModelConfig
        let model_config = ModelConfig {
            name: "test".to_string(),
            reasoning_effort: Some("high".to_string()),
            thinking: None,
            temperature: Some(0.7),
            max_tokens: Some(1000),
            system_prompt: Some("test prompt".to_string()),
            stop_sequences: Some(vec!["STOP".to_string()]),
        };
        let toml = toml::to_string(&model_config).unwrap();
        assert!(
            toml.contains("reasoning-effort"),
            "Expected 'reasoning-effort'"
        );
        assert!(toml.contains("max-tokens"), "Expected 'max-tokens'");
        assert!(toml.contains("system-prompt"), "Expected 'system-prompt'");
        assert!(toml.contains("stop-sequences"), "Expected 'stop-sequences'");
    }

    #[test]
    fn test_claude_auth_serialization() {
        let auth = ClaudeAuth {
            refresh_token: "refresh".to_string(),
            access_token: "access".to_string(),
            expires_at: 12345,
        };
        let toml = toml::to_string(&auth).unwrap();
        assert!(toml.contains("refresh-token"), "Expected 'refresh-token'");
        assert!(toml.contains("access-token"), "Expected 'access-token'");
        assert!(toml.contains("expires-at"), "Expected 'expires-at'");
    }

    #[test]
    fn test_unknown_provider_skipped() {
        // Test that configs with unknown provider types are skipped, not failed
        let toml_str = r#"
[providers.zen]
type = "zen"
enabled = true
api-key = "test-key"

[providers.zai]
type = "zai"
enabled = true
api-key = "old-provider"

[providers.claude]
type = "claude"
enabled = true
refresh-token = "refresh"
access-token = "access"
expires-at = 12345
"#;

        let config: ConfigFile = toml::from_str(toml_str).unwrap();

        // Should have loaded zen and claude, but skipped zai
        assert_eq!(config.providers.entries.len(), 2);
        assert!(config.providers.entries.contains_key("zen"));
        assert!(config.providers.entries.contains_key("claude"));
        assert!(!config.providers.entries.contains_key("zai"));
    }

    #[test]
    fn test_default_model_last_used() {
        // Test parsing ":last-used" string
        let toml_str = r#"default-model = ":last-used""#;
        let config: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_model, DefaultModel::LastUsed);
        assert!(matches!(config.default_model, DefaultModel::LastUsed));
    }

    #[test]
    fn test_default_model_specific() {
        // Test parsing a specific model string
        let toml_str = r#"default-model = "claude/claude-sonnet-4-5""#;
        let config: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.default_model,
            DefaultModel::Specific("claude/claude-sonnet-4-5".to_string())
        );
        assert!(matches!(config.default_model, DefaultModel::Specific(_)));
        // Verify the specific model string matches
        if let DefaultModel::Specific(model) = &config.default_model {
            assert_eq!(model, "claude/claude-sonnet-4-5");
        }
    }

    #[test]
    fn test_default_model_missing_defaults_to_last_used() {
        // Test that missing default-model field defaults to LastUsed
        let toml_str = r#"show-network-stats = true"#;
        let config: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_model, DefaultModel::LastUsed);
    }

    #[test]
    fn test_default_model_serialization() {
        // Test that LastUsed serializes correctly
        let config = ConfigFile {
            default_model: DefaultModel::LastUsed,
            ..Default::default()
        };
        let toml = toml::to_string(&config).unwrap();
        assert!(
            toml.contains("default-model = \":last-used\""),
            "Expected 'default-model = \":last-used\"', got: {}",
            toml
        );

        // Test that Specific serializes correctly
        let config = ConfigFile {
            default_model: DefaultModel::Specific("zen/grok-code".to_string()),
            ..Default::default()
        };
        let toml = toml::to_string(&config).unwrap();
        assert!(
            toml.contains("default-model = \"zen/grok-code\""),
            "Expected 'default-model = \"zen/grok-code\"', got: {}",
            toml
        );
    }
}
