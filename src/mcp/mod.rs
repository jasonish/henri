// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::sync::Arc;

use rmcp::RoleClient;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParam;
use rmcp::model::Tool;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use tokio::process::Command;
use tokio::sync::RwLock;

use crate::error::Result;
use crate::tools::{ToolDefinition, ToolResult};

/// Configuration for an MCP server
#[derive(Debug, Clone)]
pub(crate) struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
}

/// A running MCP server client
struct McpClient {
    name: String,
    service: RunningService<RoleClient, ()>,
    tools: Vec<Tool>,
}

/// Manager for multiple MCP server connections
pub(crate) struct McpManager {
    clients: RwLock<Vec<McpClient>>,
}

impl McpManager {
    pub(crate) fn new() -> Self {
        Self {
            clients: RwLock::new(Vec::new()),
        }
    }

    /// Start an MCP server and connect to it
    pub(crate) async fn start_server(&self, config: &McpServerConfig) -> Result<()> {
        let mut clients = self.clients.write().await;
        if clients.iter().any(|c| c.name == config.name) {
            return Ok(());
        }

        println!(
            "[MCP] Starting server: {} ({} {:?})",
            config.name, config.command, config.args
        );

        let mut cmd = Command::new(&config.command);
        for arg in &config.args {
            cmd.arg(arg);
        }
        // Set environment variables for the MCP server
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        let transport = TokioChildProcess::new(cmd)
            .map_err(|e| crate::error::Error::Mcp(format!("Failed to spawn MCP server: {}", e)))?;

        let service = ().serve(transport).await.map_err(|e| {
            crate::error::Error::Mcp(format!("Failed to initialize MCP client: {}", e))
        })?;

        // List available tools from this server
        let tools_result = service
            .list_tools(Default::default())
            .await
            .map_err(|e| crate::error::Error::Mcp(format!("Failed to list tools: {}", e)))?;

        let tools = tools_result.tools;
        println!(
            "[MCP] Server '{}' provides {} tools:",
            config.name,
            tools.len()
        );
        for tool in &tools {
            println!(
                "[MCP]   - {}: {}",
                tool.name,
                tool.description.as_deref().unwrap_or("")
            );
        }

        let client = McpClient {
            name: config.name.clone(),
            service,
            tools,
        };

        clients.push(client);

        Ok(())
    }

    /// Get all tool definitions from all connected MCP servers
    pub(crate) async fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
        let clients = self.clients.read().await;
        let mut definitions = Vec::new();

        for client in clients.iter() {
            for tool in &client.tools {
                definitions.push(mcp_tool_to_definition(&client.name, tool));
            }
        }

        definitions
    }

    /// Execute a tool by name
    /// Returns None if the tool is not found in any MCP server
    pub(crate) async fn execute_tool(
        &self,
        tool_name: &str,
        tool_use_id: &str,
        input: serde_json::Value,
    ) -> Option<ToolResult> {
        let clients = self.clients.read().await;

        // Find which client has this tool
        for client in clients.iter() {
            // Check if this server has the tool (with or without prefix)
            let has_tool = client.tools.iter().any(|t| {
                let prefixed_name = format!("mcp_{}_{}", client.name, t.name);
                t.name == tool_name || prefixed_name == tool_name
            });

            if has_tool {
                // Extract the actual tool name (remove prefix if present)
                let prefix = format!("mcp_{}_", client.name);
                let actual_name = if tool_name.starts_with(&prefix) {
                    tool_name.strip_prefix(&prefix).unwrap_or(tool_name)
                } else {
                    tool_name
                };

                let params = CallToolRequestParam {
                    name: actual_name.to_string().into(),
                    arguments: input.as_object().cloned(),
                };

                match client.service.call_tool(params).await {
                    Ok(result) => {
                        // Convert MCP result to our ToolResult format
                        let content = result
                            .content
                            .iter()
                            .filter_map(|annotated| {
                                use rmcp::model::RawContent;
                                match &**annotated {
                                    RawContent::Text(text) => Some(text.text.to_string()),
                                    _ => None,
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n");

                        return Some(ToolResult {
                            tool_use_id: tool_use_id.to_string(),
                            kind: "tool_result".to_string(),
                            content,
                            is_error: result.is_error.unwrap_or(false),
                        });
                    }
                    Err(e) => {
                        return Some(ToolResult::error(
                            tool_use_id,
                            format!("MCP tool execution failed: {}", e),
                        ));
                    }
                }
            }
        }

        None
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert an MCP Tool to our ToolDefinition format
fn mcp_tool_to_definition(server_name: &str, tool: &Tool) -> ToolDefinition {
    // Prefix tool names with mcp_{server_name}_ to avoid conflicts
    let name = format!("mcp_{}_{}", server_name, tool.name);

    // Convert the Arc<Map> to serde_json::Value
    let input_schema = serde_json::Value::Object((*tool.input_schema).clone());

    ToolDefinition {
        name,
        description: tool
            .description
            .clone()
            .map(|s| s.to_string())
            .unwrap_or_default(),
        input_schema,
    }
}

/// Global MCP manager instance
static MCP_MANAGER: std::sync::OnceLock<Arc<McpManager>> = std::sync::OnceLock::new();

/// Get the global MCP manager
pub(crate) fn manager() -> Arc<McpManager> {
    MCP_MANAGER
        .get_or_init(|| Arc::new(McpManager::new()))
        .clone()
}

/// Initialize MCP servers from configuration
pub(crate) async fn initialize(servers: Vec<McpServerConfig>) -> Result<()> {
    let mgr = manager();
    for config in servers {
        if let Err(e) = mgr.start_server(&config).await {
            eprintln!("[MCP] Failed to start server '{}': {}", config.name, e);
        }
    }
    Ok(())
}
