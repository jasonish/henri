// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish

//! Version information for the Henri application.
//! This module provides compile-time version constants.

/// The version string from Cargo.toml (e.g., "0.3.0")
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
