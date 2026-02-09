// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Jason Ish

use crossterm::style::Color;

pub(super) const SOFTWARE_CURSOR_ON: &str = "\x1b[7m";
pub(super) const SOFTWARE_CURSOR_OFF: &str = "\x1b[27m";

pub(super) const MENU_BG_NORMAL: Color = Color::Rgb {
    r: 20,
    g: 20,
    b: 20,
};

pub(super) const MENU_BG_SELECTED: Color = Color::Rgb {
    r: 30,
    g: 30,
    b: 30,
};
