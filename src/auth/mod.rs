// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use colored::Colorize;
use inquire::error::InquireError;
use inquire::{Select, Text};
use rand::Rng;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

use crate::config::{
    AntigravityProviderConfig, ClaudeAuth, ClaudeProviderConfig, ConfigFile, CopilotProviderConfig,
    ModelConfig, OpenAiCompatProviderConfig, OpenAiProviderConfig, OpenRouterConfig,
    ProviderConfig, ProviderType, ZenProviderConfig,
};
use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub(crate) enum LoginProvider {
    Antigravity,
    GitHubCopilot,
    OpenCodeZen,
    Claude,
    OpenAi,
    OpenAiCompat,
    OpenRouter,
}

impl fmt::Display for LoginProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoginProvider::Antigravity => write!(f, "Google Antigravity"),
            LoginProvider::GitHubCopilot => write!(f, "GitHub Copilot"),
            LoginProvider::OpenCodeZen => write!(f, "OpenCode Zen"),
            LoginProvider::Claude => write!(f, "Claude (Max/Pro)"),
            LoginProvider::OpenAi => write!(f, "OpenAI"),
            LoginProvider::OpenAiCompat => write!(f, "OpenAI Compatible"),
            LoginProvider::OpenRouter => write!(f, "OpenRouter"),
        }
    }
}

impl LoginProvider {
    pub(crate) fn all() -> &'static [LoginProvider] {
        &[
            LoginProvider::GitHubCopilot,
            LoginProvider::OpenCodeZen,
            LoginProvider::Claude,
            LoginProvider::OpenAi,
            LoginProvider::OpenAiCompat,
            LoginProvider::OpenRouter,
            LoginProvider::Antigravity,
        ]
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    token_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenError {
    error: String,
    error_description: Option<String>,
}

const OPENAI_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const OPENAI_REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const OPENAI_SCOPE: &str = "openid profile email offline_access";
const OPENAI_DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUDIENCE: &str = "https://api.openai.com/v1";
const OPENAI_OAUTH_SUCCESS_HTML: &str = r###"<!DOCTYPE html><html><body><h1>OpenAI authentication successful</h1><p>You can close this window.</p></body></html>"###;

pub(crate) async fn login() -> Result<Option<LoginProvider>> {
    let providers = LoginProvider::all().to_vec();

    let selection = match Select::new("Select a provider to login:", providers)
        .with_page_size(crate::output::menu_page_size())
        .prompt()
    {
        Ok(selection) => selection,
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            println!("Login cancelled.");
            return Ok(None);
        }
        Err(e) => return Err(Error::Prompt(e.to_string())),
    };

    match selection {
        LoginProvider::Antigravity => {
            login_antigravity().await?;
            Ok(Some(LoginProvider::Antigravity))
        }
        LoginProvider::GitHubCopilot => {
            login_github_copilot().await?;
            Ok(Some(LoginProvider::GitHubCopilot))
        }
        LoginProvider::OpenCodeZen => {
            login_opencode_zen().await?;
            Ok(Some(LoginProvider::OpenCodeZen))
        }
        LoginProvider::Claude => {
            login_claude().await?;
            Ok(Some(LoginProvider::Claude))
        }
        LoginProvider::OpenAi => {
            login_openai().await?;
            Ok(Some(LoginProvider::OpenAi))
        }
        LoginProvider::OpenAiCompat => {
            login_openai_compat().await?;
            Ok(Some(LoginProvider::OpenAiCompat))
        }
        LoginProvider::OpenRouter => {
            login_openrouter().await?;
            Ok(Some(LoginProvider::OpenRouter))
        }
    }
}

async fn login_opencode_zen() -> Result<()> {
    println!("\n{}", "OpenCode Zen Authentication".cyan().bold());
    println!("{}", "═".repeat(50).cyan());

    println!(
        "{}",
        "Enter your OpenCode Zen API key (from https://opencode.ai/zen).".yellow()
    );

    let api_key = loop {
        match Text::new("API key:").prompt() {
            Ok(key) => {
                let key = key.trim();
                if !key.is_empty() {
                    break key.to_string();
                }
                println!("{}", "API key cannot be empty.".red());
            }
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                return Err(Error::Auth("Login cancelled".to_string()));
            }
            Err(e) => return Err(Error::Prompt(e.to_string())),
        }
    };

    let mut config = ConfigFile::load()?;
    let local_id = determine_local_id(&config, ProviderType::Zen)?;
    config.set_provider(
        local_id.clone(),
        ProviderConfig::Zen(ZenProviderConfig {
            enabled: true,
            api_key,
        }),
    );
    config.save()?;

    println!(
        "{}",
        format!(
            "✓ OpenCode Zen account '{}' saved. You can now chat with Henri.",
            local_id
        )
        .green()
        .bold()
    );

    Ok(())
}

async fn login_openai_compat() -> Result<()> {
    println!("\n{}", "OpenAI Compatible Provider Setup".cyan().bold());
    println!("{}", "═".repeat(50).cyan());

    // Step 1: Provider ID
    println!(
        "{}",
        "Enter a unique ID for this provider (e.g., 'ollama', 'together', 'groq').".yellow()
    );
    println!(
        "{}",
        "This will be the prefix when selecting models (e.g., 'ollama/llama3')".bright_black()
    );

    let provider_id = Text::new("Provider ID:")
        .with_validator(|input: &str| {
            if input.trim().is_empty() {
                Err(Box::from("Provider ID cannot be empty"))
            } else if input.contains('/') || input.contains(' ') {
                Err(Box::from("Provider ID cannot contain '/' or spaces"))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .prompt()
        .map_err(|e| Error::Prompt(e.to_string()))?;

    let provider_id = provider_id.trim().to_lowercase();

    // Step 2: Base URL
    println!("\n{}", "Enter the base URL for the API.".yellow());
    println!(
        "{}",
        "Examples: http://localhost:11434/v1, https://api.together.xyz/v1".bright_black()
    );

    let base_url = Text::new("Base URL:")
        .with_default("http://localhost:11434/v1")
        .with_validator(|input: &str| {
            if input.trim().is_empty() {
                Err(Box::from("Base URL cannot be empty"))
            } else if !input.starts_with("http://") && !input.starts_with("https://") {
                Err(Box::from("Base URL must start with http:// or https://"))
            } else {
                Ok(inquire::validator::Validation::Valid)
            }
        })
        .prompt()
        .map_err(|e| Error::Prompt(e.to_string()))?;

    // Step 3: API Key
    println!(
        "\n{}",
        "Enter the API key (leave empty if not required, e.g., local Ollama).".yellow()
    );

    let api_key = Text::new("API Key:")
        .prompt()
        .map_err(|e| Error::Prompt(e.to_string()))?
        .trim()
        .to_string();

    // Step 4: Models
    println!("\n{}", "Add models for this provider.".yellow());
    println!(
        "{}",
        "For each model, you can optionally set a reasoning effort level.".bright_black()
    );

    let mut models: Vec<String> = Vec::new();
    let mut model_configs: Vec<ModelConfig> = Vec::new();

    loop {
        let model_name = Text::new("Model name (or press Enter to finish):")
            .prompt()
            .map_err(|e| Error::Prompt(e.to_string()))?;

        let model_name = model_name.trim();
        if model_name.is_empty() {
            if models.is_empty() && model_configs.is_empty() {
                println!("{}", "At least one model is required.".red());
                continue;
            }
            break;
        }

        // Ask for reasoning effort level
        let reasoning_options = vec!["none", "low", "medium", "high"];
        let reasoning = Select::new("Reasoning effort level:", reasoning_options)
            .with_help_message("Select reasoning effort for this model")
            .with_page_size(crate::output::menu_page_size())
            .prompt()
            .map_err(|e| Error::Prompt(e.to_string()))?;

        if reasoning == "none" {
            // Simple model entry
            models.push(model_name.to_string());
        } else {
            // Model with reasoning effort config
            model_configs.push(ModelConfig {
                name: model_name.to_string(),
                reasoning_effort: Some(reasoning.to_string()),
                thinking: None,
                temperature: None,
                max_tokens: None,
                system_prompt: None,
                stop_sequences: None,
            });
        }

        println!("{}", format!("✓ Added model: {}", model_name).green());
    }

    // Save configuration
    let openai_compat_config = OpenAiCompatProviderConfig {
        enabled: true,
        api_key,
        base_url: base_url.trim().to_string(),
        models,
        model_configs,
    };

    let mut config = ConfigFile::load()?;

    // Check if provider ID already exists
    if config.get_provider(&provider_id).is_some() {
        let overwrite = inquire::Confirm::new(&format!(
            "Provider '{}' already exists. Overwrite?",
            provider_id
        ))
        .with_default(false)
        .prompt()
        .map_err(|e| Error::Prompt(e.to_string()))?;

        if !overwrite {
            println!("{}", "Setup cancelled.".yellow());
            return Ok(());
        }
    }

    config.set_provider(
        provider_id.clone(),
        ProviderConfig::OpenaiCompat(openai_compat_config),
    );
    config.save()?;

    println!(
        "\n{}",
        format!("✓ Provider '{}' configured successfully!", provider_id)
            .green()
            .bold()
    );
    println!(
        "{}",
        format!(
            "You can now select models like '{}/model-name'",
            provider_id
        )
        .blue()
    );

    Ok(())
}

async fn login_openrouter() -> Result<()> {
    println!("\n{}", "OpenRouter Setup".cyan().bold());
    println!("{}", "═".repeat(50).cyan());

    println!(
        "{}",
        "Get your API key from https://openrouter.ai/keys".yellow()
    );

    // Step 1: API Key
    let api_key = loop {
        match Text::new("API key:").prompt() {
            Ok(key) => {
                let key = key.trim();
                if !key.is_empty() {
                    break key.to_string();
                }
                println!("{}", "API key cannot be empty.".red());
            }
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                return Err(Error::Auth("Setup cancelled".to_string()));
            }
            Err(e) => return Err(Error::Prompt(e.to_string())),
        }
    };

    // Step 2: Models
    println!("\n{}", "Add models for OpenRouter.".yellow());
    println!(
        "{}",
        "Examples: anthropic/claude-sonnet-4, openai/gpt-4o, google/gemini-2.5-pro".bright_black()
    );

    let mut models: Vec<String> = Vec::new();
    let mut model_configs: Vec<ModelConfig> = Vec::new();

    loop {
        let model_name = Text::new("Model name (or press Enter to finish):")
            .prompt()
            .map_err(|e| Error::Prompt(e.to_string()))?;

        let model_name = model_name.trim();
        if model_name.is_empty() {
            if models.is_empty() && model_configs.is_empty() {
                println!("{}", "At least one model is required.".red());
                continue;
            }
            break;
        }

        // Ask for reasoning effort level
        let reasoning_options = vec!["none", "low", "medium", "high"];
        let reasoning = Select::new("Reasoning effort level:", reasoning_options)
            .with_help_message("Select reasoning effort for this model")
            .with_page_size(crate::output::menu_page_size())
            .prompt()
            .map_err(|e| Error::Prompt(e.to_string()))?;

        if reasoning == "none" {
            models.push(model_name.to_string());
        } else {
            model_configs.push(ModelConfig {
                name: model_name.to_string(),
                reasoning_effort: Some(reasoning.to_string()),
                thinking: None,
                temperature: None,
                max_tokens: None,
                system_prompt: None,
                stop_sequences: None,
            });
        }

        println!("{}", format!("✓ Added model: {}", model_name).green());
    }

    let openrouter_config = OpenRouterConfig {
        enabled: true,
        api_key,
        models,
        model_configs,
    };

    let mut config = ConfigFile::load()?;
    let local_id = determine_local_id(&config, ProviderType::OpenRouter)?;

    // Check if provider ID already exists
    if config.get_provider(&local_id).is_some() {
        let overwrite = inquire::Confirm::new(&format!(
            "Provider '{}' already exists. Overwrite?",
            local_id
        ))
        .with_default(false)
        .prompt()
        .map_err(|e| Error::Prompt(e.to_string()))?;

        if !overwrite {
            println!("{}", "Setup cancelled.".yellow());
            return Ok(());
        }
    }

    config.set_provider(
        local_id.clone(),
        ProviderConfig::Openrouter(openrouter_config),
    );
    config.save()?;

    println!(
        "\n{}",
        format!("✓ OpenRouter '{}' configured successfully!", local_id)
            .green()
            .bold()
    );
    println!(
        "{}",
        "You can now select models like 'openrouter/anthropic/claude-sonnet-4'".blue()
    );

    Ok(())
}

/// Determine the local identifier for a new provider account
fn determine_local_id(config: &ConfigFile, provider_type: ProviderType) -> Result<String> {
    let existing_count = config.providers_of_type(provider_type).len();

    if existing_count == 0 {
        // Smart default: use provider's default ID for first account
        Ok(provider_type.default_id().to_string())
    } else {
        // Prompt for name since accounts already exist - loop until valid or cancelled
        loop {
            match Text::new(&format!(
                "Enter a name for this {} account (e.g., '{}-work'):",
                provider_type.display_name(),
                provider_type.default_id()
            ))
            .prompt()
            {
                Ok(name) => {
                    let name = name.trim();
                    if name.is_empty() {
                        println!("{}", "Account name cannot be empty.".red());
                        continue;
                    }
                    if config.get_provider(name).is_some() {
                        println!(
                            "{}",
                            format!("A provider with name '{}' already exists.", name).red()
                        );
                        continue;
                    }
                    return Ok(name.to_string());
                }
                Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
                    return Err(Error::Auth("Account setup cancelled".to_string()));
                }
                Err(e) => return Err(Error::Prompt(e.to_string())),
            }
        }
    }
}

fn generate_state() -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..16).map(|_| rng.random()).collect();
    URL_SAFE_NO_PAD.encode(bytes)
}

fn parse_openai_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }

    if let Ok(url) = Url::parse(value) {
        let code = url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned());
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned());
        return (code, state);
    }

    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_string()), Some(state.to_string()));
    }

    if value.contains("code=") {
        let mut code = None;
        let mut state = None;
        for pair in value.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                let decoded = urlencoding::decode(v).unwrap_or_else(|_| v.into());
                match k {
                    "code" => code = Some(decoded.into_owned()),
                    "state" => state = Some(decoded.into_owned()),
                    _ => {}
                }
            }
        }
        return (code, state);
    }

    (Some(value.to_string()), None)
}

async fn start_callback_server(
    port: u16,
    state: String,
    success_html: &'static str,
) -> (bool, oneshot::Receiver<Option<String>>) {
    let (tx, rx) = oneshot::channel();

    match TcpListener::bind(format!("127.0.0.1:{}", port)).await {
        Ok(listener) => {
            tokio::spawn(async move {
                if let Ok((mut stream, _)) = listener.accept().await {
                    let mut buffer = [0u8; 4096];
                    if let Ok(read) = stream.read(&mut buffer).await {
                        let request = String::from_utf8_lossy(&buffer[..read]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1));

                        if let Some(path) = path
                            && let Ok(url) = Url::parse(&format!("http://localhost{}", path))
                        {
                            let returned_state = url
                                .query_pairs()
                                .find(|(k, _)| k == "state")
                                .map(|(_, v)| v.into_owned());

                            let code = url
                                .query_pairs()
                                .find(|(k, _)| k == "code")
                                .map(|(_, v)| v.into_owned());

                            // Validate state parameter if provided (skip validation if empty)

                            let state_valid = if state.is_empty() {
                                true
                            } else {
                                returned_state.as_deref() == Some(state.as_str())
                            };

                            if state_valid && let Some(code) = code {
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
                                    success_html.len(),
                                    success_html
                                );
                                let _ = stream.write_all(response.as_bytes()).await;
                                let _ = tx.send(Some(code));
                                return;
                            }
                        }
                    }
                    let _ = tx.send(None);
                } else {
                    let _ = tx.send(None);
                }
            });
            (true, rx)
        }
        Err(e) => {
            println!(
                "{}",
                format!("Failed to start local callback server: {e}").yellow()
            );
            let _ = tx.send(None);
            (false, rx)
        }
    }
}

async fn login_openai() -> Result<()> {
    println!("\n{}", "OpenAI Authentication".cyan().bold());
    println!("{}", "═".repeat(50).cyan());

    let client_id = std::env::var("OPENAI_OAUTH_CLIENT_ID")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| OPENAI_DEFAULT_CLIENT_ID.to_string());

    let scope = std::env::var("OPENAI_OAUTH_SCOPE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| OPENAI_SCOPE.to_string());

    let client = Client::new();
    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);
    let state = generate_state();

    let mut auth_url = Url::parse(OPENAI_AUTHORIZE_URL).map_err(|e| Error::Auth(e.to_string()))?;
    auth_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", OPENAI_REDIRECT_URI)
        .append_pair("scope", &scope)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "codex_cli_rs");

    let (server_running, code_rx) =
        start_callback_server(1455, state.clone(), OPENAI_OAUTH_SUCCESS_HTML).await;

    println!(
        "{}",
        "A browser window will open for OpenAI authentication.".yellow()
    );
    if let Err(e) = open::that(auth_url.as_str()) {
        println!(
            "{}",
            format!("Failed to open browser automatically: {e}").yellow()
        );
    }
    println!(
        "{}",
        format!(
            "If the browser did not open, navigate to:\n{}",
            auth_url.as_str()
        )
        .blue()
    );

    let mut code = None;

    if let Ok(Ok(Some(callback_code))) = timeout(Duration::from_secs(180), code_rx).await {
        println!(
            "{}",
            "Authorization code received via redirect.".green().bold()
        );
        code = Some(callback_code);
    } else if server_running {
        println!(
            "{}",
            "Timed out waiting for redirect. You can paste the URL manually.".yellow()
        );
    }

    if code.is_none() {
        println!(
            "{}",
            "Paste the full redirect URL or authorization code from your browser.".yellow()
        );
        let manual_input = Text::new("Authorization code or URL:")
            .with_help_message("Accepts callback URL, code#state, or raw code")
            .prompt()
            .map_err(|e| Error::Prompt(e.to_string()))?;
        let (manual_code, returned_state) = parse_openai_authorization_input(&manual_input);
        if let Some(returned_state) = returned_state
            && returned_state != state
        {
            return Err(Error::Auth(
                "State mismatch. Please restart the OpenAI login.".to_string(),
            ));
        }
        code = manual_code;
    }

    let code = code.ok_or_else(|| Error::Auth("Authorization code is required.".to_string()))?;

    let token_response = client
        .post(OPENAI_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("code_verifier", code_verifier.as_str()),
            ("redirect_uri", OPENAI_REDIRECT_URI),
        ])
        .send()
        .await
        .map_err(|e| Error::Auth(e.to_string()))?;

    if !token_response.status().is_success() {
        let status = token_response.status();
        let body = token_response.text().await.unwrap_or_default();
        return Err(Error::Auth(format!(
            "OpenAI token exchange failed: {} - {}",
            status, body
        )));
    }

    let token: TokenResponse = token_response
        .json()
        .await
        .map_err(|e| Error::Auth(format!("Failed to parse token response: {}", e)))?;

    let refresh_token = token.refresh_token.clone().unwrap_or_default();
    if refresh_token.is_empty() {
        return Err(Error::Auth(
            "OpenAI did not return a refresh token. Ensure offline_access is enabled.".to_string(),
        ));
    }

    let expires_at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system time before Unix epoch")
        .as_millis() as u64
        + (token.expires_in.unwrap_or(3600) * 1000);

    let openai_config = OpenAiProviderConfig {
        enabled: true,
        client_id: client_id.clone(),
        audience: OPENAI_AUDIENCE.to_string(),
        refresh_token,
        access_token: token.access_token,
        expires_at,
        project_id: std::env::var("OPENAI_PROJECT")
            .ok()
            .filter(|v| !v.is_empty()),
    };

    let mut config = ConfigFile::load()?;
    let local_id = determine_local_id(&config, ProviderType::Openai)?;
    config.set_provider(local_id.clone(), ProviderConfig::Openai(openai_config));
    config.save()?;

    println!(
        "{}",
        format!(
            "✓ OpenAI account '{}' connected. You can now select OpenAI models.",
            local_id
        )
        .green()
        .bold()
    );

    Ok(())
}

async fn login_github_copilot() -> Result<()> {
    let client = Client::new();

    let device_response = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .header("User-Agent", "henri-cli")
        .form(&[
            ("client_id", "Iv1.b507a08c87ecfe98"),
            ("scope", "read:user"),
        ])
        .send()
        .await
        .map_err(|e| Error::Auth(e.to_string()))?;

    if !device_response.status().is_success() {
        let status = device_response.status();
        let body = device_response.text().await.unwrap_or_default();
        return Err(Error::Auth(format!(
            "Device code request failed: {} - {}",
            status, body
        )));
    }

    let response_text = device_response
        .text()
        .await
        .map_err(|e| Error::Auth(e.to_string()))?;

    let device_code_response: DeviceCodeResponse = serde_json::from_str(&response_text)
        .map_err(|e| Error::Auth(format!("Failed to parse device code response: {}", e)))?;

    println!("\n{}", "GitHub Copilot Authentication".green().bold());
    println!(
        "1. Please visit: {}",
        device_code_response.verification_uri.blue().underline()
    );
    println!(
        "2. Enter code: {}",
        device_code_response.user_code.yellow().bold()
    );
    println!("3. Waiting for authentication...\n");

    let mut interval = Duration::from_secs(device_code_response.interval);
    let expiry = SystemTime::now() + Duration::from_secs(device_code_response.expires_in);

    loop {
        if SystemTime::now() > expiry {
            return Err(Error::Auth(
                "Device code expired. Please try again.".to_string(),
            ));
        }

        sleep(interval).await;

        let token_response = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", "Iv1.b507a08c87ecfe98"),
                ("device_code", &device_code_response.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;

        if !token_response.status().is_success() {
            return Err(Error::Auth(format!(
                "Token request failed: {}",
                token_response.status()
            )));
        }

        let text = token_response
            .text()
            .await
            .map_err(|e| Error::Auth(e.to_string()))?;

        if let Ok(token) = serde_json::from_str::<TokenResponse>(&text) {
            let user_response = client
                .get("https://api.github.com/user")
                .header("Authorization", format!("Bearer {}", token.access_token))
                .header("User-Agent", "henri-copilot-cli")
                .send()
                .await
                .map_err(|e| Error::Auth(e.to_string()))?;

            if !user_response.status().is_success() {
                return Err(Error::Auth("Failed to verify GitHub token".to_string()));
            }

            let user_data: serde_json::Value = user_response
                .json()
                .await
                .map_err(|e| Error::Auth(e.to_string()))?;
            let username = user_data["login"].as_str().unwrap_or("unknown");

            println!(
                "{}",
                format!("✓ Successfully authenticated as {username}").green()
            );

            let expires_at = token
                .expires_in
                .map(|expires_in| chrono::Utc::now().timestamp() + expires_in as i64);

            let github_config = CopilotProviderConfig {
                enabled: true,
                access_token: token.access_token,
                refresh_token: token.refresh_token,
                expires_at,
                copilot_token: None,
                copilot_expires_at: None,
            };

            let mut config = ConfigFile::load()?;
            let local_id = determine_local_id(&config, ProviderType::GithubCopilot)?;
            config.set_provider(
                local_id.clone(),
                ProviderConfig::GithubCopilot(github_config),
            );
            config.save()?;

            println!(
                "{}",
                format!("✓ GitHub Copilot account '{}' saved!", local_id).green()
            );
            println!(
                "{}",
                "You can now use Henri with GitHub Copilot models.".blue()
            );

            return Ok(());
        }

        if let Ok(error_response) = serde_json::from_str::<TokenError>(&text) {
            match error_response.error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    interval += Duration::from_secs(5);
                    continue;
                }
                "expired_token" => {
                    return Err(Error::Auth("Device code expired".to_string()));
                }
                "access_denied" => {
                    return Err(Error::Auth("Access denied by user".to_string()));
                }
                _ => {
                    return Err(Error::Auth(format!(
                        "Unknown error: {}",
                        error_response.error
                    )));
                }
            }
        }

        return Err(Error::Auth(format!("Unexpected response format: {}", text)));
    }
}

async fn login_claude() -> Result<()> {
    let config = ConfigFile::load()?;
    let existing_providers = config.providers_of_type(ProviderType::Claude);

    let provider_id = if !existing_providers.is_empty() {
        let options = vec!["Add new instance", "Update existing instance"];
        let action = Select::new("Claude provider already configured:", options)
            .with_page_size(crate::output::menu_page_size())
            .prompt()
            .map_err(|e| Error::Prompt(e.to_string()))?;

        if action == "Add new instance" {
            None
        } else {
            // Update existing
            if existing_providers.len() == 1 {
                let (id, _) = existing_providers[0];
                Some(id.clone())
            } else {
                // Select which one
                let ids: Vec<String> = existing_providers
                    .iter()
                    .map(|(id, _)| id.to_string())
                    .collect();
                let selected = Select::new("Select instance to update:", ids)
                    .with_page_size(crate::output::menu_page_size())
                    .prompt()
                    .map_err(|e| Error::Prompt(e.to_string()))?;
                Some(selected)
            }
        }
    } else {
        None
    };

    login_claude_oauth("https://claude.ai/oauth/authorize", provider_id).await
}

async fn login_claude_oauth(auth_url: &str, target_provider_id: Option<String>) -> Result<()> {
    let client = Client::new();

    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);

    let client_id = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
    let redirect_uri = "https://console.anthropic.com/oauth/code/callback";
    let scopes = "org:create_api_key user:profile user:inference";

    let full_auth_url = format!(
        "{}?code=true&client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        auth_url,
        client_id,
        urlencoding::encode(redirect_uri),
        urlencoding::encode(scopes),
        code_challenge,
        code_verifier
    );

    println!("\n{}", "Step 1: Authorization".green().bold());
    println!("Opening browser for authentication...");

    if let Err(e) = open::that(&full_auth_url) {
        println!("{}", format!("Failed to open browser: {e}").yellow());
        println!("Please manually open this URL:");
        println!("{}", full_auth_url.blue().underline());
    }

    println!("\n{}", "Step 2: Authorization Code".green().bold());
    println!("After authorizing, you'll receive a code in the format: code#state");

    let auth_code = Text::new("Enter the authorization code:")
        .prompt()
        .map_err(|e| Error::Prompt(e.to_string()))?;

    let parts: Vec<&str> = auth_code.split('#').collect();
    if parts.len() != 2 {
        return Err(Error::Auth(
            "Invalid authorization code format. Expected: code#state".to_string(),
        ));
    }

    let (code, state) = (parts[0], parts[1]);

    if state != code_verifier {
        return Err(Error::Auth(
            "State mismatch. This may indicate a security issue.".to_string(),
        ));
    }

    println!("\n{}", "Step 3: Token Exchange".green().bold());
    println!("Exchanging authorization code for tokens...");

    let token_request = serde_json::json!({
        "code": code,
        "state": state,
        "grant_type": "authorization_code",
        "client_id": client_id,
        "redirect_uri": redirect_uri,
        "code_verifier": code_verifier
    });

    let response = client
        .post("https://console.anthropic.com/v1/oauth/token")
        .header("Content-Type", "application/json")
        .json(&token_request)
        .send()
        .await
        .map_err(|e| Error::Auth(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(Error::Auth(format!(
            "Token exchange failed: {} - {}",
            status, body
        )));
    }

    let token_response: serde_json::Value = response
        .json()
        .await
        .map_err(|e| Error::Auth(e.to_string()))?;

    let access_token = token_response["access_token"]
        .as_str()
        .ok_or_else(|| Error::Auth("Missing access_token in response".to_string()))?;
    let refresh_token = token_response["refresh_token"]
        .as_str()
        .ok_or_else(|| Error::Auth("Missing refresh_token in response".to_string()))?;
    let expires_in = token_response["expires_in"]
        .as_u64()
        .ok_or_else(|| Error::Auth("Missing expires_in in response".to_string()))?;

    let expires_at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system time before Unix epoch")
        .as_millis() as u64
        + (expires_in * 1000);

    let claude_config = ClaudeProviderConfig {
        enabled: true,
        auth: ClaudeAuth {
            refresh_token: refresh_token.to_string(),
            access_token: access_token.to_string(),
            expires_at,
        },
    };

    let mut config = ConfigFile::load()?;
    let local_id = if let Some(id) = target_provider_id {
        id
    } else {
        determine_local_id(&config, ProviderType::Claude)?
    };

    config.set_provider(local_id.clone(), ProviderConfig::Claude(claude_config));
    config.save()?;

    println!(
        "{}",
        format!(
            "✓ Claude account '{}' authenticated successfully!",
            local_id
        )
        .green()
        .bold()
    );
    println!(
        "{}",
        "You can now use Claude by selecting it in your configuration.".green()
    );

    Ok(())
}

fn generate_code_verifier() -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.random()).collect();
    URL_SAFE_NO_PAD.encode(&bytes)
}

fn generate_code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    URL_SAFE_NO_PAD.encode(hash)
}

// Antigravity OAuth constants
pub(crate) const DEFAULT_ANTIGRAVITY_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub(crate) const DEFAULT_ANTIGRAVITY_CLIENT_SECRET: &str = "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";
pub(crate) const ANTIGRAVITY_CALLBACK_PORT: u16 = 51121;
pub(crate) const ANTIGRAVITY_CALLBACK_PATH: &str = "/oauthcallback";
pub(crate) const ANTIGRAVITY_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

pub(crate) fn get_antigravity_client_id() -> String {
    std::env::var("ANTIGRAVITY_CLIENT_ID")
        .unwrap_or_else(|_| DEFAULT_ANTIGRAVITY_CLIENT_ID.to_string())
}

pub(crate) fn get_antigravity_client_secret() -> String {
    std::env::var("ANTIGRAVITY_CLIENT_SECRET")
        .unwrap_or_else(|_| DEFAULT_ANTIGRAVITY_CLIENT_SECRET.to_string())
}

pub(crate) const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub(crate) const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub(crate) const GOOGLE_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
const ANTIGRAVITY_DISCOVERY_URL: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const ANTIGRAVITY_ONBOARD_URL: &str = "https://cloudcode-pa.googleapis.com/v1internal:onboardUser";
const ANTIGRAVITY_OAUTH_SUCCESS_HTML: &str = r###"<!DOCTYPE html><html><body><h1>Antigravity authentication successful</h1><p>You can close this window.</p></body></html>"###;

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: u64,
    _token_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    email: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadCodeAssistResponse {
    #[serde(default)]
    current_tier: Option<String>,
    #[serde(default)]
    cloudaicompanion_project: Option<String>,
}

async fn login_antigravity() -> Result<()> {
    let config = ConfigFile::load()?;
    let existing_providers = config.providers_of_type(ProviderType::Antigravity);

    // Determine provider ID before authentication
    let target_provider_id = if !existing_providers.is_empty() {
        let options = vec!["Add new account", "Update existing account"];
        let action = Select::new("Antigravity provider already configured:", options)
            .with_page_size(crate::output::menu_page_size())
            .prompt()
            .map_err(|e| Error::Prompt(e.to_string()))?;

        if action == "Add new account" {
            // Prompt for new account name before starting OAuth
            determine_local_id(&config, ProviderType::Antigravity)?
        } else {
            // Update existing
            if existing_providers.len() == 1 {
                let (id, _) = existing_providers[0];
                id.clone()
            } else {
                // Select which one
                let ids: Vec<String> = existing_providers
                    .iter()
                    .map(|(id, _)| id.to_string())
                    .collect();
                Select::new("Select account to update:", ids)
                    .with_page_size(crate::output::menu_page_size())
                    .prompt()
                    .map_err(|e| Error::Prompt(e.to_string()))?
            }
        }
    } else {
        // First account - use default ID
        ProviderType::Antigravity.default_id().to_string()
    };

    println!("\n{}", "Antigravity Authentication".cyan().bold());
    println!("{}", "═".repeat(50).cyan());

    let client = Client::new();
    let state = generate_state();

    let scopes = ANTIGRAVITY_SCOPES.join(" ");
    let redirect_uri = format!(
        "http://localhost:{}{}",
        ANTIGRAVITY_CALLBACK_PORT, ANTIGRAVITY_CALLBACK_PATH
    );

    let mut auth_url = Url::parse(GOOGLE_AUTH_URL).map_err(|e| Error::Auth(e.to_string()))?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &get_antigravity_client_id())
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", &scopes)
        .append_pair("access_type", "offline")
        .append_pair("response_type", "code")
        .append_pair("prompt", "consent")
        .append_pair("state", &state);

    let (server_running, code_rx) = start_callback_server(
        ANTIGRAVITY_CALLBACK_PORT,
        state.clone(),
        ANTIGRAVITY_OAUTH_SUCCESS_HTML,
    )
    .await;

    println!(
        "{}",
        "A browser window will open for Google authentication.".yellow()
    );
    if let Err(e) = open::that(auth_url.as_str()) {
        println!(
            "{}",
            format!("Failed to open browser automatically: {e}").yellow()
        );
    }
    println!(
        "{}",
        format!(
            "If the browser did not open, navigate to:\n{}",
            auth_url.as_str()
        )
        .blue()
    );

    let mut code = None;

    if let Ok(Ok(Some(callback_code))) = timeout(Duration::from_secs(300), code_rx).await {
        println!(
            "{}",
            "Authorization code received via redirect.".green().bold()
        );
        code = Some(callback_code);
    } else if server_running {
        println!(
            "{}",
            "Timed out waiting for redirect. You can paste the URL manually.".yellow()
        );
    }

    if code.is_none() {
        println!(
            "{}",
            "Paste the full redirect URL or authorization code from your browser.".yellow()
        );
        let manual_input = Text::new("Authorization code or URL:")
            .with_help_message("Accepts callback URL or raw code")
            .prompt()
            .map_err(|e| Error::Prompt(e.to_string()))?;
        let (manual_code, returned_state) = parse_google_authorization_input(&manual_input);
        if let Some(returned_state) = returned_state
            && returned_state != state
        {
            return Err(Error::Auth(
                "State mismatch. Please restart the Antigravity login.".to_string(),
            ));
        }
        code = manual_code;
    }

    let code = code.ok_or_else(|| Error::Auth("Authorization code is required.".to_string()))?;

    println!("{}", "Exchanging authorization code for tokens...".yellow());

    // Exchange code for tokens
    let token_response = client
        .post(GOOGLE_TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &get_antigravity_client_id()),
            ("client_secret", &get_antigravity_client_secret()),
            ("code", code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
        ])
        .send()
        .await
        .map_err(|e| Error::Auth(e.to_string()))?;

    if !token_response.status().is_success() {
        let status = token_response.status();
        let body = token_response.text().await.unwrap_or_default();
        return Err(Error::Auth(format!(
            "Token exchange failed: {} - {}",
            status, body
        )));
    }

    let token: GoogleTokenResponse = token_response
        .json()
        .await
        .map_err(|e| Error::Auth(format!("Failed to parse token response: {}", e)))?;

    let refresh_token = token.refresh_token.clone().unwrap_or_default();
    if refresh_token.is_empty() {
        return Err(Error::Auth(
            "Google did not return a refresh token. Please try again.".to_string(),
        ));
    }

    let expires_at = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("system time before Unix epoch")
        .as_millis() as u64
        + (token.expires_in * 1000);

    // Fetch user email
    println!("{}", "Fetching user information...".yellow());
    let user_info: GoogleUserInfo = client
        .get(GOOGLE_USERINFO_URL)
        .header("Authorization", format!("Bearer {}", token.access_token))
        .send()
        .await
        .map_err(|e| Error::Auth(format!("Failed to fetch user info: {}", e)))?
        .json()
        .await
        .map_err(|e| Error::Auth(format!("Failed to parse user info: {}", e)))?;

    println!(
        "{}",
        format!("Authenticated as: {}", user_info.email).green()
    );

    // Discover tier and project
    println!("{}", "Discovering Code Assist configuration...".yellow());
    let (tier, project_id) = discover_antigravity_config(&client, &token.access_token).await?;

    if let Some(ref t) = tier {
        println!("{}", format!("Tier: {}", t).green());
    }
    if let Some(ref p) = project_id {
        println!("{}", format!("Project: {}", p).green());
    }

    let antigravity_config = AntigravityProviderConfig {
        enabled: true,
        access_token: token.access_token,
        refresh_token,
        expires_at,
        email: Some(user_info.email),
        tier,
        project_id,
    };

    let mut config = ConfigFile::load()?;
    config.set_provider(
        target_provider_id.clone(),
        ProviderConfig::Antigravity(antigravity_config),
    );
    config.save()?;

    println!(
        "{}",
        format!(
            "✓ Antigravity account '{}' connected. You can now select Antigravity models.",
            target_provider_id
        )
        .green()
        .bold()
    );

    Ok(())
}

fn parse_google_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }

    if let Ok(url) = Url::parse(value) {
        let code = url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned());
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned());
        return (code, state);
    }

    // Try parsing as query string
    if value.contains("code=") {
        let mut code = None;
        let mut state = None;
        for pair in value.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                let decoded = urlencoding::decode(v).unwrap_or_else(|_| v.into());
                match k {
                    "code" => code = Some(decoded.into_owned()),
                    "state" => state = Some(decoded.into_owned()),
                    _ => {}
                }
            }
        }
        return (code, state);
    }

    // Assume raw code
    (Some(value.to_string()), None)
}

async fn discover_antigravity_config(
    client: &Client,
    access_token: &str,
) -> Result<(Option<String>, Option<String>)> {
    let discovery_body = serde_json::json!({
        "cloudaicompanionProject": null,
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });

    let response = client
        .post(ANTIGRAVITY_DISCOVERY_URL)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&discovery_body)
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let data: LoadCodeAssistResponse =
                resp.json().await.unwrap_or(LoadCodeAssistResponse {
                    current_tier: None,
                    cloudaicompanion_project: None,
                });
            Ok((data.current_tier, data.cloudaicompanion_project))
        }
        Ok(resp) if resp.status() == 404 => {
            // User not onboarded, try to onboard them
            println!(
                "{}",
                "First time setup - onboarding to Code Assist...".yellow()
            );
            onboard_antigravity_user(client, access_token).await
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // Non-fatal - just log and continue without tier/project
            println!(
                "{}",
                format!(
                    "Warning: Could not discover Code Assist config: {} - {}",
                    status, body
                )
                .yellow()
            );
            Ok((None, None))
        }
        Err(e) => {
            println!(
                "{}",
                format!("Warning: Could not discover Code Assist config: {}", e).yellow()
            );
            Ok((None, None))
        }
    }
}

async fn onboard_antigravity_user(
    client: &Client,
    access_token: &str,
) -> Result<(Option<String>, Option<String>)> {
    let onboard_body = serde_json::json!({
        "tierId": "free-tier",
        "cloudaicompanionProject": null,
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });

    let response = client
        .post(ANTIGRAVITY_ONBOARD_URL)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&onboard_body)
        .send()
        .await
        .map_err(|e| Error::Auth(format!("Onboarding failed: {}", e)))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        println!(
            "{}",
            format!("Warning: Onboarding returned {}: {}", status, body).yellow()
        );
        return Ok((Some("free-tier".to_string()), None));
    }

    // Poll for completion (simplified - just wait a bit and retry discovery)
    println!("{}", "Waiting for onboarding to complete...".yellow());
    sleep(Duration::from_secs(3)).await;

    // Retry discovery
    let discovery_body = serde_json::json!({
        "cloudaicompanionProject": null,
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });

    let response = client
        .post(ANTIGRAVITY_DISCOVERY_URL)
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .json(&discovery_body)
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            let data: LoadCodeAssistResponse =
                resp.json().await.unwrap_or(LoadCodeAssistResponse {
                    current_tier: None,
                    cloudaicompanion_project: None,
                });
            Ok((
                data.current_tier.or(Some("free-tier".to_string())),
                data.cloudaicompanion_project,
            ))
        }
        _ => Ok((Some("free-tier".to_string()), None)),
    }
}
