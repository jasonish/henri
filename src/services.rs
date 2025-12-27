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
}

impl Services {
    pub(crate) fn new() -> Self {
        Self {
            mcp: crate::mcp::manager(),
            lsp: crate::lsp::manager(),
            interrupted: None,
        }
    }

    /// Create an isolated Services instance for tests.
    #[cfg(test)]
    pub(crate) fn null() -> Self {
        Self {
            mcp: Arc::new(McpManager::new()),
            lsp: Arc::new(LspManager::new()),
            interrupted: None,
        }
    }

    /// Return a clone with the interrupt flag set.
    pub(crate) fn with_interrupted(&self, flag: Arc<AtomicBool>) -> Self {
        Self {
            mcp: self.mcp.clone(),
            lsp: self.lsp.clone(),
            interrupted: Some(flag),
        }
    }

    /// Check if the interrupt flag is set.
    pub(crate) fn is_interrupted(&self) -> bool {
        self.interrupted
            .as_ref()
            .is_some_and(|f| f.load(Ordering::SeqCst))
    }
}

impl Default for Services {
    fn default() -> Self {
        Self::new()
    }
}
