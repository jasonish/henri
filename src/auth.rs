use anyhow::{Context, Result};
use colored::Colorize;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

use crate::config::{Config, GitHubCopilotConfig};

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
    println!("Select a provider to login:");
    println!("1. GitHub Copilot");
    println!("2. Claude (not implemented)");

    print!("Enter your choice (1-2): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let choice = input.trim();

    match choice {
        "1" => {
            login_github_copilot().await?;
        }
        "2" => {
            println!(
                "{}",
                "Claude authentication is not yet implemented.".yellow()
            );
        }
        _ => {
            println!("{}", "Invalid choice. Please select 1 or 2.".red());
        }
    }

    Ok(())
}

async fn login_github_copilot() -> Result<()> {
    let client = Client::new();

    println!("{}", "Starting GitHub Copilot authentication...".blue());

    // Step 1: Request device code
    let device_code_response = request_device_code(&client).await?;

    println!(
        "\n{}",
        "Please visit the following URL and enter the code:".green()
    );
    println!("{}", device_code_response.verification_uri.cyan());
    println!(
        "{}",
        format!("Code: {}", device_code_response.user_code)
            .yellow()
            .bold()
    );

    // Step 2: Poll for token
    let token_response = poll_for_token(&client, &device_code_response).await?;

    // Step 3: Save to config
    let expires_at = if let Some(expires_in) = token_response.expires_in {
        let current_time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        // Convert to i64 safely - tokens typically don't expire beyond i64::MAX seconds
        #[allow(clippy::cast_possible_wrap)]
        Some(current_time.saturating_add(expires_in).min(i64::MAX as u64) as i64)
    } else {
        None
    };

    let github_config = GitHubCopilotConfig {
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        expires_at,
        copilot_token: None, // Will be obtained on first use
        copilot_expires_at: None,
    };

    let mut config = Config::load()?;
    config.set_github_copilot(github_config);
    config.save()?;

    println!(
        "{}",
        "✓ Successfully authenticated with GitHub Copilot!".green()
    );

    Ok(())
}

async fn request_device_code(client: &Client) -> Result<DeviceCodeResponse> {
    let response = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .header("User-Agent", "coder/0.1.0")
        .form(&[("client_id", "Iv1.b507a08c87ecfe98"), ("scope", "copilot")])
        .send()
        .await
        .context("Failed to request device code")?;

    if !response.status().is_success() {
        anyhow::bail!("GitHub API returned error: {}", response.status());
    }

    let device_code: DeviceCodeResponse = response
        .json()
        .await
        .context("Failed to parse device code response")?;

    Ok(device_code)
}

async fn poll_for_token(
    client: &Client,
    device_code: &DeviceCodeResponse,
) -> Result<TokenResponse> {
    let mut interval = Duration::from_secs(device_code.interval);
    let timeout = Duration::from_secs(device_code.expires_in);
    let start_time = std::time::Instant::now();

    println!("\n{}", "Waiting for authentication...".blue());

    loop {
        if start_time.elapsed() > timeout {
            anyhow::bail!("Authentication timed out");
        }

        sleep(interval).await;

        let response = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .header("User-Agent", "coder/0.1.0")
            .form(&[
                ("client_id", "Iv1.b507a08c87ecfe98"),
                ("device_code", &device_code.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .context("Failed to poll for token")?;

        if !response.status().is_success() {
            anyhow::bail!("GitHub API returned error: {}", response.status());
        }

        let text = response.text().await?;

        // Try to parse as success response first
        if let Ok(token_response) = serde_json::from_str::<TokenResponse>(&text) {
            return Ok(token_response);
        }

        // Try to parse as error response
        if let Ok(error_response) = serde_json::from_str::<TokenError>(&text) {
            match error_response.error.as_str() {
                "authorization_pending" => {
                    print!(".");
                    io::stdout().flush()?;
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
