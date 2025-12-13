// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use crate::provider::{ContentBlock, Message, MessageContent, Role};

/// Result of a compaction operation
pub struct CompactionResult {
    pub messages_compacted: usize,
}

/// System prompt for the summarization request
const SUMMARIZATION_SYSTEM_PROMPT: &str = r#"You are summarizing a coding conversation to preserve context.

Provide a structured summary including:
- What was accomplished
- Current work in progress
- Files modified or discussed
- Key decisions and rationale
- User preferences or constraints
- Next steps if identified

Be detailed enough that work can continue seamlessly. Use markdown formatting."#;

/// Returns the system prompt for summarization
pub(crate) fn summarization_system_prompt() -> &'static str {
    SUMMARIZATION_SYSTEM_PROMPT
}

/// Segment messages into (to_compact, to_preserve).
///
/// Rules:
/// - Preserve last N "turns" (a turn = one user message + following assistant response)
/// - Never split a tool_use from its tool_result (they must stay together)
/// - System messages are not compacted (passed through)
///
/// Returns (messages_to_compact, messages_to_preserve)
pub(crate) fn segment_messages(
    messages: &[Message],
    preserve_recent_turns: usize,
) -> (Vec<Message>, Vec<Message>) {
    if messages.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // Count turns from the end (a turn starts with a User message)
    let mut turn_count = 0;
    let mut preserve_from_idx = messages.len();

    for (idx, msg) in messages.iter().enumerate().rev() {
        if msg.role == Role::User {
            // Check if this is a tool_result message (user message containing only tool results)
            let is_tool_result = matches!(&msg.content, MessageContent::Blocks(blocks)
                if blocks.iter().all(|b| matches!(b, ContentBlock::ToolResult { .. })));

            if !is_tool_result {
                turn_count += 1;
                if turn_count > preserve_recent_turns {
                    break;
                }
                preserve_from_idx = idx;
            }
        }
    }

    // Now we need to ensure we don't split tool_use from tool_result
    // Walk backward from preserve_from_idx to find a safe split point
    let safe_split_idx = find_safe_split_point(messages, preserve_from_idx);

    let to_compact = messages[..safe_split_idx].to_vec();
    let to_preserve = messages[safe_split_idx..].to_vec();

    (to_compact, to_preserve)
}

/// Find a safe index to split messages without breaking tool_use/tool_result pairs.
/// A tool_use in an assistant message must be followed by its tool_result in the next user message.
fn find_safe_split_point(messages: &[Message], suggested_idx: usize) -> usize {
    if suggested_idx == 0 || suggested_idx >= messages.len() {
        return suggested_idx;
    }

    // Check if the message at suggested_idx-1 is an assistant message with tool_use
    // If so, we need to include the following tool_result message
    let prev_msg = &messages[suggested_idx.saturating_sub(1)];
    if prev_msg.role == Role::Assistant
        && let MessageContent::Blocks(blocks) = &prev_msg.content
    {
        let has_tool_use = blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
        if has_tool_use {
            // The assistant message has tool_use, so the message at suggested_idx
            // should be the tool_result. We need to move the split point back.
            return suggested_idx.saturating_sub(1);
        }
    }

    suggested_idx
}

/// Build the text content for a summarization request.
/// Returns just the prompt text (not wrapped in a Message).
pub(crate) fn build_summarization_request_text(messages_to_summarize: &[Message]) -> String {
    // Format the conversation for summarization
    let mut conversation_text = String::new();

    for msg in messages_to_summarize {
        let role_label = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        };

        conversation_text.push_str(&format!("\n## {}\n", role_label));

        match &msg.content {
            MessageContent::Text(text) => {
                conversation_text.push_str(text);
            }
            MessageContent::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            conversation_text.push_str(text);
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            conversation_text.push_str(&format!(
                                "[Thinking: {}...]\n",
                                &thinking.chars().take(200).collect::<String>()
                            ));
                        }
                        ContentBlock::ToolUse { name, .. } => {
                            conversation_text.push_str(&format!("[Tool call: {}]\n", name));
                        }
                        ContentBlock::ToolResult {
                            content, is_error, ..
                        } => {
                            let preview = if content.chars().count() > 500 {
                                let truncated: String = content.chars().take(500).collect();
                                format!("{}... ({} chars)", truncated, content.len())
                            } else {
                                content.clone()
                            };
                            if *is_error {
                                conversation_text.push_str(&format!("[Tool error: {}]\n", preview));
                            } else {
                                conversation_text
                                    .push_str(&format!("[Tool result: {}]\n", preview));
                            }
                        }
                        ContentBlock::Summary {
                            summary,
                            messages_compacted,
                        } => {
                            conversation_text.push_str(&format!(
                                "[Previous compaction of {} messages: {}]\n",
                                messages_compacted, summary
                            ));
                        }
                        ContentBlock::Image { .. } => {
                            conversation_text.push_str("[Image]\n");
                        }
                    }
                }
            }
        }
        conversation_text.push('\n');
    }

    format!(
        "Please summarize the following conversation:\n\n{}\n\nProvide a comprehensive summary.",
        conversation_text
    )
}

/// Build the user message that asks for summarization
pub(crate) fn build_summarization_request(messages_to_summarize: &[Message]) -> Message {
    Message::user(build_summarization_request_text(messages_to_summarize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_empty_messages() {
        let (to_compact, to_preserve) = segment_messages(&[], 2);
        assert!(to_compact.is_empty());
        assert!(to_preserve.is_empty());
    }

    #[test]
    fn test_segment_few_messages() {
        let messages = vec![
            Message::user("Hello"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "Hi!".to_string(),
            }]),
        ];
        let (to_compact, to_preserve) = segment_messages(&messages, 2);
        // With only 1 turn (< 2), nothing to compact
        assert!(to_compact.is_empty());
        assert_eq!(to_preserve.len(), 2);
    }

    #[test]
    fn test_segment_multiple_turns() {
        let messages = vec![
            Message::user("First message"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "First response".to_string(),
            }]),
            Message::user("Second message"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "Second response".to_string(),
            }]),
            Message::user("Third message"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "Third response".to_string(),
            }]),
        ];

        let (to_compact, to_preserve) = segment_messages(&messages, 2);
        // Should preserve last 2 turns (4 messages), compact first turn (2 messages)
        assert_eq!(to_compact.len(), 2);
        assert_eq!(to_preserve.len(), 4);
    }

    #[test]
    fn test_summarization_system_prompt() {
        let prompt = summarization_system_prompt();
        assert!(prompt.contains("summarizing"));
        assert!(prompt.contains("accomplished"));
    }

    #[test]
    fn test_build_summarization_request() {
        let messages = vec![
            Message::user("What is 2+2?"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "2+2 equals 4".to_string(),
            }]),
        ];

        let request = build_summarization_request(&messages);
        assert_eq!(request.role, Role::User);
        if let MessageContent::Text(text) = &request.content {
            assert!(text.contains("summarize"));
            assert!(text.contains("User"));
            assert!(text.contains("Assistant"));
        } else {
            panic!("Expected text content");
        }
    }
}
