// SPDX-License-Identifier: AGPL-3.0-only

use textwrap::{Options, wrap};

/// Wraps text to fit within the specified width, preserving formatting where possible
pub fn wrap_text(text: &str, width: usize) -> String {
    // Handle edge cases
    if width == 0 || text.is_empty() {
        return text.to_string();
    }

    let options = Options::new(width)
        .break_words(false)
        .word_separator(textwrap::WordSeparator::AsciiSpace);

    // Process the text line by line, tracking code blocks
    let mut result = Vec::new();
    let mut in_code_block = false;

    for line in text.lines() {
        if line.starts_with("```") {
            // Toggle code block state
            in_code_block = !in_code_block;
            result.push(line.to_string());
        } else if in_code_block || line.starts_with("    ") || line.starts_with("\t") {
            // Don't wrap code blocks or indented code
            result.push(line.to_string());
        } else if line.trim().is_empty() {
            // Preserve empty lines
            result.push(String::new());
        } else {
            // Wrap normal text
            let wrapped = wrap(line, &options);
            for wrapped_line in wrapped {
                result.push(wrapped_line.to_string());
            }
        }
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_text() {
        let text =
            "This is a very long line that should be wrapped to fit within a specific width.";
        let wrapped = wrap_text(text, 20);
        assert!(wrapped.lines().all(|line| line.len() <= 20));
    }

    #[test]
    fn test_preserve_code_blocks() {
        let text =
            "Normal text\n```\nvery long code line that should not be wrapped\n```\nMore text";
        let wrapped = wrap_text(text, 20);
        assert!(wrapped.contains("very long code line that should not be wrapped"));
    }

    #[test]
    fn test_preserve_empty_lines() {
        let text = "First paragraph\n\nSecond paragraph";
        let wrapped = wrap_text(text, 50);
        assert_eq!(wrapped, "First paragraph\n\nSecond paragraph");
    }
}
