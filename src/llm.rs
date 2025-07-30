// SPDX-License-Identifier: AGPL-3.0-only

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt;
use std::process::Command;

use crate::chat::{ChatMessage, MessageRole};
use crate::config::{AnthropicConfig, Config, GitHubCopilotConfig, OpenRouterConfig};

/// Helper to build requests with verbose logging
struct VerboseRequestBuilder {
    builder: RequestBuilder,
    headers: Vec<(String, String)>,
    endpoint: String,
    verbose: bool,
}

impl VerboseRequestBuilder {
    fn new(builder: RequestBuilder, endpoint: String, verbose: bool) -> Self {
        Self {
            builder,
            headers: Vec::new(),
            endpoint,
            verbose,
        }
    }

    fn header(mut self, key: &str, value: String) -> Self {
        // Mask sensitive values for logging
        let display_value = if key.to_lowercase() == "authorization" {
            if let Some(token) = value.strip_prefix("Bearer ") {
                if token.len() > 8 {
                    format!("Bearer {}...{}", &token[..4], &token[token.len() - 4..])
                } else {
                    "Bearer ***".to_string()
                }
            } else {
                "***".to_string()
            }
        } else {
            value.clone()
        };

        self.headers.push((key.to_string(), display_value));
        self.builder = self.builder.header(key, value);
        self
    }

    fn json<T: serde::Serialize>(mut self, json: &T) -> Self {
        self.builder = self.builder.json(json);
        self
    }

    async fn send(self) -> Result<reqwest::Response, reqwest::Error> {
        if self.verbose {
            eprintln!("🔍 Debug: Sending request to: {}", self.endpoint);
            eprintln!("🔍 Debug: Request headers:");
            for (key, value) in &self.headers {
                eprintln!("  {key}: {value}");
            }
        }
        self.builder.send().await
    }
}

#[derive(Debug, Clone)]
pub enum Provider {
    GitHubCopilot,
    OpenRouter,
    Anthropic,
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Provider::GitHubCopilot => write!(f, "GitHub Copilot"),
            Provider::OpenRouter => write!(f, "OpenRouter"),
            Provider::Anthropic => write!(f, "Anthropic"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub provider: Provider,
}

#[async_trait]
#[allow(clippy::upper_case_acronyms)]
pub trait LLM: Send + Sync {
    #[allow(dead_code)]
    async fn get_copilot_token(&mut self) -> Result<String>;
    #[allow(dead_code)]
    async fn send_raw_json_request(
        &mut self,
        json_str: &str,
        verbose: bool,
    ) -> Result<(String, Option<CopilotUsage>)>;
    async fn send_chat_request(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)>;
    async fn send_tool_results(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        tool_results: &[ToolExecutionResult],
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)>;
    #[allow(dead_code)]
    fn set_verbose(&mut self, verbose: bool);
}

pub enum ProviderClient {
    GitHubCopilot(LLMClient),
    OpenRouter(OpenRouterClient),
    Anthropic(AnthropicClient),
}

impl ProviderClient {
    pub fn set_verbose(&mut self, verbose: bool) {
        match self {
            ProviderClient::GitHubCopilot(client) => client.set_verbose(verbose),
            ProviderClient::OpenRouter(client) => client.set_verbose(verbose),
            ProviderClient::Anthropic(client) => client.set_verbose(verbose),
        }
    }
}

#[async_trait]
impl LLM for ProviderClient {
    async fn get_copilot_token(&mut self) -> Result<String> {
        match self {
            ProviderClient::GitHubCopilot(client) => client.get_copilot_token().await,
            ProviderClient::OpenRouter(client) => client.get_copilot_token().await,
            ProviderClient::Anthropic(client) => client.get_copilot_token().await,
        }
    }

    async fn send_raw_json_request(
        &mut self,
        json_str: &str,
        verbose: bool,
    ) -> Result<(String, Option<CopilotUsage>)> {
        match self {
            ProviderClient::GitHubCopilot(client) => {
                client.send_raw_json_request(json_str, verbose).await
            }
            ProviderClient::OpenRouter(client) => {
                client.send_raw_json_request(json_str, verbose).await
            }
            ProviderClient::Anthropic(client) => {
                client.send_raw_json_request(json_str, verbose).await
            }
        }
    }

    async fn send_chat_request(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        match self {
            ProviderClient::GitHubCopilot(client) => {
                client.send_chat_request(messages, verbose).await
            }
            ProviderClient::OpenRouter(client) => client.send_chat_request(messages, verbose).await,
            ProviderClient::Anthropic(client) => client.send_chat_request(messages, verbose).await,
        }
    }

    async fn send_tool_results(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        tool_results: &[ToolExecutionResult],
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        match self {
            ProviderClient::GitHubCopilot(client) => {
                client
                    .send_tool_results(messages, tool_results, verbose)
                    .await
            }
            ProviderClient::OpenRouter(client) => {
                client
                    .send_tool_results(messages, tool_results, verbose)
                    .await
            }
            ProviderClient::Anthropic(client) => {
                client
                    .send_tool_results(messages, tool_results, verbose)
                    .await
            }
        }
    }

    fn set_verbose(&mut self, verbose: bool) {
        self.set_verbose(verbose)
    }
}

/// Get the default/fallback list of models
fn get_default_models() -> Vec<ModelInfo> {
    vec![
        // GitHub Copilot models
        ModelInfo {
            id: "gpt-4o".to_string(),
            provider: Provider::GitHubCopilot,
        },
        ModelInfo {
            id: "gpt-4.1".to_string(),
            provider: Provider::GitHubCopilot,
        },
        ModelInfo {
            id: "claude-sonnet-4".to_string(),
            provider: Provider::GitHubCopilot,
        },
        ModelInfo {
            id: "gemini-2.0-flash-001".to_string(),
            provider: Provider::GitHubCopilot,
        },
        ModelInfo {
            id: "gemini-2.5-pro".to_string(),
            provider: Provider::GitHubCopilot,
        },
        // OpenRouter models
        ModelInfo {
            id: "anthropic/claude-sonnet-4".to_string(),
            provider: Provider::OpenRouter,
        },
        ModelInfo {
            id: "anthropic/claude-opus-4".to_string(),
            provider: Provider::OpenRouter,
        },
        // Anthropic models
        ModelInfo {
            id: "claude-sonnet-4-20250514".to_string(),
            provider: Provider::Anthropic,
        },
    ]
}

pub fn get_available_models(config: &Config) -> Vec<ModelInfo> {
    let mut models = Vec::new();
    let all_models = get_default_models();

    // Add GitHub Copilot models if configured
    if config.providers.github_copilot.is_some() {
        models.extend(
            all_models
                .iter()
                .filter(|m| matches!(m.provider, Provider::GitHubCopilot))
                .cloned(),
        );
    }

    // Add OpenRouter models if configured
    if config.providers.open_router.is_some() {
        models.extend(
            all_models
                .iter()
                .filter(|m| matches!(m.provider, Provider::OpenRouter))
                .cloned(),
        );
    }

    // Add Anthropic models if configured
    if config.providers.anthropic.is_some() {
        models.extend(
            all_models
                .iter()
                .filter(|m| matches!(m.provider, Provider::Anthropic))
                .cloned(),
        );
    }

    models
}

/// Determine the provider for a given model ID

#[derive(Debug, Serialize)]
struct CopilotChatRequest {
    messages: Vec<CopilotMessage>,
    model: String,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolDefinition {
    #[serde(rename = "type")]
    tool_type: String,
    function: FunctionDefinition,
}

#[derive(Debug, Serialize, Deserialize)]
struct FunctionDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct CopilotMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize)]
struct CopilotChatResponse {
    choices: Vec<CopilotChoice>,
    usage: Option<CopilotUsage>,
}

#[derive(Debug, Deserialize)]
pub struct CopilotUsage {
    #[allow(dead_code)]
    pub completion_tokens: u32,
    pub prompt_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct CopilotChoice {
    pub message: CopilotResponseMessage,
}

#[derive(Debug, Deserialize)]
pub struct CopilotResponseMessage {
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Serialize)]
pub struct ToolExecutionResult {
    pub tool_call_id: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: u64,
}

pub struct LLMClient {
    client: Client,
    config: GitHubCopilotConfig,
    verbose: bool,
}

impl LLMClient {
    pub fn new(config: GitHubCopilotConfig, verbose: bool) -> Self {
        Self {
            client: Client::new(),
            config,
            verbose,
        }
    }

    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    fn convert_messages_to_copilot_format(messages: &VecDeque<ChatMessage>) -> Vec<CopilotMessage> {
        messages
            .iter()
            .map(|msg| CopilotMessage {
                role: match msg.role {
                    MessageRole::User => "user".to_string(),
                    MessageRole::Assistant => "assistant".to_string(),
                    MessageRole::System => "system".to_string(),
                    MessageRole::Tool => "tool".to_string(),
                },
                content: match msg.role {
                    MessageRole::Tool if msg.tool_call_id.is_some() => Some(msg.content.as_text()),
                    MessageRole::Assistant if msg.tool_calls.is_some() => {
                        // Assistant messages with tool calls might have empty content
                        let text = msg.content.as_text();
                        if text.is_empty() { None } else { Some(text) }
                    }
                    _ => Some(msg.content.as_text()),
                },
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls: msg.tool_calls.clone(),
            })
            .collect()
    }

    pub async fn get_copilot_token(&mut self) -> Result<String> {
        // Check if we have a valid token with 5-minute buffer
        if let (Some(token), Some(expires_at)) =
            (&self.config.copilot_token, self.config.copilot_expires_at)
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            // Refresh token 5 minutes before expiration
            if now < expires_at.saturating_sub(300) {
                if self.verbose {
                    eprintln!("🔍 Debug: Using cached Copilot token");
                }
                return Ok(token.clone());
            }
        }

        if self.verbose {
            eprintln!("🔍 Debug: Exchanging GitHub token for Copilot token...");
        }

        let endpoint = "https://api.github.com/copilot_internal/v2/token";

        // Get new Copilot token
        let response = VerboseRequestBuilder::new(
            self.client.get(endpoint),
            endpoint.to_string(),
            self.verbose,
        )
        .header("Accept", "application/json".to_string())
        .header(
            "Authorization",
            format!("Bearer {}", self.config.access_token),
        )
        .header("Editor-Version", "vscode/1.99.3".to_string())
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7".to_string())
        .header("User-Agent", "GitHubCopilotChat/0.26.7".to_string())
        .header("X-GitHub-Api-Version", "2022-11-28".to_string())
        .send()
        .await
        .context("Failed to get Copilot token")?;

        if self.verbose {
            eprintln!(
                "🔍 Debug: Copilot token exchange status: {}",
                response.status()
            );
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if self.verbose {
                eprintln!("🔍 Debug: Copilot token exchange error: {body}");
            }
            anyhow::bail!("Copilot token request failed: {} - {}", status, body);
        }

        let copilot_response: CopilotTokenResponse = response
            .json()
            .await
            .context("Failed to parse Copilot token response")?;

        if self.verbose {
            eprintln!("🔍 Debug: Successfully obtained Copilot token");
        }

        // Update our config with the new token
        self.config.copilot_token = Some(copilot_response.token.clone());
        self.config.copilot_expires_at = Some(copilot_response.expires_at);

        // Save the updated config
        let mut config = Config::load()?;
        if let Some(ref mut github_config) = config.providers.github_copilot {
            github_config.copilot_token = Some(copilot_response.token.clone());
            github_config.copilot_expires_at = Some(copilot_response.expires_at);
            config.save()?;
        }

        Ok(copilot_response.token)
    }

    pub async fn send_raw_json_request(
        &mut self,
        json_str: &str,
        verbose: bool,
    ) -> Result<(String, Option<CopilotUsage>)> {
        // Parse the JSON string
        let request_value: serde_json::Value =
            serde_json::from_str(json_str).context("Invalid JSON format")?;

        // Get Copilot token (will exchange if needed)
        let copilot_token = self.get_copilot_token().await?;

        let endpoint = "https://api.githubcopilot.com/chat/completions";

        if verbose {
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request_value).unwrap_or_default()
            );
        }

        let response =
            VerboseRequestBuilder::new(self.client.post(endpoint), endpoint.to_string(), verbose)
                .header("Authorization", format!("Bearer {copilot_token}"))
                .header("User-Agent", "GitHubCopilotChat/1.0".to_string())
                .header("Content-Type", "application/json".to_string())
                .header("Accept", "application/json".to_string())
                .header("X-GitHub-Api-Version", "2023-07-07".to_string())
                .header("Editor-Version", "vscode/1.85.0".to_string())
                .header("Editor-Plugin-Version", "copilot-chat/0.11.1".to_string())
                .json(&request_value)
                .send()
                .await
                .context("Failed to send chat request")?;

        if verbose {
            eprintln!("🔍 Debug: Response status: {}", response.status());
            eprintln!("🔍 Debug: Response headers: {:?}", response.headers());
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if verbose {
                eprintln!("🔍 Debug: Error response body: {text}");
            }
            anyhow::bail!("API request failed with status {}: {}", status, text);
        }

        // Get the raw response text first for logging in verbose mode
        let response_text = response
            .text()
            .await
            .context("Failed to get response text")?;

        if verbose {
            // Parse and pretty print the JSON response
            if let Ok(parsed_json) = serde_json::from_str::<serde_json::Value>(&response_text) {
                if let Ok(pretty_json) = serde_json::to_string_pretty(&parsed_json) {
                    eprintln!("🔍 Debug: Raw response JSON:\n{pretty_json}");
                } else {
                    eprintln!("🔍 Debug: Raw response: {response_text}");
                }
            } else {
                eprintln!("🔍 Debug: Raw response: {response_text}");
            }
        }

        // Parse response
        let copilot_response: CopilotChatResponse =
            serde_json::from_str(&response_text).context("Failed to parse response JSON")?;

        if let Some(choice) = copilot_response.choices.first() {
            let content = choice.message.content.clone().unwrap_or_default();
            Ok((content, copilot_response.usage))
        } else {
            anyhow::bail!("No choices in API response")
        }
    }

    pub async fn send_chat_request(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        // Get Copilot token (will exchange if needed)
        let copilot_token = self.get_copilot_token().await?;

        let copilot_messages = Self::convert_messages_to_copilot_format(messages);

        // Get the selected model from config, defaulting to gpt-4o
        let config = Config::load()?;
        let selected_model = config
            .get_selected_model()
            .cloned()
            .unwrap_or_else(|| "gpt-4o".to_string());

        // Create shell tool definition
        let shell_tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "shell".to_string(),
                description: "Execute shell commands and return their output. Use this to run any shell command, script, or program. Returns both stdout and stderr.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
        };

        // Create read_lines tool definition
        let read_lines_tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "read_lines".to_string(),
                description: "Read lines from a file starting at a given offset. Use this to read file contents efficiently.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to read"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "The line number to start reading from (0-based). Default is 0.",
                            "default": 0
                        },
                        "limit": {
                            "type": "integer",
                            "description": "The number of lines to read. 0 means read the entire file. Default is 0.",
                            "default": 0
                        }
                    },
                    "required": ["file_path"]
                }),
            },
        };

        let request = CopilotChatRequest {
            messages: copilot_messages,
            model: selected_model,
            stream: false,
            tools: Some(vec![shell_tool, read_lines_tool]),
            tool_choice: None,
        };

        self.execute_request(request, &copilot_token, verbose).await
    }

    pub async fn send_tool_results(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        _tool_results: &[ToolExecutionResult],
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        // Get Copilot token (will exchange if needed)
        let copilot_token = self.get_copilot_token().await?;

        // Convert messages to Copilot format
        let copilot_messages = Self::convert_messages_to_copilot_format(messages);

        // Get the selected model from config, defaulting to gpt-4o
        let config = Config::load()?;
        let selected_model = config
            .get_selected_model()
            .cloned()
            .unwrap_or_else(|| "gpt-4o".to_string());

        // Create shell tool definition
        let shell_tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "shell".to_string(),
                description: "Execute shell commands and return their output. Use this to run any shell command, script, or program. Returns both stdout and stderr.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
        };

        // Create read_lines tool definition
        let read_lines_tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "read_lines".to_string(),
                description: "Read lines from a file starting at a given offset. Use this to read file contents efficiently.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to read"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "The line number to start reading from (0-based). Default is 0.",
                            "default": 0
                        },
                        "limit": {
                            "type": "integer",
                            "description": "The number of lines to read. 0 means read the entire file. Default is 0.",
                            "default": 0
                        }
                    },
                    "required": ["file_path"]
                }),
            },
        };

        let request = CopilotChatRequest {
            messages: copilot_messages,
            model: selected_model,
            stream: false,
            tools: Some(vec![shell_tool, read_lines_tool]),
            tool_choice: None,
        };

        self.execute_request(request, &copilot_token, verbose).await
    }

    async fn execute_request(
        &self,
        request: CopilotChatRequest,
        copilot_token: &str,
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        let endpoint = "https://api.githubcopilot.com/chat/completions";

        if verbose {
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request).unwrap_or_default()
            );
        }

        let response =
            VerboseRequestBuilder::new(self.client.post(endpoint), endpoint.to_string(), verbose)
                .header("Authorization", format!("Bearer {copilot_token}"))
                .header("User-Agent", "GitHubCopilotChat/1.0".to_string())
                .header("Content-Type", "application/json".to_string())
                .header("Accept", "application/json".to_string())
                .header("X-GitHub-Api-Version", "2023-07-07".to_string())
                .header("Editor-Version", "vscode/1.85.0".to_string())
                .header("Editor-Plugin-Version", "copilot-chat/0.11.1".to_string())
                .json(&request)
                .send()
                .await
                .context("Failed to send chat request")?;

        if verbose {
            eprintln!("🔍 Debug: Response status: {}", response.status());
            eprintln!("🔍 Debug: Response headers: {:?}", response.headers());
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if verbose {
                eprintln!("🔍 Debug: Error response body: {text}");
            }
            anyhow::bail!("API request failed with status {}: {}", status, text);
        }

        // Get the raw response text first for logging in verbose mode
        let response_text = response
            .text()
            .await
            .context("Failed to get response text")?;

        if verbose {
            // Parse and pretty print the JSON response
            if let Ok(parsed_json) = serde_json::from_str::<serde_json::Value>(&response_text) {
                if let Ok(pretty_json) = serde_json::to_string_pretty(&parsed_json) {
                    eprintln!("🔍 Debug: Raw response JSON:\n{pretty_json}");
                } else {
                    eprintln!("🔍 Debug: Raw response: {response_text}");
                }
            } else {
                eprintln!("🔍 Debug: Raw response: {response_text}");
            }
        }

        let chat_response: CopilotChatResponse = match serde_json::from_str(&response_text) {
            Ok(resp) => resp,
            Err(e) => {
                if verbose {
                    eprintln!("🔍 Debug: Failed to parse response: {e}");
                    eprintln!("🔍 Debug: Raw response was: {response_text}");
                }
                anyhow::bail!("Failed to parse chat response: {}", e);
            }
        };

        if chat_response.choices.is_empty() {
            if verbose {
                eprintln!("🔍 Debug: API returned empty choices array");
                eprintln!("🔍 Debug: Full response: {chat_response:?}");
            }
            anyhow::bail!("No response choices received from API");
        }

        // Return all choices and usage
        Ok((chat_response.choices, chat_response.usage))
    }
}

pub struct OpenRouterClient {
    client: Client,
    config: OpenRouterConfig,
    verbose: bool,
}

impl OpenRouterClient {
    pub fn new(config: OpenRouterConfig, verbose: bool) -> Self {
        Self {
            client: Client::new(),
            config,
            verbose,
        }
    }

    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    fn convert_messages_to_copilot_format(messages: &VecDeque<ChatMessage>) -> Vec<CopilotMessage> {
        messages
            .iter()
            .map(|msg| CopilotMessage {
                role: match msg.role {
                    MessageRole::User => "user".to_string(),
                    MessageRole::Assistant => "assistant".to_string(),
                    MessageRole::System => "system".to_string(),
                    MessageRole::Tool => "tool".to_string(),
                },
                content: match msg.role {
                    MessageRole::Tool if msg.tool_call_id.is_some() => Some(msg.content.as_text()),
                    MessageRole::Assistant if msg.tool_calls.is_some() => {
                        // Assistant messages with tool calls might have empty content
                        let text = msg.content.as_text();
                        if text.is_empty() { None } else { Some(text) }
                    }
                    _ => Some(msg.content.as_text()),
                },
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls: msg.tool_calls.clone(),
            })
            .collect()
    }

    pub async fn get_copilot_token(&mut self) -> Result<String> {
        // OpenRouter uses API key directly, no token exchange needed
        Ok(self.config.api_key.clone())
    }

    pub async fn send_raw_json_request(
        &mut self,
        json_str: &str,
        verbose: bool,
    ) -> Result<(String, Option<CopilotUsage>)> {
        // Parse the JSON string
        let request_value: serde_json::Value =
            serde_json::from_str(json_str).context("Invalid JSON format")?;

        let endpoint = "https://openrouter.ai/api/v1/chat/completions";

        if verbose {
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request_value).unwrap_or_default()
            );
        }

        let response =
            VerboseRequestBuilder::new(self.client.post(endpoint), endpoint.to_string(), verbose)
                .header("Authorization", format!("Bearer {}", self.config.api_key))
                .header("Content-Type", "application/json".to_string())
                .header("Accept", "application/json".to_string())
                .json(&request_value)
                .send()
                .await
                .context("Failed to send chat request")?;

        if verbose {
            eprintln!("🔍 Debug: Response status: {}", response.status());
            eprintln!("🔍 Debug: Response headers: {:?}", response.headers());
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if verbose {
                eprintln!("🔍 Debug: Error response body: {text}");
            }
            anyhow::bail!("API request failed with status {}: {}", status, text);
        }

        // Get the raw response text first for logging in verbose mode
        let response_text = response
            .text()
            .await
            .context("Failed to get response text")?;

        if verbose {
            // Parse and pretty print the JSON response
            if let Ok(parsed_json) = serde_json::from_str::<serde_json::Value>(&response_text) {
                if let Ok(pretty_json) = serde_json::to_string_pretty(&parsed_json) {
                    eprintln!("🔍 Debug: Raw response JSON:\n{pretty_json}");
                } else {
                    eprintln!("🔍 Debug: Raw response: {response_text}");
                }
            } else {
                eprintln!("🔍 Debug: Raw response: {response_text}");
            }
        }

        // Parse response
        let copilot_response: CopilotChatResponse =
            serde_json::from_str(&response_text).context("Failed to parse response JSON")?;

        if let Some(choice) = copilot_response.choices.first() {
            let content = choice.message.content.clone().unwrap_or_default();
            Ok((content, copilot_response.usage))
        } else {
            anyhow::bail!("No choices in API response")
        }
    }

    pub async fn send_chat_request(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        let copilot_messages = Self::convert_messages_to_copilot_format(messages);

        // Get the selected model from config
        let config = Config::load()?;
        let selected_model = config
            .get_selected_model()
            .cloned()
            .unwrap_or_else(|| "anthropic/claude-sonnet-4".to_string());

        // Create shell tool definition
        let shell_tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "shell".to_string(),
                description: "Execute a shell command and return the output".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
        };

        // Create read_lines tool definition
        let read_lines_tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "read_lines".to_string(),
                description: "Read lines from a file starting at a given offset. Use this to read file contents efficiently.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to read"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "The line number to start reading from (0-based). Default is 0.",
                            "default": 0
                        },
                        "limit": {
                            "type": "integer",
                            "description": "The number of lines to read. 0 means read the entire file. Default is 0.",
                            "default": 0
                        }
                    },
                    "required": ["file_path"]
                }),
            },
        };

        let request = CopilotChatRequest {
            messages: copilot_messages,
            model: selected_model.clone(),
            stream: false,
            tools: Some(vec![shell_tool, read_lines_tool]),
            tool_choice: Some("auto".to_string()),
        };

        if verbose {
            eprintln!("🔍 Debug: Using model: {selected_model}");
            eprintln!("🔍 Debug: Tools enabled: shell, read_lines");
        }

        self.execute_request(request, verbose).await
    }

    pub async fn send_tool_results(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        _tool_results: &[ToolExecutionResult],
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        // Convert messages to the format expected by OpenRouter
        let copilot_messages = Self::convert_messages_to_copilot_format(messages);

        // Get the selected model from config
        let config = Config::load()?;
        let selected_model = config
            .get_selected_model()
            .cloned()
            .unwrap_or_else(|| "anthropic/claude-sonnet-4".to_string());

        // Create shell tool definition
        let shell_tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "shell".to_string(),
                description: "Execute a shell command and return the output".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to execute"
                        }
                    },
                    "required": ["command"]
                }),
            },
        };

        // Create read_lines tool definition
        let read_lines_tool = ToolDefinition {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: "read_lines".to_string(),
                description: "Read lines from a file starting at a given offset. Use this to read file contents efficiently.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "The path to the file to read"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "The line number to start reading from (0-based). Default is 0.",
                            "default": 0
                        },
                        "limit": {
                            "type": "integer",
                            "description": "The number of lines to read. 0 means read the entire file. Default is 0.",
                            "default": 0
                        }
                    },
                    "required": ["file_path"]
                }),
            },
        };

        let request = CopilotChatRequest {
            messages: copilot_messages,
            model: selected_model.clone(),
            stream: false,
            tools: Some(vec![shell_tool, read_lines_tool]),
            tool_choice: None,
        };

        if verbose {
            eprintln!("🔍 Debug: Using model: {selected_model}");
            eprintln!("🔍 Debug: Tools enabled: shell, read_lines");
        }

        self.execute_request(request, verbose).await
    }

    async fn execute_request(
        &self,
        request: CopilotChatRequest,
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        let endpoint = "https://openrouter.ai/api/v1/chat/completions";

        if verbose {
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request).unwrap_or_default()
            );
        }

        let response =
            VerboseRequestBuilder::new(self.client.post(endpoint), endpoint.to_string(), verbose)
                .header("Authorization", format!("Bearer {}", self.config.api_key))
                .header("Content-Type", "application/json".to_string())
                .header("Accept", "application/json".to_string())
                .json(&request)
                .send()
                .await
                .context("Failed to send chat request")?;

        if verbose {
            eprintln!("🔍 Debug: Response status: {}", response.status());
            eprintln!("🔍 Debug: Response headers: {:?}", response.headers());
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if verbose {
                eprintln!("🔍 Debug: Error response body: {text}");
            }
            anyhow::bail!("API request failed with status {}: {}", status, text);
        }

        // Get the raw response text first for logging in verbose mode
        let response_text = response
            .text()
            .await
            .context("Failed to get response text")?;

        if verbose {
            // Parse and pretty print the JSON response
            if let Ok(parsed_json) = serde_json::from_str::<serde_json::Value>(&response_text) {
                if let Ok(pretty_json) = serde_json::to_string_pretty(&parsed_json) {
                    eprintln!("🔍 Debug: Raw response JSON:\n{pretty_json}");
                } else {
                    eprintln!("🔍 Debug: Raw response: {response_text}");
                }
            } else {
                eprintln!("🔍 Debug: Raw response: {response_text}");
            }
        }

        let chat_response: CopilotChatResponse = match serde_json::from_str(&response_text) {
            Ok(resp) => resp,
            Err(e) => {
                if verbose {
                    eprintln!("🔍 Debug: Failed to parse response: {e}");
                    eprintln!("🔍 Debug: Raw response was: {response_text}");
                }
                anyhow::bail!("Failed to parse chat response: {}", e);
            }
        };

        if chat_response.choices.is_empty() {
            if verbose {
                eprintln!("🔍 Debug: API returned empty choices array");
                eprintln!("🔍 Debug: Full response: {chat_response:?}");
            }
            anyhow::bail!("No response from API - empty choices array");
        }

        Ok((chat_response.choices, chat_response.usage))
    }
}

pub struct AnthropicClient {
    client: Client,
    config: AnthropicConfig,
    verbose: bool,
}

impl AnthropicClient {
    pub fn new(config: AnthropicConfig, verbose: bool) -> Self {
        Self {
            client: Client::new(),
            config,
            verbose,
        }
    }

    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    fn convert_messages_to_copilot_format(messages: &VecDeque<ChatMessage>) -> Vec<CopilotMessage> {
        messages
            .iter()
            .map(|msg| CopilotMessage {
                role: match msg.role {
                    MessageRole::User => "user".to_string(),
                    MessageRole::Assistant => "assistant".to_string(),
                    MessageRole::System => "system".to_string(),
                    MessageRole::Tool => "tool".to_string(),
                },
                content: match msg.role {
                    MessageRole::Tool if msg.tool_call_id.is_some() => Some(msg.content.as_text()),
                    MessageRole::Assistant if msg.tool_calls.is_some() => {
                        let text = msg.content.as_text();
                        if text.is_empty() { None } else { Some(text) }
                    }
                    _ => Some(msg.content.as_text()),
                },
                tool_call_id: msg.tool_call_id.clone(),
                tool_calls: msg.tool_calls.clone(),
            })
            .collect()
    }

    async fn get_access_token(&mut self) -> Result<String> {
        // Check if token needs refresh first
        let needs_refresh = match &self.config {
            AnthropicConfig::OAuth { expires, .. } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;
                now >= expires.saturating_sub(300_000)
            }
            AnthropicConfig::ApiKey { .. } => false,
        };

        if needs_refresh {
            if self.verbose {
                eprintln!("🔍 Debug: Access token expired, refreshing...");
            }
            self.refresh_token().await?;
        }

        match &self.config {
            AnthropicConfig::OAuth { access, .. } => Ok(access.clone()),
            AnthropicConfig::ApiKey { key } => Ok(key.clone()),
        }
    }

    async fn refresh_token(&mut self) -> Result<()> {
        if let AnthropicConfig::OAuth {
            refresh,
            access,
            expires,
        } = &mut self.config
        {
            let refresh_request = serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh,
                "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
            });

            let response = self
                .client
                .post("https://console.anthropic.com/v1/oauth/token")
                .header("Content-Type", "application/json")
                .json(&refresh_request)
                .send()
                .await
                .context("Failed to refresh token")?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                anyhow::bail!("Token refresh failed: {} - {}", status, body);
            }

            let token_response: serde_json::Value = response
                .json()
                .await
                .context("Failed to parse token refresh response")?;

            if let (Some(new_access), Some(expires_in)) = (
                token_response["access_token"].as_str(),
                token_response["expires_in"].as_u64(),
            ) {
                *access = new_access.to_string();
                *expires = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64
                    + (expires_in * 1000);

                // Update refresh token if provided
                if let Some(new_refresh) = token_response["refresh_token"].as_str() {
                    *refresh = new_refresh.to_string();
                }

                // Save updated config
                let mut config = Config::load()?;
                config.providers.anthropic = Some(self.config.clone());
                config.save()?;

                if self.verbose {
                    eprintln!("🔍 Debug: Successfully refreshed Anthropic token");
                }
            } else {
                anyhow::bail!("Invalid token refresh response");
            }
        }
        Ok(())
    }

    pub async fn get_copilot_token(&mut self) -> Result<String> {
        self.get_access_token().await
    }

    pub async fn send_raw_json_request(
        &mut self,
        json_str: &str,
        verbose: bool,
    ) -> Result<(String, Option<CopilotUsage>)> {
        let request_value: serde_json::Value =
            serde_json::from_str(json_str).context("Invalid JSON format")?;

        let access_token = self.get_access_token().await?;
        let endpoint = "https://api.anthropic.com/v1/messages";

        if verbose {
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request_value).unwrap_or_default()
            );
        }

        let mut builder =
            VerboseRequestBuilder::new(self.client.post(endpoint), endpoint.to_string(), verbose);

        // Add appropriate headers based on auth type
        match &self.config {
            AnthropicConfig::OAuth { .. } => {
                builder = builder
                    .header("Authorization", format!("Bearer {access_token}"))
                    .header("anthropic-beta", "oauth-2025-04-20".to_string());
            }
            AnthropicConfig::ApiKey { .. } => {
                builder = builder.header("x-api-key", access_token);
            }
        }

        let response = builder
            .header("Content-Type", "application/json".to_string())
            .header("anthropic-version", "2023-06-01".to_string())
            .json(&request_value)
            .send()
            .await
            .context("Failed to send request")?;

        if verbose {
            eprintln!("🔍 Debug: Response status: {}", response.status());
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if verbose {
                eprintln!("🔍 Debug: Error response body: {text}");
            }
            anyhow::bail!("API request failed with status {}: {}", status, text);
        }

        let response_text = response
            .text()
            .await
            .context("Failed to get response text")?;

        if verbose {
            if let Ok(parsed_json) = serde_json::from_str::<serde_json::Value>(&response_text) {
                if let Ok(pretty_json) = serde_json::to_string_pretty(&parsed_json) {
                    eprintln!("🔍 Debug: Raw response JSON:\n{pretty_json}");
                }
            }
        }

        // Parse Anthropic response and convert to expected format
        let anthropic_response: serde_json::Value =
            serde_json::from_str(&response_text).context("Failed to parse response JSON")?;

        if let Some(content) = anthropic_response["content"].as_array() {
            if let Some(text_content) = content.first() {
                if let Some(text) = text_content["text"].as_str() {
                    let usage = anthropic_response["usage"]
                        .as_object()
                        .map(|u| CopilotUsage {
                            prompt_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
                            completion_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
                            total_tokens: (u["input_tokens"].as_u64().unwrap_or(0)
                                + u["output_tokens"].as_u64().unwrap_or(0))
                                as u32,
                        });
                    return Ok((text.to_string(), usage));
                }
            }
        }

        anyhow::bail!("No content in API response")
    }

    pub async fn send_chat_request(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        let access_token = self.get_access_token().await?;
        let copilot_messages = Self::convert_messages_to_copilot_format(messages);

        let config = Config::load()?;
        let selected_model = config
            .get_selected_model()
            .cloned()
            .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string());

        // Convert to Anthropic format
        let mut anthropic_messages = Vec::new();
        let mut system_message = None;

        for msg in &copilot_messages {
            match msg.role.as_str() {
                "system" => {
                    if let Some(content) = &msg.content {
                        system_message = Some(content.clone());
                    }
                }
                "user" | "assistant" => {
                    let mut message = serde_json::json!({
                        "role": msg.role,
                    });

                    // Handle assistant messages with tool calls
                    if msg.role == "assistant" && msg.tool_calls.is_some() {
                        let mut content = Vec::new();

                        // Add text content if present
                        if let Some(text) = &msg.content {
                            if !text.is_empty() {
                                content.push(serde_json::json!({
                                    "type": "text",
                                    "text": text
                                }));
                            }
                        }

                        // Add tool use blocks
                        if let Some(tool_calls) = &msg.tool_calls {
                            for tool_call in tool_calls {
                                content.push(serde_json::json!({
                                    "type": "tool_use",
                                    "id": tool_call.id,
                                    "name": tool_call.function.name,
                                    "input": serde_json::from_str::<serde_json::Value>(&tool_call.function.arguments).unwrap_or_default()
                                }));
                            }
                        }

                        message["content"] = serde_json::json!(content);
                    } else {
                        // Regular text content
                        message["content"] =
                            serde_json::json!(msg.content.as_ref().unwrap_or(&String::new()));
                    }

                    anthropic_messages.push(message);
                }
                "tool" => {
                    // Convert tool results to Anthropic format
                    if let (Some(content), Some(tool_call_id)) = (&msg.content, &msg.tool_call_id) {
                        anthropic_messages.push(serde_json::json!({
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": tool_call_id,
                                "content": content
                            }]
                        }));
                    }
                }
                _ => {} // Skip other roles
            }
        }

        let mut request = serde_json::json!({
            "model": selected_model,
            "max_tokens": 4096,
            "messages": anthropic_messages
        });

        if let Some(system) = system_message {
            request["system"] = serde_json::json!([
                {
                    "type": "text",
                    "text": "You are Claude Code, Anthropic's official CLI for Claude.",
                },
                {
                    "type": "text",
                    "text": system,
                }
            ]);
        }

        // Add tools to the request
        let shell_tool = serde_json::json!({
            "name": "shell",
            "description": "Execute shell commands and return their output. Use this to run any shell command, script, or program. Returns both stdout and stderr.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    }
                },
                "required": ["command"]
            }
        });

        let read_lines_tool = serde_json::json!({
            "name": "read_lines",
            "description": "Read lines from a file starting at a given offset. Use this to read file contents efficiently.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "The path to the file to read"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "The line number to start reading from (0-based). Default is 0.",
                        "default": 0
                    },
                    "limit": {
                        "type": "integer",
                        "description": "The number of lines to read. 0 means read the entire file. Default is 0.",
                        "default": 0
                    }
                },
                "required": ["file_path"]
            }
        });

        request["tools"] = serde_json::json!([shell_tool, read_lines_tool]);
        request["tool_choice"] = serde_json::json!({"type": "auto"});

        self.execute_request(request, &access_token, verbose).await
    }

    pub async fn send_tool_results(
        &mut self,
        messages: &VecDeque<ChatMessage>,
        _tool_results: &[ToolExecutionResult],
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        // Tool results are already added to messages as tool role messages
        // so we can just send a regular chat request
        self.send_chat_request(messages, verbose).await
    }

    async fn execute_request(
        &self,
        request: serde_json::Value,
        access_token: &str,
        verbose: bool,
    ) -> Result<(Vec<CopilotChoice>, Option<CopilotUsage>)> {
        let endpoint = "https://api.anthropic.com/v1/messages";

        if verbose {
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request).unwrap_or_default()
            );
        }

        let mut builder =
            VerboseRequestBuilder::new(self.client.post(endpoint), endpoint.to_string(), verbose);

        // Add appropriate headers based on auth type
        match &self.config {
            AnthropicConfig::OAuth { .. } => {
                builder = builder
                    .header("Authorization", format!("Bearer {access_token}"))
                    .header("anthropic-beta", "oauth-2025-04-20".to_string());
            }
            AnthropicConfig::ApiKey { .. } => {
                builder = builder.header("x-api-key", access_token.to_string());
            }
        }

        let response = builder
            .header("Content-Type", "application/json".to_string())
            .header("anthropic-version", "2023-06-01".to_string())
            .json(&request)
            .send()
            .await
            .context("Failed to send request")?;

        if verbose {
            eprintln!("🔍 Debug: Response status: {}", response.status());
        }

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if verbose {
                eprintln!("🔍 Debug: Error response body: {text}");
            }
            anyhow::bail!("API request failed with status {}: {}", status, text);
        }

        let response_text = response
            .text()
            .await
            .context("Failed to get response text")?;

        if verbose {
            if let Ok(parsed_json) = serde_json::from_str::<serde_json::Value>(&response_text) {
                if let Ok(pretty_json) = serde_json::to_string_pretty(&parsed_json) {
                    eprintln!("🔍 Debug: Raw response JSON:\n{pretty_json}");
                }
            }
        }

        // Parse Anthropic response and convert to expected format
        let anthropic_response: serde_json::Value =
            serde_json::from_str(&response_text).context("Failed to parse response JSON")?;

        let usage = anthropic_response["usage"]
            .as_object()
            .map(|u| CopilotUsage {
                prompt_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: (u["input_tokens"].as_u64().unwrap_or(0)
                    + u["output_tokens"].as_u64().unwrap_or(0))
                    as u32,
            });

        if let Some(content) = anthropic_response["content"].as_array() {
            // Process content blocks to extract text and tool calls
            let mut text_content = String::new();
            let mut tool_calls = Vec::new();

            for block in content {
                if let Some(block_type) = block["type"].as_str() {
                    match block_type {
                        "text" => {
                            if let Some(text) = block["text"].as_str() {
                                text_content.push_str(text);
                            }
                        }
                        "tool_use" => {
                            if let (Some(id), Some(name)) =
                                (block["id"].as_str(), block["name"].as_str())
                            {
                                let input = &block["input"];
                                tool_calls.push(ToolCall {
                                    id: id.to_string(),
                                    call_type: "function".to_string(),
                                    function: FunctionCall {
                                        name: name.to_string(),
                                        arguments: serde_json::to_string(input).unwrap_or_default(),
                                    },
                                });
                            }
                        }
                        _ => {} // Ignore other block types
                    }
                }
            }

            let choice = CopilotChoice {
                message: CopilotResponseMessage {
                    content: if text_content.is_empty() {
                        None
                    } else {
                        Some(text_content)
                    },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                },
            };

            return Ok((vec![choice], usage));
        }

        anyhow::bail!("No content in API response")
    }
}

pub fn execute_shell_command(command: &str) -> Result<(String, String, i32)> {
    let output = if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", command])
            .output()
            .context("Failed to execute command")?
    } else {
        Command::new("sh")
            .args(["-c", command])
            .output()
            .context("Failed to execute command")?
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    // Normalize the output for consistent error handling
    if exit_code != 0 && !stderr.is_empty() {
        // For failed commands with stderr, combine stdout and stderr for better context
        let combined = if stdout.is_empty() {
            format!("Command failed with exit code {exit_code}:\n{stderr}")
        } else {
            format!("{stdout}\n\nError output:\n{stderr}")
        };
        Ok((combined, String::new(), exit_code))
    } else if exit_code != 0 && stdout.is_empty() && stderr.is_empty() {
        // Command failed with no output
        Ok((
            format!("Command failed with exit code {exit_code}"),
            String::new(),
            exit_code,
        ))
    } else {
        // Success case or has stdout - keep original behavior
        Ok((stdout, stderr, exit_code))
    }
}

pub fn execute_read_lines(
    file_path: &str,
    offset: usize,
    limit: usize,
) -> Result<(String, String, i32)> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    match File::open(file_path) {
        Ok(file) => {
            let reader = BufReader::new(file);
            let mut lines = reader.lines();

            // Skip to the offset
            for _ in 0..offset {
                if lines.next().is_none() {
                    return Ok((
                        format!("Error: Offset {offset} exceeds file length"),
                        String::new(),
                        1,
                    ));
                }
            }

            // Read the requested lines
            let mut result = Vec::new();
            let lines_to_read = if limit == 0 { usize::MAX } else { limit };

            for (i, line) in lines.enumerate() {
                if i >= lines_to_read {
                    break;
                }

                match line {
                    Ok(content) => result.push(content),
                    Err(e) => {
                        // Include partial results with error message
                        if !result.is_empty() {
                            result.push(format!(
                                "\nError: Failed to read line {}: {}",
                                offset + i,
                                e
                            ));
                        } else {
                            return Ok((
                                format!("Error: Failed to read line {}: {}", offset + i, e),
                                String::new(),
                                1,
                            ));
                        }
                        return Ok((result.join("\n"), String::new(), 1));
                    }
                }
            }

            Ok((result.join("\n"), String::new(), 0))
        }
        Err(e) => Ok((
            format!("Error: Failed to open file '{file_path}': {e}"),
            String::new(),
            1,
        )),
    }
}

pub async fn execute_tool_calls(
    tool_calls: &[ToolCall],
    verbose: bool,
) -> Result<Vec<ToolExecutionResult>> {
    let mut results = Vec::new();

    for tool_call in tool_calls {
        if verbose {
            eprintln!(
                "🔧 Executing tool: {} (id: {})",
                tool_call.function.name, tool_call.id
            );
        }

        match tool_call.function.name.as_str() {
            "shell" => {
                // Parse the arguments to get the command
                let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
                    .context("Failed to parse tool arguments")?;

                if let Some(command) = args.get("command").and_then(|v| v.as_str()) {
                    // Always display the command being executed
                    eprintln!("🐚 Executing: {command}");

                    if verbose {
                        eprintln!("🔍 Running command: {command}");
                    }

                    match execute_shell_command(command) {
                        Ok((stdout, stderr, exit_code)) => {
                            if verbose {
                                if exit_code == 0 {
                                    eprintln!("✅ Command completed successfully");
                                } else {
                                    eprintln!("⚠️  Command failed with exit code: {exit_code}");
                                }
                                if !stdout.is_empty() {
                                    eprintln!("📤 output: {stdout}");
                                }
                                if !stderr.is_empty() {
                                    eprintln!("📤 stderr: {stderr}");
                                }
                            }

                            results.push(ToolExecutionResult {
                                tool_call_id: tool_call.id.clone(),
                                stdout,
                                stderr,
                                exit_code,
                            });
                        }
                        Err(e) => {
                            if verbose {
                                eprintln!("❌ Command execution failed: {e}");
                            }
                            results.push(ToolExecutionResult {
                                tool_call_id: tool_call.id.clone(),
                                stdout: format!("Error: Failed to execute command: {e}"),
                                stderr: String::new(),
                                exit_code: -1,
                            });
                        }
                    }
                } else {
                    results.push(ToolExecutionResult {
                        tool_call_id: tool_call.id.clone(),
                        stdout: "Error: Missing 'command' parameter".to_string(),
                        stderr: String::new(),
                        exit_code: -1,
                    });
                }
            }
            "read_lines" => {
                // Parse the arguments to get file_path, offset, and limit
                let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
                    .context("Failed to parse tool arguments")?;

                let file_path = args
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'file_path' parameter"))?;

                let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                if verbose {
                    eprintln!("📖 Reading file: {file_path} (offset: {offset}, limit: {limit})");
                }

                match execute_read_lines(file_path, offset, limit) {
                    Ok((stdout, stderr, exit_code)) => {
                        if verbose {
                            if exit_code == 0 {
                                eprintln!("✅ File read completed successfully");
                                if !stdout.is_empty() {
                                    eprintln!("📤 content: {} lines", stdout.lines().count());
                                }
                            } else {
                                eprintln!("⚠️  File read failed with exit code: {exit_code}");
                                eprintln!("📤 output: {stdout}");
                            }
                        }

                        results.push(ToolExecutionResult {
                            tool_call_id: tool_call.id.clone(),
                            stdout,
                            stderr,
                            exit_code,
                        });
                    }
                    Err(e) => {
                        if verbose {
                            eprintln!("❌ File read operation failed: {e}");
                        }
                        results.push(ToolExecutionResult {
                            tool_call_id: tool_call.id.clone(),
                            stdout: format!("Error: Failed to read file: {e}"),
                            stderr: String::new(),
                            exit_code: -1,
                        });
                    }
                }
            }
            _ => {
                if verbose {
                    eprintln!("⚠️  Unknown tool: {}", tool_call.function.name);
                }
                results.push(ToolExecutionResult {
                    tool_call_id: tool_call.id.clone(),
                    stdout: format!("Error: Unknown tool: {}", tool_call.function.name),
                    stderr: String::new(),
                    exit_code: -1,
                });
            }
        }
    }

    Ok(results)
}

pub fn create_llm_client(verbose: bool) -> Result<Option<ProviderClient>> {
    let config = Config::load()?;

    if verbose {
        eprintln!("🔍 Debug: Config loaded, checking providers...");
    }

    // Get selected model to determine which provider to use
    let selected_model = config.get_selected_model().cloned();

    if verbose {
        eprintln!("🔍 Debug: Selected model: {selected_model:?}");
    }

    // Determine which provider to use based on the selected model
    if let Some(model_id) = &selected_model {
        // Get available models to find the provider for the selected model
        let models = get_available_models(&config);
        if let Some(model_info) = models.iter().find(|m| &m.id == model_id) {
            match &model_info.provider {
                Provider::OpenRouter => {
                    if let Some(openrouter_config) = config.providers.open_router {
                        if verbose {
                            eprintln!("🔍 Debug: Using OpenRouter for model: {model_id}");
                        }
                        return Ok(Some(ProviderClient::OpenRouter(OpenRouterClient::new(
                            openrouter_config,
                            verbose,
                        ))));
                    } else {
                        if verbose {
                            eprintln!(
                                "🔍 Debug: Model {model_id} requires OpenRouter but no OpenRouter config found"
                            );
                        }
                        anyhow::bail!(
                            "Selected model '{}' requires OpenRouter authentication. Please run 'henri login openrouter' to configure.",
                            model_id
                        );
                    }
                }
                Provider::Anthropic => {
                    if let Some(anthropic_config) = config.providers.anthropic {
                        if verbose {
                            eprintln!("🔍 Debug: Using Anthropic for model: {model_id}");
                        }
                        return Ok(Some(ProviderClient::Anthropic(AnthropicClient::new(
                            anthropic_config,
                            verbose,
                        ))));
                    } else {
                        if verbose {
                            eprintln!(
                                "🔍 Debug: Model {model_id} requires Anthropic but no Anthropic config found"
                            );
                        }
                        anyhow::bail!(
                            "Selected model '{}' requires Anthropic authentication. Please run 'henri login anthropic' to configure.",
                            model_id
                        );
                    }
                }
                Provider::GitHubCopilot => {
                    // Fall through to the default GitHub Copilot logic below
                }
            }
        }
    }

    // Default to GitHub Copilot for other models
    if let Some(github_config) = config.providers.github_copilot {
        if verbose {
            let token_preview = if github_config.access_token.len() > 8 {
                format!(
                    "{}...{}",
                    &github_config.access_token[..4],
                    &github_config.access_token[github_config.access_token.len() - 4..]
                )
            } else {
                "***".to_string()
            };

            eprintln!("🔍 Debug: Found GitHub Copilot config with token: {token_preview}");
        }

        // Check if token is still valid (if we have expiry info)
        if let Some(expires_at) = github_config.expires_at {
            let current_time = chrono::Utc::now().timestamp();
            if verbose {
                eprintln!("🔍 Debug: Token expires at: {expires_at}, current time: {current_time}");
            }
            if current_time >= expires_at {
                anyhow::bail!(
                    "GitHub Copilot token has expired. Please run 'henri login' to re-authenticate."
                );
            }
        } else if verbose {
            eprintln!("🔍 Debug: No expiry time set for token");
        }

        Ok(Some(ProviderClient::GitHubCopilot(LLMClient::new(
            github_config,
            verbose,
        ))))
    } else {
        if verbose {
            eprintln!("🔍 Debug: No GitHub Copilot config found");
        }
        Ok(None)
    }
}
