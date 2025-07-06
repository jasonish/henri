use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::chat::{ChatMessage, MessageRole};
use crate::config::{Config, GitHubCopilotConfig};

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
}

/// Get the default/fallback list of models
fn get_default_models() -> Vec<ModelInfo> {
    vec![
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
    ]
}

pub async fn get_github_copilot_models(_client: Option<&mut LLMClient>) -> Vec<ModelInfo> {
    // Return the hardcoded list of known models
    get_default_models()
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
    content: String,
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
struct CopilotChoice {
    message: CopilotResponseMessage,
    #[serde(default)]
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CopilotResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: FunctionCall,
}

#[derive(Debug, Deserialize, Serialize)]
struct FunctionCall {
    name: String,
    arguments: String,
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

    async fn get_copilot_token(&mut self) -> Result<String> {
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
    ) -> Result<(String, Option<CopilotUsage>)> {
        // Get Copilot token (will exchange if needed)
        let copilot_token = self.get_copilot_token().await?;
        let copilot_messages: Vec<CopilotMessage> = messages
            .iter()
            .map(|msg| CopilotMessage {
                role: match msg.role {
                    MessageRole::User => "user".to_string(),
                    MessageRole::Assistant => "assistant".to_string(),
                    MessageRole::System => "system".to_string(),
                },
                content: msg.content.as_text(),
            })
            .collect();

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

        // Don't log token usage here - return it instead

        // Extract content from the response
        let content = chat_response.choices[0]
            .message
            .content
            .clone()
            .unwrap_or_default();

        // TODO: Handle tool_calls if present
        if let Some(tool_calls) = &chat_response.choices[0].message.tool_calls {
            if !tool_calls.is_empty() {
                // For now, just include tool calls in the content
                let tool_json = serde_json::json!({
                    "tool_calls": tool_calls
                });
                if let Ok(json_str) = serde_json::to_string_pretty(&tool_json) {
                    return Ok((
                        format!("{content}\n\n```json\n{json_str}\n```"),
                        chat_response.usage,
                    ));
                }
            }
        }

        Ok((content, chat_response.usage))
    }

    pub fn log_token_usage(usage: &CopilotUsage) {
        println!(
            "📊 Prompt tokens: {}; Tokens used: {}",
            usage.prompt_tokens, usage.total_tokens
        );
    }
}

pub fn create_llm_client(verbose: bool) -> Result<Option<LLMClient>> {
    let config = Config::load()?;

    if verbose {
        eprintln!("🔍 Debug: Config loaded, checking providers...");
    }

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
                    "GitHub Copilot token has expired. Please run 'coder login' to re-authenticate."
                );
            }
        } else if verbose {
            eprintln!("🔍 Debug: No expiry time set for token");
        }

        Ok(Some(LLMClient::new(github_config, verbose)))
    } else {
        if verbose {
            eprintln!("🔍 Debug: No GitHub Copilot config found");
        }
        Ok(None)
    }
}
