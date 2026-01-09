// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, TimeZone, Utc};
use colored::Colorize;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::provider::anthropic::{ANTHROPIC_BETA, ANTHROPIC_VERSION, API_URL, AnthropicClient};

/// Rate limit information from Anthropic API
#[derive(Debug, Default)]
pub(crate) struct RateLimits {
    pub unified_5h_reset: Option<i64>,
    pub unified_5h_utilization: Option<f64>,
    pub unified_7d_reset: Option<i64>,
    pub unified_7d_utilization: Option<f64>,
    pub unified_7d_sonnet_reset: Option<i64>,
    pub unified_7d_sonnet_utilization: Option<f64>,
}

impl RateLimits {
    fn format_reset_time(timestamp: i64) -> String {
        let dt: DateTime<Utc> = Utc.timestamp_opt(timestamp, 0).unwrap();
        let now = Utc::now();
        let duration = dt.signed_duration_since(now);

        if duration.num_seconds() <= 0 {
            return "now".to_string();
        }

        let hours = duration.num_hours();
        let minutes = (duration.num_minutes() % 60).abs();

        let duration_str = if hours > 24 {
            let days = hours / 24;
            let remaining_hours = hours % 24;
            format!("{}d {}h", days, remaining_hours)
        } else if hours > 0 {
            format!("{}h {}m", hours, minutes)
        } else {
            format!("{}m", minutes)
        };

        let local_time: DateTime<chrono::Local> = dt.into();
        let time_str = local_time.format("%a %b %d %H:%M").to_string();

        format!("{} ({})", duration_str, time_str)
    }

    fn format_utilization(util: f64) -> String {
        format!("{:.1}%", util * 100.0)
    }

    fn utilization_bar(util: f64) -> String {
        let width = 20;
        let filled = ((util * width as f64).round() as usize).min(width);
        let empty = width - filled;

        let bar = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
        if util < 0.5 {
            bar.green().to_string()
        } else if util < 0.8 {
            bar.yellow().to_string()
        } else {
            bar.red().to_string()
        }
    }

    pub(crate) fn display(&self) {
        println!("\n{}", "Anthropic Rate Limits".bold());
        println!();

        if let (Some(util), Some(reset)) = (self.unified_5h_utilization, self.unified_5h_reset) {
            println!(
                "  5-hour limit:   {} {} (resets in {})",
                Self::utilization_bar(util),
                Self::format_utilization(util),
                Self::format_reset_time(reset)
            );
        }

        if let (Some(util), Some(reset)) = (self.unified_7d_utilization, self.unified_7d_reset) {
            println!(
                "  7-day limit:    {} {} (resets in {})",
                Self::utilization_bar(util),
                Self::format_utilization(util),
                Self::format_reset_time(reset)
            );
        }

        if let (Some(util), Some(reset)) = (
            self.unified_7d_sonnet_utilization,
            self.unified_7d_sonnet_reset,
        ) {
            println!(
                "  7d Sonnet:      {} {} (resets in {})",
                Self::utilization_bar(util),
                Self::format_utilization(util),
                Self::format_reset_time(reset)
            );
        }

        println!();
    }
}

#[derive(Serialize)]
struct MinimalRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    system: Vec<serde_json::Value>,
    max_tokens: u32,
}

/// Fetch rate limit information from Anthropic API by making a minimal request
pub async fn fetch_anthropic_rate_limits() -> Result<RateLimits> {
    let client = AnthropicClient::try_new()?;
    let access_token = client.get_access_token().await?;

    let request = MinimalRequest {
        model: "claude-sonnet-4-5".to_string(),
        messages: vec![serde_json::json!({
            "role": "user",
            "content": "ok"
        })],
        system: vec![
            serde_json::json!({"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}),
        ],
        max_tokens: 5,
    };

    let response = client
        .http_client()
        .post(API_URL)
        .header("content-type", "application/json")
        .header("user-agent", "claude-cli/2.1.2 (external, cli)")
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("anthropic-beta", ANTHROPIC_BETA)
        .header("authorization", format!("Bearer {}", access_token))
        .json(&request)
        .send()
        .await
        .map_err(|e| Error::Auth(e.to_string()))?;

    // Extract headers before consuming the response
    // Rate limit headers are present even in error responses (e.g., 429 rate limit exceeded)
    let headers = response.headers().clone();

    let mut limits = RateLimits::default();

    if let Some(val) = headers.get("anthropic-ratelimit-unified-5h-reset")
        && let Ok(s) = val.to_str()
    {
        limits.unified_5h_reset = s.parse().ok();
    }
    if let Some(val) = headers.get("anthropic-ratelimit-unified-5h-utilization")
        && let Ok(s) = val.to_str()
    {
        limits.unified_5h_utilization = s.parse().ok();
    }
    if let Some(val) = headers.get("anthropic-ratelimit-unified-7d-reset")
        && let Ok(s) = val.to_str()
    {
        limits.unified_7d_reset = s.parse().ok();
    }
    if let Some(val) = headers.get("anthropic-ratelimit-unified-7d-utilization")
        && let Ok(s) = val.to_str()
    {
        limits.unified_7d_utilization = s.parse().ok();
    }
    if let Some(val) = headers.get("anthropic-ratelimit-unified-7d_sonnet-reset")
        && let Ok(s) = val.to_str()
    {
        limits.unified_7d_sonnet_reset = s.parse().ok();
    }
    if let Some(val) = headers.get("anthropic-ratelimit-unified-7d_sonnet-utilization")
        && let Ok(s) = val.to_str()
    {
        limits.unified_7d_sonnet_utilization = s.parse().ok();
    }

    // Consume the response body to complete the request
    // We don't care about success/error - we only need the rate limit headers
    let _ = response.text().await;

    Ok(limits)
}

#[derive(Default)]
pub(crate) struct NetworkStats {
    tx_bytes: AtomicU64,
    rx_bytes: AtomicU64,
}

impl NetworkStats {
    pub(crate) fn record_tx(&self, bytes: u64) {
        self.tx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn record_rx(&self, bytes: u64) {
        self.rx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn tx_bytes(&self) -> u64 {
        self.tx_bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn rx_bytes(&self) -> u64 {
        self.rx_bytes.load(Ordering::Relaxed)
    }

    pub(crate) fn clear(&self) {
        self.tx_bytes.store(0, Ordering::Relaxed);
        self.rx_bytes.store(0, Ordering::Relaxed);
    }
}

static NETWORK_STATS: std::sync::OnceLock<NetworkStats> = std::sync::OnceLock::new();

pub(crate) fn network_stats() -> &'static NetworkStats {
    NETWORK_STATS.get_or_init(NetworkStats::default)
}

#[derive(Default)]
pub(crate) struct Usage {
    last_input_tokens: AtomicU64,
    last_output_tokens: AtomicU64,
    last_cache_creation_tokens: AtomicU64,
    last_cache_read_tokens: AtomicU64,
    total_input_tokens: AtomicU64,
    total_output_tokens: AtomicU64,
    total_cache_creation_tokens: AtomicU64,
    total_cache_read_tokens: AtomicU64,
    // Turn-level tracking: accumulated across all API calls in a single user interaction
    turn_total_tokens: AtomicU64,
    turn_cache_creation_tokens: AtomicU64,
    turn_cache_read_tokens: AtomicU64,
}

impl Usage {
    /// Reset turn counters. Call this at the start of each user interaction.
    pub(crate) fn start_turn(&self) {
        self.turn_total_tokens.store(0, Ordering::Relaxed);
        self.turn_cache_creation_tokens.store(0, Ordering::Relaxed);
        self.turn_cache_read_tokens.store(0, Ordering::Relaxed);
    }

    pub(crate) fn record_input(&self, tokens: u64) {
        self.last_input_tokens.store(tokens, Ordering::Relaxed);
        self.total_input_tokens.fetch_add(tokens, Ordering::Relaxed);
        self.turn_total_tokens.fetch_add(tokens, Ordering::Relaxed);
    }

    pub(crate) fn record_output(&self, tokens: u64) {
        self.last_output_tokens.store(tokens, Ordering::Relaxed);
        self.total_output_tokens
            .fetch_add(tokens, Ordering::Relaxed);
        self.turn_total_tokens.fetch_add(tokens, Ordering::Relaxed);
    }

    pub(crate) fn add_cache_creation(&self, tokens: u64) {
        self.last_cache_creation_tokens
            .store(tokens, Ordering::Relaxed);
        self.total_cache_creation_tokens
            .fetch_add(tokens, Ordering::Relaxed);
        self.turn_cache_creation_tokens
            .fetch_add(tokens, Ordering::Relaxed);
    }

    pub(crate) fn add_cache_read(&self, tokens: u64) {
        self.last_cache_read_tokens.store(tokens, Ordering::Relaxed);
        self.total_cache_read_tokens
            .fetch_add(tokens, Ordering::Relaxed);
        self.turn_cache_read_tokens
            .fetch_add(tokens, Ordering::Relaxed);
    }

    pub(crate) fn last_input(&self) -> u64 {
        self.last_input_tokens.load(Ordering::Relaxed)
    }

    /// Get total context size (input tokens + cache read tokens)
    pub(crate) fn last_context(&self) -> u64 {
        self.last_input() + self.last_cache_read_tokens.load(Ordering::Relaxed)
    }

    pub(crate) fn turn_total(&self) -> u64 {
        self.turn_total_tokens.load(Ordering::Relaxed)
    }

    pub(crate) fn turn_cache_read(&self) -> u64 {
        self.turn_cache_read_tokens.load(Ordering::Relaxed)
    }

    pub(crate) fn total_input(&self) -> u64 {
        self.total_input_tokens.load(Ordering::Relaxed)
    }

    pub(crate) fn total_output(&self) -> u64 {
        self.total_output_tokens.load(Ordering::Relaxed)
    }

    /// Print a summary of the current turn's usage (accumulated across all API calls)
    pub(crate) fn print_last_usage(&self, context_limit: Option<u64>) {
        // last_input shows the final request's context size
        // turn_total shows total tokens consumed (input + output) across all API calls in this turn
        let input = self.last_input();
        let total = self.turn_total();
        let cache_read = self.turn_cache_read();

        // Build a descriptive summary
        let mut parts = Vec::new();

        if let Some(limit) = context_limit {
            parts.push(format!("context: {}/{}", input, limit));
        } else {
            parts.push(format!("context: {}", input));
        }

        if total > 0 {
            parts.push(format!("total: {}", total));
        }

        if cache_read > 0 {
            parts.push(format!("cache_read: {}", cache_read));
        }

        let summary = parts.join(", ");

        // Print in dim style with blank line before
        println!();
        println!("{}", format!("[{}]", summary).dimmed());
    }
}

static ANTHROPIC_USAGE: std::sync::OnceLock<Usage> = std::sync::OnceLock::new();

pub(crate) fn anthropic() -> &'static Usage {
    ANTHROPIC_USAGE.get_or_init(Usage::default)
}

static ZEN_USAGE: std::sync::OnceLock<Usage> = std::sync::OnceLock::new();

pub(crate) fn zen() -> &'static Usage {
    ZEN_USAGE.get_or_init(Usage::default)
}

static OPENAI_COMPAT_USAGE: std::sync::OnceLock<Usage> = std::sync::OnceLock::new();

pub(crate) fn openai_compat() -> &'static Usage {
    OPENAI_COMPAT_USAGE.get_or_init(Usage::default)
}

static OPENAI_USAGE: std::sync::OnceLock<Usage> = std::sync::OnceLock::new();

pub(crate) fn openai() -> &'static Usage {
    OPENAI_USAGE.get_or_init(Usage::default)
}

static OPENROUTER_USAGE: std::sync::OnceLock<Usage> = std::sync::OnceLock::new();

pub(crate) fn openrouter() -> &'static Usage {
    OPENROUTER_USAGE.get_or_init(Usage::default)
}

static ANTIGRAVITY_USAGE: std::sync::OnceLock<Usage> = std::sync::OnceLock::new();

pub(crate) fn antigravity() -> &'static Usage {
    ANTIGRAVITY_USAGE.get_or_init(Usage::default)
}
