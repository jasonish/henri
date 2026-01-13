// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Service container for dependency injection.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::lsp::LspManager;
use crate::mcp::McpManager;

/// Container for shared services. Clone is cheap (uses Arc).
#[derive(Clone)]
pub(crate) struct Services {
    pub mcp: Arc<McpManager>,
    pub lsp: Arc<LspManager>,
    /// Interrupt flag for cancellable operations (e.g., bash commands).
    interrupted: Option<Arc<AtomicBool>>,
    /// Sandbox enabled flag (true by default, can be disabled via /yolo command).
    sandbox_enabled: Arc<AtomicBool>,
    /// Read-only flag (false by default, can be toggled via /read-only or /ro).
    read_only: Arc<AtomicBool>,
}

impl Services {
    pub(crate) fn new() -> Self {
        Self {
            mcp: crate::mcp::manager(),
            lsp: crate::lsp::manager(),
            interrupted: None,
            sandbox_enabled: Arc::new(AtomicBool::new(true)),
            read_only: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create an isolated Services instance for tests.
    #[cfg(test)]
    pub(crate) fn null() -> Self {
        Self {
            mcp: Arc::new(McpManager::new()),
            lsp: Arc::new(LspManager::new()),
            interrupted: None,
            sandbox_enabled: Arc::new(AtomicBool::new(true)),
            read_only: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Return a clone with the interrupt flag set.
    pub(crate) fn with_interrupted(&self, flag: Arc<AtomicBool>) -> Self {
        Self {
            mcp: self.mcp.clone(),
            lsp: self.lsp.clone(),
            interrupted: Some(flag),
            sandbox_enabled: self.sandbox_enabled.clone(),
            read_only: self.read_only.clone(),
        }
    }

    /// Check if the interrupt flag is set.
    pub(crate) fn is_interrupted(&self) -> bool {
        self.interrupted
            .as_ref()
            .is_some_and(|f| f.load(Ordering::SeqCst))
    }

    /// Check if sandbox is enabled.
    pub(crate) fn is_sandbox_enabled(&self) -> bool {
        self.sandbox_enabled.load(Ordering::SeqCst)
    }

    /// Set sandbox enabled/disabled.
    pub(crate) fn set_sandbox_enabled(&self, enabled: bool) {
        self.sandbox_enabled.store(enabled, Ordering::SeqCst);
    }

    /// Check if read-only mode is enabled.
    pub(crate) fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::SeqCst)
    }

    /// Set read-only mode enabled/disabled.
    pub(crate) fn set_read_only(&self, enabled: bool) {
        self.read_only.store(enabled, Ordering::SeqCst);
    }

    /// Cycle through sandbox modes: RW -> RO -> YOLO -> RW
    /// Returns a string describing the new mode.
    pub(crate) fn cycle_sandbox_mode(&self) -> &'static str {
        let is_read_only = self.is_read_only();
        let is_sandbox = self.is_sandbox_enabled();

        if is_read_only {
            // RO -> YOLO
            self.set_read_only(false);
            self.set_sandbox_enabled(false);
            "YOLO (no restrictions)"
        } else if is_sandbox {
            // RW -> RO
            self.set_read_only(true);
            "Read-Only"
        } else {
            // YOLO -> RW
            self.set_sandbox_enabled(true);
            "Read-Write (sandboxed)"
        }
    }
}

impl Default for Services {
    fn default() -> Self {
        Self::new()
    }
}
