// SPDX-License-Identifier: MIT
// Selection and position types for the TUI

use ratatui::prelude::*;

/// A position within the message content (message index + byte offset in display text)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContentPosition {
    pub message_idx: usize,
    pub byte_offset: usize,
}

impl ContentPosition {
    pub(crate) fn new(message_idx: usize, byte_offset: usize) -> Self {
        Self {
            message_idx,
            byte_offset,
        }
    }
}

impl Ord for ContentPosition {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.message_idx
            .cmp(&other.message_idx)
            .then(self.byte_offset.cmp(&other.byte_offset))
    }
}

impl PartialOrd for ContentPosition {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Text selection state
#[derive(Clone, Debug, Default)]
pub(crate) struct Selection {
    /// The anchor point where selection started (fixed during drag)
    pub anchor: Option<ContentPosition>,
    /// The current end point of selection (moves during drag)
    pub cursor: Option<ContentPosition>,
}

impl Selection {
    pub(crate) fn is_active(&self) -> bool {
        match self.ordered() {
            Some((start, end)) => start != end,
            None => false,
        }
    }

    /// Returns true if selection spans more than one character
    /// (different messages, or same message with >1 byte difference)
    pub(crate) fn spans_multiple_chars(&self) -> bool {
        match self.ordered() {
            Some((start, end)) => {
                start.message_idx != end.message_idx
                    || end.byte_offset.saturating_sub(start.byte_offset) > 1
            }
            None => false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.anchor = None;
        self.cursor = None;
    }

    /// Returns (start, end) in document order
    pub(crate) fn ordered(&self) -> Option<(ContentPosition, ContentPosition)> {
        match (self.anchor, self.cursor) {
            (Some(a), Some(c)) => {
                if a <= c {
                    Some((a, c))
                } else {
                    Some((c, a))
                }
            }
            _ => None,
        }
    }
}

/// Maps screen coordinates to content positions for the current render
#[derive(Default)]
pub(crate) struct PositionMap {
    /// The screen area where messages are rendered
    pub chat_area: Rect,
    /// For each visible row, maps column -> ContentPosition
    /// Indexed by (screen_y - chat_area.y)
    pub rows: Vec<Vec<Option<ContentPosition>>>,
}

impl PositionMap {
    pub(crate) fn init(&mut self, chat_area: Rect) {
        self.chat_area = chat_area;
        self.rows.clear();
        self.rows.resize(chat_area.height as usize, Vec::new());
        for row in &mut self.rows {
            row.clear();
            row.resize(chat_area.width as usize, None);
        }
    }

    /// Look up the content position for a screen coordinate
    pub(crate) fn lookup(&self, screen_x: u16, screen_y: u16) -> Option<ContentPosition> {
        if screen_y < self.chat_area.y || screen_x < self.chat_area.x {
            return None;
        }
        let row_idx = (screen_y - self.chat_area.y) as usize;
        let col_idx = (screen_x - self.chat_area.x) as usize;
        self.rows
            .get(row_idx)
            .and_then(|row| row.get(col_idx).copied())
            .flatten()
    }

    /// Set the content position for a screen coordinate
    pub(crate) fn set(&mut self, screen_x: u16, screen_y: u16, pos: ContentPosition) {
        if screen_y < self.chat_area.y || screen_x < self.chat_area.x {
            return;
        }
        let row_idx = (screen_y - self.chat_area.y) as usize;
        let col_idx = (screen_x - self.chat_area.x) as usize;
        if let Some(row) = self.rows.get_mut(row_idx)
            && let Some(cell) = row.get_mut(col_idx)
        {
            *cell = Some(pos);
        }
    }

    /// Find nearest valid position when clicking on empty space
    pub(crate) fn lookup_nearest(&self, screen_x: u16, screen_y: u16) -> Option<ContentPosition> {
        // First try exact position
        if let Some(pos) = self.lookup(screen_x, screen_y) {
            return Some(pos);
        }

        if screen_y < self.chat_area.y {
            return None;
        }
        let row_idx = (screen_y - self.chat_area.y) as usize;
        let col_idx = if screen_x >= self.chat_area.x {
            (screen_x - self.chat_area.x) as usize
        } else {
            0
        };

        // Try to find the last valid position on this row before the click
        if let Some(row) = self.rows.get(row_idx) {
            // Search backwards from click position
            for i in (0..=col_idx.min(row.len().saturating_sub(1))).rev() {
                if let Some(pos) = row[i] {
                    return Some(pos);
                }
            }
            // If nothing before, search forwards
            if let Some(pos) = row.iter().skip(col_idx).flatten().next() {
                return Some(*pos);
            }
        }

        // Don't search previous rows to avoid selecting content from messages above frames
        // Only search forward into next rows
        for r in (row_idx + 1)..self.rows.len() {
            if let Some(row) = self.rows.get(r)
                && let Some(pos) = row.iter().flatten().next()
            {
                return Some(*pos);
            }
        }

        None
    }
}

/// Selection state for input field (byte offsets)
#[derive(Clone, Debug, Default)]
pub(crate) struct InputSelection {
    pub anchor: Option<usize>,
    pub cursor: Option<usize>,
}

impl InputSelection {
    pub(crate) fn is_active(&self) -> bool {
        match (self.anchor, self.cursor) {
            (Some(a), Some(c)) => a != c,
            _ => false,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.anchor = None;
        self.cursor = None;
    }

    pub(crate) fn ordered(&self) -> Option<(usize, usize)> {
        match (self.anchor, self.cursor) {
            (Some(a), Some(c)) => Some((a.min(c), a.max(c))),
            _ => None,
        }
    }
}
