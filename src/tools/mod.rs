// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

mod bash;
mod fetch;
mod file_edit;
mod file_read;
mod file_write;
mod sandbox;

pub(crate) use bash::Bash;
pub(crate) use fetch::Fetch;
pub(crate) use file_edit::FileEdit;
pub(crate) use file_read::FileRead;
pub(crate) use file_write::FileWrite;

use serde::{Deserialize, Serialize, de};

pub(crate) const READ_ONLY_DISABLED_TOOLS: &[&str] = &["file_edit", "file_write"];

/// Built-in tool names and their human-readable descriptions.
/// This is the single source of truth for tool metadata used in menus and UIs.
pub(crate) const TOOL_INFO: &[(&str, &str)] = &[
    ("bash", "Execute shell commands"),
    ("fetch", "Fetch URLs and convert to markdown"),
    ("file_edit", "Edit files with string replacements"),
    ("file_read", "Read file contents"),
    ("file_write", "Write content to files"),
];

const BUILTIN_TOOL_ALIASES: &[(&str, &str)] = &[
    ("edit", "file_edit"),
    ("read", "file_read"),
    ("cat", "file_read"),
    ("write", "file_write"),
];

/// Tool definition for AI model consumption
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Result of executing a tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolResult {
    pub tool_use_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub content: String,
    /// Optional base64-encoded binary data (e.g., for images).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// MIME type for the data field (e.g., "image/png").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ToolResult {
    pub(crate) fn success(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            kind: "tool_result".to_string(),
            content: content.into(),
            is_error: false,
            exit_code: None,
            summary: None,
            data: None,
            mime_type: None,
        }
    }

    pub(crate) fn error(tool_use_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            kind: "tool_result".to_string(),
            content: message.into(),
            is_error: true,
            exit_code: None,
            summary: None,
            data: None,
            mime_type: None,
        }
    }

    pub(crate) fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    pub(crate) fn with_data(mut self, data: impl Into<String>) -> Self {
        self.data = Some(data.into());
        self
    }

    pub(crate) fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }
}

/// Trait that all tools must implement
pub(crate) trait Tool: Send + Sync {
    /// Returns the tool definition for AI consumption
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool with the given input
    fn execute(
        &self,
        tool_use_id: &str,
        input: serde_json::Value,
        output: &crate::output::OutputContext,
        services: &crate::services::Services,
    ) -> impl std::future::Future<Output = ToolResult> + Send;
}

/// Helper to deserialize tool input and return an error ToolResult on failure
pub(crate) fn deserialize_input<T: serde::de::DeserializeOwned>(
    tool_use_id: &str,
    input: serde_json::Value,
) -> Result<T, Box<ToolResult>> {
    serde_json::from_value(input).map_err(|e| {
        Box::new(ToolResult::error(
            tool_use_id,
            format!("Invalid input: {}", e),
        ))
    })
}

pub(crate) fn deserialize_optional_usize<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => parse_usize_value(&value)
            .map(Some)
            .map_err(de::Error::custom),
    }
}

pub(crate) fn deserialize_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: de::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => parse_u64_value(&value).map(Some).map_err(de::Error::custom),
    }
}

pub(crate) fn parse_usize_value(value: &serde_json::Value) -> Result<usize, String> {
    let parsed = parse_u64_value(value)?;
    if parsed > usize::MAX as u64 {
        Err("value is too large".to_string())
    } else {
        Ok(parsed as usize)
    }
}

pub(crate) fn parse_u64_value(value: &serde_json::Value) -> Result<u64, String> {
    match value {
        serde_json::Value::Number(num) => {
            if let Some(n) = num.as_u64() {
                Ok(n)
            } else if let Some(n) = num.as_i64() {
                if n < 0 {
                    Err("value must be a non-negative integer".to_string())
                } else {
                    Ok(n as u64)
                }
            } else if let Some(n) = num.as_f64() {
                if n.is_finite() && n >= 0.0 && n.fract() == 0.0 {
                    if n > u64::MAX as f64 {
                        Err("value is too large".to_string())
                    } else {
                        Ok(n as u64)
                    }
                } else {
                    Err("value must be a non-negative integer".to_string())
                }
            } else {
                Err("value must be a non-negative integer".to_string())
            }
        }
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err("value must be a non-negative integer".to_string());
            }
            trimmed
                .parse::<u64>()
                .map_err(|_| "value must be a non-negative integer".to_string())
        }
        _ => Err("value must be a non-negative integer".to_string()),
    }
}

pub(crate) fn optional_usize_from_value(value: Option<&serde_json::Value>) -> Option<usize> {
    value.and_then(|v| parse_usize_value(v).ok())
}

pub(crate) fn canonicalize_builtin_tool_name(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "bash" => Some("bash"),
        "fetch" => Some("fetch"),
        "file_edit" => Some("file_edit"),
        "file_read" => Some("file_read"),
        "file_write" => Some("file_write"),
        _ => BUILTIN_TOOL_ALIASES
            .iter()
            .find_map(|(alias, canonical)| (*alias == lower).then_some(*canonical)),
    }
}

/// Validate that a path exists, returning an error ToolResult if not
pub(crate) fn validate_path_exists(
    tool_use_id: &str,
    path: &std::path::Path,
    path_str: &str,
) -> Result<(), Box<ToolResult>> {
    if !path.exists() {
        Err(Box::new(ToolResult::error(
            tool_use_id,
            format!("Path not found: {}", path_str),
        )))
    } else {
        Ok(())
    }
}

/// Validate that a path is a file, returning an error ToolResult if not
pub(crate) fn validate_is_file(
    tool_use_id: &str,
    path: &std::path::Path,
    path_str: &str,
) -> Result<(), Box<ToolResult>> {
    if !path.is_file() {
        Err(Box::new(ToolResult::error(
            tool_use_id,
            format!("Path is not a file: {}", path_str),
        )))
    } else {
        Ok(())
    }
}

/// Expand `~` to the user's home directory.
///
/// Supports:
/// - `~` -> home directory
/// - `~/path` -> home directory / path
pub(crate) fn expand_tilde(path: &str) -> String {
    // Only expand if path starts with ~ (and is exactly ~ or ~/...)
    if !path.starts_with('~') {
        return path.to_string();
    }

    // If it's exactly ~, return home directory
    if path == "~" {
        return dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
    }

    // If it starts with ~/, expand ~ to home directory
    if let Some(home) = dirs::home_dir() {
        // Skip the ~ and use the rest of the path
        let rest = &path[1..];
        // home.join() doesn't work well with absolute paths like /foo, so
        // we need to handle this by stripping the leading slash from rest
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        return home.join(rest).to_string_lossy().to_string();
    }

    // Couldn't determine home directory, return original
    path.to_string()
}

/// Collapse the user's home directory to `~` for display purposes.
fn collapse_home_for_display(path: &str) -> String {
    // If the input already uses ~, don't expand and re-collapse it.
    if path.starts_with('~') {
        return path.to_string();
    }

    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };

    let path = std::path::Path::new(path);
    if path == home {
        return "~".to_string();
    }

    if let Ok(rest) = path.strip_prefix(&home) {
        if rest.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display())
        }
    } else {
        path.to_string_lossy().to_string()
    }
}

/// Notify the LSP about a file change and return diagnostics.
///
/// This handles:
/// 1. Checking if LSP handles this file type
/// 2. Notifying the LSP of the file change
/// 3. Emitting info messages when new LSP servers are activated
/// 4. Waiting for and returning diagnostics
///
/// Returns an empty Vec if LSP doesn't handle this file.
pub(crate) async fn notify_lsp_and_get_diagnostics(
    path: &std::path::Path,
    content: &str,
    services: &crate::services::Services,
    output: &crate::output::OutputContext,
) -> Vec<crate::lsp::FileDiagnostic> {
    if !services.lsp.handles_file(path).await {
        return Vec::new();
    }

    if let Ok(started_servers) = services.lsp.notify_file_changed(path, content).await {
        for server in &started_servers {
            let extensions = server.file_extensions.join(", ");
            output.emit(crate::output::OutputEvent::Info(format!(
                "[LSP activated: {} ({})]",
                server.name, extensions
            )));
        }
    }

    services.lsp.get_diagnostics_with_wait(path).await
}

/// Format a message with LSP diagnostics appended.
///
/// This handles:
/// 1. Emitting a diagnostic summary as an info message
/// 2. Returning the message with diagnostics appended
///
/// If there are no diagnostics, returns the original message unchanged.
pub(crate) fn format_message_with_diagnostics(
    msg: String,
    diagnostics: &[crate::lsp::FileDiagnostic],
    output: &crate::output::OutputContext,
) -> String {
    if diagnostics.is_empty() {
        return msg;
    }

    if let Some(summary) = crate::lsp::diagnostic_summary(diagnostics) {
        output.emit(crate::output::OutputEvent::Info(summary));
    }
    format!("{}{}", msg, crate::lsp::format_diagnostics(diagnostics))
}

/// Generates a one-liner description for a tool call (used for UI display)
pub(crate) fn format_tool_call_description(tool_name: &str, input: &serde_json::Value) -> String {
    let tool_name = canonicalize_builtin_tool_name(tool_name).unwrap_or(tool_name);
    match tool_name {
        "bash" => {
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("command")
                .trim();

            // Tool-call banners render best as a single line. If the model provides a multi-line
            // script, render the full command while keeping it on one line by replacing
            // newlines with a visible separator.
            if command.contains('\n') {
                let lines: Vec<&str> = command
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .collect();

                // Keep the banner to a single terminal line by making newlines explicit.
                // Use an ASCII-friendly marker so it renders everywhere.
                let preview = lines.join(" \\n ");

                format!("Running bash: {}", preview)
            } else {
                format!("Running: {}", command)
            }
        }
        "file_read" => {
            let filename = input
                .get("filename")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            let filename = collapse_home_for_display(filename);
            let offset = optional_usize_from_value(input.get("offset"));
            let limit = optional_usize_from_value(input.get("limit"));
            match (offset, limit) {
                (Some(o), Some(l)) => format!("Reading {} (offset: {}, limit: {})", filename, o, l),
                (Some(o), None) => format!("Reading {} (offset: {})", filename, o),
                (None, Some(l)) => format!("Reading {} (limit: {})", filename, l),
                (None, None) => format!("Reading {}", filename),
            }
        }
        "file_edit" => {
            let filepath = input
                .get("filePath")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            let filepath = collapse_home_for_display(filepath);
            format!("Editing {}", filepath)
        }
        "file_write" => {
            let filepath = input
                .get("filePath")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            let filepath = collapse_home_for_display(filepath);
            format!("Writing {}", filepath)
        }
        "fetch" => {
            let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("url");
            format!("Fetching {}", url)
        }
        name if name.starts_with("mcp_") => {
            // MCP tools: format as "tool_name via server_name"
            // Format: mcp_{server}_{tool}
            let parts: Vec<&str> = name
                .strip_prefix("mcp_")
                .unwrap_or(name)
                .splitn(2, '_')
                .collect();
            if parts.len() == 2 {
                format!("Calling {} via MCP ({})", parts[1], parts[0])
            } else {
                format!("Calling MCP tool: {}", name)
            }
        }
        _ => {
            format!("Calling tool: {}", tool_name)
        }
    }
}

/// Get all available tool definitions (built-in only)
/// Takes config parameters to avoid redundant config loading.
pub(crate) fn builtin_definitions(
    disabled_tools: &[String],
    read_only: bool,
) -> Vec<ToolDefinition> {
    let is_disabled = |name: &str| {
        disabled_tools.iter().any(|t| t == name)
            || (read_only && READ_ONLY_DISABLED_TOOLS.contains(&name))
    };

    let mut tools = Vec::new();

    if !is_disabled("bash") {
        tools.push(Bash.definition());
    }
    if !is_disabled("fetch") {
        tools.push(Fetch.definition());
    }
    if !is_disabled("file_edit") {
        tools.push(FileEdit.definition());
    }
    if !is_disabled("file_read") {
        tools.push(FileRead.definition());
    }
    if !is_disabled("file_write") {
        tools.push(FileWrite.definition());
    }
    tools
}

/// Get all available tool definitions including MCP tools
pub(crate) async fn all_definitions(services: &crate::services::Services) -> Vec<ToolDefinition> {
    // Load config once and extract all needed values
    let config = crate::config::ConfigFile::load().unwrap_or_default();
    let mut defs = builtin_definitions(&config.disabled_tools, services.is_read_only());
    let mcp_defs = services.mcp.all_tool_definitions().await;
    defs.extend(mcp_defs);
    defs
}

/// Execute a tool by name
pub(crate) async fn execute(
    name: &str,
    tool_use_id: &str,
    input: serde_json::Value,
    output: &crate::output::OutputContext,
    services: &crate::services::Services,
) -> Option<ToolResult> {
    // Load config once and check all conditions
    let config = crate::config::ConfigFile::load().unwrap_or_default();
    let canonical_name = canonicalize_builtin_tool_name(name);

    if let Some(name) = canonical_name {
        // Check if tool is disabled
        if config.disabled_tools.iter().any(|t| t == name) {
            return Some(ToolResult::error(
                tool_use_id,
                format!("Tool '{}' is disabled in configuration", name),
            ));
        }

        if services.is_read_only() && READ_ONLY_DISABLED_TOOLS.contains(&name) {
            return Some(ToolResult::error(
                tool_use_id,
                format!("Read-only mode: tool '{}' is disabled", name),
            ));
        }

        // First try built-in tools (including aliases)
        match name {
            "bash" => return Some(Bash.execute(tool_use_id, input, output, services).await),
            "fetch" => return Some(Fetch.execute(tool_use_id, input, output, services).await),
            "file_edit" => {
                return Some(FileEdit.execute(tool_use_id, input, output, services).await);
            }
            "file_read" => {
                return Some(FileRead.execute(tool_use_id, input, output, services).await);
            }
            "file_write" => {
                return Some(
                    FileWrite
                        .execute(tool_use_id, input, output, services)
                        .await,
                );
            }
            _ => {}
        }
    }

    // Try MCP tools
    services.mcp.execute_tool(name, tool_use_id, input).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde_exact_tilde() {
        let home = dirs::home_dir().map(|p| p.to_string_lossy().to_string());
        if let Some(h) = home {
            assert_eq!(expand_tilde("~"), h);
        }
    }

    #[test]
    fn test_expand_tilde_with_path() {
        let home = dirs::home_dir().map(|p| p.to_string_lossy().to_string());
        if let Some(h) = home {
            assert_eq!(expand_tilde("~/Documents"), format!("{}/Documents", h));
            assert_eq!(expand_tilde("~/foo/bar.txt"), format!("{}/foo/bar.txt", h));
        }
    }

    #[test]
    fn test_expand_tilde_no_change() {
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
        assert_eq!(expand_tilde("relative/path"), "relative/path");
        assert_eq!(expand_tilde(""), "");
    }

    #[test]
    fn test_expand_tilde_in_middle() {
        // ~ should not be expanded in the middle of paths
        let result = expand_tilde("foo/~bar");
        assert_eq!(result, "foo/~bar");
    }

    #[test]
    fn test_builtin_definitions_read_only() {
        let defs = builtin_definitions(&[], true);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(!names.contains(&"file_write"));
        assert!(!names.contains(&"file_edit"));
        assert!(names.contains(&"file_read"));
        assert!(names.contains(&"bash"));
    }
}
