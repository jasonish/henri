// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use dirs::home_dir;
use serde::{Deserialize, Serialize};

use crate::error::{self, Result};

const CONFIG_FILE: &str = "config.toml";
const CONFIG_DIR: &str = ".config/henri";

static CONFIG_DIR_OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn config_dir_override() -> Option<PathBuf> {
    CONFIG_DIR_OVERRIDE
        .get_or_init(|| RwLock::new(None))
        .read()
        .ok()
        .and_then(|guard| guard.clone())
}

pub(crate) fn set_config_dir_override(dir: Option<PathBuf>) {
    let lock = CONFIG_DIR_OVERRIDE.get_or_init(|| RwLock::new(None));
    *lock.write().expect("config dir override lock poisoned") = dir;
}

fn resolve_config_dir(override_dir: Option<&Path>) -> PathBuf {
    if let Some(dir) = override_dir {
        return dir.to_path_buf();
    }

    home_dir()
        .map(|home| home.join(CONFIG_DIR))
        .unwrap_or_else(|| PathBuf::from(CONFIG_DIR))
}

pub(crate) fn config_dir() -> PathBuf {
    let override_dir = config_dir_override();
    resolve_config_dir(override_dir.as_deref())
}

pub(crate) fn persist_last_used_model(model: &str) {
    if model.trim().is_empty() {
        return;
    }

    let Ok(mut config) = ConfigFile::load() else {
        return;
    };

    let mut state = config.state.unwrap_or_default();
    state.last_model = Some(model.to_string());
    config.state = Some(state);

    let _ = config.save();
}

/// Default model selection strategy on startup.
///
/// Can be either ":last-used" to use the most recently selected model,
/// or a specific model string like "claude/claude-sonnet-4-5".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DefaultModel {
    /// Use the most recently selected model
    #[default]
    #[serde(rename = ":last-used")]
    LastUsed,
    /// Use a specific model (stored as untagged to allow plain strings in TOML)
    #[serde(untagged)]
    Specific(String),
}

/// Auto-compaction configuration section.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct AutoCompactConfig {
    /// Whether auto-compaction is enabled. Defaults to true.
    #[serde(default = "default_auto_compact_enabled")]
    pub enabled: bool,
    /// Context usage threshold (0.0-1.0) at which to trigger compaction.
    /// Defaults to 0.75 (75%).
    #[serde(default = "default_auto_compact_threshold")]
    pub threshold: f64,
    /// Number of recent turns to preserve when auto-compacting.
    /// Defaults to 2.
    #[serde(default = "default_auto_compact_preserve_turns")]
    pub preserve_turns: usize,
}

impl Default for AutoCompactConfig {
    fn default() -> Self {
        Self {
            enabled: default_auto_compact_enabled(),
            threshold: default_auto_compact_threshold(),
            preserve_turns: default_auto_compact_preserve_turns(),
        }
    }
}

fn default_auto_compact_enabled() -> bool {
    true
}

fn default_auto_compact_threshold() -> f64 {
    0.75
}

fn default_auto_compact_preserve_turns() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConfigFile {
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
    #[serde(default = "default_show_network_stats")]
    pub show_network_stats: bool,
    #[serde(default = "default_show_diffs")]
    pub show_diffs: bool,
    #[serde(
        default = "default_show_image_previews",
        rename = "show-image-previews"
    )]
    pub show_image_previews: bool,
    /// Enable LSP integration for diagnostics (default: true)
    #[serde(default = "default_lsp_enabled", rename = "lsp-enabled")]
    pub lsp_enabled: bool,
    /// List of favorite model identifiers (e.g., "claude/claude-sonnet-4-5")
    #[serde(
        default,
        rename = "favorite-models",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub favorite_models: Vec<String>,
    /// Auto-compaction settings
    #[serde(default, rename = "auto-compact")]
    pub auto_compact: AutoCompactConfig,
    /// Enable the todo_read/todo_write tools (default: true)
    #[serde(default = "default_todo_enabled", rename = "todo-enabled")]
    pub todo_enabled: bool,
    /// List of disabled tool names
    #[serde(
        default,
        rename = "disabled-tools",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub disabled_tools: Vec<String>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            providers: Providers::default(),
            model: None,
            default_model: DefaultModel::LastUsed,
            state: None,
            mcp: None,
            lsp: None,
            show_network_stats: default_show_network_stats(),
            show_diffs: default_show_diffs(),
            show_image_previews: default_show_image_previews(),
            lsp_enabled: default_lsp_enabled(),
            favorite_models: Vec::new(),
            auto_compact: AutoCompactConfig::default(),
            todo_enabled: default_todo_enabled(),
            disabled_tools: Vec::new(),
        }
    }
}

fn default_show_network_stats() -> bool {
    true
}

fn default_show_diffs() -> bool {
    true
}

fn default_show_image_previews() -> bool {
    true
}

fn default_todo_enabled() -> bool {
    true
}

fn default_lsp_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct McpConfig {
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct LspConfig {
    #[serde(default)]
    pub servers: Vec<LspServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct LspServerConfig {
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
pub(crate) struct McpServerConfig {
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
pub(crate) enum ProviderType {
    Antigravity,
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
            ProviderType::Antigravity => "antigravity",
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
            ProviderType::Antigravity => "Antigravity",
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
pub(crate) enum ProviderConfig {
    Antigravity(AntigravityProviderConfig),
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
            ProviderConfig::Antigravity(c) => c.enabled,
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
            ProviderConfig::Antigravity(_) => ProviderType::Antigravity,
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

    /// Get the Antigravity config if this is an Antigravity provider
    pub(crate) fn as_antigravity(&self) -> Option<&AntigravityProviderConfig> {
        match self {
            ProviderConfig::Antigravity(c) => Some(c),
            _ => None,
        }
    }
}

/// New Providers struct using HashMap with flatten
#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct Providers {
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
#[serde(rename_all = "kebab-case")]
pub(crate) struct AntigravityProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ClaudeProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(flatten)]
    pub auth: ClaudeAuth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ClaudeAuth {
    #[serde(rename = "refresh-token")]
    pub refresh_token: String,
    #[serde(rename = "access-token")]
    pub access_token: String,
    #[serde(rename = "expires-at")]
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct CopilotProviderConfig {
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
pub(crate) struct OpenAiProviderConfig {
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
pub(crate) struct ZenProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub api_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct OpenAiCompatProviderConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    pub base_url: String,
    #[serde(default, rename = "model", skip_serializing_if = "Vec::is_empty")]
    pub model_configs: Vec<ModelConfig>,
}

impl OpenAiCompatProviderConfig {
    /// Get all available model names for UI display
    pub(crate) fn all_models(&self) -> Vec<String> {
        self.model_configs
            .iter()
            .map(|m| m.display_name().to_string())
            .collect()
    }

    /// Get configuration for a specific model by display name
    pub(crate) fn get_model_config(&self, display_name: &str) -> Option<&ModelConfig> {
        self.model_configs
            .iter()
            .find(|m| m.display_name() == display_name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct OpenRouterConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub api_key: String,
    #[serde(default, rename = "model", skip_serializing_if = "Vec::is_empty")]
    pub model_configs: Vec<ModelConfig>,
}

impl OpenRouterConfig {
    /// Get all available model names for UI display
    pub(crate) fn all_models(&self) -> Vec<String> {
        self.model_configs
            .iter()
            .map(|m| m.display_name().to_string())
            .collect()
    }

    /// Get configuration for a specific model by display name
    pub(crate) fn get_model_config(&self, display_name: &str) -> Option<&ModelConfig> {
        self.model_configs
            .iter()
            .find(|m| m.display_name() == display_name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct ModelConfig {
    /// Model identifier sent to the API
    pub id: String,
    /// Optional display name for the UI (defaults to id if not set)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Reasoning effort level (low, medium, high)
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Temperature for sampling (0.0 - 2.0)
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Top-p (nucleus) sampling parameter (0.0 - 1.0)
    #[serde(default)]
    pub top_p: Option<f64>,
    /// Maximum tokens to generate
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

impl ModelConfig {
    /// Get the display name for UI purposes (falls back to id if name is not set)
    pub(crate) fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub(crate) struct State {
    pub last_model: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Config {
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
        crate::config::config_dir()
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

        match toml::from_str(&content) {
            Ok(config) => Ok(config),
            Err(_) => {
                // Try to load with toml::Value first to get what we can
                Self::load_with_fallback(&content)
            }
        }
    }

    /// Attempt to load config with fallback for invalid fields.
    /// This parses the TOML as a raw Value first, then selectively
    /// deserializes fields that work, using defaults for the rest.
    fn load_with_fallback(content: &str) -> Result<Self> {
        let raw: toml::Value =
            toml::from_str(content).map_err(|e| error::Error::Config(e.to_string()))?;

        let mut config = Self::default();

        // Try to extract each field, falling back to default on failure
        if let Some(table) = raw.as_table() {
            // providers
            if let Some(providers_val) = table.get("providers")
                && let Ok(providers) = providers_val.clone().try_into()
            {
                config.providers = providers;
            }

            // model
            if let Some(model_val) = table.get("model")
                && let Some(model) = model_val.as_str()
            {
                config.model = Some(model.to_string());
            }

            // default-model
            if let Some(dm_val) = table.get("default-model")
                && let Ok(dm) = dm_val.clone().try_into()
            {
                config.default_model = dm;
            }

            // state
            if let Some(state_val) = table.get("state")
                && let Ok(state) = state_val.clone().try_into()
            {
                config.state = Some(state);
            }

            // mcp
            if let Some(mcp_val) = table.get("mcp")
                && let Ok(mcp) = mcp_val.clone().try_into()
            {
                config.mcp = Some(mcp);
            }

            // lsp
            if let Some(lsp_val) = table.get("lsp")
                && let Ok(lsp) = lsp_val.clone().try_into()
            {
                config.lsp = Some(lsp);
            }

            // show-network-stats
            if let Some(val) = table.get("show-network-stats")
                && let Some(b) = val.as_bool()
            {
                config.show_network_stats = b;
            }

            // show-diffs
            if let Some(val) = table.get("show-diffs")
                && let Some(b) = val.as_bool()
            {
                config.show_diffs = b;
            }

            // show-image-previews
            if let Some(val) = table.get("show-image-previews")
                && let Some(b) = val.as_bool()
            {
                config.show_image_previews = b;
            }

            // lsp-enabled
            if let Some(val) = table.get("lsp-enabled")
                && let Some(b) = val.as_bool()
            {
                config.lsp_enabled = b;
            }

            // favorite-models
            if let Some(val) = table.get("favorite-models")
                && let Ok(fav) = val.clone().try_into()
            {
                config.favorite_models = fav;
            }

            // auto-compact
            if let Some(val) = table.get("auto-compact")
                && let Ok(ac) = val.clone().try_into()
            {
                config.auto_compact = ac;
            }

            // todo-enabled
            if let Some(val) = table.get("todo-enabled")
                && let Some(b) = val.as_bool()
            {
                config.todo_enabled = b;
            }

            // disabled-tools
            if let Some(val) = table.get("disabled-tools")
                && let Ok(dt) = val.clone().try_into()
            {
                config.disabled_tools = dt;
            }
        }

        Ok(config)
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

    /// Check if a model is marked as a favorite
    pub(crate) fn is_favorite(&self, model_id: &str) -> bool {
        self.favorite_models.iter().any(|m| m == model_id)
    }

    /// Add a model to favorites
    pub(crate) fn add_favorite(&mut self, model_id: String) {
        if !self.is_favorite(&model_id) {
            self.favorite_models.push(model_id);
        }
    }

    /// Remove a model from favorites
    pub(crate) fn remove_favorite(&mut self, model_id: &str) {
        self.favorite_models.retain(|m| m != model_id);
    }

    /// Toggle a model's favorite status
    pub(crate) fn toggle_favorite(&mut self, model_id: &str) -> bool {
        if self.is_favorite(model_id) {
            self.remove_favorite(model_id);
            false
        } else {
            self.add_favorite(model_id.to_string());
            true
        }
    }

    /// Check if a tool is disabled
    pub(crate) fn is_tool_disabled(&self, tool_name: &str) -> bool {
        self.disabled_tools.iter().any(|t| t == tool_name)
    }

    /// Toggle a tool's disabled status. Returns true if tool is now enabled.
    pub(crate) fn toggle_tool_disabled(&mut self, tool_name: &str) -> bool {
        if self.is_tool_disabled(tool_name) {
            self.disabled_tools.retain(|t| t != tool_name);
            true // now enabled
        } else {
            self.disabled_tools.push(tool_name.to_string());
            false // now disabled
        }
    }

    /// Set todo_enabled and ensure underlying tools are enabled when enabling.
    pub(crate) fn set_todo_enabled(&mut self, enabled: bool) {
        self.todo_enabled = enabled;
        if enabled {
            self.disabled_tools
                .retain(|t| t != "todo_read" && t != "todo_write");
        }
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
        // 2. default-model setting (Specific -> explicit model, LastUsed -> state.last_model)
        // 3. Legacy config.model field (for backward compatibility)
        let Some(model) = model
            .or_else(|| match &config.default_model {
                DefaultModel::Specific(m) => Some(m.clone()),
                DefaultModel::LastUsed => config.state.as_ref().and_then(|s| s.last_model.clone()),
            })
            .or_else(|| config.model.clone())
        else {
            return Err(error::Error::NoModelConfigured);
        };

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
}

/// Initialize MCP and LSP servers from configuration.
///
/// The `lsp_override` parameter allows command-line options to override the config:
/// - `Some(true)` forces LSP enabled (--lsp)
/// - `Some(false)` forces LSP disabled (--no-lsp)
/// - `None` uses the config file's `lsp_enabled` setting
///
/// Note: MCP servers are registered but not started by default.
/// Use the /mcp command or MCP menu to start them.
pub async fn initialize_servers(working_dir: &Path, lsp_override: Option<bool>) {
    let config_file = ConfigFile::load().unwrap_or_default();

    // Register MCP servers (but don't start them - they're disabled by default)
    if let Some(mcp_config) = &config_file.mcp {
        let servers: Vec<crate::mcp::McpServerConfig> = mcp_config
            .servers
            .iter()
            .filter(|s| s.enabled)
            .map(|s| crate::mcp::McpServerConfig {
                name: s.name.clone(),
                _command: s.command.clone(),
                _args: s.args.clone(),
                _env: s.env.clone(),
            })
            .collect();
        crate::mcp::register_servers(servers).await;
    }

    // Determine if LSP is enabled: command-line override takes precedence
    let lsp_enabled = lsp_override.unwrap_or(config_file.lsp_enabled);

    // Register LSP server configs for lazy initialization (servers start on-demand)
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
        crate::lsp::register_configs(servers).await;
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
            id: "test-model-id".to_string(),
            name: Some("Test Model".to_string()),
            reasoning_effort: Some("high".to_string()),
            temperature: Some(0.7),
            top_p: None,
            max_tokens: Some(1000),
        };
        let toml = toml::to_string(&model_config).unwrap();
        assert!(toml.contains("id = "), "Expected 'id'");
        assert!(toml.contains("name = "), "Expected 'name'");
        assert!(
            toml.contains("reasoning-effort"),
            "Expected 'reasoning-effort'"
        );
        assert!(toml.contains("max-tokens"), "Expected 'max-tokens'");

        // Test display_name falls back to id when name is None
        let model_config_no_name = ModelConfig {
            id: "fallback-id".to_string(),
            name: None,
            reasoning_effort: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
        };
        assert_eq!(model_config_no_name.display_name(), "fallback-id");
        assert_eq!(model_config.display_name(), "Test Model");
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
    fn test_show_image_previews_parsing() {
        let toml_str = r#"show-image-previews = false"#;
        let config: ConfigFile = toml::from_str(toml_str).unwrap();
        assert!(!config.show_image_previews);
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
            default_model: DefaultModel::Specific("claude/claude-sonnet-4-5".to_string()),
            ..Default::default()
        };
        let toml = toml::to_string(&config).unwrap();
        assert!(
            toml.contains("default-model = \"claude/claude-sonnet-4-5\""),
            "Expected 'default-model = \"claude/claude-sonnet-4-5\"', got: {}",
            toml
        );
    }

    #[test]
    fn test_model_config_aliases() {
        // Test that multiple model configs can share the same API id but have different names
        let config = OpenAiCompatProviderConfig {
            enabled: true,
            api_key: "test".to_string(),
            base_url: "https://example.com".to_string(),
            model_configs: vec![
                ModelConfig {
                    id: "claude-opus-4-5-thinking".to_string(),
                    name: Some("claude-opus-4-5-thinking#off".to_string()),
                    reasoning_effort: Some("off".to_string()),
                    temperature: None,
                    top_p: None,
                    max_tokens: None,
                },
                ModelConfig {
                    id: "claude-opus-4-5-thinking".to_string(),
                    name: Some("claude-opus-4-5-thinking#high".to_string()),
                    reasoning_effort: Some("high".to_string()),
                    temperature: None,
                    top_p: None,
                    max_tokens: None,
                },
            ],
        };

        // all_models() should return the display names
        let models = config.all_models();
        assert_eq!(models.len(), 2);
        assert!(models.contains(&"claude-opus-4-5-thinking#off".to_string()));
        assert!(models.contains(&"claude-opus-4-5-thinking#high".to_string()));

        // get_model_config() should find by display name
        let off_config = config.get_model_config("claude-opus-4-5-thinking#off");
        assert!(off_config.is_some());
        let off_config = off_config.unwrap();
        assert_eq!(off_config.id, "claude-opus-4-5-thinking");
        assert_eq!(off_config.reasoning_effort, Some("off".to_string()));

        let high_config = config.get_model_config("claude-opus-4-5-thinking#high");
        assert!(high_config.is_some());
        let high_config = high_config.unwrap();
        assert_eq!(high_config.id, "claude-opus-4-5-thinking");
        assert_eq!(high_config.reasoning_effort, Some("high".to_string()));

        // Looking up by API id should not find anything (we lookup by display name)
        let by_id = config.get_model_config("claude-opus-4-5-thinking");
        assert!(by_id.is_none());
    }
}
