// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Markdown formatting for CLI output.

use colored::Colorize;
use unicode_width::UnicodeWidthChar;

const LINK_FG: (u8, u8, u8) = (120, 190, 255);
const HEADING_FG: (u8, u8, u8) = (0, 175, 255);

fn style_inline_code_tick(s: &str) -> String {
    s.bright_black().to_string()
}

fn style_inline_code(s: &str) -> String {
    s.cyan().to_string()
}

fn style_link(s: &str) -> String {
    s.truecolor(LINK_FG.0, LINK_FG.1, LINK_FG.2)
        .underline()
        .to_string()
}

pub(crate) fn render_markdown_line(line: &str) -> String {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let leading_len = line.len() - trimmed.len();

    // Headings: bold the entire line including the # markers.
    if is_heading_line(trimmed) {
        let (hashes, rest) = trimmed.split_once(' ').unwrap_or((trimmed, ""));
        let mut out = String::new();
        out.push_str(&line[..leading_len]);
        out.push_str(
            &format!("{} {}", hashes, rest.trim())
                .truecolor(HEADING_FG.0, HEADING_FG.1, HEADING_FG.2)
                .bold()
                .to_string(),
        );
        return out;
    }

    render_markdown_inlines(line)
}

pub(crate) fn is_heading_line(line: &str) -> bool {
    let trimmed = line.trim_start_matches([' ', '\t']);
    if let Some((hashes, _rest)) = trimmed.split_once(' ') {
        !hashes.is_empty() && hashes.chars().all(|c| c == '#')
    } else {
        false
    }
}

pub(crate) fn render_markdown_inlines(text: &str) -> String {
    render_markdown_inlines_with_style(text, None)
}

pub(crate) fn render_markdown_inlines_with_style(text: &str, base_style: Option<&str>) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 2);

    let mut chars = text.chars().peekable();
    let mut in_inline_code = false;
    let mut inline_code_buf = String::new();

    while let Some(ch) = chars.next() {
        if in_inline_code {
            if ch == '`' {
                if !inline_code_buf.is_empty() {
                    out.push_str(&style_inline_code(&inline_code_buf));
                    if let Some(style) = base_style {
                        out.push_str(style);
                    }
                    inline_code_buf.clear();
                }
                out.push_str(&style_inline_code_tick("`"));
                if let Some(style) = base_style {
                    out.push_str(style);
                }
                in_inline_code = false;
            } else {
                inline_code_buf.push(ch);
            }
            continue;
        }

        match ch {
            '`' => {
                out.push_str(&style_inline_code_tick("`"));
                in_inline_code = true;
                inline_code_buf.clear();
            }
            '*' => {
                // Check for **bold** pattern
                if chars.peek() == Some(&'*') {
                    chars.next(); // consume second *
                    let mut bold_content = String::new();
                    let mut closed = false;

                    while let Some(next) = chars.next() {
                        if next == '*' && chars.peek() == Some(&'*') {
                            chars.next(); // consume closing **
                            closed = true;
                            break;
                        }
                        bold_content.push(next);
                    }

                    if closed && !bold_content.is_empty() {
                        out.push_str("**");
                        out.push_str(&bold_content.bold().to_string());
                        if let Some(style) = base_style {
                            out.push_str(style);
                        }
                        out.push_str("**");
                    } else {
                        // Not a valid bold span, output literally
                        out.push_str("**");
                        out.push_str(&bold_content);
                    }
                } else {
                    out.push('*');
                }
            }
            '_' => {
                // Check for __bold__ pattern
                if chars.peek() == Some(&'_') {
                    chars.next(); // consume second _
                    let mut bold_content = String::new();
                    let mut closed = false;

                    while let Some(next) = chars.next() {
                        if next == '_' && chars.peek() == Some(&'_') {
                            chars.next(); // consume closing __
                            closed = true;
                            break;
                        }
                        bold_content.push(next);
                    }

                    if closed && !bold_content.is_empty() {
                        out.push_str("__");
                        out.push_str(&bold_content.bold().to_string());
                        if let Some(style) = base_style {
                            out.push_str(style);
                        }
                        out.push_str("__");
                    } else {
                        // Not a valid bold span, output literally
                        out.push_str("__");
                        out.push_str(&bold_content);
                    }
                } else {
                    out.push('_');
                }
            }
            '[' => {
                // Simple markdown link: [label](url)
                // If it doesn't match exactly, fall back to literal output.
                let mut label = String::new();
                let mut ok = false;

                for next in chars.by_ref() {
                    if next == ']' {
                        ok = true;
                        break;
                    }
                    label.push(next);
                }

                if ok && chars.peek() == Some(&'(') {
                    chars.next();
                    let mut url = String::new();
                    let mut closed = false;
                    for next in chars.by_ref() {
                        if next == ')' {
                            closed = true;
                            break;
                        }
                        url.push(next);
                    }

                    if closed {
                        out.push('[');
                        out.push_str(&style_link(&label));
                        if let Some(style) = base_style {
                            out.push_str(style);
                        }
                        out.push_str("](");
                        out.push_str(&style_link(&url));
                        if let Some(style) = base_style {
                            out.push_str(style);
                        }
                        out.push(')');
                    } else {
                        out.push('[');
                        out.push_str(&label);
                        out.push_str("](");
                        out.push_str(&url);
                    }
                } else {
                    out.push('[');
                    out.push_str(&label);
                    if ok {
                        out.push(']');
                    }
                }
            }
            _ => out.push(ch),
        }
    }
    // If the line ended while still inside a code span, flush buffered text.
    if in_inline_code && !inline_code_buf.is_empty() {
        out.push_str(&style_inline_code(&inline_code_buf));
        if let Some(style) = base_style {
            out.push_str(style);
        }
    }

    out
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

/// Parse a table row into cells
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

/// Calculate the display width of a string
pub(crate) fn display_width(s: &str) -> usize {
    s.chars().filter_map(UnicodeWidthChar::width).sum()
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
                            col_widths[col_idx] = col_widths[col_idx].max(display_width(cell));
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
                    let padding = width.saturating_sub(display_width(cell));
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
                        let padding = width.saturating_sub(display_width(cell));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn enable_colors() {
        colored::control::set_override(true);
    }

    #[test]
    fn test_render_markdown_inline_code_adds_ansi() {
        enable_colors();
        let input = "Use `cargo test` now";
        let rendered = render_markdown_line(input);
        assert!(rendered.contains("cargo test"));
        // Should have cyan color code
        assert!(rendered.contains("\x1b[36m")); // cyan
    }

    #[test]
    fn test_render_markdown_link_adds_ansi() {
        enable_colors();
        let input = "See [docs](https://example.com)";
        let rendered = render_markdown_line(input);
        assert!(rendered.contains("docs"));
        assert!(rendered.contains("https://example.com"));
        assert!(rendered.contains("\x1b["));
    }

    #[test]
    fn test_render_markdown_bold_text() {
        enable_colors();
        let input = "This is **bold** text";
        let rendered = render_markdown_inlines(input);
        assert!(rendered.contains("bold"));
        assert!(rendered.contains("\x1b[1m")); // bold ANSI code
    }

    #[test]
    fn test_render_markdown_heading_bold() {
        enable_colors();
        let input = "## My Heading";
        let rendered = render_markdown_line(input);
        assert!(rendered.contains("##"));
        assert!(rendered.contains("My Heading"));
        // colored crate combines bold with color as \x1b[1;38;2;...
        assert!(rendered.contains("\x1b[1;"));
    }

    #[test]
    fn test_is_heading_line() {
        assert!(is_heading_line("# Heading"));
        assert!(is_heading_line("  ### Heading"));
        assert!(!is_heading_line("####Heading"));
        assert!(!is_heading_line("Not heading"));
    }

    #[test]
    fn test_is_table_separator_valid() {
        assert!(is_table_separator("|---|---|"));
        assert!(is_table_separator("|-------|-------|"));
        assert!(is_table_separator("|:---|:---|"));
        assert!(is_table_separator("| --- | --- |"));
    }

    #[test]
    fn test_is_table_separator_invalid() {
        assert!(!is_table_separator("not a table"));
        assert!(!is_table_separator("|---|--")); // uneven columns
        assert!(!is_table_separator("| cell | cell |"));
    }

    #[test]
    fn test_parse_table_row() {
        let row = "| header1 | header2 |";
        let cells = parse_table_row(row);
        assert_eq!(
            cells,
            Some(vec!["header1".to_string(), "header2".to_string()])
        );
    }

    #[test]
    fn test_parse_table_row_invalid() {
        // Single cell row is valid as a single-column table
        assert!(parse_table_row("| cell |").is_some());
        // Non-table text should return None
        assert!(parse_table_row("not a row").is_none());
    }

    #[test]
    fn test_display_width() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("你好"), 4);
    }

    #[test]
    fn test_align_markdown_tables() {
        let input = "| Name | Age |\n|---\n| Alice | 30 |\n| Bob | 25 |";
        let result = align_markdown_tables(input, Some(80));
        // Name (4 chars) and Age (3 chars) - Name doesn't need padding but Age doesn't need extra space
        assert!(result.contains("| Name | Age |"));
        assert!(result.contains("| Alice | 30 |"));
        assert!(result.contains("| Bob | 25 |")); // Bob doesn't need padding
    }

    #[test]
    fn test_align_markdown_tables_too_wide() {
        // Create a table that would exceed 20 columns
        let input = "| VeryLongHeader1 | VeryLongHeader2 |\n|---\n| Value1 | Value2 |";
        let result = align_markdown_tables(input, Some(20));
        // Should preserve original when too wide
        assert_eq!(result, input);
    }

    #[test]
    fn test_align_markdown_tables_no_tables() {
        let input = "Just some text\nwith no tables";
        let result = align_markdown_tables(input, Some(80));
        assert_eq!(result, input);
    }

    #[test]
    fn test_calculate_table_row_width() {
        let widths = vec![5, 5, 5];
        // | cell | cell | cell | = 1 + (5+3)*3 = 1 + 24 = 25
        assert_eq!(calculate_table_row_width(&widths), 25);
    }
}
