// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

//! Terminal image preview using the Kitty graphics protocol.
//!
//! The Kitty graphics protocol transmits images as base64-encoded data
//! within escape sequences. Large images are sent in chunks.
//!
//! Protocol reference: <https://sw.kovidgoyal.net/kitty/graphics-protocol/>

use std::io::Cursor;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossterm::terminal;
use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};

/// Maximum chunk size for Kitty graphics protocol (4096 bytes of base64 data).
const KITTY_CHUNK_SIZE: usize = 4096;

/// Maximum height in pixels for image previews.
///
/// We avoid downscaling already-small images (e.g., short screenshots).
const MAX_PREVIEW_HEIGHT_PX: u32 = 300;

/// Maximum width in pixels for image previews.
const MAX_PREVIEW_WIDTH_PX: u32 = 1000;

/// Fallback assumed cell height in pixels.
const CELL_HEIGHT_PX: u32 = 18;

/// Fallback assumed cell width in pixels.
const CELL_WIDTH_PX: u32 = 8;

/// Unicode placeholder character for Kitty images.
const PLACEHOLDER_CHAR: char = '\u{10EEEE}';

/// Row/column diacritics for Unicode placeholders (first 64 entries).
const ROW_DIACRITICS: [char; 64] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
];

const COL_ZERO_DIACRITIC: char = ROW_DIACRITICS[0];
const MAX_PLACEHOLDER_ROWS: usize = ROW_DIACRITICS.len();
const MAX_IMAGE_ID: u32 = 0x00FF_FFFF;

/// Cached result of Kitty terminal detection.
static IS_KITTY: OnceLock<bool> = OnceLock::new();
static NEXT_IMAGE_ID: AtomicU32 = AtomicU32::new(1);

/// Check if the terminal supports the Kitty graphics protocol.
///
/// Detection is based on environment variables:
/// - `TERM` contains "kitty"
/// - `KITTY_WINDOW_ID` is set
/// - `TERM_PROGRAM` is "WezTerm" (also supports Kitty protocol)
/// - `GHOSTTY_RESOURCES_DIR` is set (Ghostty terminal)
pub(crate) fn is_kitty_terminal() -> bool {
    *IS_KITTY.get_or_init(|| {
        // Check TERM
        if let Ok(term) = std::env::var("TERM")
            && term.contains("kitty")
        {
            return true;
        }

        // Check KITTY_WINDOW_ID
        if std::env::var("KITTY_WINDOW_ID").is_ok() {
            return true;
        }

        // Check TERM_PROGRAM for WezTerm (supports Kitty graphics protocol)
        if let Ok(term_program) = std::env::var("TERM_PROGRAM")
            && term_program == "WezTerm"
        {
            return true;
        }

        // Check for Ghostty
        if std::env::var("GHOSTTY_RESOURCES_DIR").is_ok() {
            return true;
        }

        // Check WEZTERM_PANE
        if std::env::var("WEZTERM_PANE").is_ok() {
            return true;
        }

        false
    })
}

/// Result of preparing an image for preview.
struct PreparedImage {
    /// PNG-encoded image data
    data: Vec<u8>,
    /// Number of terminal rows the image will occupy
    rows: u32,
    /// Number of terminal columns the image will occupy
    cols: u32,
}

fn next_image_id() -> u32 {
    NEXT_IMAGE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            let next = if current >= MAX_IMAGE_ID {
                1
            } else {
                current + 1
            };
            Some(next)
        })
        .unwrap_or(1)
}

fn terminal_cell_height_px() -> Option<f64> {
    let ws = terminal::window_size().ok()?;
    if ws.rows == 0 || ws.height == 0 {
        return None;
    }
    Some(ws.height as f64 / ws.rows as f64)
}

fn terminal_cell_width_px() -> Option<f64> {
    let ws = terminal::window_size().ok()?;
    if ws.columns == 0 || ws.width == 0 {
        return None;
    }
    Some(ws.width as f64 / ws.columns as f64)
}

fn rows_for_pixel_height(pixel_height: u32) -> u32 {
    if pixel_height == 0 {
        return 1;
    }

    if let Some(cell_height) = terminal_cell_height_px()
        && cell_height > 0.0
    {
        return ((pixel_height as f64 / cell_height).ceil() as u32).max(1);
    }

    pixel_height.div_ceil(CELL_HEIGHT_PX).max(1)
}

fn cols_for_pixel_width(pixel_width: u32) -> u32 {
    if pixel_width == 0 {
        return 1;
    }

    if let Some(cell_width) = terminal_cell_width_px()
        && cell_width > 0.0
    {
        return ((pixel_width as f64 / cell_width).ceil() as u32).max(1);
    }

    pixel_width.div_ceil(CELL_WIDTH_PX).max(1)
}

/// Resize image data to fit within preview constraints.
/// Returns the resized image as PNG bytes and the number of rows it will occupy.
fn resize_for_preview(data: &[u8]) -> Option<PreparedImage> {
    let img = image::load_from_memory(data).ok()?;
    let (w, h) = img.dimensions();

    let (term_cols, _) = terminal::size().unwrap_or((80, 24));
    let max_rows = MAX_PLACEHOLDER_ROWS as u32;

    let max_width_px = {
        let width_limit = if let Some(cell_width) = terminal_cell_width_px() {
            (cell_width * term_cols as f64).floor().max(1.0) as u32
        } else {
            term_cols as u32 * CELL_WIDTH_PX
        };
        MAX_PREVIEW_WIDTH_PX.min(width_limit).max(1)
    };

    let max_height_px = {
        let height_limit = if let Some(cell_height) = terminal_cell_height_px() {
            (cell_height * max_rows as f64).floor().max(1.0) as u32
        } else {
            max_rows.saturating_mul(CELL_HEIGHT_PX).max(1)
        };
        MAX_PREVIEW_HEIGHT_PX.min(height_limit).max(1)
    };

    // Calculate scale to fit within bounds
    let scale_w = max_width_px as f64 / w as f64;
    let scale_h = max_height_px as f64 / h as f64;
    let scale = scale_w.min(scale_h).min(1.0); // Don't upscale

    let new_w = ((w as f64) * scale).round().max(1.0) as u32;
    let new_h = ((h as f64) * scale).round().max(1.0) as u32;

    let resized = if new_w != w || new_h != h {
        img.resize(new_w, new_h, FilterType::Triangle)
    } else {
        img
    };

    // Calculate terminal cell occupancy.
    let (resized_w, resized_h) = resized.dimensions();
    let rows = rows_for_pixel_height(resized_h);
    let cols = cols_for_pixel_width(resized_w);

    if rows as usize > MAX_PLACEHOLDER_ROWS || cols == 0 {
        return None;
    }

    // Encode as PNG
    let mut output = Vec::new();
    resized
        .write_to(&mut Cursor::new(&mut output), ImageFormat::Png)
        .ok()?;

    Some(PreparedImage {
        data: output,
        rows,
        cols,
    })
}

fn id_to_rgb(image_id: u32) -> (u8, u8, u8) {
    (
        ((image_id >> 16) & 0xFF) as u8,
        ((image_id >> 8) & 0xFF) as u8,
        (image_id & 0xFF) as u8,
    )
}

fn build_unicode_placeholder_lines(image_id: u32, rows: u32, cols: u32) -> Option<Vec<String>> {
    if rows == 0 || cols == 0 {
        return None;
    }

    if rows as usize > MAX_PLACEHOLDER_ROWS {
        return None;
    }

    let (r, g, b) = id_to_rgb(image_id);
    let color_prefix = format!("\x1b[38;2;{};{};{}m", r, g, b);
    let mut lines = Vec::with_capacity(rows as usize);

    for row in 0..rows {
        let row_diacritic = ROW_DIACRITICS[row as usize];
        let mut line = String::new();
        line.push_str(&color_prefix);
        for col in 0..cols {
            line.push(PLACEHOLDER_CHAR);
            line.push(row_diacritic);
            if col == 0 {
                line.push(COL_ZERO_DIACRITIC);
            }
        }
        line.push_str("\x1b[0m");
        lines.push(line);
    }

    Some(lines)
}

/// Build the Kitty graphics protocol escape sequence for displaying an image.
///
/// Control codes used:
/// - `a=T` - action: transmit and display (virtual placement for placeholders)
/// - `f=100` - format: PNG
/// - `t=d` - transmission: direct (data inline)
/// - `q=2` - quiet mode: suppress responses
/// - `m=1` - more data follows (for chunking)
/// - `m=0` - last/only chunk
/// - `i` - image id (referenced by Unicode placeholders)
/// - `U=1` - create a virtual placement for Unicode placeholders
/// - `c`/`r` - number of columns/rows for the placement
fn build_kitty_escape_sequence(base64_data: &str, image_id: u32, cols: u32, rows: u32) -> String {
    let mut result = String::new();

    // Split into chunks if necessary
    let chunks: Vec<&str> = base64_data
        .as_bytes()
        .chunks(KITTY_CHUNK_SIZE)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
        .collect();

    let total_chunks = chunks.len();

    for (i, chunk) in chunks.iter().enumerate() {
        let is_first = i == 0;
        let is_last = i == total_chunks - 1;
        let more = if is_last { 0 } else { 1 };

        // Build the control data
        let control = if is_first {
            // First chunk: include all control parameters
            // a=T: transmit and display
            // f=100: PNG format
            // t=d: direct transmission
            // q=2: quiet mode (suppress OK/error responses)
            // m=0/1: more data indicator
            // i: image id
            // U=1: virtual placement for Unicode placeholders
            // c/r: placement size
            // Note: `C=1` prevents the terminal from moving the cursor after placing the image.
            // We manage cursor movement ourselves to keep tool output spacing consistent.
            format!(
                "a=T,f=100,t=d,q=2,C=1,i={},U=1,c={},r={},m={}",
                image_id, cols, rows, more
            )
        } else {
            // Subsequent chunks: only need more indicator
            format!("m={}", more)
        };

        // Kitty escape sequence format: ESC_Gcontrol;payloadESC\
        // Using APC (Application Program Command): \x1b_ ... \x1b\\
        result.push_str("\x1b_G");
        result.push_str(&control);
        result.push(';');
        result.push_str(chunk);
        result.push_str("\x1b\\");
    }

    result
}

/// Image preview result containing the escape sequence and placeholder lines.
pub(crate) struct ImagePreview {
    /// The Kitty graphics escape sequence to transmit the image data.
    pub escape_sequence: String,
    /// Unicode placeholder lines for scrollback-friendly rendering.
    pub placeholder_lines: Vec<String>,
    /// Image MIME type (for future rendering decisions).
    pub mime_type: String,
}

/// Get the image preview for displaying in the terminal.
///
/// Returns `Some(ImagePreview)` if Kitty graphics are supported,
/// `None` otherwise.
pub(crate) fn get_image_preview(data: &[u8], mime_type: &str) -> Option<ImagePreview> {
    if !is_kitty_terminal() || data.is_empty() {
        return None;
    }

    // Resize the image to fit preview constraints
    let prepared = resize_for_preview(data)?;
    let image_id = next_image_id();

    let base64_data = STANDARD.encode(&prepared.data);
    let image_sequence =
        build_kitty_escape_sequence(&base64_data, image_id, prepared.cols, prepared.rows);
    let placeholder_lines =
        build_unicode_placeholder_lines(image_id, prepared.rows, prepared.cols)?;

    Some(ImagePreview {
        escape_sequence: image_sequence,
        placeholder_lines,
        mime_type: mime_type.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_kitty_escape_sequence_small() {
        // Small data that fits in one chunk
        let data = "SGVsbG8gV29ybGQ="; // "Hello World" in base64
        let result = build_kitty_escape_sequence(data, 42, 12, 7);

        // Should have single chunk with m=0 (no more data)
        assert!(result.starts_with("\x1b_G"));
        assert!(result.contains("a=T"));
        assert!(result.contains("i=42"));
        assert!(result.contains("U=1"));
        assert!(result.contains("c=12"));
        assert!(result.contains("r=7"));
        assert!(result.contains("m=0"));
        assert!(result.contains("q=2")); // quiet mode
        assert!(!result.contains("z="));
        assert!(result.contains(data));
        assert!(result.ends_with("\x1b\\"));
    }

    #[test]
    fn test_build_kitty_escape_sequence_chunked() {
        // Create data larger than chunk size
        let large_data = "A".repeat(KITTY_CHUNK_SIZE + 100);
        let result = build_kitty_escape_sequence(&large_data, 7, 3, 2);

        // Should have multiple escape sequences
        let chunk_count = result.matches("\x1b_G").count();
        assert!(
            chunk_count >= 2,
            "Expected multiple chunks, got {}",
            chunk_count
        );

        // First chunk should have m=1, last should have m=0
        assert!(result.contains("m=1"));
        assert!(result.contains("m=0"));
    }

    #[test]
    fn test_resize_for_preview() {
        // Create a small test image
        let img = image::RgbImage::from_fn(100, 50, |_x, _y| image::Rgb([128, 128, 128]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();

        let prepared = resize_for_preview(&bytes);
        assert!(prepared.is_some());

        let prepared = prepared.unwrap();
        // Verify the resized image dimensions
        let resized_img = image::load_from_memory(&prepared.data).unwrap();
        let (w, h) = resized_img.dimensions();
        assert!(w <= MAX_PREVIEW_WIDTH_PX);
        assert!(h <= MAX_PREVIEW_HEIGHT_PX);
        assert!(prepared.rows >= 1);
        assert!(prepared.cols >= 1);
    }

    #[test]
    fn test_resize_large_image() {
        // Create a large test image
        let img = image::RgbImage::from_fn(1000, 500, |_x, _y| image::Rgb([128, 128, 128]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();

        let prepared = resize_for_preview(&bytes);
        assert!(prepared.is_some());

        let prepared = prepared.unwrap();
        // Verify the resized image is smaller
        let resized_img = image::load_from_memory(&prepared.data).unwrap();
        let (w, h) = resized_img.dimensions();
        assert!(w <= MAX_PREVIEW_WIDTH_PX);
        assert!(h <= MAX_PREVIEW_HEIGHT_PX);
        // Should have been scaled down
        assert!(w < 1000);
        assert!(h < 500);
    }
}
