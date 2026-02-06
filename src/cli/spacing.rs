// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Shared block spacing rules for CLI output.
//!
//! Both live streaming (`listener.rs`) and history replay (`render.rs`) use the same
//! state-machine rules to decide when to insert a blank padding line between logical blocks.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LastBlock {
    UserPrompt,
    Thinking,
    Text,
    Info,
    /// Tool call banner lines (the "● ..." lines).
    ToolCall,
    /// Tool-related content (tool results, tool output, file previews/diffs, etc.).
    ToolContent,
}

/// Determine if a blank line should be inserted before `current` based on the previous block.
///
/// A "blank line" means ensuring there is an empty row between the two blocks (i.e. at least two
/// trailing newlines in the output stream).
///
/// Callers should fall back to `terminal::ensure_line_break()` (or equivalent) when this returns
/// false so the next block still begins on a fresh line when needed.
pub(crate) fn needs_blank_line_before(prev: Option<LastBlock>, current: LastBlock) -> bool {
    let Some(prev) = prev else {
        return false;
    };

    // Info is always visually separated from other blocks, but consecutive info lines should stay
    // grouped.
    match (prev, current) {
        (LastBlock::Info, LastBlock::Info) => false,
        (_, LastBlock::Info) => true,
        (LastBlock::Info, _) => true,
        (LastBlock::UserPrompt, LastBlock::UserPrompt) => false,
        (_, LastBlock::UserPrompt) => true,
        _ => matches!(
            (prev, current),
            // UserPrompt -> Thinking: blank line
            (LastBlock::UserPrompt, LastBlock::Thinking)
                // Tool -> Thinking: blank line
                | (LastBlock::ToolCall | LastBlock::ToolContent, LastBlock::Thinking)
                // UserPrompt -> Text: blank line
                | (LastBlock::UserPrompt, LastBlock::Text)
                // Thinking -> Text: blank line
                | (LastBlock::Thinking, LastBlock::Text)
                // Tool -> Text: blank line
                | (LastBlock::ToolCall | LastBlock::ToolContent, LastBlock::Text)
                // Tool -> ToolCall: blank line (separate distinct tool calls)
                | (LastBlock::ToolCall | LastBlock::ToolContent, LastBlock::ToolCall)
                // Thinking/Text -> ToolCall: blank line
                | (LastBlock::Thinking | LastBlock::Text, LastBlock::ToolCall)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{LastBlock, needs_blank_line_before};

    #[test]
    fn info_blocks_are_separated_but_grouped() {
        assert!(!needs_blank_line_before(None, LastBlock::Info));
        assert!(needs_blank_line_before(
            Some(LastBlock::Text),
            LastBlock::Info
        ));
        assert!(!needs_blank_line_before(
            Some(LastBlock::Info),
            LastBlock::Info
        ));
        assert!(needs_blank_line_before(
            Some(LastBlock::Info),
            LastBlock::ToolContent
        ));
        assert!(needs_blank_line_before(
            Some(LastBlock::Info),
            LastBlock::Text
        ));
    }

    #[test]
    fn user_prompt_is_separated_from_previous_turn() {
        assert!(needs_blank_line_before(
            Some(LastBlock::Text),
            LastBlock::UserPrompt
        ));
        assert!(needs_blank_line_before(
            Some(LastBlock::ToolContent),
            LastBlock::UserPrompt
        ));
        assert!(!needs_blank_line_before(
            Some(LastBlock::UserPrompt),
            LastBlock::UserPrompt
        ));
    }

    #[test]
    fn tool_calls_space_between_themselves() {
        assert!(needs_blank_line_before(
            Some(LastBlock::ToolCall),
            LastBlock::ToolCall
        ));
        assert!(needs_blank_line_before(
            Some(LastBlock::ToolContent),
            LastBlock::ToolCall
        ));
        assert!(needs_blank_line_before(
            Some(LastBlock::Thinking),
            LastBlock::ToolCall
        ));
        assert!(needs_blank_line_before(
            Some(LastBlock::Text),
            LastBlock::ToolCall
        ));
    }

    #[test]
    fn tool_banner_is_grouped_with_tool_content() {
        assert!(!needs_blank_line_before(
            Some(LastBlock::ToolCall),
            LastBlock::ToolContent
        ));
    }
}
