// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

/// Thinking modes for models that support extended thinking
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingMode {
    /// Thinking disabled
    Off,
    /// Minimal thinking (gemini-3-flash only)
    Minimal,
    /// Low thinking effort
    Low,
    /// Medium thinking effort (gemini-3-flash only)
    Medium,
    /// High thinking effort
    High,
}

impl ThinkingMode {
    /// Get the available thinking modes for gemini-3-pro
    pub(crate) fn gemini_pro_modes() -> &'static [ThinkingMode] {
        &[ThinkingMode::Off, ThinkingMode::Low, ThinkingMode::High]
    }

    /// Get the available thinking modes for gemini-3-flash
    pub(crate) fn gemini_flash_modes() -> &'static [ThinkingMode] {
        &[
            ThinkingMode::Off,
            ThinkingMode::Minimal,
            ThinkingMode::Low,
            ThinkingMode::Medium,
            ThinkingMode::High,
        ]
    }

    /// Get the next thinking mode in the cycle for the given model
    pub(crate) fn next_for_model(current: ThinkingMode, model: &str) -> ThinkingMode {
        let modes = match model {
            "gemini-3-pro" => Self::gemini_pro_modes(),
            "gemini-3-flash" => Self::gemini_flash_modes(),
            _ => return current, // No cycling for other models
        };

        let current_idx = modes.iter().position(|&m| m == current).unwrap_or(0);
        let next_idx = (current_idx + 1) % modes.len();
        modes[next_idx]
    }

    /// Convert to the API parameter value (for Gemini API)
    pub(crate) fn to_gemini_mode(self) -> Option<&'static str> {
        match self {
            ThinkingMode::Off => None,
            ThinkingMode::Minimal => Some("minimal"),
            ThinkingMode::Low => Some("low"),
            ThinkingMode::Medium => Some("medium"),
            ThinkingMode::High => Some("high"),
        }
    }

    /// Get a display string for the mode
    pub(crate) fn display(self) -> &'static str {
        match self {
            ThinkingMode::Off => "off",
            ThinkingMode::Minimal => "minimal",
            ThinkingMode::Low => "low",
            ThinkingMode::Medium => "medium",
            ThinkingMode::High => "high",
        }
    }

    /// Get the default thinking mode for a model
    pub(crate) fn default_for_model(model: &str) -> ThinkingMode {
        match model {
            "gemini-3-pro" => ThinkingMode::Low,
            "gemini-3-flash" => ThinkingMode::Medium,
            _ => ThinkingMode::Off,
        }
    }
}
