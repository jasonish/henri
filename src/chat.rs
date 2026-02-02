// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish
//
// Reusable chat session management for sending prompts to providers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::output;
use crate::provider::{
    ChatResponse, ContentBlock, Message, MessageContent, Provider, Role, StopReason,
};
use crate::services::Services;
use crate::tools;

/// Maximum number of retries for the session-level "slow" retry loop.
/// This handles persistent failures after internal provider retries are exhausted.
const MAX_RETRIES: u32 = 5;

/// Initial delay between retries (doubles with each attempt)
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Result of a single chat iteration
pub(crate) enum ChatIterationResult {
    /// Model finished responding, no more tool calls
    Done,
    /// Tools were executed, should continue with another iteration
    Continue,
}

/// Send a chat request to the provider with retry logic for transient errors.
async fn send_with_retry<P: Provider>(
    provider: &P,
    messages: Vec<Message>,
    interrupted: &Arc<AtomicBool>,
    output: &output::OutputContext,
) -> Result<ChatResponse> {
    let mut attempts = 0;
    let mut delay = INITIAL_RETRY_DELAY;

    loop {
        // Check for interrupt before each attempt
        if interrupted.load(Ordering::SeqCst) {
            output::emit_interrupted(output);
            return Err(Error::Interrupted);
        }

        // Notify that we're waiting for the model
        output::emit_waiting(output);

        // Race the model call against interrupt check
        let result = tokio::select! {
            biased;
            _ = async {
                while !interrupted.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            } => {
                output::emit_interrupted(output);
                return Err(Error::Interrupted);
            }
            result = provider.chat(messages.clone(), output) => result
        };

        match result {
            Ok(response) => return Ok(response),
            Err(e) if e.is_retryable() && attempts < MAX_RETRIES => {
                attempts += 1;
                output::emit_warning(
                    output,
                    &format!(
                        "{} (retrying in {}s, attempt {}/{})",
                        e.display_message(),
                        delay.as_secs(),
                        attempts,
                        MAX_RETRIES
                    ),
                );

                // Wait before retrying, but check for interrupts
                tokio::select! {
                    biased;
                    _ = async {
                        while !interrupted.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    } => {
                        output::emit_interrupted(output);
                        return Err(Error::Interrupted);
                    }
                    _ = tokio::time::sleep(delay) => {}
                };

                // Exponential backoff
                delay *= 2;
            }
            Err(e) => {
                output::emit_error(output, &e.display_message());
                return Err(e);
            }
        }
    }
}

/// Run a single chat iteration: one provider call plus tool execution if needed.
///
/// Returns `Done` if the model finished, or `Continue` if tools were executed
/// and another iteration should follow.
pub(crate) async fn run_chat_iteration<P: Provider>(
    provider: &P,
    messages: &mut Vec<Message>,
    interrupted: &Arc<AtomicBool>,
    output: &output::OutputContext,
    services: &Services,
) -> Result<ChatIterationResult> {
    // Check if already interrupted before starting model call
    if interrupted.load(Ordering::SeqCst) {
        output::emit_interrupted(output);
        return Err(Error::Interrupted);
    }

    // Send the chat request with retry logic for transient errors
    let response = send_with_retry(provider, messages.clone(), interrupted, output).await?;

    // If no tool calls, add the response and we're done
    if response.stop_reason != StopReason::ToolUse || response.tool_calls.is_empty() {
        if !response.content_blocks.is_empty() {
            messages.push(Message::assistant_blocks(response.content_blocks.clone()));
        }
        output::emit_done(output);
        return Ok(ChatIterationResult::Done);
    }

    // Execute each tool call and collect results
    let mut tool_results: Vec<ContentBlock> = Vec::new();
    let services = services.with_interrupted(interrupted.clone());

    for tool_call in &response.tool_calls {
        // Check for interrupt before starting each tool
        if interrupted.load(Ordering::SeqCst) {
            output::emit_interrupted(output);
            return Err(Error::Interrupted);
        }

        let description = tools::format_tool_call_description(&tool_call.name, &tool_call.input);
        // Skip tool call banner for todo tools - they emit their own display
        if !tool_call.name.starts_with("todo_") {
            output::print_tool_call(output, &tool_call.name, &description);
        }

        let result = tools::execute(
            &tool_call.name,
            &tool_call.id,
            tool_call.input.clone(),
            output,
            &services,
        )
        .await;

        match result {
            Some(tool_result) => {
                let error_preview = if tool_result.is_error {
                    Some(tool_result.content.clone())
                } else {
                    None
                };
                // Skip tool result indicator for todo tools
                if !tool_call.name.starts_with("todo_") {
                    output::print_tool_result(
                        output,
                        &tool_call.name,
                        tool_result.is_error,
                        error_preview,
                        tool_result.exit_code,
                        tool_result.summary,
                    );
                }

                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: tool_call.id.clone(),
                    content: tool_result.content,
                    is_error: tool_result.is_error,
                    data: tool_result.data,
                    mime_type: tool_result.mime_type,
                });
            }
            None => {
                let error_msg = format!("Unknown tool: {}", tool_call.name);
                let summary = Some(error_msg.clone());
                output::print_tool_result(
                    output,
                    &tool_call.name,
                    true,
                    Some(error_msg.clone()),
                    None,
                    summary,
                );

                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: tool_call.id.clone(),
                    content: error_msg,
                    is_error: true,
                    data: None,
                    mime_type: None,
                });
            }
        }

        // Check for interrupt after each tool execution
        if interrupted.load(Ordering::SeqCst) {
            output::emit_interrupted(output);
            return Err(Error::Interrupted);
        }
    }

    // Add assistant message and tool results together atomically
    if !response.content_blocks.is_empty() {
        messages.push(Message::assistant_blocks(response.content_blocks.clone()));
    }
    messages.push(Message {
        role: Role::User,
        content: MessageContent::Blocks(tool_results),
    });

    Ok(ChatIterationResult::Continue)
}
