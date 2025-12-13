// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Service container for dependency injection.

use std::sync::Arc;

use crate::lsp::LspManager;
use crate::mcp::McpManager;

/// Container for shared services. Clone is cheap (uses Arc).
#[derive(Clone)]
pub struct Services {
    pub mcp: Arc<McpManager>,
    pub lsp: Arc<LspManager>,
}

impl Services {
    pub fn new() -> Self {
        Self {
            mcp: crate::mcp::manager(),
            lsp: crate::lsp::manager(),
        }
    }

    /// Create an isolated Services instance for tests.
    pub fn null() -> Self {
        Self {
            mcp: Arc::new(McpManager::new()),
            lsp: Arc::new(LspManager::new()),
        }
    }
}

impl Default for Services {
    fn default() -> Self {
        Self::new()
    }
}
