// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Syntax highlighting for TUI code blocks.
//!
//! This module provides TUI-specific syntax highlighting functionality,
//! delegating to the shared `crate::syntax` module for actual highlighting.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use ratatui::style::Color;

/// A highlighted span with byte range and color (TUI-specific with ratatui Color)
#[derive(Debug, Clone)]
pub(crate) struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    pub color: Color,
}

/// Result of parsing a code block from text
#[derive(Debug, Clone)]
pub(crate) struct CodeBlock {
    /// Byte offset where the content starts (after the opening ``` line)
    pub content_start: usize,
    /// Byte offset where the content ends (before the closing ```)
    pub content_end: usize,
    /// Byte offset where the entire block ends (after the closing ``` line)
    block_end: usize,
    /// Highlighted spans for the code content (offsets relative to content_start)
    pub highlights: Vec<HighlightSpan>,
}

/// Parse and highlight all code blocks in the given text
pub(crate) fn highlight_code_blocks(text: &str) -> Vec<CodeBlock> {
    let mut blocks = Vec::new();
    let mut search_start = 0;

    while let Some(block) = find_next_code_block(text, search_start) {
        search_start = block.block_end;
        blocks.push(block);
    }

    blocks
}

/// Find the next code block starting from the given byte offset
fn find_next_code_block(text: &str, start: usize) -> Option<CodeBlock> {
    let remaining = &text[start..];

    // Find opening fence
    let fence_start = remaining.find("```")?;
    let absolute_fence_start = start + fence_start;

    // Find the end of the opening fence line
    let after_fence = &remaining[fence_start + 3..];
    let line_end = after_fence.find('\n').unwrap_or(after_fence.len());
    let language = after_fence[..line_end].trim();
    let language = if language.is_empty() {
        None
    } else {
        Some(language.to_string())
    };

    let content_start = absolute_fence_start + 3 + line_end + 1; // +1 for newline
    if content_start > text.len() {
        return None;
    }

    // Find closing fence
    let content_remaining = &text[content_start..];
    let closing_fence = find_closing_fence(content_remaining)?;
    let content_end = content_start + closing_fence;

    // Calculate block end (after the closing ``` line)
    // content_end points to where ``` starts
    let after_closing_fence = content_end + 3; // skip past ```
    let block_end =
        if after_closing_fence < text.len() && text.as_bytes()[after_closing_fence] == b'\n' {
            after_closing_fence + 1
        } else {
            after_closing_fence
        };

    // Extract and highlight the code content
    let code_content = &text[content_start..content_end];
    let highlights = highlight_code(code_content, language.as_deref());

    Some(CodeBlock {
        content_start,
        content_end,
        block_end,
        highlights,
    })
}

/// Find the closing ``` fence, accounting for nested code blocks
fn find_closing_fence(text: &str) -> Option<usize> {
    // Simple approach: find ``` at the start of a line
    let mut pos = 0;
    for line in text.lines() {
        if line.trim() == "```" {
            return Some(pos);
        }
        pos += line.len() + 1; // +1 for newline
    }
    None
}

/// Highlight code content and return spans with ratatui Colors.
/// Delegates to the shared syntax module and converts colors.
fn highlight_code(code: &str, language: Option<&str>) -> Vec<HighlightSpan> {
    crate::syntax::highlight_code(code, language)
        .into_iter()
        .map(|span| HighlightSpan {
            start: span.start,
            end: span.end,
            color: Color::Rgb(span.color.r, span.color.g, span.color.b),
        })
        .collect()
}

/// Cache for parsed code blocks to avoid re-parsing on every render
static HIGHLIGHT_CACHE: OnceLock<Mutex<HashMap<u64, Vec<CodeBlock>>>> = OnceLock::new();

/// Maximum number of entries in the cache before eviction
const MAX_CACHE_SIZE: usize = 32;

fn cache() -> &'static Mutex<HashMap<u64, Vec<CodeBlock>>> {
    HIGHLIGHT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Simple hash function for text content
fn hash_text(text: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Lookup structure for efficient per-character style queries
pub(crate) struct HighlightLookup {
    /// Code blocks with their absolute byte positions
    blocks: Vec<CodeBlock>,
}

impl HighlightLookup {
    /// Create a new lookup from the given text
    pub(crate) fn new(text: &str) -> Self {
        let hash = hash_text(text);

        // Try to get from cache first
        if let Ok(mut cache) = cache().lock() {
            if let Some(blocks) = cache.get(&hash) {
                return Self {
                    blocks: blocks.clone(),
                };
            }

            // Not in cache, compute it
            let blocks = highlight_code_blocks(text);

            // Evict old entries if cache is too large
            if cache.len() >= MAX_CACHE_SIZE {
                cache.clear();
            }

            cache.insert(hash, blocks.clone());

            return Self { blocks };
        }

        // Fallback if lock fails (shouldn't happen in practice)
        Self {
            blocks: highlight_code_blocks(text),
        }
    }

    /// Get the highlight color for a given byte offset in the text
    /// Returns None if the offset is not within a highlighted code block
    pub(crate) fn color_at(&self, byte_offset: usize) -> Option<Color> {
        for block in &self.blocks {
            if byte_offset >= block.content_start && byte_offset < block.content_end {
                // Find the span containing this offset
                let relative_offset = byte_offset - block.content_start;
                for span in &block.highlights {
                    if relative_offset >= span.start && relative_offset < span.end {
                        return Some(span.color);
                    }
                }
                // Inside code block but no specific highlight found
                return None;
            }
        }
        None
    }
}

/// Highlight code for diff rendering.
pub(crate) fn highlight_code_for_diff(code: &str, language: &str) -> Vec<HighlightSpan> {
    highlight_code(code, Some(language))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_highlighting() {
        let text = r#"```python
def hello():
    print("world")
```"#;

        let lookup = HighlightLookup::new(text);
        // "def" keyword should have syntax highlighting
        let def_start = text.find("def").unwrap();
        assert!(
            lookup.color_at(def_start).is_some(),
            "Python 'def' keyword should be highlighted"
        );
    }

    #[test]
    fn test_multiple_code_blocks() {
        let text = r#"Some text
```rust
fn main() {}
```

```python
def test():
    pass
```

```javascript
const x = 42;
```
"#;

        let blocks = highlight_code_blocks(text);
        println!("\nFound {} code blocks", blocks.len());

        for (i, block) in blocks.iter().enumerate() {
            let code = &text[block.content_start..block.content_end];
            println!(
                "Block {}: {} highlights, content: {:?}",
                i,
                block.highlights.len(),
                &code[..code.len().min(20)]
            );
        }

        assert_eq!(blocks.len(), 3, "Should find 3 code blocks");
        assert!(
            !blocks[0].highlights.is_empty(),
            "Rust block should have highlights"
        );
        assert!(
            !blocks[1].highlights.is_empty(),
            "Python block should have highlights"
        );
        assert!(
            !blocks[2].highlights.is_empty(),
            "JavaScript block should have highlights"
        );
    }

    #[test]
    fn test_find_code_block() {
        let text = r#"Some text
```rust
fn main() {
    println!("hello");
}
```
More text"#;

        let blocks = highlight_code_blocks(text);
        assert_eq!(blocks.len(), 1);

        let code = &text[blocks[0].content_start..blocks[0].content_end];
        assert!(code.starts_with("fn main()"));
    }

    #[test]
    fn test_highlight_lookup() {
        let text = r#"Hello
```rust
let x = 42;
```
World"#;

        let lookup = HighlightLookup::new(text);

        // "Hello" should not have syntax highlighting (not in code block)
        assert!(lookup.color_at(0).is_none());

        // The code content should have syntax highlighting
        let code_start = text.find("let x").unwrap();
        assert!(lookup.color_at(code_start).is_some());

        // "World" should not have syntax highlighting (not in code block)
        let world_start = text.find("World").unwrap();
        assert!(lookup.color_at(world_start).is_none());
    }
}
