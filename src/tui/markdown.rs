// SPDX-License-Identifier: MIT
// Markdown parsing and styling for the TUI

use super::layout::char_display_width;

/// Markdown inline styling
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum MarkdownStyle {
    Normal,
    Bold,
    InlineCode,
}

/// A span of markdown-formatted text
pub(crate) struct MarkdownSpan {
    pub start: usize,        // byte offset of content start (after opening marker)
    pub end: usize,          // byte offset of content end (before closing marker)
    pub marker_start: usize, // byte offset where marker begins
    pub marker_end: usize,   // byte offset where closing marker ends
    pub style: MarkdownStyle,
}

/// Find markdown spans in text (bold with ** and inline code with `)
pub(crate) fn find_markdown_spans(text: &str) -> Vec<MarkdownSpan> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Check for inline code: `code`
        if bytes[i] == b'`' && (i + 1 >= len || bytes[i + 1] != b'`') {
            // Single backtick - find closing backtick
            if let Some(end) = find_single_backtick_end(bytes, i + 1) {
                spans.push(MarkdownSpan {
                    start: i + 1,
                    end,
                    marker_start: i,
                    marker_end: end + 1,
                    style: MarkdownStyle::InlineCode,
                });
                i = end + 1;
                continue;
            }
        }

        // Check for bold: **text**
        if i + 1 < len
            && bytes[i] == b'*'
            && bytes[i + 1] == b'*'
            && let Some(end) = find_double_asterisk_end(bytes, i + 2)
        {
            spans.push(MarkdownSpan {
                start: i + 2,
                end,
                marker_start: i,
                marker_end: end + 2,
                style: MarkdownStyle::Bold,
            });
            i = end + 2;
            continue;
        }

        i += 1;
    }

    spans
}

/// Find the closing single backtick (not inside a word)
fn find_single_backtick_end(bytes: &[u8], start: usize) -> Option<usize> {
    for (i, &byte) in bytes.iter().enumerate().skip(start) {
        if byte == b'`' {
            // Found closing backtick
            return Some(i);
        }
        if byte == b'\n' {
            // No inline code across newlines
            return None;
        }
    }
    None
}

/// Find the closing ** for bold
fn find_double_asterisk_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Get the markdown style at a byte offset, also returns whether this is a marker character
pub(crate) fn get_markdown_style(
    byte_offset: usize,
    spans: &[MarkdownSpan],
) -> (MarkdownStyle, bool) {
    for span in spans {
        // Check if in marker region (should be hidden)
        if (byte_offset >= span.marker_start && byte_offset < span.start)
            || (byte_offset >= span.end && byte_offset < span.marker_end)
        {
            return (span.style, true); // is_marker = true
        }
        // Check if in content region
        if byte_offset >= span.start && byte_offset < span.end {
            return (span.style, false);
        }
    }
    (MarkdownStyle::Normal, false)
}

/// Detect if a line is a table separator (e.g., |---|---|)
pub(crate) fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') || trimmed.len() < 2 {
        return false;
    }

    // Check if all cells contain only hyphens, colons, and whitespace
    let inner = &trimmed[1..trimmed.len() - 1];
    for cell in inner.split('|') {
        let cell_trimmed = cell.trim();
        if cell_trimmed.is_empty() {
            continue;
        }
        if !cell_trimmed
            .chars()
            .all(|c| c == '-' || c == ':' || c.is_whitespace())
        {
            return false;
        }
    }
    true
}

/// Parse a table row into cells, preserving markdown formatting
pub(crate) fn parse_table_row(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.ends_with('|') || trimmed.len() < 2 {
        return None;
    }

    let inner = &trimmed[1..trimmed.len() - 1];
    Some(
        inner
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect(),
    )
}

/// Calculate the display width of a string, excluding markdown markers
pub(crate) fn calculate_display_width(text: &str) -> usize {
    let spans = find_markdown_spans(text);
    let mut width = 0;

    for (byte_idx, ch) in text.char_indices() {
        let (_, is_marker) = get_markdown_style(byte_idx, &spans);
        if !is_marker {
            width += char_display_width(ch);
        }
    }

    width
}

/// Calculate the total width of a formatted table row
fn calculate_table_row_width(col_widths: &[usize]) -> usize {
    // Format: | cell1 | cell2 | ... |
    // Each column contributes: 1 (space) + width + 1 (space) + 1 (|)
    // Plus 1 for the leading |
    1 + col_widths.iter().map(|w| w + 3).sum::<usize>()
}

/// Align markdown tables for consistent column widths.
/// If max_width is provided and the formatted table would exceed it,
/// the original table text is preserved without formatting.
pub(crate) fn align_markdown_tables(text: &str, max_width: Option<usize>) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return text.to_string();
    }

    let mut result = String::with_capacity(text.len() + text.len() / 10);
    let mut i = 0;

    while i < lines.len() {
        // Check if this line starts a table
        if let Some(header_cells) = parse_table_row(lines[i]) {
            // Look for separator on next line
            if i + 1 < lines.len() && is_table_separator(lines[i + 1]) {
                // Found a table - collect all rows
                let mut table_rows: Vec<Vec<String>> = vec![header_cells];
                let table_start = i;
                let mut j = i + 2; // Skip header and separator

                while j < lines.len() {
                    if let Some(cells) = parse_table_row(lines[j]) {
                        table_rows.push(cells);
                        j += 1;
                    } else {
                        break;
                    }
                }

                // Calculate column widths
                let num_cols = table_rows.iter().map(|r| r.len()).max().unwrap_or(0);
                let mut col_widths: Vec<usize> = vec![0; num_cols];

                for row in &table_rows {
                    for (col_idx, cell) in row.iter().enumerate() {
                        if col_idx < num_cols {
                            col_widths[col_idx] =
                                col_widths[col_idx].max(calculate_display_width(cell));
                        }
                    }
                }

                // Minimum width of 3 for separator dashes
                for w in &mut col_widths {
                    *w = (*w).max(3);
                }

                // Check if formatted table would be too wide
                let table_width = calculate_table_row_width(&col_widths);
                if let Some(max) = max_width
                    && table_width > max
                {
                    // Table is too wide - preserve original lines without formatting
                    for line in lines.iter().take(j).skip(table_start) {
                        result.push_str(line);
                        result.push('\n');
                    }
                    i = j;
                    continue;
                }

                // Render header
                result.push('|');
                for (col_idx, cell) in table_rows[0].iter().enumerate() {
                    let width = col_widths.get(col_idx).copied().unwrap_or(3);
                    let display_width = calculate_display_width(cell);
                    let padding = width.saturating_sub(display_width);
                    result.push(' ');
                    result.push_str(cell);
                    for _ in 0..padding {
                        result.push(' ');
                    }
                    result.push_str(" |");
                }
                result.push('\n');

                // Render separator
                result.push('|');
                for &width in &col_widths {
                    result.push('-');
                    for _ in 0..width {
                        result.push('-');
                    }
                    result.push_str("-|");
                }
                result.push('\n');

                // Render data rows
                for row in table_rows.iter().skip(1) {
                    result.push('|');
                    for col_idx in 0..num_cols {
                        let cell = row.get(col_idx).map(|s| s.as_str()).unwrap_or("");
                        let width = col_widths.get(col_idx).copied().unwrap_or(3);
                        let display_width = calculate_display_width(cell);
                        let padding = width.saturating_sub(display_width);
                        result.push(' ');
                        result.push_str(cell);
                        for _ in 0..padding {
                            result.push(' ');
                        }
                        result.push_str(" |");
                    }
                    result.push('\n');
                }

                i = j;
                continue;
            }
        }

        // Not a table line, just copy it
        result.push_str(lines[i]);
        result.push('\n');
        i += 1;
    }

    // Remove trailing newline if original didn't have one
    if !text.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
}
