// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

mod bash;
mod fetch;
mod file_delete;
mod file_edit;
mod file_read;
mod file_write;
mod glob;
mod grep;
mod list_dir;
mod sandbox;
pub(crate) mod todo;

pub(crate) use bash::Bash;
pub(crate) use fetch::Fetch;
pub(crate) use file_delete::FileDelete;
pub(crate) use file_edit::FileEdit;
pub(crate) use file_read::FileRead;
pub(crate) use file_write::FileWrite;
pub(crate) use glob::Glob;
pub(crate) use grep::Grep;
pub(crate) use list_dir::ListDir;
pub(crate) use todo::{TodoItem, TodoRead, TodoStatus, TodoWrite};

use serde::{Deserialize, Serialize};

pub(crate) const READ_ONLY_DISABLED_TOOLS: &[&str] = &["file_delete", "file_edit", "file_write"];

/// Built-in tool names and their human-readable descriptions.
/// This is the single source of truth for tool metadata used in menus and UIs.
///
/// Note: "todo" is a consolidated UI entry representing both `todo_read` and
/// `todo_write` tools. The actual tools exposed to the AI remain separate.
pub(crate) const TOOL_INFO: &[(&str, &str)] = &[
    ("bash", "Execute shell commands"),
    ("fetch", "Fetch URLs and convert to markdown"),
    ("file_delete", "Delete files from the filesystem"),
    ("file_edit", "Edit files with string replacements"),
    ("file_read", "Read file contents"),
    ("file_write", "Write content to files"),
    ("glob", "Find files using glob patterns"),
    ("grep", "Search for patterns in files"),
    ("list_dir", "List directory contents"),
    ("todo", "Todo list tools (read/write)"),
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
        }
    }

    pub(crate) fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
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
) -> Result<T, ToolResult> {
    serde_json::from_value(input)
        .map_err(|e| ToolResult::error(tool_use_id, format!("Invalid input: {}", e)))
}

/// Validate that a path exists, returning an error ToolResult if not
pub(crate) fn validate_path_exists(
    tool_use_id: &str,
    path: &std::path::Path,
    path_str: &str,
) -> Result<(), ToolResult> {
    if !path.exists() {
        Err(ToolResult::error(
            tool_use_id,
            format!("Path not found: {}", path_str),
        ))
    } else {
        Ok(())
    }
}

/// Validate that a path is a directory, returning an error ToolResult if not
pub(crate) fn validate_is_directory(
    tool_use_id: &str,
    path: &std::path::Path,
    path_str: &str,
) -> Result<(), ToolResult> {
    if !path.is_dir() {
        Err(ToolResult::error(
            tool_use_id,
            format!("Path is not a directory: {}", path_str),
        ))
    } else {
        Ok(())
    }
}

/// Validate that a path is a file, returning an error ToolResult if not
pub(crate) fn validate_is_file(
    tool_use_id: &str,
    path: &std::path::Path,
    path_str: &str,
) -> Result<(), ToolResult> {
    if !path.is_file() {
        Err(ToolResult::error(
            tool_use_id,
            format!("Path is not a file: {}", path_str),
        ))
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
            let offset = input.get("offset").and_then(|v| v.as_u64());
            let limit = input.get("limit").and_then(|v| v.as_u64());
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
            format!("Editing {}", filepath)
        }
        "file_write" => {
            let filepath = input
                .get("filePath")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            format!("Writing {}", filepath)
        }
        "glob" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("pattern");
            let path = input.get("path").and_then(|v| v.as_str());
            match path {
                Some(p) => format!("Finding \"{}\" in {}", pattern, p),
                None => format!("Finding \"{}\"", pattern),
            }
        }
        "grep" => {
            let pattern = input
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("pattern");
            let path = input.get("path").and_then(|v| v.as_str());
            match path {
                Some(p) => format!("Grep \"{}\" in {}", pattern, p),
                None => format!("Grep \"{}\"", pattern),
            }
        }
        "file_delete" => {
            let filepath = input
                .get("filePath")
                .and_then(|v| v.as_str())
                .unwrap_or("file");
            format!("Deleting {}", filepath)
        }
        "fetch" => {
            let url = input.get("url").and_then(|v| v.as_str()).unwrap_or("url");
            format!("Fetching {}", url)
        }
        "list_dir" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("Listing {}", path)
        }
        "todo_read" => "Reading todo list".to_string(),
        "todo_write" => {
            let count = input
                .get("todos")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            format!("Updating todo list ({} items)", count)
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
    todo_enabled: bool,
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
    if !is_disabled("file_delete") {
        tools.push(FileDelete.definition());
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
    if !is_disabled("glob") {
        tools.push(Glob.definition());
    }
    if !is_disabled("grep") {
        tools.push(Grep.definition());
    }
    if !is_disabled("list_dir") {
        tools.push(ListDir.definition());
    }
    if todo_enabled && !is_disabled("todo_read") {
        tools.push(TodoRead.definition());
    }
    if todo_enabled && !is_disabled("todo_write") {
        tools.push(TodoWrite.definition());
    }
    tools
}

/// Get all available tool definitions including MCP tools
pub(crate) async fn all_definitions(services: &crate::services::Services) -> Vec<ToolDefinition> {
    // Load config once and extract all needed values
    let config = crate::config::ConfigFile::load().unwrap_or_default();
    let mut defs = builtin_definitions(
        config.todo_enabled,
        &config.disabled_tools,
        services.is_read_only(),
    );
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

    // First try built-in tools
    match name {
        "bash" => return Some(Bash.execute(tool_use_id, input, output, services).await),
        "fetch" => return Some(Fetch.execute(tool_use_id, input, output, services).await),
        "file_delete" => {
            return Some(
                FileDelete
                    .execute(tool_use_id, input, output, services)
                    .await,
            );
        }
        "file_edit" => return Some(FileEdit.execute(tool_use_id, input, output, services).await),
        "file_read" => return Some(FileRead.execute(tool_use_id, input, output, services).await),
        "file_write" => {
            return Some(
                FileWrite
                    .execute(tool_use_id, input, output, services)
                    .await,
            );
        }
        "glob" => return Some(Glob.execute(tool_use_id, input, output, services).await),
        "grep" => return Some(Grep.execute(tool_use_id, input, output, services).await),
        "list_dir" => return Some(ListDir.execute(tool_use_id, input, output, services).await),
        "todo_read" | "todo_write" => {
            if !config.todo_enabled {
                return Some(ToolResult::error(
                    tool_use_id,
                    "Todo tools are disabled in configuration",
                ));
            }
            if name == "todo_read" {
                return Some(TodoRead.execute(tool_use_id, input, output, services).await);
            } else {
                return Some(
                    TodoWrite
                        .execute(tool_use_id, input, output, services)
                        .await,
                );
            }
        }
        _ => {}
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
        let defs = builtin_definitions(false, &[], true);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(!names.contains(&"file_write"));
        assert!(!names.contains(&"file_edit"));
        assert!(!names.contains(&"file_delete"));
        assert!(names.contains(&"file_read"));
        assert!(names.contains(&"bash"));
    }
}
