// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

mod anthropic;
mod gemini;
mod responses;

use std::sync::LazyLock;

use reqwest::Client;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::provider::openai_compat::OpenAiCompatProvider;
use crate::provider::{ChatResponse, Message, Provider};
use crate::usage;

const ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";

/// Common context for chat operations across API backends
pub(super) struct ChatContext<'a> {
    pub client: &'a Client,
    pub api_key: &'a str,
    pub model: &'a str,
    pub thinking_enabled: bool,
    pub thinking_mode: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
enum ApiType {
    Anthropic,
    OpenAiCompatible,
    OpenAiResponses,
    Gemini,
}

#[derive(Debug, Clone, Copy)]
struct ZenModelSpec {
    name: &'static str,
    api_type: ApiType,
    supports_thinking: bool,
}

impl ZenModelSpec {
    const fn new(name: &'static str, api_type: ApiType, supports_thinking: bool) -> Self {
        Self {
            name,
            api_type,
            supports_thinking,
        }
    }
}

const ZEN_MODELS: &[ZenModelSpec] = &[
    ZenModelSpec::new("big-pickle", ApiType::OpenAiCompatible, true),
    ZenModelSpec::new("claude-haiku-4-5", ApiType::Anthropic, true),
    ZenModelSpec::new("claude-opus-4-5", ApiType::Anthropic, true),
    ZenModelSpec::new("claude-sonnet-4-5", ApiType::Anthropic, true),
    ZenModelSpec::new("gemini-3-pro", ApiType::Gemini, true),
    ZenModelSpec::new("gemini-3-flash", ApiType::Gemini, true),
    ZenModelSpec::new("glm-4.6", ApiType::OpenAiCompatible, true),
    ZenModelSpec::new("glm-4.7-free", ApiType::OpenAiCompatible, true),
    ZenModelSpec::new("gpt-5.1", ApiType::OpenAiResponses, true),
    ZenModelSpec::new("gpt-5.1-codex", ApiType::OpenAiResponses, true),
    ZenModelSpec::new("gpt-5.1-codex-max", ApiType::OpenAiResponses, true),
    ZenModelSpec::new("grok-code", ApiType::OpenAiCompatible, false),
    ZenModelSpec::new("minimax-m2.1-free", ApiType::Anthropic, false),
];

fn get_model_spec(model: &str) -> Option<&'static ZenModelSpec> {
    ZEN_MODELS.iter().find(|m| m.name == model)
}

static ZEN_MODEL_NAMES: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| ZEN_MODELS.iter().map(|m| m.name).collect());

fn strip_unsupported_schema_fields(schema: serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(mut map) => {
            map.remove("$schema");
            map.remove("additionalProperties");
            let cleaned: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .map(|(k, v)| (k, strip_unsupported_schema_fields(v)))
                .collect();
            serde_json::Value::Object(cleaned)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.into_iter()
                .map(strip_unsupported_schema_fields)
                .collect(),
        ),
        other => other,
    }
}

pub(crate) struct ZenProvider {
    client: Client,
    api_key: String,
    model: String,
    thinking_enabled: bool,
    thinking_mode: Option<String>,
    openai_compat_delegate: Option<OpenAiCompatProvider>,
}

impl ZenProvider {
    pub(crate) fn new(config: &Config) -> Self {
        let openai_compat_delegate = Self::create_openai_compat_delegate(config);

        Self {
            client: Client::new(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            thinking_enabled: true,
            thinking_mode: None,
            openai_compat_delegate,
        }
    }

    fn create_openai_compat_delegate(config: &Config) -> Option<OpenAiCompatProvider> {
        let mut provider_config = crate::config::OpenAiCompatProviderConfig {
            enabled: true,
            api_key: config.api_key.clone(),
            base_url: "https://opencode.ai/zen/v1".to_string(),
            models: Vec::new(),
            model_configs: Vec::new(),
        };

        provider_config
            .model_configs
            .push(crate::config::ModelConfig {
                name: "big-pickle".to_string(),
                reasoning_effort: None,
                thinking: Some(serde_json::json!({"type": "enabled"})),
                temperature: None,
                max_tokens: None,
                system_prompt: None,
                stop_sequences: None,
            });

        provider_config
            .model_configs
            .push(crate::config::ModelConfig {
                name: "glm-4.6".to_string(),
                reasoning_effort: None,
                thinking: Some(serde_json::json!({"type": "enabled"})),
                temperature: None,
                max_tokens: None,
                system_prompt: None,
                stop_sequences: None,
            });

        provider_config
            .model_configs
            .push(crate::config::ModelConfig {
                name: "grok-code".to_string(),
                reasoning_effort: None,
                thinking: None,
                temperature: None,
                max_tokens: None,
                system_prompt: None,
                stop_sequences: None,
            });

        Some(OpenAiCompatProvider::with_config(
            "zen",
            provider_config,
            usage::zen(),
        ))
    }

    pub(crate) fn set_thinking_enabled(&mut self, enabled: bool) {
        self.thinking_enabled = enabled;
    }

    pub(crate) fn set_thinking_mode(&mut self, mode: Option<String>) {
        self.thinking_mode = mode;
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.model = model.clone();

        if let Some(ref mut delegate) = self.openai_compat_delegate {
            delegate.set_model(model);
        }
    }

    /// Returns the available thinking modes for the given model.
    pub(crate) fn thinking_modes(model: &str) -> &'static [&'static str] {
        if model == "gemini-3-flash" {
            &["off", "minimal", "low", "medium", "high"]
        } else if model == "gemini-3-pro" {
            &["off", "low", "high"]
        } else {
            &["off", "on"]
        }
    }

    /// Returns the default thinking state for the given model.
    pub(crate) fn default_thinking_state(model: &str) -> crate::providers::ThinkingState {
        if matches!(model, "gemini-3-pro" | "gemini-3-flash") {
            crate::providers::ThinkingState::new(true, Some("low".to_string()))
        } else {
            crate::providers::ThinkingState::new(true, None)
        }
    }

    pub(crate) fn models() -> &'static [&'static str] {
        &ZEN_MODEL_NAMES
    }

    /// Get the context limit for a given model name
    pub(crate) fn context_limit(model: &str) -> Option<u64> {
        // GLM models
        if model.contains("glm") {
            return Some(200_000);
        }
        // Zen big-pickle
        if model.contains("big-pickle") {
            return Some(200_000);
        }
        // GPT-5 models hosted by Zen
        if model.contains("gpt-5") {
            return Some(400_000);
        }
        None
    }
}

impl Provider for ZenProvider {
    async fn chat(
        &self,
        messages: Vec<Message>,
        output: &crate::output::OutputContext,
    ) -> Result<ChatResponse> {
        let api_type = get_model_spec(&self.model)
            .map(|m| m.api_type)
            .unwrap_or(ApiType::OpenAiCompatible);

        let ctx = ChatContext {
            client: &self.client,
            api_key: &self.api_key,
            model: &self.model,
            thinking_enabled: self.thinking_enabled,
            thinking_mode: self.thinking_mode.as_deref(),
        };

        match api_type {
            ApiType::Anthropic => anthropic::chat(&ctx, messages, output).await,
            ApiType::Gemini => gemini::chat(&ctx, messages, output).await,
            ApiType::OpenAiCompatible => match &self.openai_compat_delegate {
                Some(delegate) => delegate.chat(messages, output).await,
                None => Err(Error::Auth("Zen provider not configured".to_string())),
            },
            ApiType::OpenAiResponses => responses::chat(&ctx, messages, output).await,
        }
    }

    async fn prepare_request(&self, messages: Vec<Message>) -> Result<serde_json::Value> {
        let api_type = get_model_spec(&self.model)
            .map(|m| m.api_type)
            .unwrap_or(ApiType::OpenAiCompatible);

        match api_type {
            ApiType::OpenAiCompatible => match &self.openai_compat_delegate {
                Some(delegate) => delegate.prepare_request(messages).await,
                None => Err(Error::Auth("Zen provider not configured".to_string())),
            },
            ApiType::OpenAiResponses => {
                let request =
                    responses::build_request(&self.model, &messages, self.thinking_enabled).await;
                responses::prepare_request_value(&request)
            }
            ApiType::Anthropic => {
                let request =
                    anthropic::build_request(&self.model, &messages, self.thinking_enabled).await;
                anthropic::prepare_request_value(&request)
            }
            ApiType::Gemini => {
                let request = gemini::build_request(
                    &self.model,
                    &messages,
                    self.thinking_enabled,
                    self.thinking_mode.as_deref(),
                )
                .await;
                gemini::prepare_request_value(&request)
            }
        }
    }

    fn start_turn(&self) {
        crate::usage::zen().start_turn();
    }
}
