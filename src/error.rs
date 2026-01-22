// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use thiserror::Error;

/// Helper module for error handling utilities and formatting.
/// Extract a concise error message from common API error JSON formats.
/// Handles Google, Anthropic, OpenAI, and similar API error structures.
/// Returns the most specific error message available.
fn extract_error_message(json: &serde_json::Value) -> Option<String> {
    // Common patterns:
    // Google: {"error": {"message": "...", "status": "...", "reason": "..."}}
    // Anthropic: {"error": {"type": "...", "message": "..."}}

    // Try nested error object first
    if let Some(error_obj) = json.get("error") {
        // If error is a string directly
        if let Some(msg) = error_obj.as_str() {
            return Some(msg.to_string());
        }

        // If error is an object, extract the message field
        if let Some(msg) = error_obj.get("message").and_then(|v| v.as_str()) {
            // Optionally include status/type for context
            let status = error_obj
                .get("status")
                .and_then(|v| v.as_str())
                .or_else(|| error_obj.get("type").and_then(|v| v.as_str()));

            if let Some(status) = status {
                return Some(format!("{}: {}", status, msg));
            }
            return Some(msg.to_string());
        }
    }

    // Try top-level message
    if let Some(msg) = json.get("message").and_then(|v| v.as_str()) {
        return Some(msg.to_string());
    }

    None
}

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

    #[error("LSP error: {0}")]
    Lsp(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Returns a concise message suitable for display.
    /// For API errors, extracts a human-readable message from JSON error responses.
    pub(crate) fn display_message(&self) -> String {
        match self {
            Error::Api { message, .. } | Error::Retryable { message, .. } => {
                // Try to extract a concise message from JSON error responses
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(message) {
                    extract_error_message(&json).unwrap_or_else(|| message.clone())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_message_google_api_error() {
        // Google API error format with status and message
        let json = r#"{
            "error": {
                "code": 429,
                "message": "No capacity available for model claude-opus-4-5-thinking on the server",
                "status": "RESOURCE_EXHAUSTED"
            }
        }"#;
        let err = Error::Retryable {
            status: 429,
            message: json.to_string(),
        };
        assert_eq!(
            err.display_message(),
            "RESOURCE_EXHAUSTED: No capacity available for model claude-opus-4-5-thinking on the server"
        );
    }

    #[test]
    fn test_display_message_openai_error() {
        // OpenAI error format with type and message
        let json = r#"{"error": {"type": "rate_limit_error", "message": "Rate limit exceeded"}}"#;
        let err = Error::Api {
            status: 429,
            message: json.to_string(),
        };
        assert_eq!(
            err.display_message(),
            "rate_limit_error: Rate limit exceeded"
        );
    }

    #[test]
    fn test_display_message_simple_error_string() {
        // Simple error string (not JSON)
        let err = Error::Retryable {
            status: 500,
            message: "Internal server error".to_string(),
        };
        assert_eq!(err.display_message(), "Internal server error");
    }

    #[test]
    fn test_display_message_message_only() {
        // JSON with just error.message, no status/type
        let json = r#"{"error": {"message": "Something went wrong"}}"#;
        let err = Error::Api {
            status: 500,
            message: json.to_string(),
        };
        assert_eq!(err.display_message(), "Something went wrong");
    }

    #[test]
    fn test_display_message_error_as_string() {
        // JSON where error is a direct string
        let json = r#"{"error": "Rate limit exceeded"}"#;
        let err = Error::Retryable {
            status: 429,
            message: json.to_string(),
        };
        assert_eq!(err.display_message(), "Rate limit exceeded");
    }

    #[test]
    fn test_display_message_top_level_message() {
        // JSON with top-level message
        let json = r#"{"message": "Something failed"}"#;
        let err = Error::Api {
            status: 400,
            message: json.to_string(),
        };
        assert_eq!(err.display_message(), "Something failed");
    }
}
