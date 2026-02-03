// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Bash tool.
//!
//! This tool executes a shell command and returns a `ToolResult` whose `content` is what gets sent
//! back into the model context.
//!
//! ## Output limits (model-facing)
//! The returned `content` is truncated to keep prompts bounded:
//! - Max bytes: 32KiB (`MAX_OUTPUT_BYTES`)
//! - Max lines: 2000 (`MAX_OUTPUT_LINES`)
//!
//! Truncation keeps the **tail** (end) of output, dropping earlier content until both limits are
//! satisfied. If truncation happens, an in-band bracketed notice is appended describing the line
//! range and bytes kept/truncated.
//!
//! Note: if truncation occurs due to the byte limit, the first kept line may be partial; this is
//! called out in the notice.

use std::path::PathBuf;
use std::process::Stdio;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::output;

use super::sandbox;
use super::{Tool, ToolDefinition, ToolResult};

const DEFAULT_TIMEOUT_SECS: u64 = 120;

// Keep tool-result payloads (what gets sent back to the model) bounded.
// These are intended to match Pi defaults (50KB or 2000 lines, whichever hits first).
const MAX_OUTPUT_BYTES: usize = 32 * 1024;
const MAX_OUTPUT_LINES: usize = 2000;

pub(crate) struct Bash;

async fn capture_stream_output<R>(reader: R, output: output::OutputContext) -> CapturedOutput
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut output_buf = String::new();
    let mut line_count = 0usize;

    let mut reader = BufReader::new(reader);
    let mut raw = Vec::new();

    loop {
        raw.clear();
        let Ok(n) = reader.read_until(b'\n', &mut raw).await else {
            break;
        };
        if n == 0 {
            break;
        }

        line_count += 1;

        let chunk = String::from_utf8_lossy(&raw);
        output_buf.push_str(&chunk);
        output::emit_tool_output(&output, &chunk);
    }

    let byte_count = output_buf.len();
    CapturedOutput {
        text: output_buf,
        line_count,
        byte_count,
    }
}

#[derive(Debug, Default)]
struct CapturedOutput {
    text: String,
    line_count: usize,
    byte_count: usize,
}

#[derive(Debug, Deserialize)]
struct BashInput {
    command: String,
    #[serde(default, deserialize_with = "super::deserialize_optional_u64")]
    timeout: Option<u64>,
    cwd: Option<String>,
}

#[derive(Debug, Default)]
struct TailTruncation {
    did_truncate: bool,
    start_line: usize,
    end_line: usize,
    kept_lines: usize,
    kept_bytes: usize,
    total_lines: usize,
    total_bytes: usize,
    truncated_lines: usize,
    truncated_bytes: usize,
    first_line_partial: bool,
}

fn count_newlines(s: &str) -> usize {
    s.as_bytes().iter().filter(|b| **b == b'\n').count()
}

fn count_lines(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }

    let newlines = count_newlines(s);
    if s.ends_with('\n') {
        newlines
    } else {
        newlines + 1
    }
}

fn truncate_tail_for_model(
    text: &str,
    total_lines: usize,
    total_bytes: usize,
) -> (String, TailTruncation) {
    let within_limits = total_lines <= MAX_OUTPUT_LINES && total_bytes <= MAX_OUTPUT_BYTES;
    if within_limits {
        let kept_lines = total_lines;
        return (
            text.to_string(),
            TailTruncation {
                did_truncate: false,
                start_line: if kept_lines == 0 { 0 } else { 1 },
                end_line: kept_lines,
                kept_lines,
                kept_bytes: total_bytes,
                total_lines,
                total_bytes,
                truncated_lines: 0,
                truncated_bytes: 0,
                first_line_partial: false,
            },
        );
    }

    let mut truncation = TailTruncation {
        did_truncate: true,
        total_lines,
        total_bytes,
        ..TailTruncation::default()
    };

    // First: enforce max lines by dropping whole lines from the head.
    let mut candidate = text;
    let mut start_line = 1usize;
    if total_lines > MAX_OUTPUT_LINES {
        let drop_lines = total_lines - MAX_OUTPUT_LINES;

        let mut remaining = drop_lines;
        let mut start_idx = 0usize;
        for (idx, b) in text.as_bytes().iter().enumerate() {
            if *b == b'\n' {
                remaining = remaining.saturating_sub(1);
                if remaining == 0 {
                    start_idx = idx + 1;
                    break;
                }
            }
        }

        // `start_idx` is always on a UTF-8 boundary because it's after a '\n' byte.
        candidate = &text[start_idx..];
        start_line = drop_lines + 1;
    }

    // Second: enforce max bytes by dropping from the head (keep tail).
    if candidate.len() > MAX_OUTPUT_BYTES {
        let mut byte_start = candidate.len().saturating_sub(MAX_OUTPUT_BYTES);
        while byte_start < candidate.len() && !candidate.is_char_boundary(byte_start) {
            byte_start += 1;
        }

        let prefix = &candidate[..byte_start];
        let dropped_lines_in_prefix = count_newlines(prefix);
        start_line = start_line.saturating_add(dropped_lines_in_prefix);
        truncation.first_line_partial = !prefix.is_empty() && !prefix.ends_with('\n');

        candidate = &candidate[byte_start..];
    }

    let kept = candidate.to_string();
    let kept_lines = count_lines(&kept);

    let (start_line, end_line) = if kept_lines == 0 {
        (0, 0)
    } else {
        (start_line, start_line + kept_lines - 1)
    };

    truncation.start_line = start_line;
    truncation.end_line = end_line;
    truncation.kept_lines = kept_lines;
    truncation.kept_bytes = kept.len();
    truncation.truncated_lines = total_lines.saturating_sub(kept_lines);
    truncation.truncated_bytes = total_bytes.saturating_sub(kept.len());

    (kept, truncation)
}

impl Tool for Bash {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: r#"Execute a bash command and return its output. Use this to run shell commands, list files, search with ripgrep, etc.

Web content fetching:
- Use curl with -sL flags (silent, follow redirects)
- Pipe through pandoc to convert HTML to markdown: curl -sL "URL" | pandoc -f html -t markdown
- For JSON APIs, curl alone is sufficient: curl -sL "URL""#
                .to_string(),
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
        output: &crate::output::OutputContext,
        services: &crate::services::Services,
    ) -> ToolResult {
        let input: BashInput = match super::deserialize_input(tool_use_id, input) {
            Ok(i) => i,
            Err(e) => return *e,
        };

        let timeout_secs = input.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);

        // Determine effective working directory
        let effective_cwd: PathBuf = if let Some(ref cwd) = input.cwd {
            let path = std::path::Path::new(cwd);
            if !path.is_dir() {
                return ToolResult::error(
                    tool_use_id,
                    format!("Working directory does not exist: {}", cwd),
                );
            }
            path.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        };

        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(&input.command);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Prevent interactive editors from opening (git rebase, git commit, etc.)
        cmd.env("GIT_EDITOR", "true");
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.env("EDITOR", "true");
        cmd.env("VISUAL", "true");

        cmd.current_dir(&effective_cwd);

        if services.is_read_only() {
            if let Some(ruleset) = sandbox::create_read_only_ruleset() {
                let mut ruleset = Some(ruleset);
                unsafe {
                    cmd.pre_exec(move || {
                        if let Some(rs) = ruleset.take() {
                            sandbox::apply_ruleset(rs)
                        } else {
                            Ok(())
                        }
                    });
                }
            }
        } else if services.is_sandbox_enabled()
            && let Some(ruleset) = sandbox::create_bash_ruleset(&effective_cwd)
        {
            // Wrap in Option so we can take() it in the FnMut closure
            let mut ruleset = Some(ruleset);

            // SAFETY: The pre_exec closure runs after fork but before exec.
            // sandbox::apply_ruleset only makes direct syscalls (landlock_restrict_self,
            // prctl) which are async-signal-safe. The ruleset file descriptors were
            // opened before fork.
            unsafe {
                cmd.pre_exec(move || {
                    if let Some(rs) = ruleset.take() {
                        sandbox::apply_ruleset(rs)
                    } else {
                        Ok(())
                    }
                });
            }
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

        let stdout_output = output.clone();
        let stdout_task =
            tokio::spawn(async move { capture_stream_output(stdout, stdout_output).await });

        let stderr_output = output.clone();
        let stderr_task =
            tokio::spawn(async move { capture_stream_output(stderr, stderr_output).await });

        // Wait for child with interrupt and timeout handling.
        // We separate child.wait() from output collection so we can kill on interrupt/timeout.
        enum WaitOutcome {
            Completed(Result<std::process::ExitStatus, std::io::Error>),
            Interrupted,
            TimedOut,
        }

        let wait_result = tokio::select! {
            biased;
            // Check for interrupt every 100ms
            _ = async {
                while !services.is_interrupted() {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            } => {
                let _ = child.kill().await;
                WaitOutcome::Interrupted
            }
            result = tokio::time::timeout(timeout_duration, child.wait()) => {
                match result {
                    Ok(status) => WaitOutcome::Completed(status),
                    Err(_) => {
                        let _ = child.kill().await;
                        WaitOutcome::TimedOut
                    }
                }
            }
        };

        match wait_result {
            WaitOutcome::Completed(Ok(status)) => {
                // Collect output from spawned tasks
                let stdout_output: CapturedOutput = stdout_task.await.unwrap_or_default();
                let stderr_output: CapturedOutput = stderr_task.await.unwrap_or_default();

                let total_lines = stdout_output.line_count + stderr_output.line_count;
                let total_bytes = stdout_output.byte_count + stderr_output.byte_count;
                let summary = Some(format!("[read {total_lines} lines, {total_bytes} bytes]"));

                let mut combined = stdout_output.text;
                if !stderr_output.text.is_empty() {
                    combined.push_str(&stderr_output.text);
                }

                let (mut content, truncation) =
                    truncate_tail_for_model(&combined, total_lines, total_bytes);

                if truncation.did_truncate {
                    let mut notice = if truncation.kept_lines == 0 {
                        format!(
                            "[Output truncated: showing 0 lines of {} (kept 0 bytes of {}; limits: {} lines, {} bytes)]",
                            truncation.total_lines,
                            truncation.total_bytes,
                            MAX_OUTPUT_LINES,
                            MAX_OUTPUT_BYTES,
                        )
                    } else {
                        format!(
                            "[Output truncated: showing lines {}-{} of {} (kept {} lines / {} bytes; truncated {} lines / {} bytes; limits: {} lines, {} bytes)]",
                            truncation.start_line,
                            truncation.end_line,
                            truncation.total_lines,
                            truncation.kept_lines,
                            truncation.kept_bytes,
                            truncation.truncated_lines,
                            truncation.truncated_bytes,
                            MAX_OUTPUT_LINES,
                            MAX_OUTPUT_BYTES,
                        )
                    };

                    if truncation.first_line_partial {
                        notice.insert_str(notice.len() - 1, "; first line is partial");
                    }

                    content.push_str("\n\n");
                    content.push_str(&notice);
                }

                let exit_code = status.code().unwrap_or(-1);
                if exit_code == 0 {
                    ToolResult {
                        tool_use_id: tool_use_id.to_string(),
                        kind: "tool_result".to_string(),
                        content,
                        is_error: false,
                        exit_code: Some(exit_code),
                        summary,
                        data: None,
                        mime_type: None,
                    }
                } else if content.is_empty() {
                    ToolResult {
                        tool_use_id: tool_use_id.to_string(),
                        kind: "tool_result".to_string(),
                        content: format!("[Exit code: {}]", exit_code),
                        is_error: true,
                        exit_code: Some(exit_code),
                        summary,
                        data: None,
                        mime_type: None,
                    }
                } else {
                    let error_output = format!("{}\n[Exit code: {}]", content, exit_code);
                    ToolResult {
                        tool_use_id: tool_use_id.to_string(),
                        kind: "tool_result".to_string(),
                        content: error_output,
                        is_error: true,
                        exit_code: Some(exit_code),
                        summary,
                        data: None,
                        mime_type: None,
                    }
                }
            }
            WaitOutcome::Completed(Err(e)) => {
                ToolResult::error(tool_use_id, format!("Command execution failed: {}", e))
            }
            WaitOutcome::TimedOut => {
                stdout_task.abort();
                stderr_task.abort();
                ToolResult::error(
                    tool_use_id,
                    format!("Command timed out after {} seconds", timeout_secs),
                )
            }
            WaitOutcome::Interrupted => {
                stdout_task.abort();
                stderr_task.abort();
                ToolResult::error(tool_use_id, "Interrupted by user").with_summary("Interrupted")
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
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.content.trim(), "hello");
        assert_eq!(result.summary, Some("[read 1 lines, 6 bytes]".to_string()));
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
        assert_eq!(result.exit_code, Some(42));
        assert_eq!(result.content, "[Exit code: 42]");
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
        assert_eq!(result.exit_code, Some(42));
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

    #[tokio::test]
    async fn test_truncates_tail_by_lines_includes_line_range() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "seq 1 2500"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("2500"));
        assert!(!result.content.contains("\n1\n"));
        assert!(
            result
                .content
                .contains("[Output truncated: showing lines 501-2500 of 2500 (kept 2000 lines")
        );
    }

    #[tokio::test]
    async fn test_truncates_by_lines_only_reports_correct_counts() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "yes x | head -n 2500"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("[Output truncated:"));
        assert!(
            result
                .content
                .contains("[Output truncated: showing lines 501-2500 of 2500 (kept 2000 lines / 4000 bytes; truncated 500 lines / 1000 bytes; limits: 2000 lines, 32768 bytes)]")
        );
        assert!(!result.content.contains("first line is partial"));
    }

    #[tokio::test]
    async fn test_truncates_by_bytes_keeps_exact_byte_limit() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "head -c 60000 /dev/zero | tr '\\0' 'a'"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("[Output truncated:"));
        assert!(result.content.contains("kept 1 lines / 32768 bytes"));
        assert!(result.content.contains("limits: 2000 lines, 32768 bytes"));
        assert!(result.content.contains("first line is partial"));
    }

    #[tokio::test]
    async fn test_truncates_by_lines_then_bytes_reports_final_line_range() {
        let tool = Bash;
        let result = tool
            .execute(
                "test-id",
                serde_json::json!({
                    "command": "for _ in $(seq 1 2500); do printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\\n'; done"
                }),
                &crate::output::OutputContext::null(),
                &crate::services::Services::null(),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.content.contains("[Output truncated:"));

        // Each line is 40 'a' + '\n' (41 bytes). Total: 2500 * 41 = 102500 bytes.
        // After line trimming, we have 2000 lines. After byte trimming, we keep the last 32768
        // bytes, which corresponds to 800 lines (799 full + 1 partial), i.e. lines 1701-2500.
        assert!(result.content.contains("showing lines 1701-2500 of 2500"));
        assert!(result.content.contains("kept 800 lines / 32768 bytes"));
        assert!(result.content.contains("first line is partial"));
    }
}
