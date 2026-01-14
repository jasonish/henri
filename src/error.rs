// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use thiserror::Error;

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("Authentication error: {0}")]
    Auth(String),

    #[error(
        "Refresh token expired or revoked. Please run `henri provider add` to re-authenticate."
    )]
    RefreshTokenExpired,

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Prompt error: {0}")]
    Prompt(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("API error: {status} - {message}")]
    Api { status: u16, message: String },

    #[error("Retryable API error: {status} - {message}")]
    Retryable { status: u16, message: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Session corrupted: {0}")]
    SessionCorrupted(String),

    #[error("Interrupted by user")]
    Interrupted,

    #[error("MCP error: {0}")]
    Mcp(String),

    #[error("LSP error: {0}")]
    Lsp(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Returns a concise message suitable for display in the TUI.
    /// For API errors, returns just the response body without the status prefix.
    /// If the message is JSON, it will be pretty-printed.
    pub(crate) fn tui_message(&self) -> String {
        match self {
            Error::Api { message, .. } | Error::Retryable { message, .. } => {
                // Try to pretty-print if it's JSON
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(message) {
                    serde_json::to_string_pretty(&json).unwrap_or_else(|_| message.clone())
                } else {
                    message.clone()
                }
            }
            other => other.to_string(),
        }
    }

    /// Check if this error is retryable (server overloaded, timeout, etc.)
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(self, Error::Retryable { .. } | Error::Http(_))
    }
}

pub(crate) type Result<T> = std::result::Result<T, Error>;
