// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

use std::collections::HashMap;

use crate::provider::{ContentBlock, Message, MessageContent, Role};

/// Result of a compaction operation
pub(crate) struct CompactionResult {
    pub messages_compacted: usize,
}

/// System prompt for the summarization request
const SUMMARIZATION_SYSTEM_PROMPT: &str = r#"You are summarizing a coding conversation to preserve context.

The conversation is provided in XML format with the following structure:
- <conversation> - root element containing all messages
- <message role="user|assistant"> - individual messages
- <text> - text content within messages
- <thinking> - assistant's reasoning (provider-specific data stripped)
- <tool_call name="..."> - tool invocations with <input> containing JSON parameters
- <tool_result name="..." status="success|error"> - tool outputs
- <image mime_type="..." size_bytes="..."/> - placeholder for images
- <previous_summary messages_compacted="N"> - summaries from prior compactions

Provide a structured summary including:
- What was accomplished
- Current work in progress
- Files modified or discussed
- Key decisions and rationale
- User preferences or constraints
- Next steps if identified

Be detailed enough that work can continue seamlessly. Use markdown formatting."#;

/// Escape special XML characters in text content.
fn xml_escape(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&apos;"),
            _ => result.push(c),
        }
    }
    result
}

/// Collected tool result information for pairing with tool calls.
struct ToolResultInfo {
    content: String,
    is_error: bool,
}

/// Extract tool results from a message, indexed by tool_use_id.
fn extract_tool_results(msg: &Message) -> HashMap<String, ToolResultInfo> {
    let mut results = HashMap::new();
    if let MessageContent::Blocks(blocks) = &msg.content {
        for block in blocks {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } = block
            {
                results.insert(
                    tool_use_id.clone(),
                    ToolResultInfo {
                        content: content.clone(),
                        is_error: *is_error,
                    },
                );
            }
        }
    }
    results
}

/// Build an XML representation of the conversation history for summarization.
///
/// This format:
/// - Contains full content (no truncation)
/// - Strips provider-specific data (IDs, signatures, provider_data)
/// - Pairs tool calls with their results within assistant messages
/// - Enables cross-model compaction
fn build_history_xml(messages: &[Message]) -> String {
    let mut xml = String::from("<conversation>\n");

    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];

        // Check if the next message is a tool-result-only user message
        // If so, we'll merge those results into this assistant message
        let tool_results = if msg.role == Role::Assistant && i + 1 < messages.len() {
            let next_msg = &messages[i + 1];
            if next_msg.is_tool_result_only() {
                extract_tool_results(next_msg)
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        // Skip tool-result-only user messages (they're merged into the previous assistant message)
        if msg.is_tool_result_only() {
            i += 1;
            continue;
        }

        let role_str = match msg.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
        };

        xml.push_str(&format!("  <message role=\"{}\">\n", role_str));

        match &msg.content {
            MessageContent::Text(text) => {
                xml.push_str(&format!("    <text>{}</text>\n", xml_escape(text)));
            }
            MessageContent::Blocks(blocks) => {
                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            xml.push_str(&format!("    <text>{}</text>\n", xml_escape(text)));
                        }
                        ContentBlock::Thinking { thinking, .. } => {
                            // Strip provider_data, just keep the thinking content
                            xml.push_str(&format!(
                                "    <thinking>{}</thinking>\n",
                                xml_escape(thinking)
                            ));
                        }
                        ContentBlock::ToolUse {
                            id, name, input, ..
                        } => {
                            // Strip id and thought_signature, keep name and input
                            let input_str = serde_json::to_string(input).unwrap_or_default();
                            xml.push_str(&format!(
                                "    <tool_call name=\"{}\">\n      <input>{}</input>\n",
                                xml_escape(name),
                                xml_escape(&input_str)
                            ));

                            // Look up the corresponding tool result and include it
                            if let Some(result) = tool_results.get(id) {
                                let status = if result.is_error { "error" } else { "success" };
                                xml.push_str(&format!(
                                    "      <tool_result name=\"{}\" status=\"{}\">{}</tool_result>\n",
                                    xml_escape(name),
                                    status,
                                    xml_escape(&result.content)
                                ));
                            }

                            xml.push_str("    </tool_call>\n");
                        }
                        ContentBlock::ToolResult { .. } => {
                            // Tool results in non-tool-result-only messages are handled inline
                            // This shouldn't typically happen, but handle it just in case
                        }
                        ContentBlock::Summary {
                            summary,
                            messages_compacted,
                        } => {
                            xml.push_str(&format!(
                                "    <previous_summary messages_compacted=\"{}\">{}</previous_summary>\n",
                                messages_compacted,
                                xml_escape(summary)
                            ));
                        }
                        ContentBlock::Image { mime_type, data } => {
                            let size_bytes = data.len();
                            xml.push_str(&format!(
                                "    <image mime_type=\"{}\" size_bytes=\"{}\"/>\n",
                                xml_escape(mime_type),
                                size_bytes
                            ));
                        }
                    }
                }
            }
        }

        xml.push_str("  </message>\n");
        i += 1;
    }

    xml.push_str("</conversation>");
    xml
}

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
        if msg.role == Role::User && !msg.is_tool_result_only() {
            turn_count += 1;
            if turn_count > preserve_recent_turns {
                break;
            }
            preserve_from_idx = idx;
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
///
/// Uses XML format to preserve full conversation structure while stripping
/// provider-specific data, enabling cross-model compaction.
pub(crate) fn build_summarization_request_text(messages_to_summarize: &[Message]) -> String {
    let conversation_xml = build_history_xml(messages_to_summarize);

    format!(
        "Please summarize the following conversation:\n\n{}\n\nProvide a comprehensive summary.",
        conversation_xml
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
        let (to_compact, to_preserve) = segment_messages(&messages, 0);
        // With preserve_recent_turns=0, everything can be compacted.
        assert_eq!(to_compact.len(), 2);
        assert!(to_preserve.is_empty());
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
        assert!(prompt.contains("XML format"));
        assert!(prompt.contains("<conversation>"));
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
            assert!(text.contains("<conversation>"));
            assert!(text.contains("</conversation>"));
            assert!(text.contains("<message role=\"user\">"));
            assert!(text.contains("<message role=\"assistant\">"));
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_xml_escape() {
        assert_eq!(xml_escape("hello"), "hello");
        assert_eq!(xml_escape("<tag>"), "&lt;tag&gt;");
        assert_eq!(xml_escape("a & b"), "a &amp; b");
        assert_eq!(xml_escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(xml_escape("it's"), "it&apos;s");
        assert_eq!(
            xml_escape("<script>alert('xss')</script>"),
            "&lt;script&gt;alert(&apos;xss&apos;)&lt;/script&gt;"
        );
    }

    #[test]
    fn test_build_history_xml_simple_conversation() {
        let messages = vec![
            Message::user("Hello"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "Hi there!".to_string(),
            }]),
        ];

        let xml = build_history_xml(&messages);

        assert!(xml.starts_with("<conversation>"));
        assert!(xml.ends_with("</conversation>"));
        assert!(xml.contains("<message role=\"user\">"));
        assert!(xml.contains("<text>Hello</text>"));
        assert!(xml.contains("<message role=\"assistant\">"));
        assert!(xml.contains("<text>Hi there!</text>"));
    }

    #[test]
    fn test_build_history_xml_with_tool_use() {
        let messages = vec![
            Message::user("List files"),
            Message::assistant_blocks(vec![ContentBlock::ToolUse {
                id: "tool_123".to_string(),
                name: "file_list".to_string(),
                input: serde_json::json!({"path": "."}),
                thought_signature: Some("sig_abc".to_string()),
            }]),
            Message {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "tool_123".to_string(),
                    content: "file1.rs\nfile2.rs".to_string(),
                    is_error: false,
                    data: None,
                    mime_type: None,
                }]),
            },
        ];

        let xml = build_history_xml(&messages);

        // Should have tool call with nested result
        assert!(xml.contains("<tool_call name=\"file_list\">"));
        assert!(xml.contains("<input>{&quot;path&quot;:&quot;.&quot;}</input>"));
        assert!(xml.contains("<tool_result name=\"file_list\" status=\"success\">"));
        assert!(xml.contains("file1.rs\nfile2.rs</tool_result>"));

        // Should NOT contain provider-specific IDs or signatures
        assert!(!xml.contains("tool_123"));
        assert!(!xml.contains("sig_abc"));

        // Tool result message should be merged, not separate
        let user_message_count = xml.matches("<message role=\"user\">").count();
        assert_eq!(user_message_count, 1); // Only the "List files" message
    }

    #[test]
    fn test_build_history_xml_with_tool_error() {
        let messages = vec![
            Message::user("Read nonexistent file"),
            Message::assistant_blocks(vec![ContentBlock::ToolUse {
                id: "tool_456".to_string(),
                name: "file_read".to_string(),
                input: serde_json::json!({"path": "/nonexistent"}),
                thought_signature: None,
            }]),
            Message {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "tool_456".to_string(),
                    content: "File not found".to_string(),
                    is_error: true,
                    data: None,
                    mime_type: None,
                }]),
            },
        ];

        let xml = build_history_xml(&messages);

        assert!(xml.contains("<tool_result name=\"file_read\" status=\"error\">"));
        assert!(xml.contains("File not found</tool_result>"));
    }

    #[test]
    fn test_build_history_xml_with_thinking() {
        let messages = vec![
            Message::user("Complex question"),
            Message::assistant_blocks(vec![
                ContentBlock::Thinking {
                    thinking: "Let me think about this...".to_string(),
                    provider_data: Some(serde_json::json!({
                        "signature": "encrypted_sig",
                        "encrypted_content": "secret_data"
                    })),
                },
                ContentBlock::Text {
                    text: "Here's my answer".to_string(),
                },
            ]),
        ];

        let xml = build_history_xml(&messages);

        // Should have thinking content
        assert!(xml.contains("<thinking>Let me think about this...</thinking>"));

        // Should NOT contain provider_data
        assert!(!xml.contains("encrypted_sig"));
        assert!(!xml.contains("secret_data"));
        assert!(!xml.contains("signature"));
    }

    #[test]
    fn test_build_history_xml_with_image() {
        let messages = vec![
            Message::user("Check this image"),
            Message {
                role: Role::User,
                content: MessageContent::Blocks(vec![ContentBlock::Image {
                    mime_type: "image/png".to_string(),
                    data: vec![0u8; 1024], // 1KB of fake image data
                }]),
            },
        ];

        let xml = build_history_xml(&messages);

        // Should have image placeholder with metadata
        assert!(xml.contains("<image mime_type=\"image/png\" size_bytes=\"1024\"/>"));

        // Should NOT contain base64 or raw image data
        assert!(!xml.contains("AAAA")); // base64 encoding of zeros
    }

    #[test]
    fn test_build_history_xml_with_previous_summary() {
        let messages = vec![Message {
            role: Role::User,
            content: MessageContent::Blocks(vec![ContentBlock::Summary {
                summary: "Earlier we discussed file operations.".to_string(),
                messages_compacted: 15,
            }]),
        }];

        let xml = build_history_xml(&messages);

        assert!(xml.contains("<previous_summary messages_compacted=\"15\">"));
        assert!(xml.contains("Earlier we discussed file operations.</previous_summary>"));
    }

    #[test]
    fn test_build_history_xml_escapes_special_chars() {
        let messages = vec![
            Message::user("What about <script> tags & \"quotes\"?"),
            Message::assistant_blocks(vec![ContentBlock::Text {
                text: "Here's a <div> example: x < y && y > z".to_string(),
            }]),
        ];

        let xml = build_history_xml(&messages);

        assert!(xml.contains("&lt;script&gt;"));
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("&quot;quotes&quot;"));
        assert!(xml.contains("&lt;div&gt;"));
        assert!(xml.contains("x &lt; y &amp;&amp; y &gt; z"));
    }

    #[test]
    fn test_build_history_xml_multiple_tool_calls() {
        let messages = vec![
            Message::user("Read two files"),
            Message::assistant_blocks(vec![
                ContentBlock::ToolUse {
                    id: "tool_1".to_string(),
                    name: "file_read".to_string(),
                    input: serde_json::json!({"path": "a.txt"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_2".to_string(),
                    name: "file_read".to_string(),
                    input: serde_json::json!({"path": "b.txt"}),
                    thought_signature: None,
                },
            ]),
            Message {
                role: Role::User,
                content: MessageContent::Blocks(vec![
                    ContentBlock::ToolResult {
                        tool_use_id: "tool_1".to_string(),
                        content: "Content of a.txt".to_string(),
                        is_error: false,
                        data: None,
                        mime_type: None,
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "tool_2".to_string(),
                        content: "Content of b.txt".to_string(),
                        is_error: false,
                        data: None,
                        mime_type: None,
                    },
                ]),
            },
        ];

        let xml = build_history_xml(&messages);

        // Both tool calls should be present with their results
        assert!(xml.contains("Content of a.txt"));
        assert!(xml.contains("Content of b.txt"));

        // Tool result message should be merged (only 1 user message)
        let user_message_count = xml.matches("<message role=\"user\">").count();
        assert_eq!(user_message_count, 1);
    }
}
