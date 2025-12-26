// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::process::Stdio;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::{Tool, ToolDefinition, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MAX_OUTPUT_BYTES: usize = 30_000;

pub struct Bash;

async fn capture_stream_output<R>(reader: R) -> String
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut output = String::new();
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        output.push_str(&line);
        output.push('\n');
    }

    output
}

#[derive(Debug, Deserialize)]
struct BashInput {
    command: String,
    timeout: Option<u64>,
    cwd: Option<String>,
}

impl Tool for Bash {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: r#"Execute a bash command and return its output. Use this to run shell commands, list files, search with ripgrep, etc.

Web content fetching:
- Use curl with -sL flags (silent, follow redirects)
- Pipe through pandoc to convert HTML to markdown: curl -sL "URL" | pandoc -f html -t markdown
- For JSON APIs, curl alone is sufficient: curl -sL "URL""#.to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The bash command to execute"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 120)"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for the command"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(
        &self,
        tool_use_id: &str,
        input: serde_json::Value,
        _output: &crate::output::OutputContext,
        services: &crate::services::Services,
    ) -> ToolResult {
        let input: BashInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        let timeout_secs = input.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);

        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(&input.command);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Prevent interactive editors from opening (git rebase, git commit, etc.)
        cmd.env("GIT_EDITOR", "true");
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.env("EDITOR", "true");
        cmd.env("VISUAL", "true");

        if let Some(ref cwd) = input.cwd {
            let path = std::path::Path::new(cwd);
            if !path.is_dir() {
                return ToolResult::error(
                    tool_use_id,
                    format!("Working directory does not exist: {}", cwd),
                );
            }
            cmd.current_dir(cwd);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::error(tool_use_id, format!("Failed to spawn command: {}", e));
            }
        };

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let timeout_duration = std::time::Duration::from_secs(timeout_secs);

        let stdout_task = tokio::spawn(async move { capture_stream_output(stdout).await });

        let stderr_task = tokio::spawn(async move { capture_stream_output(stderr).await });

        // Wait for child with interrupt and timeout handling.
        // We separate child.wait() from output collection so we can kill on interrupt/timeout.
        let wait_result = tokio::select! {
            biased;
            // Check for interrupt every 100ms
            _ = async {
                while !services.is_interrupted() {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            } => {
                let _ = child.kill().await;
                return ToolResult::error(tool_use_id, "Interrupted by user");
            }
            result = tokio::time::timeout(timeout_duration, child.wait()) => result
        };

        match wait_result {
            Ok(Ok(status)) => {
                // Collect output from spawned tasks
                let stdout_output = stdout_task.await.unwrap_or_default();
                let stderr_output = stderr_task.await.unwrap_or_default();

                let mut combined = stdout_output;
                if !stderr_output.is_empty() {
                    combined.push_str(&stderr_output);
                }

                let truncated = if combined.len() > MAX_OUTPUT_BYTES {
                    let truncated_content = &combined[..MAX_OUTPUT_BYTES];
                    format!(
                        "{}\n\n[Output truncated: {} bytes total]",
                        truncated_content,
                        combined.len()
                    )
                } else {
                    combined
                };

                let exit_code = status.code().unwrap_or(-1);
                if exit_code == 0 {
                    ToolResult::success(tool_use_id, truncated)
                } else if truncated.is_empty() {
                    ToolResult::error(tool_use_id, truncated)
                } else {
                    let error_output = format!("{}\n[Exit code: {}]", truncated, exit_code);
                    ToolResult::error(tool_use_id, error_output)
                }
            }
            Ok(Err(e)) => {
                ToolResult::error(tool_use_id, format!("Command execution failed: {}", e))
            }
            Err(_) => {
                let _ = child.kill().await;
                ToolResult::error(
                    tool_use_id,
                    format!("Command timed out after {} seconds", timeout_secs),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_echo() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "echo hello"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        assert_eq!(result.content.trim(), "hello");
    }

    #[tokio::test]
    async fn test_nonzero_exit() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "exit 42"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert_eq!(result.content, "");
    }

    #[tokio::test]
    async fn test_nonzero_exit_with_output() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "echo 'something went wrong' && exit 42"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("something went wrong"));
        assert!(result.content.contains("[Exit code: 42]"));
    }

    #[tokio::test]
    async fn test_timeout() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "sleep 10",
                    "timeout": 1
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }

    #[tokio::test]
    async fn test_cwd() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "pwd",
                    "cwd": "/tmp"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        assert_eq!(result.content.trim(), "/tmp");
    }

    #[tokio::test]
    async fn test_invalid_cwd() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "pwd",
                    "cwd": "/nonexistent/directory"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(result.is_error);
        assert!(result.content.contains("does not exist"));
    }

    #[tokio::test]
    async fn test_stderr() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "echo error >&2"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("error"));
    }
}
