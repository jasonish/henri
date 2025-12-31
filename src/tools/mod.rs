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

/// Built-in tool names and their human-readable descriptions.
/// This is the single source of truth for tool metadata used in menus and UIs.
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
    ("todo_read", "Read the current todo list"),
    ("todo_write", "Update the todo list"),
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
}

impl ToolResult {
    pub(crate) fn success(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            kind: "tool_result".to_string(),
            content: content.into(),
            is_error: false,
        }
    }

    pub(crate) fn error(tool_use_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            kind: "tool_result".to_string(),
            content: message.into(),
            is_error: true,
        }
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

/// Generates a one-liner description for a tool call (used for UI display)
pub(crate) fn format_tool_call_description(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "bash" => {
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("command");
            // Indent subsequent lines for better readability with vertical line
            let lines: Vec<&str> = command.lines().collect();
            if lines.len() > 1 {
                let mut result = format!("Running: {}", lines[0]);
                for line in &lines[1..] {
                    result.push_str(&format!("\n  │ {}", line));
                }
                result
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
) -> Vec<ToolDefinition> {
    let mut tools = Vec::new();

    // Add tools only if not disabled
    if !disabled_tools.iter().any(|t| t == "bash") {
        tools.push(Bash.definition());
    }
    if !disabled_tools.iter().any(|t| t == "fetch") {
        tools.push(Fetch.definition());
    }
    if !disabled_tools.iter().any(|t| t == "file_delete") {
        tools.push(FileDelete.definition());
    }
    if !disabled_tools.iter().any(|t| t == "file_edit") {
        tools.push(FileEdit.definition());
    }
    if !disabled_tools.iter().any(|t| t == "file_read") {
        tools.push(FileRead.definition());
    }
    if !disabled_tools.iter().any(|t| t == "file_write") {
        tools.push(FileWrite.definition());
    }
    if !disabled_tools.iter().any(|t| t == "glob") {
        tools.push(Glob.definition());
    }
    if !disabled_tools.iter().any(|t| t == "grep") {
        tools.push(Grep.definition());
    }
    if !disabled_tools.iter().any(|t| t == "list_dir") {
        tools.push(ListDir.definition());
    }
    if todo_enabled && !disabled_tools.iter().any(|t| t == "todo_read") {
        tools.push(TodoRead.definition());
    }
    if todo_enabled && !disabled_tools.iter().any(|t| t == "todo_write") {
        tools.push(TodoWrite.definition());
    }
    tools
}

/// Get all available tool definitions including MCP tools
pub(crate) async fn all_definitions() -> Vec<ToolDefinition> {
    // Load config once and extract all needed values
    let config = crate::config::ConfigFile::load().unwrap_or_default();
    let mut defs = builtin_definitions(config.todo_enabled, &config.disabled_tools);
    let mcp_defs = crate::mcp::manager().all_tool_definitions().await;
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
