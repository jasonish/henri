// SPDX-License-Identifier: AGPL-3.0-only

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use colored::Colorize;
use inquire::Select;
use rand::{Rng, thread_rng};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime};
use tokio::time::sleep;

use crate::config::{AnthropicConfig, Config, GitHubCopilotConfig, OpenRouterConfig};

#[derive(Debug, Serialize, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    token_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenError {
    error: String,
    error_description: Option<String>,
}

pub async fn login() -> Result<()> {
    let providers = vec!["GitHub Copilot", "OpenRouter", "Anthropic"];

    let selection = Select::new("Select a provider to login:", providers)
        .prompt()
        .context("Failed to select provider")?;

    match selection {
        "GitHub Copilot" => {
            login_github_copilot().await?;
        }
        "OpenRouter" => {
            login_open_router().await?;
        }
        "Anthropic" => {
            login_anthropic().await?;
        }
        _ => unreachable!(),
    }

    Ok(())
}

async fn login_github_copilot() -> Result<()> {
    let client = Client::new();

    // Step 1: Request device code
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
        .context("Failed to request device code")?;

    if !device_response.status().is_success() {
        let status = device_response.status();
        let body = device_response.text().await.unwrap_or_default();
        anyhow::bail!("Device code request failed: {} - {}", status, body);
    }

    // Debug: Get the response text first to see what we're receiving
    let response_text = device_response
        .text()
        .await
        .context("Failed to read device code response")?;

    let device_code_response: DeviceCodeResponse = serde_json::from_str(&response_text)
        .with_context(|| format!("Failed to parse device code response: {response_text}"))?;

    // Step 2: Display the code to the user
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

    // Step 3: Poll for token
    let mut interval = Duration::from_secs(device_code_response.interval);
    let expiry = SystemTime::now() + Duration::from_secs(device_code_response.expires_in);

    loop {
        if SystemTime::now() > expiry {
            anyhow::bail!("Device code expired. Please try again.");
        }

        sleep(interval).await;

        let token_response = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .header("User-Agent", "henri-cli")
            .form(&[
                ("client_id", "Iv1.b507a08c87ecfe98"),
                ("device_code", &device_code_response.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .context("Failed to poll for token")?;

        if !token_response.status().is_success() {
            anyhow::bail!("Token request failed: {}", token_response.status());
        }

        let text = token_response.text().await?;

        // Try to parse as successful token response
        if let Ok(token) = serde_json::from_str::<TokenResponse>(&text) {
            // Step 4: Get user info to verify the token works
            let user_response = client
                .get("https://api.github.com/user")
                .header("Authorization", format!("Bearer {}", token.access_token))
                .header("User-Agent", "henri-copilot-cli")
                .send()
                .await
                .context("Failed to get user info")?;

            if !user_response.status().is_success() {
                anyhow::bail!("Failed to verify GitHub token");
            }

            let user_data: serde_json::Value = user_response.json().await?;
            let username = user_data["login"].as_str().unwrap_or("unknown");

            println!(
                "{}",
                format!("✓ Successfully authenticated as {username}").green()
            );

            // Step 5: Save configuration
            let expires_at = token
                .expires_in
                .map(|expires_in| chrono::Utc::now().timestamp() + expires_in as i64);

            let github_config = GitHubCopilotConfig {
                access_token: token.access_token,
                refresh_token: token.refresh_token,
                expires_at,
                copilot_token: None,
                copilot_expires_at: None,
            };

            let mut config = Config::load().unwrap_or_default();
            config.providers.github_copilot = Some(github_config);
            config.save()?;

            println!("{}", "✓ Configuration saved!".green());
            println!(
                "{}",
                "You can now use Henri with GitHub Copilot models.".blue()
            );

            return Ok(());
        }

        // Try to parse as error response
        if let Ok(error_response) = serde_json::from_str::<TokenError>(&text) {
            match error_response.error.as_str() {
                "authorization_pending" => {
                    // Still waiting for user to authorize
                    continue;
                }
                "slow_down" => {
                    interval += Duration::from_secs(5);
                    continue;
                }
                "expired_token" => {
                    anyhow::bail!("Device code expired");
                }
                "access_denied" => {
                    anyhow::bail!("Access denied by user");
                }
                _ => {
                    anyhow::bail!("Unknown error: {}", error_response.error);
                }
            }
        }

        // If we can't parse as either success or error, show the raw response
        anyhow::bail!("Unexpected response format: {}", text);
    }
}

pub async fn test_auth_interactive(verbose: bool) -> Result<()> {
    let config = Config::load()?;

    // Collect configured providers
    let mut providers = Vec::new();

    if config.providers.github_copilot.is_some() {
        providers.push("GitHub Copilot");
    }
    if config.providers.open_router.is_some() {
        providers.push("OpenRouter");
    }
    if config.providers.anthropic.is_some() {
        providers.push("Anthropic");
    }

    if providers.is_empty() {
        println!(
            "{}",
            "No providers configured. Please run 'henri login' first.".yellow()
        );
        return Ok(());
    }

    // If only one provider is configured, test it directly
    let provider = if providers.len() == 1 {
        providers[0]
    } else {
        // Use inquire to select provider
        Select::new("Select a provider to test:", providers)
            .prompt()
            .context("Failed to select provider")?
    };

    match provider {
        "GitHub Copilot" => test_github_copilot_auth(&config, verbose).await,
        "OpenRouter" => test_open_router_auth(&config, verbose).await,
        "Anthropic" => test_anthropic_auth(&config, verbose).await,
        _ => unreachable!(),
    }
}

async fn test_github_copilot_auth(config: &Config, verbose: bool) -> Result<()> {
    println!("{}", "Testing GitHub Copilot authentication...".blue());

    if let Some(github_config) = &config.providers.github_copilot {
        // Check if token is expired
        if let Some(expires_at) = github_config.expires_at {
            let current_time = chrono::Utc::now().timestamp();
            if current_time >= expires_at {
                anyhow::bail!(
                    "GitHub Copilot token has expired. Please run 'henri login' to re-authenticate."
                );
            }
        }

        // Create a temporary LLM client to test the connection
        let mut client = crate::llm::LLMClient::new(github_config.clone(), verbose);

        // Try to get Copilot token (this will test GitHub token exchange)
        match client.get_copilot_token().await {
            Ok(_) => {
                println!("{}", "✓ GitHub authentication is valid".green());
                println!("{}", "✓ Successfully exchanged for Copilot token".green());

                // Try a simple API call
                let test_message = r#"{"messages": [{"role": "user", "content": "Say 'Authentication test successful' and nothing else"}], "model": "gpt-4o", "stream": false}"#;

                match client.send_raw_json_request(test_message, verbose).await {
                    Ok((response, _)) => {
                        println!("{}", "✓ API call successful".green());
                        if verbose {
                            println!("Response: {response}");
                        }
                    }
                    Err(e) => {
                        println!("{}", format!("⚠ API call failed: {e}").yellow());
                    }
                }
            }
            Err(e) => {
                anyhow::bail!("Failed to exchange GitHub token for Copilot token: {}", e);
            }
        }
    } else {
        anyhow::bail!("GitHub Copilot is not configured. Please run 'henri login' first.");
    }

    Ok(())
}

async fn login_open_router() -> Result<()> {
    println!("\n{}", "OpenRouter Authentication".cyan().bold());
    println!("{}", "═".repeat(50).cyan());

    println!("\n{}", "To use OpenRouter, you need an API key.".yellow());
    println!(
        "{}",
        "1. Visit https://openrouter.ai/keys to get your API key".yellow()
    );
    println!("{}", "2. Copy your API key and paste it below".yellow());
    println!();

    let api_key = inquire::Password::new("Enter your OpenRouter API key:")
        .without_confirmation()
        .prompt()
        .context("Failed to read API key")?;

    // Basic validation - check if the key is not empty
    if api_key.trim().is_empty() {
        anyhow::bail!("API key cannot be empty");
    }

    // Create OpenRouter config
    let openrouter_config = OpenRouterConfig {
        api_key: api_key.trim().to_string(),
    };

    // Load existing config or create new one
    let mut config = Config::load().unwrap_or_default();
    config.set_open_router(openrouter_config);

    // Save the config
    config.save()?;

    println!(
        "\n{}",
        "✓ Successfully authenticated with OpenRouter!"
            .green()
            .bold()
    );
    println!(
        "{}",
        "You can now use OpenRouter models by selecting them with /model".green()
    );

    Ok(())
}

async fn test_open_router_auth(config: &Config, verbose: bool) -> Result<()> {
    println!("{}", "Testing OpenRouter authentication...".blue());

    if let Some(openrouter_config) = &config.providers.open_router {
        // Create a temporary OpenRouter client to test the connection
        let mut client = crate::llm::OpenRouterClient::new(openrouter_config.clone(), verbose);

        // Try a simple API call
        let test_message = r#"{"messages": [{"role": "user", "content": "Say 'Authentication test successful' and nothing else"}], "model": "anthropic/claude-sonnet-4", "stream": false}"#;

        match client.send_raw_json_request(test_message, verbose).await {
            Ok((response, _)) => {
                println!("{}", "✓ OpenRouter authentication is valid".green());
                println!("{}", "✓ API call successful".green());
                if verbose {
                    println!("Response: {response}");
                }
            }
            Err(e) => {
                anyhow::bail!("Failed to authenticate with OpenRouter: {}", e);
            }
        }
    } else {
        anyhow::bail!("OpenRouter is not configured. Please run 'henri login openrouter' first.");
    }

    Ok(())
}

async fn login_anthropic() -> Result<()> {
    println!("\n{}", "Anthropic Authentication".cyan().bold());
    println!("{}", "═".repeat(50).cyan());

    login_anthropic_oauth("https://claude.ai/oauth/authorize").await
}

async fn login_anthropic_oauth(auth_url: &str) -> Result<()> {
    let client = Client::new();

    // Generate PKCE challenge and verifier
    let code_verifier = generate_code_verifier();
    let code_challenge = generate_code_challenge(&code_verifier);

    // Build authorization URL
    let client_id = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
    let redirect_uri = "https://console.anthropic.com/oauth/code/callback";
    let scopes = "user:profile user:inference";

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

    // Open browser
    if let Err(e) = open::that(&full_auth_url) {
        println!("{}", format!("Failed to open browser: {e}").yellow());
        println!("Please manually open this URL:");
        println!("{}", full_auth_url.blue().underline());
    }

    println!("\n{}", "Step 2: Authorization Code".green().bold());
    println!("After authorizing, you'll receive a code in the format: code#state");

    let auth_code = inquire::Text::new("Enter the authorization code:")
        .prompt()
        .context("Failed to read authorization code")?;

    // Parse the authorization code (format: code#state)
    let parts: Vec<&str> = auth_code.split('#').collect();
    if parts.len() != 2 {
        anyhow::bail!("Invalid authorization code format. Expected: code#state");
    }

    let (code, state) = (parts[0], parts[1]);

    // Verify state matches our code verifier
    if state != code_verifier {
        anyhow::bail!("State mismatch. This may indicate a security issue.");
    }

    println!("\n{}", "Step 3: Token Exchange".green().bold());
    println!("Exchanging authorization code for tokens...");

    // Exchange code for tokens
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
        .context("Failed to exchange authorization code")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed: {} - {}", status, body);
    }

    let token_response: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse token response")?;

    let access_token = token_response["access_token"]
        .as_str()
        .context("Missing access_token in response")?;
    let refresh_token = token_response["refresh_token"]
        .as_str()
        .context("Missing refresh_token in response")?;
    let expires_in = token_response["expires_in"]
        .as_u64()
        .context("Missing expires_in in response")?;

    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + (expires_in * 1000);

    // OAuth mode - store tokens
    let anthropic_config = AnthropicConfig::OAuth {
        refresh: refresh_token.to_string(),
        access: access_token.to_string(),
        expires,
    };

    // Save configuration
    let mut config = Config::load().unwrap_or_default();
    config.set_anthropic(anthropic_config);
    config.save()?;

    println!(
        "\n{}",
        "✓ Successfully authenticated with Anthropic!"
            .green()
            .bold()
    );
    println!(
        "{}",
        "You can now use Anthropic models by selecting them with /model".green()
    );

    Ok(())
}

fn generate_code_verifier() -> String {
    let mut rng = thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.r#gen()).collect();
    URL_SAFE_NO_PAD.encode(&bytes)
}

fn generate_code_challenge(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    URL_SAFE_NO_PAD.encode(hash)
}

async fn test_anthropic_auth(config: &Config, verbose: bool) -> Result<()> {
    println!("{}", "Testing Anthropic authentication...".blue());

    if let Some(anthropic_config) = &config.providers.anthropic {
        // Create a temporary Anthropic client to test the connection
        let mut client = crate::llm::AnthropicClient::new(anthropic_config.clone(), verbose);

        // Try a simple API call
        let test_message = json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 10,
            "messages": [
                {
                    "role": "user",
                    "content": "Say 'Authentication test successful' and what your name is. Nothing else."
                }
            ],
            "system": [
                {
                    "type": "text",
                    "text": "You are Claude Code, Anthropic's official CLI for Claude.",
                },
                {
                    "type": "text",
                    "text": "Your name is Henri",
                },
            ]
        });
        let test_message = serde_json::to_string(&test_message).unwrap();

        match client.send_raw_json_request(&test_message, verbose).await {
            Ok((response, _)) => {
                println!("{}", "✓ Anthropic authentication is valid".green());
                println!("{}", "✓ API call successful".green());
                if verbose {
                    println!("Response: {response}");
                }
            }
            Err(e) => {
                anyhow::bail!("Failed to authenticate with Anthropic: {}", e);
            }
        }
    } else {
        anyhow::bail!("Anthropic is not configured. Please run 'henri login anthropic' first.");
    }

    Ok(())
}
