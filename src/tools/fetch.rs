// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use serde::Deserialize;

use super::{Tool, ToolDefinition, ToolResult};
use crate::version::VERSION;

pub(crate) struct Fetch;

#[derive(Debug, Deserialize)]
struct FetchInput {
    url: String,
    /// If true, return raw content without any conversion
    raw: Option<bool>,
}

fn convert_html_to_markdown(html: &str) -> String {
    htmd::convert(html).unwrap_or_else(|_| html.to_string())
}

fn pretty_print_json(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| text.to_string())
}

impl Tool for Fetch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fetch".to_string(),
            description: "Fetch a URL and return its content. HTML is converted to Markdown, JSON is pretty-printed. Use raw=true to skip processing."
                .to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    },
                    "raw": {
                        "type": "boolean",
                        "description": "Return raw content without conversion (default: false)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn execute(
        &self,
        tool_use_id: &str,
        input: serde_json::Value,
        output: &crate::output::OutputContext,
        _services: &crate::services::Services,
    ) -> ToolResult {
        let input: FetchInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return *e,
        };

        if url::Url::parse(&input.url).is_err() {
            return ToolResult::error(tool_use_id, format!("Invalid URL: {}", input.url));
        }

        let client = reqwest::Client::builder()
            .user_agent(format!("henri/{VERSION}"))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        let response = match client.get(&input.url).send().await {
            Ok(r) => r,
            Err(e) => return ToolResult::error(tool_use_id, format!("Failed to fetch URL: {}", e)),
        };

        let status = response.status();
        if !status.is_success() {
            return ToolResult::error(
                tool_use_id,
                format!("HTTP request failed with status: {}", status),
            );
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = match response.text().await {
            Ok(t) => t,
            Err(e) => {
                return ToolResult::error(
                    tool_use_id,
                    format!("Failed to read response body: {}", e),
                );
            }
        };

        let content = if input.raw.unwrap_or(false) {
            body
        } else if content_type.contains("text/html") {
            convert_html_to_markdown(&body)
        } else if content_type.contains("application/json") {
            pretty_print_json(&body)
        } else {
            body
        };

        for line in content.lines() {
            crate::output::emit_tool_output(output, &format!("{}\n", line));
        }

        let line_count = content.lines().count();
        let byte_count = content.len();
        let summary = format!("Fetched {} lines, {} bytes", line_count, byte_count);
        ToolResult::success(tool_use_id, content).with_summary(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fetch_invalid_url() {
        let tool = Fetch;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({ "url": "not-a-url" }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("Invalid URL"));
    }

    #[test]
    fn test_convert_html_to_markdown() {
        let html = "<h1>Hello</h1><p>World</p>";
        let md = convert_html_to_markdown(html);
        assert!(md.contains("Hello"));
        assert!(md.contains("World"));
    }

    #[test]
    fn test_pretty_print_json() {
        let json = r#"{"key":"value","num":42}"#;
        let pretty = pretty_print_json(json);
        assert!(pretty.contains("\"key\": \"value\""));
        assert!(pretty.contains("\"num\": 42"));
    }

    #[test]
    fn test_pretty_print_invalid_json() {
        let not_json = "this is not json";
        let result = pretty_print_json(not_json);
        assert_eq!(result, not_json);
    }
}
