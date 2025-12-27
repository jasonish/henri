// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish
//
// Provider management for multiple AI backends.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::chat;
use crate::compaction;
use crate::config::{Config, ConfigFile, ProviderType};
use crate::error::Result;
use crate::output::OutputContext;
use crate::provider::anthropic::AnthropicProvider;
use crate::provider::antigravity::AntigravityProvider;
use crate::provider::copilot::CopilotProvider;
use crate::provider::openai::OpenAiProvider;
use crate::provider::openai_compat::OpenAiCompatProvider;
use crate::provider::openrouter::OpenRouterProvider;
use crate::provider::zen::ZenProvider;
use crate::provider::{ContentBlock, Message, MessageContent, Role};
use crate::services::Services;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ModelProvider {
    Antigravity,
    OpenCodeZen,
    GitHubCopilot,
    Claude,
    OpenAi,
    OpenAiCompat,
    OpenRouter,
}

impl ModelProvider {
    pub(crate) fn display_name(&self) -> &'static str {
        match self {
            ModelProvider::Antigravity => "Antigravity",
            ModelProvider::OpenCodeZen => "OpenCode Zen",
            ModelProvider::GitHubCopilot => "GitHub Copilot",
            ModelProvider::Claude => "Anthropic Claude",
            ModelProvider::OpenAi => "OpenAI",
            ModelProvider::OpenAiCompat => "OpenAI Compatible",
            ModelProvider::OpenRouter => "OpenRouter",
        }
    }

    pub(crate) fn id(&self) -> &'static str {
        match self {
            ModelProvider::Antigravity => "antigravity",
            ModelProvider::OpenCodeZen => "zen",
            ModelProvider::GitHubCopilot => "copilot",
            ModelProvider::Claude => "claude",
            ModelProvider::OpenAi => "openai",
            ModelProvider::OpenAiCompat => "openai_compat",
            ModelProvider::OpenRouter => "openrouter",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Self> {
        match id {
            "antigravity" => Some(ModelProvider::Antigravity),
            "zen" => Some(ModelProvider::OpenCodeZen),
            "copilot" => Some(ModelProvider::GitHubCopilot),
            "claude" => Some(ModelProvider::Claude),
            "openai" => Some(ModelProvider::OpenAi),
            "openai_compat" => Some(ModelProvider::OpenAiCompat),
            "openrouter" => Some(ModelProvider::OpenRouter),
            _ => None,
        }
    }
}

/// A model choice representing a provider/model combination
#[derive(Clone, Debug)]
pub struct ModelChoice {
    pub provider: ModelProvider,
    pub model_id: String,
    pub custom_provider: Option<String>,
    /// Whether this model is marked as a favorite
    pub is_favorite: bool,
}

impl ModelChoice {
    pub(crate) fn display(&self) -> String {
        if let Some(custom) = &self.custom_provider {
            format!("{}: {}", custom, self.model_id)
        } else {
            format!("{}: {}", self.provider.display_name(), self.model_id)
        }
    }

    /// Get the display suffix (provider type name in parentheses) for custom providers
    pub(crate) fn display_suffix(&self) -> Option<String> {
        if self.custom_provider.is_some() {
            Some(format!(" ({})", self.provider.display_name()))
        } else {
            None
        }
    }

    pub(crate) fn short_display(&self) -> String {
        match self.provider {
            ModelProvider::OpenAiCompat
            | ModelProvider::OpenCodeZen
            | ModelProvider::OpenRouter => {
                if let Some(custom) = &self.custom_provider {
                    format!("{}/{}", custom, self.model_id)
                } else {
                    format!("{}/{}", self.provider.id(), self.model_id)
                }
            }
            _ => format!("{}/{}", self.provider.id(), self.model_id),
        }
    }
}

impl std::fmt::Display for ModelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let star = if self.is_favorite { "*" } else { " " };
        write!(f, "{}{}", star, self.display())
    }
}

/// Build a list of all available model choices from configured providers
pub(crate) fn build_model_choices() -> Vec<ModelChoice> {
    let mut choices = Vec::new();

    let config = match ConfigFile::load().ok() {
        Some(c) => c,
        None => return choices,
    };

    // Helper to check if a model is a favorite
    let is_favorite =
        |choice: &ModelChoice| -> bool { config.is_favorite(&choice.short_display()) };

    // Count how many providers of each type are enabled
    let mut type_counts: HashMap<ProviderType, usize> = HashMap::new();
    for provider_config in config.providers.entries.values() {
        if provider_config.is_enabled() {
            let is_usable = match provider_config.provider_type() {
                ProviderType::Zen => provider_config
                    .as_zen()
                    .is_some_and(|c| !c.api_key.is_empty()),
                _ => true,
            };
            if is_usable {
                *type_counts
                    .entry(provider_config.provider_type())
                    .or_insert(0) += 1;
            }
        }
    }

    for (local_id, provider_config) in &config.providers.entries {
        if !provider_config.is_enabled() {
            continue;
        }

        let provider_type = provider_config.provider_type();
        let use_custom_name = type_counts.get(&provider_type).copied().unwrap_or(0) > 1;

        match provider_type {
            ProviderType::Antigravity => {
                for &model in AntigravityProvider::models() {
                    let mut choice = ModelChoice {
                        provider: ModelProvider::Antigravity,
                        model_id: model.to_string(),
                        custom_provider: Some(local_id.clone()),
                        is_favorite: false,
                    };
                    choice.is_favorite = is_favorite(&choice);
                    choices.push(choice);
                }
            }
            ProviderType::GithubCopilot => {
                for &model in CopilotProvider::models() {
                    let mut choice = ModelChoice {
                        provider: ModelProvider::GitHubCopilot,
                        model_id: model.to_string(),
                        custom_provider: if use_custom_name {
                            Some(local_id.clone())
                        } else {
                            None
                        },
                        is_favorite: false,
                    };
                    choice.is_favorite = is_favorite(&choice);
                    choices.push(choice);
                }
            }
            ProviderType::Zen => {
                if let Some(zen_config) = provider_config.as_zen()
                    && !zen_config.api_key.is_empty()
                {
                    for &model in ZenProvider::models() {
                        let mut choice = ModelChoice {
                            provider: ModelProvider::OpenCodeZen,
                            model_id: model.to_string(),
                            custom_provider: Some(local_id.clone()),
                            is_favorite: false,
                        };
                        choice.is_favorite = is_favorite(&choice);
                        choices.push(choice);
                    }
                }
            }
            ProviderType::Claude => {
                for &model in AnthropicProvider::models() {
                    let mut choice = ModelChoice {
                        provider: ModelProvider::Claude,
                        model_id: model.to_string(),
                        custom_provider: if use_custom_name {
                            Some(local_id.clone())
                        } else {
                            None
                        },
                        is_favorite: false,
                    };
                    choice.is_favorite = is_favorite(&choice);
                    choices.push(choice);
                }
            }
            ProviderType::Openai => {
                for &model in OpenAiProvider::models() {
                    let mut choice = ModelChoice {
                        provider: ModelProvider::OpenAi,
                        model_id: model.to_string(),
                        custom_provider: if use_custom_name {
                            Some(local_id.clone())
                        } else {
                            None
                        },
                        is_favorite: false,
                    };
                    choice.is_favorite = is_favorite(&choice);
                    choices.push(choice);
                }
            }
            ProviderType::OpenaiCompat => {
                if let Some(compat_config) = provider_config.as_openai_compat() {
                    let models = compat_config.all_models();
                    if models.is_empty() {
                        let mut choice = ModelChoice {
                            provider: ModelProvider::OpenAiCompat,
                            model_id: "default".to_string(),
                            custom_provider: Some(local_id.clone()),
                            is_favorite: false,
                        };
                        choice.is_favorite = is_favorite(&choice);
                        choices.push(choice);
                    } else {
                        for model in models {
                            let mut choice = ModelChoice {
                                provider: ModelProvider::OpenAiCompat,
                                model_id: model,
                                custom_provider: Some(local_id.clone()),
                                is_favorite: false,
                            };
                            choice.is_favorite = is_favorite(&choice);
                            choices.push(choice);
                        }
                    }
                }
            }
            ProviderType::OpenRouter => {
                if let Some(openrouter_config) = provider_config.as_openrouter() {
                    let models = openrouter_config.all_models();
                    if models.is_empty() {
                        let mut choice = ModelChoice {
                            provider: ModelProvider::OpenRouter,
                            model_id: "default".to_string(),
                            custom_provider: if use_custom_name {
                                Some(local_id.clone())
                            } else {
                                None
                            },
                            is_favorite: false,
                        };
                        choice.is_favorite = is_favorite(&choice);
                        choices.push(choice);
                    } else {
                        for model in models {
                            let mut choice = ModelChoice {
                                provider: ModelProvider::OpenRouter,
                                model_id: model,
                                custom_provider: if use_custom_name {
                                    Some(local_id.clone())
                                } else {
                                    None
                                },
                                is_favorite: false,
                            };
                            choice.is_favorite = is_favorite(&choice);
                            choices.push(choice);
                        }
                    }
                }
            }
        }
    }

    // Sort choices for predictable ordering: by provider name, then model ID
    choices.sort_by(|a, b| {
        let provider_a = a.custom_provider.as_deref().unwrap_or(a.provider.id());
        let provider_b = b.custom_provider.as_deref().unwrap_or(b.provider.id());
        provider_a
            .cmp(provider_b)
            .then_with(|| a.model_id.cmp(&b.model_id))
    });

    choices
}

/// Manages all provider instances and handles routing chat requests
pub struct ProviderManager {
    zen_provider: ZenProvider,
    antigravity_providers: HashMap<String, AntigravityProvider>,
    copilot_provider: Option<CopilotProvider>,
    anthropic_provider: Option<AnthropicProvider>,
    openai_provider: Option<OpenAiProvider>,
    openai_compat_providers: HashMap<String, OpenAiCompatProvider>,
    openrouter_provider: Option<OpenRouterProvider>,
    current_provider: ModelProvider,
    current_model_id: String,
    current_custom_provider: Option<String>,
    services: Services,
}

impl ProviderManager {
    pub(crate) fn new(config: &Config, services: Services) -> Self {
        let zen_provider = ZenProvider::new(config);
        let copilot_provider = CopilotProvider::try_new().ok();
        let anthropic_provider = AnthropicProvider::try_new().ok();
        let openai_provider = OpenAiProvider::try_new().ok();
        let openrouter_provider = OpenRouterProvider::try_new("openrouter").ok();

        // Load all configured OpenAI-compatible providers
        let mut openai_compat_providers = HashMap::new();
        let mut antigravity_providers = HashMap::new();
        if let Ok(cfg) = crate::config::ConfigFile::load() {
            for (name, provider_config) in &cfg.providers.entries {
                if provider_config.as_openai_compat().is_some()
                    && let Ok(provider) = OpenAiCompatProvider::try_new(name)
                {
                    openai_compat_providers.insert(name.clone(), provider);
                }
                if provider_config.as_antigravity().is_some()
                    && let Ok(provider) = AntigravityProvider::try_new(name)
                {
                    antigravity_providers.insert(name.clone(), provider);
                }
            }
        }

        let (current_provider, current_model_id, current_custom_provider) =
            parse_model_spec(&config.model);

        Self {
            zen_provider,
            antigravity_providers,
            copilot_provider,
            anthropic_provider,
            openai_provider,
            openai_compat_providers,
            openrouter_provider,
            current_provider,
            current_model_id,
            current_custom_provider,
            services,
        }
    }

    pub(crate) fn current_model_string(&self) -> String {
        match self.current_provider {
            ModelProvider::Antigravity | ModelProvider::OpenAiCompat => {
                if let Some(custom_name) = &self.current_custom_provider {
                    format!("{}/{}", custom_name, self.current_model_id)
                } else {
                    format!("{}/{}", self.current_provider.id(), self.current_model_id)
                }
            }
            _ => format!("{}/{}", self.current_provider.id(), self.current_model_id),
        }
    }

    pub(crate) fn set_model(
        &mut self,
        provider: ModelProvider,
        model_id: String,
        custom_provider: Option<String>,
    ) {
        self.current_provider = provider;
        self.current_model_id = model_id.clone();
        self.current_custom_provider = custom_provider.clone();

        // Helper to initialize and set model on an optional provider
        macro_rules! init_and_set {
            ($provider:expr, $init:expr, $name:literal) => {{
                if $provider.is_none() {
                    match $init {
                        Ok(p) => $provider = Some(p),
                        Err(e) => eprintln!("Note: {} initialization failed: {}", $name, e),
                    }
                }
                if let Some(ref mut p) = $provider {
                    p.set_model(model_id);
                }
            }};
        }

        match provider {
            ModelProvider::Antigravity => {
                if let Some(custom_name) = &custom_provider {
                    if !self.antigravity_providers.contains_key(custom_name) {
                        if let Ok(provider) = AntigravityProvider::try_new(custom_name) {
                            self.antigravity_providers
                                .insert(custom_name.clone(), provider);
                        } else {
                            eprintln!("Antigravity provider '{}' not configured.", custom_name);
                            return;
                        }
                    }
                    if let Some(p) = self.antigravity_providers.get_mut(custom_name) {
                        p.set_model(model_id);
                    }
                } else {
                    eprintln!("Antigravity provider requires a custom provider name.");
                }
            }
            ModelProvider::OpenCodeZen => self.zen_provider.set_model(model_id),
            ModelProvider::GitHubCopilot => init_and_set!(
                self.copilot_provider,
                CopilotProvider::try_new(),
                "GitHub Copilot"
            ),
            ModelProvider::Claude => init_and_set!(
                self.anthropic_provider,
                AnthropicProvider::try_new(),
                "Anthropic"
            ),
            ModelProvider::OpenAi => {
                init_and_set!(self.openai_provider, OpenAiProvider::try_new(), "OpenAI")
            }
            ModelProvider::OpenRouter => init_and_set!(
                self.openrouter_provider,
                OpenRouterProvider::try_new("openrouter"),
                "OpenRouter"
            ),
            ModelProvider::OpenAiCompat => {
                if let Some(custom_name) = &custom_provider {
                    // Try to get or initialize the specific custom provider
                    if !self.openai_compat_providers.contains_key(custom_name) {
                        if let Ok(provider) = OpenAiCompatProvider::try_new(custom_name) {
                            self.openai_compat_providers
                                .insert(custom_name.clone(), provider);
                        } else {
                            eprintln!(
                                "OpenAI Compatible provider '{}' not configured.",
                                custom_name
                            );
                            return;
                        }
                    }
                    if let Some(p) = self.openai_compat_providers.get_mut(custom_name) {
                        p.set_model(model_id);
                    }
                } else {
                    eprintln!("OpenAI Compatible provider requires a custom provider name.");
                }
            }
        }
    }

    pub(crate) fn set_thinking_enabled(&mut self, enabled: bool) {
        match self.current_provider {
            ModelProvider::Antigravity => {
                if let Some(custom_name) = &self.current_custom_provider
                    && let Some(p) = self.antigravity_providers.get_mut(custom_name)
                {
                    p.set_thinking_enabled(enabled);
                }
            }
            ModelProvider::OpenCodeZen => self.zen_provider.set_thinking_enabled(enabled),
            ModelProvider::GitHubCopilot => {
                if let Some(ref mut p) = self.copilot_provider {
                    p.set_thinking_enabled(enabled);
                }
            }
            ModelProvider::Claude => {
                if let Some(ref mut p) = self.anthropic_provider {
                    p.set_thinking_enabled(enabled);
                }
            }
            ModelProvider::OpenAi => {
                if let Some(ref mut p) = self.openai_provider {
                    p.set_thinking_enabled(enabled);
                }
            }
            ModelProvider::OpenRouter | ModelProvider::OpenAiCompat => {
                // OpenAI-compat providers always display reasoning if provided
            }
        }
    }

    /// Set the thinking mode for Zen provider's Gemini models
    /// mode should be one of: "minimal", "low", "medium", "high", or None for default
    pub(crate) fn set_thinking_mode(&mut self, mode: Option<String>) {
        if self.current_provider == ModelProvider::OpenCodeZen {
            self.zen_provider.set_thinking_mode(mode);
        }
    }

    /// Send a chat request, emitting events via output::emit()
    pub async fn chat(
        &mut self,
        messages: &mut Vec<Message>,
        interrupted: &Arc<AtomicBool>,
        output: &crate::output::OutputContext,
    ) -> Result<()> {
        let services = &self.services;

        // Helper to chat with an optional provider, emitting error if not configured
        macro_rules! chat_optional {
            ($provider:expr, $name:literal) => {
                match $provider.as_mut() {
                    Some(p) => {
                        p.set_model(self.current_model_id.clone());
                        chat::run_chat_loop(p, messages, interrupted, output, services).await
                    }
                    None => {
                        let msg = concat!($name, " not configured");
                        crate::output::emit_error(output, msg);
                        Err(crate::error::Error::Auth(msg.to_string()))
                    }
                }
            };
        }

        match self.current_provider {
            ModelProvider::Antigravity => {
                if let Some(custom_name) = &self.current_custom_provider {
                    match self.antigravity_providers.get_mut(custom_name) {
                        Some(p) => {
                            p.set_model(self.current_model_id.clone());
                            chat::run_chat_loop(p, messages, interrupted, output, services).await
                        }
                        None => {
                            let msg =
                                format!("Antigravity provider '{}' not configured", custom_name);
                            crate::output::emit_error(output, &msg);
                            Err(crate::error::Error::Auth(msg))
                        }
                    }
                } else {
                    let msg = "Antigravity provider requires a custom provider name";
                    crate::output::emit_error(output, msg);
                    Err(crate::error::Error::Auth(msg.to_string()))
                }
            }
            ModelProvider::OpenCodeZen => {
                self.zen_provider.set_model(self.current_model_id.clone());
                chat::run_chat_loop(&self.zen_provider, messages, interrupted, output, services)
                    .await
            }
            ModelProvider::GitHubCopilot => chat_optional!(self.copilot_provider, "GitHub Copilot"),
            ModelProvider::Claude => chat_optional!(self.anthropic_provider, "Anthropic"),
            ModelProvider::OpenAi => chat_optional!(self.openai_provider, "OpenAI"),
            ModelProvider::OpenRouter => chat_optional!(self.openrouter_provider, "OpenRouter"),
            ModelProvider::OpenAiCompat => {
                if let Some(custom_name) = &self.current_custom_provider {
                    match self.openai_compat_providers.get_mut(custom_name) {
                        Some(p) => {
                            p.set_model(self.current_model_id.clone());
                            chat::run_chat_loop(p, messages, interrupted, output, services).await
                        }
                        None => {
                            let msg = format!(
                                "OpenAI Compatible provider '{}' not configured",
                                custom_name
                            );
                            crate::output::emit_error(output, &msg);
                            Err(crate::error::Error::Auth(msg))
                        }
                    }
                } else {
                    let msg = "OpenAI Compatible provider requires a custom provider name";
                    crate::output::emit_error(output, msg);
                    Err(crate::error::Error::Auth(msg.to_string()))
                }
            }
        }
    }

    /// Prepare and return the request that would be sent to the current provider's API.
    pub async fn prepare_request(&mut self, messages: Vec<Message>) -> Result<serde_json::Value> {
        use crate::provider::Provider;

        macro_rules! prepare_optional {
            ($provider:expr, $name:literal) => {
                match $provider.as_mut() {
                    Some(p) => {
                        p.set_model(self.current_model_id.clone());
                        p.prepare_request(messages).await
                    }
                    None => Err(crate::error::Error::Auth(
                        concat!($name, " not configured").to_string(),
                    )),
                }
            };
        }

        match self.current_provider {
            ModelProvider::Antigravity => {
                if let Some(custom_name) = &self.current_custom_provider {
                    match self.antigravity_providers.get_mut(custom_name) {
                        Some(p) => {
                            p.set_model(self.current_model_id.clone());
                            p.prepare_request(messages).await
                        }
                        None => Err(crate::error::Error::Auth(format!(
                            "Antigravity provider '{}' not configured",
                            custom_name
                        ))),
                    }
                } else {
                    Err(crate::error::Error::Auth(
                        "Antigravity provider requires a custom provider name".to_string(),
                    ))
                }
            }
            ModelProvider::OpenCodeZen => {
                self.zen_provider.set_model(self.current_model_id.clone());
                self.zen_provider.prepare_request(messages).await
            }
            ModelProvider::GitHubCopilot => {
                prepare_optional!(self.copilot_provider, "GitHub Copilot")
            }
            ModelProvider::Claude => prepare_optional!(self.anthropic_provider, "Anthropic"),
            ModelProvider::OpenAi => prepare_optional!(self.openai_provider, "OpenAI"),
            ModelProvider::OpenRouter => prepare_optional!(self.openrouter_provider, "OpenRouter"),
            ModelProvider::OpenAiCompat => {
                if let Some(custom_name) = &self.current_custom_provider {
                    match self.openai_compat_providers.get_mut(custom_name) {
                        Some(p) => {
                            p.set_model(self.current_model_id.clone());
                            p.prepare_request(messages).await
                        }
                        None => Err(crate::error::Error::Auth(format!(
                            "OpenAI Compatible provider '{}' not configured",
                            custom_name
                        ))),
                    }
                } else {
                    Err(crate::error::Error::Auth(
                        "OpenAI Compatible provider requires a custom provider name".to_string(),
                    ))
                }
            }
        }
    }

    /// Count tokens for the current message state (Claude/Anthropic only).
    pub async fn count_tokens(&mut self, messages: &[Message]) -> Result<serde_json::Value> {
        match self.current_provider {
            ModelProvider::Claude => match self.anthropic_provider.as_ref() {
                Some(p) => p.count_tokens(messages).await,
                None => Err(crate::error::Error::Auth(
                    "Anthropic not configured".to_string(),
                )),
            },
            _ => Err(crate::error::Error::Other(
                "Token counting is only available for Claude/Anthropic provider".to_string(),
            )),
        }
    }

    /// Get the current provider type
    pub(crate) fn current_provider(&self) -> ModelProvider {
        self.current_provider
    }

    /// Get the current model ID
    pub(crate) fn current_model_id(&self) -> &str {
        &self.current_model_id
    }

    /// Get the custom provider name (if any)
    pub(crate) fn current_custom_provider(&self) -> Option<&str> {
        self.current_custom_provider.as_deref()
    }

    /// Compact the message context by summarizing older messages
    pub async fn compact_context(
        &mut self,
        messages: &mut Vec<Message>,
        preserve_recent_turns: usize,
        output: &OutputContext,
    ) -> Result<compaction::CompactionResult> {
        use crate::provider::Provider;

        let (to_compact, to_preserve) =
            compaction::segment_messages(messages, preserve_recent_turns);

        if to_compact.is_empty() {
            return Err(crate::error::Error::Other(
                "No messages old enough to compact".into(),
            ));
        }

        let messages_compacted = to_compact.len();

        // Build summarization request
        let user_request = compaction::build_summarization_request(&to_compact);
        let system_msg = Message::system(compaction::summarization_system_prompt());
        let request_messages = vec![system_msg, user_request];

        // Get summary from current provider
        let response = match self.current_provider {
            ModelProvider::Antigravity => {
                if let Some(custom_name) = &self.current_custom_provider {
                    match self.antigravity_providers.get_mut(custom_name) {
                        Some(p) => {
                            p.set_model(self.current_model_id.clone());
                            p.chat(request_messages, output).await?
                        }
                        None => {
                            return Err(crate::error::Error::Auth(format!(
                                "Antigravity provider '{}' not configured",
                                custom_name
                            )));
                        }
                    }
                } else {
                    return Err(crate::error::Error::Auth(
                        "Antigravity provider requires a custom provider name".into(),
                    ));
                }
            }
            ModelProvider::OpenCodeZen => {
                self.zen_provider.set_model(self.current_model_id.clone());
                self.zen_provider.chat(request_messages, output).await?
            }
            ModelProvider::GitHubCopilot => match self.copilot_provider.as_mut() {
                Some(p) => {
                    p.set_model(self.current_model_id.clone());
                    p.chat(request_messages, output).await?
                }
                None => {
                    return Err(crate::error::Error::Auth(
                        "GitHub Copilot not configured".into(),
                    ));
                }
            },
            ModelProvider::Claude => match self.anthropic_provider.as_mut() {
                Some(p) => {
                    p.set_model(self.current_model_id.clone());
                    p.chat(request_messages, output).await?
                }
                None => return Err(crate::error::Error::Auth("Anthropic not configured".into())),
            },
            ModelProvider::OpenAi => match self.openai_provider.as_mut() {
                Some(p) => {
                    p.set_model(self.current_model_id.clone());
                    p.chat(request_messages, output).await?
                }
                None => return Err(crate::error::Error::Auth("OpenAI not configured".into())),
            },
            ModelProvider::OpenRouter => match self.openrouter_provider.as_mut() {
                Some(p) => {
                    p.set_model(self.current_model_id.clone());
                    p.chat(request_messages, output).await?
                }
                None => {
                    return Err(crate::error::Error::Auth(
                        "OpenRouter not configured".into(),
                    ));
                }
            },
            ModelProvider::OpenAiCompat => {
                if let Some(custom_name) = &self.current_custom_provider {
                    match self.openai_compat_providers.get_mut(custom_name) {
                        Some(p) => {
                            p.set_model(self.current_model_id.clone());
                            p.chat(request_messages, output).await?
                        }
                        None => {
                            return Err(crate::error::Error::Auth(format!(
                                "OpenAI Compatible provider '{}' not configured",
                                custom_name
                            )));
                        }
                    }
                } else {
                    return Err(crate::error::Error::Auth(
                        "OpenAI Compatible provider requires a custom provider name".into(),
                    ));
                }
            }
        };

        // Extract summary text
        let summary = response
            .content_blocks
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text { text } = block {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Build new message list
        let summary_message = Message {
            role: Role::User,
            content: MessageContent::Blocks(vec![ContentBlock::Summary {
                summary,
                messages_compacted,
            }]),
        };

        let mut new_messages = vec![summary_message];
        new_messages.extend(to_preserve);
        *messages = new_messages;

        Ok(compaction::CompactionResult { messages_compacted })
    }
}

/// Parse a model string like "zen/big-pickle" or "ollama/llama3.2" into (provider, model_id, custom_provider_name)
/// Returns (provider_type, model_id, optional_custom_provider_name)
fn parse_model_spec(spec: &str) -> (ModelProvider, String, Option<String>) {
    if let Some((prefix, model)) = spec.split_once('/') {
        // Check for built-in providers first
        let provider = match prefix {
            "antigravity" => {
                // Find first enabled antigravity provider
                if let Ok(config) = crate::config::ConfigFile::load() {
                    for (name, provider_config) in &config.providers.entries {
                        if provider_config.as_antigravity().is_some()
                            && provider_config.is_enabled()
                        {
                            return (
                                ModelProvider::Antigravity,
                                model.to_string(),
                                Some(name.clone()),
                            );
                        }
                    }
                }
                // No antigravity configured - use "antigravity" as the name so
                // the error message is helpful ("provider 'antigravity' not configured")
                return (
                    ModelProvider::Antigravity,
                    model.to_string(),
                    Some("antigravity".to_string()),
                );
            }
            "copilot" | "github-copilot" => {
                return (ModelProvider::GitHubCopilot, model.to_string(), None);
            }
            "claude" | "anthropic" => return (ModelProvider::Claude, model.to_string(), None),
            "openai" | "oai" => return (ModelProvider::OpenAi, model.to_string(), None),
            "openrouter" => return (ModelProvider::OpenRouter, model.to_string(), None),
            "zen" => return (ModelProvider::OpenCodeZen, model.to_string(), None),
            _ => {
                // Check if it's a custom OpenAI-compatible or Antigravity provider
                if let Ok(config) = crate::config::ConfigFile::load() {
                    if config
                        .get_provider(prefix)
                        .is_some_and(|p| p.as_openai_compat().is_some())
                    {
                        return (
                            ModelProvider::OpenAiCompat,
                            model.to_string(),
                            Some(prefix.to_string()),
                        );
                    }
                    if config
                        .get_provider(prefix)
                        .is_some_and(|p| p.as_antigravity().is_some())
                    {
                        return (
                            ModelProvider::Antigravity,
                            model.to_string(),
                            Some(prefix.to_string()),
                        );
                    }
                }
                // Default to Zen if not found
                ModelProvider::OpenCodeZen
            }
        };
        return (provider, model.to_string(), None);
    }

    // Default to Zen for unqualified model names
    (ModelProvider::OpenCodeZen, spec.to_string(), None)
}
