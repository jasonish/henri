// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use reqwest::header::HeaderMap;

use crate::config::ConfigFile;
use crate::error::{Error, Result};
use crate::provider::openai_compat::{ModelConfigProvider, OpenAiChatConfig, execute_chat};
use crate::provider::{ChatResponse, Message, Provider};
use crate::usage;

pub(crate) struct OpenRouterProvider {
    config: OpenAiChatConfig,
    provider_config: crate::config::OpenRouterConfig,
}

impl OpenRouterProvider {
    pub(crate) fn try_new(provider_name: &str) -> Result<Self> {
        let config = ConfigFile::load()?;
        let openrouter_config = config
            .get_provider(provider_name)
            .and_then(|p| p.as_openrouter())
            .ok_or_else(|| {
                Error::Auth(format!(
                    "OpenRouter provider '{}' not configured.",
                    provider_name
                ))
            })?;

        if !openrouter_config.enabled {
            return Err(Error::Auth(format!(
                "OpenRouter provider '{}' is disabled.",
                provider_name
            )));
        }

        if openrouter_config.api_key.is_empty() {
            return Err(Error::Auth(format!(
                "OpenRouter provider '{}' API key is not set.",
                provider_name
            )));
        }

        let base_url = "https://openrouter.ai/api/v1".to_string();

        // Convert to OpenRouterConfig for internal use
        let provider_config = crate::config::OpenRouterConfig {
            enabled: openrouter_config.enabled,
            api_key: openrouter_config.api_key.clone(),
            models: openrouter_config.models.clone(),
            model_configs: openrouter_config.model_configs.clone(),
        };

        // Set up custom headers for OpenRouter
        let mut custom_headers = HeaderMap::new();
        custom_headers.insert(
            "HTTP-Referer",
            "https://github.com/jasonish/henri".parse().unwrap(),
        );
        custom_headers.insert("X-Title", "Henri".parse().unwrap());

        let chat_config = OpenAiChatConfig {
            provider_name: provider_name.to_string(),
            client: reqwest::Client::new(),
            api_key: openrouter_config.api_key.clone(),
            base_url,
            model: "default".to_string(),
            usage_tracker: usage::openrouter(),
            custom_headers: Some(custom_headers),
        };

        Ok(Self {
            config: chat_config,
            provider_config,
        })
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.config.model = model;
    }

    /// Get context limit for a given model name
    /// Returns None since OpenRouter hosts many different models with varying limits
    pub(crate) fn context_limit(_model: &str) -> Option<u64> {
        None
    }
}

impl Provider for OpenRouterProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        execute_chat(&self.config, &self.provider_config, &messages, output).await
    }

    async fn prepare_request(&self, messages: Vec<Message>) -> Result<serde_json::Value> {
        use crate::provider::openai_compat::build_request;
        let request = build_request(&self.config, &self.provider_config, &messages).await?;
        Ok(serde_json::to_value(&request)?)
    }

    fn start_turn(&self) {
        crate::usage::openrouter().start_turn();
    }
}

// Implement ModelConfigProvider for OpenRouterConfig
impl ModelConfigProvider for crate::config::OpenRouterConfig {
    fn get_model_config(&self, model_name: &str) -> Option<&crate::config::ModelConfig> {
        self.get_model_config(model_name)
    }
}
