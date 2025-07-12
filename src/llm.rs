// SPDX-License-Identifier: AGPL-3.0-only

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::process::Command;

use crate::chat::{ChatMessage, MessageRole};
use crate::config::{Config, GitHubCopilotConfig, OpenRouterConfig};

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
}

#[async_trait]
#[allow(clippy::upper_case_acronyms)]
pub trait LLM: Send + Sync {
    #[allow(dead_code)]
    async fn get_copilot_token(&mut self) -> Result<String>;
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
}

impl ProviderClient {
    pub fn set_verbose(&mut self, verbose: bool) {
        match self {
            ProviderClient::GitHubCopilot(client) => client.set_verbose(verbose),
            ProviderClient::OpenRouter(client) => client.set_verbose(verbose),
        }
    }
}

#[async_trait]
impl LLM for ProviderClient {
    async fn get_copilot_token(&mut self) -> Result<String> {
        match self {
            ProviderClient::GitHubCopilot(client) => client.get_copilot_token().await,
            ProviderClient::OpenRouter(client) => client.get_copilot_token().await,
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
            name: "gpt-4o (GitHub Copilot)".to_string(),
        },
        ModelInfo {
            id: "gpt-4.1".to_string(),
            name: "gpt-4.1 (GitHub Copilot)".to_string(),
        },
        ModelInfo {
            id: "claude-sonnet-4".to_string(),
            name: "claude-sonnet-4 (GitHub Copilot)".to_string(),
        },
        ModelInfo {
            id: "gemini-2.0-flash-001".to_string(),
            name: "gemini-2.0-flash-001 (GitHub Copilot)".to_string(),
        },
        ModelInfo {
            id: "gemini-2.5-pro".to_string(),
            name: "gemini-2.5-pro (GitHub Copilot)".to_string(),
        },
        // OpenRouter models
        ModelInfo {
            id: "anthropic/claude-sonnet-4".to_string(),
            name: "Claude Sonnet 4 (OpenRouter)".to_string(),
        },
        ModelInfo {
            id: "anthropic/claude-opus-4".to_string(),
            name: "Claude Opus 4 (OpenRouter)".to_string(),
        },
    ]
}

#[allow(dead_code)]
pub async fn get_github_copilot_models(_client: Option<&mut LLMClient>) -> Vec<ModelInfo> {
    // Return the hardcoded list of known models
    get_default_models()
}

pub fn get_available_models(config: &Config) -> Vec<ModelInfo> {
    let mut models = Vec::new();
    let all_models = get_default_models();

    // Add GitHub Copilot models if configured
    if config.providers.github_copilot.is_some() {
        models.extend(
            all_models
                .iter()
                .filter(|m| m.name.contains("GitHub Copilot"))
                .cloned(),
        );
    }

    // Add OpenRouter models if configured
    if config.providers.open_router.is_some() {
        models.extend(
            all_models
                .iter()
                .filter(|m| m.name.contains("OpenRouter"))
                .cloned(),
        );
    }

    models
}

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
    #[allow(dead_code)]
    created: Option<u64>,
    #[allow(dead_code)]
    id: Option<String>,
    #[allow(dead_code)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CopilotUsage {
    #[allow(dead_code)]
    pub completion_tokens: u32,
    #[allow(dead_code)]
    pub completion_tokens_details: Option<CopilotCompletionTokensDetails>,
    pub prompt_tokens: u32,
    #[allow(dead_code)]
    pub prompt_tokens_details: Option<CopilotPromptTokensDetails>,
    pub total_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct CopilotCompletionTokensDetails {
    #[allow(dead_code)]
    accepted_prediction_tokens: u32,
    #[allow(dead_code)]
    rejected_prediction_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct CopilotPromptTokensDetails {
    #[allow(dead_code)]
    cached_tokens: u32,
}

#[derive(Debug, Deserialize)]
pub struct CopilotChoice {
    pub message: CopilotResponseMessage,
    #[serde(default)]
    #[allow(dead_code)]
    pub finish_reason: Option<String>,
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
    #[allow(dead_code)]
    refresh_in: u64,
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

        // Get new Copilot token
        let response = self
            .client
            .get("https://api.github.com/copilot_internal/v2/token")
            .header("Accept", "application/json")
            .header(
                "Authorization",
                format!("Bearer {}", self.config.access_token),
            )
            .header("Editor-Version", "vscode/1.99.3")
            .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
            .header("User-Agent", "GitHubCopilotChat/0.26.7")
            .header("X-GitHub-Api-Version", "2022-11-28")
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
        let token_preview = if copilot_token.len() > 8 {
            format!(
                "{}...{}",
                &copilot_token[..4],
                &copilot_token[copilot_token.len() - 4..]
            )
        } else {
            "***".to_string()
        };

        if verbose {
            eprintln!("🔍 Debug: Sending request to: {endpoint}");
            eprintln!("🔍 Debug: Copilot token preview: {token_preview}");
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request_value).unwrap_or_default()
            );
        }

        let response = self
            .client
            .post(endpoint)
            .header("Authorization", format!("Bearer {copilot_token}"))
            .header("User-Agent", "GitHubCopilotChat/1.0")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("X-GitHub-Api-Version", "2023-07-07")
            .header("Editor-Version", "vscode/1.85.0")
            .header("Editor-Plugin-Version", "copilot-chat/0.11.1")
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

        let request = CopilotChatRequest {
            messages: copilot_messages,
            model: selected_model,
            stream: false,
            tools: Some(vec![shell_tool]),
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

        let request = CopilotChatRequest {
            messages: copilot_messages,
            model: selected_model,
            stream: false,
            tools: Some(vec![shell_tool]),
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
        let token_preview = if copilot_token.len() > 8 {
            format!(
                "{}...{}",
                &copilot_token[..4],
                &copilot_token[copilot_token.len() - 4..]
            )
        } else {
            "***".to_string()
        };

        if verbose {
            eprintln!("🔍 Debug: Sending request to: {endpoint}");
            eprintln!("🔍 Debug: Copilot token preview: {token_preview}");
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request).unwrap_or_default()
            );
        }

        let response = self
            .client
            .post(endpoint)
            .header("Authorization", format!("Bearer {copilot_token}"))
            .header("User-Agent", "GitHubCopilotChat/1.0")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("X-GitHub-Api-Version", "2023-07-07")
            .header("Editor-Version", "vscode/1.85.0")
            .header("Editor-Plugin-Version", "copilot-chat/0.11.1")
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

    pub fn log_token_usage(usage: &CopilotUsage) {
        println!(
            "📊 Prompt tokens: {}; Tokens used: {}",
            usage.prompt_tokens, usage.total_tokens
        );
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
            eprintln!("🔍 Debug: Sending request to: {endpoint}");
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request_value).unwrap_or_default()
            );
        }

        let response = self
            .client
            .post(endpoint)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("HTTP-Referer", "https://github.com/jasonish/henri")
            .header("X-Title", "henri")
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

        let request = CopilotChatRequest {
            messages: copilot_messages,
            model: selected_model.clone(),
            stream: false,
            tools: Some(vec![shell_tool]),
            tool_choice: Some("auto".to_string()),
        };

        if verbose {
            eprintln!("🔍 Debug: Using model: {selected_model}");
            eprintln!("🔍 Debug: Tools enabled: shell");
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

        let request = CopilotChatRequest {
            messages: copilot_messages,
            model: selected_model.clone(),
            stream: false,
            tools: Some(vec![shell_tool]),
            tool_choice: None,
        };

        if verbose {
            eprintln!("🔍 Debug: Using model: {selected_model}");
            eprintln!("🔍 Debug: Tools enabled: shell");
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
            eprintln!("🔍 Debug: Sending request to: {endpoint}");
            eprintln!(
                "🔍 Debug: Request payload: {}",
                serde_json::to_string_pretty(&request).unwrap_or_default()
            );
        }

        let response = self
            .client
            .post(endpoint)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("HTTP-Referer", "https://github.com/jasonish/henri")
            .header("X-Title", "henri")
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

    pub fn log_token_usage(usage: &CopilotUsage) {
        println!(
            "📊 Prompt tokens: {}; Tokens used: {}",
            usage.prompt_tokens, usage.total_tokens
        );
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

    Ok((stdout, stderr, exit_code))
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
                                eprintln!("✅ Command completed with exit code: {exit_code}");
                                if !stdout.is_empty() {
                                    eprintln!("📤 stdout: {stdout}");
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
                                eprintln!("❌ Command failed: {e}");
                            }
                            results.push(ToolExecutionResult {
                                tool_call_id: tool_call.id.clone(),
                                stdout: String::new(),
                                stderr: format!("Failed to execute command: {e}"),
                                exit_code: -1,
                            });
                        }
                    }
                } else {
                    results.push(ToolExecutionResult {
                        tool_call_id: tool_call.id.clone(),
                        stdout: String::new(),
                        stderr: "Missing 'command' parameter".to_string(),
                        exit_code: -1,
                    });
                }
            }
            _ => {
                if verbose {
                    eprintln!("⚠️  Unknown tool: {}", tool_call.function.name);
                }
                results.push(ToolExecutionResult {
                    tool_call_id: tool_call.id.clone(),
                    stdout: String::new(),
                    stderr: format!("Unknown tool: {}", tool_call.function.name),
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

    // Check if the selected model is an OpenRouter model
    if let Some(model) = &selected_model {
        if model.contains("anthropic/") {
            // Try to use OpenRouter
            if let Some(openrouter_config) = config.providers.open_router {
                if verbose {
                    eprintln!("🔍 Debug: Using OpenRouter for model: {model}");
                }
                return Ok(Some(ProviderClient::OpenRouter(OpenRouterClient::new(
                    openrouter_config,
                    verbose,
                ))));
            } else {
                if verbose {
                    eprintln!(
                        "🔍 Debug: Model {model} requires OpenRouter but no OpenRouter config found"
                    );
                }
                anyhow::bail!(
                    "Selected model '{}' requires OpenRouter authentication. Please run 'henri login openrouter' to configure.",
                    model
                );
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
