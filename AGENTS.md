# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

## Project Overview

Henri is a terminal-based AI chat application written in Rust. It supports multiple AI providers (Anthropic, GitHub Copilot, OpenAI, etc.) and provides both a shell REPL interface and a TUI (ratatui-based) interface. The application includes built-in tools for file operations and bash command execution.

## Build Commands

- `cargo build` - Build the project in debug mode
- `cargo build --release` - Build the project in release mode
- `cargo run` - Run the application (shell mode)
- `cargo run -- --tui` - Run the application (TUI mode)
- `cargo test` - Run all tests
- `cargo test <test_name>` - Run a specific test
- `cargo fmt` - Format code
- `cargo fmt --check` - Check formatting without modifying files
- `cargo clippy` - Run linter
- `just check` - Run both `cargo fmt --check` and `cargo clippy`
- `just fix` - Run `cargo clippy --fix --allow-dirty` and `cargo fmt`

## Rust Version

This project uses **Rust Edition 2024** and requires **Rust 1.88.0** or later. This is a cutting-edge edition with features like `let` chains in `if let` expressions.

## Code Style

### File Headers
All Rust source files must start with:
```rust
// SPDX-License-Identifier: MIT
// Copyright (c) 2025 Jason Ish
```

### Import Ordering
Imports are organized in this order (separated by blank lines):
1. Standard library (`std::`)
2. External crates (alphabetically)
3. Internal crate modules (`crate::`)

Example:
```rust
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::Result;
use crate::provider::Message;
```

### Naming Conventions
- **Files**: snake_case (`file_edit.rs`, `mod.rs`)
- **Modules**: snake_case
- **Types/Structs**: PascalCase (`ToolResult`, `FileEdit`)
- **Functions/Methods**: snake_case (`execute`, `get_session_path`)
- **Constants**: SCREAMING_SNAKE_CASE (`DEFAULT_MODEL`, `CONFIG_FILE`)

### Error Handling
- Use the `thiserror` crate for error type definitions
- Custom `Error` enum in `src/error.rs` with `Result<T>` type alias
- Match on specific error types when needed for user-facing messages

### Output and Logging
- **Avoid direct stdout/stderr writes** (`println!`, `eprintln!`, `write!` to stdout/stderr)
- The TUI mode requires all output to go through proper channels
- Use output functions instead:
  - `output::emit_error()` - for error messages
  - `output::print_text()` - for streaming text output
  - `provider::debug::debug_print()` - for debug information (when debug mode is enabled)

### Module Organization
- Use `mod.rs` for module declarations and re-exports
- Implementation files alongside `mod.rs` in subdirectories
- Re-export public types from `mod.rs` using `pub use`
- Limit visibility, prefer non-public functions, or `pub(crate)`.

### Dead Code Policy
- Remove unused functions instead of annotating them with `#[allow(dead_code)]`
- Structs may include unused fields to match external definitions, but those fields must be prefixed with `_` and should not use `#[allow(dead_code)]`

### Testing
- Tests use inline `#[cfg(test)] mod tests { ... }` pattern
- Use `tempfile::TempDir` for tests requiring filesystem operations
- Standard `#[test]` attribute for synchronous tests

## Architecture

### Key Patterns

- **Async Runtime**: Uses `tokio` with multi-threaded runtime
- **Streaming**: AI responses are streamed via SSE (Server-Sent Events)
- **Tool Trait**: All tools implement the `Tool` trait with `definition()` and `execute()` methods
- **Provider Trait**: AI providers implement `ToolProvider` trait for unified interface
- **Message Types**: `Message` with `Role` (user/assistant) and `ContentBlock` variants (text, tool_use, tool_result, thinking, image)
- **Custom Commands**: Loaded from `.md` files in `.henri/commands/`, `.claude/commands/`, or `~/.config/henri/commands/`. Support YAML/TOML front-matter and positional arguments ($1, $2, etc.) with quote handling

## Rules

- Always run `just check` (or `cargo fmt --check && cargo clippy`) before considering changes complete
- Maintain the SPDX license header on all new Rust files
- The project uses cutting-edge Rust features - check Rust 2024 edition capabilities
- Do not create planning documents or other external files unless explicitly requested
- Session files are stored in `~/.local/share/henri/sessions/` (hashed by working directory)
- Configuration is in `~/.config/henri/config.toml`
- Prompts in `src/prompts/` are embedded at compile time via `include_str!`
