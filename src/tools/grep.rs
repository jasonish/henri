// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;
use tokio::process::Command;

use super::{Tool, ToolDefinition, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const MAX_OUTPUT_BYTES: usize = 30_000;

pub(crate) struct Grep;

#[derive(Debug, Deserialize)]
struct GrepInput {
    pattern: String,
    path: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default)]
    include_hidden: bool,
}

impl Grep {
    /// Check if ripgrep (rg) is available in the PATH
    async fn has_ripgrep(&self) -> bool {
        Command::new("rg")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn execute_rg(
        &self,
        tool_use_id: &str,
        input: &GrepInput,
        search_path: &str,
    ) -> Result<ToolResult, String> {
        let mut cmd = Command::new("rg");

        // Output formatting to match grep
        cmd.arg("--line-number");
        cmd.arg("--with-filename");
        cmd.arg("--no-heading");
        cmd.arg("--color=never");
        cmd.arg("--no-binary"); // Explicitly ignore binary files

        if input.case_insensitive {
            cmd.arg("-i");
        } else {
            cmd.arg("-s"); // Case sensitive
        }

        if input.include_hidden {
            cmd.arg("--hidden");
            cmd.arg("--no-ignore");
        }
        // If include_hidden is false (default), rg automatically ignores hidden files and respects .gitignore

        cmd.arg(&input.pattern);
        cmd.arg(search_path);

        self.run_command(tool_use_id, cmd, input, search_path).await
    }

    async fn execute_grep(
        &self,
        tool_use_id: &str,
        input: &GrepInput,
        search_path: &str,
    ) -> Result<ToolResult, String> {
        let mut cmd = Command::new("grep");
        cmd.arg("-rHnI"); // recursive, filename, line-number, ignore-binary
        cmd.arg("--color=never");

        if input.case_insensitive {
            cmd.arg("-i");
        }

        if !input.include_hidden {
            // Manual exclusions for standard grep since it doesn't read .gitignore
            cmd.arg("--exclude-dir=.git");
            cmd.arg("--exclude-dir=.svn");
            cmd.arg("--exclude-dir=.hg");
            cmd.arg("--exclude-dir=.vscode");
            cmd.arg("--exclude-dir=.idea");
            cmd.arg("--exclude-dir=node_modules");
            cmd.arg("--exclude-dir=target");
            cmd.arg("--exclude-dir=build");
            cmd.arg("--exclude-dir=dist");
            cmd.arg("--exclude=.*"); // Exclude hidden files
        } else {
            cmd.arg("--exclude-dir=.git");
        }

        cmd.arg(&input.pattern);
        cmd.arg(search_path);

        self.run_command(tool_use_id, cmd, input, search_path).await
    }

    async fn run_command(
        &self,
        tool_use_id: &str,
        mut cmd: Command,
        input: &GrepInput,
        search_path: &str,
    ) -> Result<ToolResult, String> {
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Prevent interactive editors
        cmd.env("GIT_EDITOR", "true");

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn search command: {}", e))?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let timeout_duration = std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS);

        let stdout_task = tokio::spawn(async move {
            let mut output = String::new();
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                output.push_str(&line);
                output.push('\n');
            }
            output
        });

        let stderr_task = tokio::spawn(async move {
            let mut output = String::new();
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                output.push_str(&line);
                output.push('\n');
            }
            output
        });

        let stream_result = tokio::time::timeout(timeout_duration, async {
            tokio::join!(stdout_task, stderr_task, child.wait())
        })
        .await;

        match stream_result {
            Ok((stdout_res, stderr_res, status_res)) => {
                let stdout_output = stdout_res.unwrap_or_default();
                let stderr_output = stderr_res.unwrap_or_default();

                // Use ExitStatusExt to construct a default status if needed, but here we just need to check codes
                let status = status_res.map_err(|e| format!("Failed to wait for child: {}", e))?;
                let exit_code = status.code().unwrap_or(-1);

                // Both grep and rg return 1 for "no matches found", which is not a tool error
                if exit_code == 1 {
                    return Ok(ToolResult::success(
                        tool_use_id,
                        format!(
                            "No matches found for '{}' in {}",
                            input.pattern, search_path
                        ),
                    ));
                }

                if exit_code > 1 {
                    let error_msg = if !stderr_output.is_empty() {
                        stderr_output
                    } else {
                        format!("Search failed with exit code {}", exit_code)
                    };
                    return Ok(ToolResult::error(tool_use_id, error_msg));
                }

                let truncated = if stdout_output.len() > MAX_OUTPUT_BYTES {
                    let truncated_content = &stdout_output[..MAX_OUTPUT_BYTES];
                    format!(
                        "{}\n\n[Output truncated: {} bytes total]",
                        truncated_content,
                        stdout_output.len()
                    )
                } else {
                    stdout_output
                };

                Ok(ToolResult::success(tool_use_id, truncated))
            }
            Err(_) => {
                let _ = child.kill().await;
                Ok(ToolResult::error(
                    tool_use_id,
                    format!("Search timed out after {} seconds", DEFAULT_TIMEOUT_SECS),
                ))
            }
        }
    }
}

impl Tool for Grep {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".to_string(),
            description: "Search for patterns in files. Uses 'ripgrep' (rg) if available for speed and .gitignore support, falling back to standard 'grep'. Returns file paths, line numbers, and matching content.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "The regular expression or string to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search in (default: current directory)"
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "Ignore case distinctions (default: false)"
                    },
                    "include_hidden": {
                        "type": "boolean",
                        "description": "Include hidden files/directories and ignored files (default: false)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(
        &self,
        tool_use_id: &str,
        input: serde_json::Value,
        _output: &crate::output::OutputContext,
        _services: &crate::services::Services,
    ) -> ToolResult {
        let input: GrepInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return e,
        };

        let search_path = super::expand_tilde(input.path.as_deref().unwrap_or("."));
        let path = Path::new(&search_path);

        if let Err(e) = super::validate_path_exists(tool_use_id, path, &search_path) {
            return e;
        }

        let result = if self.has_ripgrep().await {
            self.execute_rg(tool_use_id, &input, &search_path).await
        } else {
            self.execute_grep(tool_use_id, &input, &search_path).await
        };

        match result {
            Ok(res) => res,
            Err(err_msg) => ToolResult::error(tool_use_id, err_msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_grep_basic() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::fs::write(
            temp_path.join("file.txt"),
            "hello world\nfoo bar\nhello again",
        )
        .unwrap();

        let tool = Grep;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "hello",
                    "path": temp_path.to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("file.txt:1:hello world"));
        assert!(result.content.contains("file.txt:3:hello again"));
        assert!(!result.content.contains("foo bar"));
    }

    #[tokio::test]
    async fn test_grep_no_matches() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::fs::write(temp_path.join("file.txt"), "hello world").unwrap();

        let tool = Grep;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "xyz",
                    "path": temp_path.to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("No matches found"));
    }

    #[tokio::test]
    async fn test_grep_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let temp_path = temp_dir.path();

        std::fs::create_dir(temp_path.join("subdir")).unwrap();
        std::fs::write(temp_path.join("subdir/file.txt"), "found me").unwrap();

        let tool = Grep;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "pattern": "found",
                    "path": temp_path.to_str().unwrap()
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("subdir/file.txt:1:found me"));
    }
}
