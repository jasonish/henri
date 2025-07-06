use crossterm::{
    cursor, execute,
    style::{
        Attribute, Color as CColor, Print, ResetColor, SetAttribute, SetBackgroundColor,
        SetForegroundColor,
    },
    terminal,
};
use std::io::{Write, stdout};

use crate::utils::wrap_text;

/// Display style for different types of messages
#[derive(Debug, Clone, Copy)]
pub enum DisplayStyle {
    /// Assistant responses - blue border/background
    Assistant,
    /// Tool outputs - magenta border
    Tool,
    /// Error messages - red border
    Error,
    /// System messages - yellow border
    System,
    /// User messages - green border
    User,
}

/// Enhanced display with full-width backgrounds
pub fn print_full_width_message(prefix: &str, message: &str, style: DisplayStyle) {
    let mut stdout = stdout();
    let (term_width, _) = terminal::size().unwrap_or((80, 24));
    let term_width = term_width as usize;

    // Define colors for each style
    let (prefix_color, bg_color, text_color) = match style {
        DisplayStyle::Assistant => (
            CColor::Cyan,
            CColor::Rgb {
                r: 15,
                g: 25,
                b: 35,
            }, // Dark blue background
            CColor::White,
        ),
        DisplayStyle::Tool => (
            CColor::Magenta,
            CColor::Rgb {
                r: 30,
                g: 15,
                b: 30,
            }, // Dark purple background
            CColor::White,
        ),
        DisplayStyle::Error => (
            CColor::Red,
            CColor::Rgb {
                r: 40,
                g: 15,
                b: 15,
            }, // Dark red background
            CColor::White,
        ),
        DisplayStyle::System => (
            CColor::Yellow,
            CColor::Rgb {
                r: 35,
                g: 30,
                b: 15,
            }, // Dark yellow background
            CColor::White,
        ),
        DisplayStyle::User => (
            CColor::Green,
            CColor::Rgb {
                r: 15,
                g: 30,
                b: 20,
            }, // Dark green background
            CColor::White,
        ),
    };

    // Skip prefix for assistant and user messages since background color is sufficient
    if !matches!(style, DisplayStyle::Assistant | DisplayStyle::User) {
        // Print the prefix on its own line with style
        execute!(
            stdout,
            SetForegroundColor(prefix_color),
            SetAttribute(Attribute::Bold),
            Print(format!("{prefix}> ")),
            ResetColor,
            Print("\n")
        )
        .unwrap();
    }

    // For assistant, user, and tool messages, use full-width background
    if matches!(
        style,
        DisplayStyle::Assistant | DisplayStyle::User | DisplayStyle::Tool
    ) {
        // Wrap the message to terminal width with some padding
        let content_width = term_width.saturating_sub(4); // 2 chars padding on each side
        let wrapped = wrap_text(message, content_width);

        // Add empty line before content for breathing room
        print_full_width_line("", term_width, bg_color, text_color);

        // Print each line with full-width background
        for line in wrapped.lines() {
            print_full_width_line(line, term_width, bg_color, text_color);
        }

        // Add empty line after content
        print_full_width_line("", term_width, bg_color, text_color);

        // Reset without extra spacing
        execute!(stdout, ResetColor).unwrap();
    } else {
        // For other message types, just print normally with padding
        let wrapped = wrap_text(message, term_width.saturating_sub(2));
        for line in wrapped.lines() {
            println!("  {line}");
        }
        println!();
    }

    stdout.flush().unwrap();
}

/// Clear the specified number of lines above the current cursor position
pub fn clear_lines(num_lines: usize) {
    if num_lines == 0 {
        return;
    }

    let mut stdout = stdout();

    // Move cursor up and clear each line
    for _ in 0..num_lines {
        execute!(
            stdout,
            cursor::MoveToPreviousLine(1),
            terminal::Clear(terminal::ClearType::CurrentLine)
        )
        .unwrap();
    }

    stdout.flush().unwrap();
}

/// Print a single line with full-width background
fn print_full_width_line(content: &str, term_width: usize, bg_color: CColor, text_color: CColor) {
    let mut stdout = stdout();

    // Calculate padding
    let content_len = content.chars().count();
    let left_padding = 2;
    let right_padding = term_width.saturating_sub(content_len + left_padding);

    // Print with background color covering the entire width
    execute!(
        stdout,
        SetBackgroundColor(bg_color),
        SetForegroundColor(text_color),
        Print(" ".repeat(left_padding)),
        Print(content),
        Print(" ".repeat(right_padding)),
        ResetColor,
        Print("\n")
    )
    .unwrap();
}
