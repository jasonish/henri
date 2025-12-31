# Changelog

## [Unreleased]

### Added

- `/tools` command to enable/disable built-in tools via interactive menu
  in both TUI and CLI modes - disabled tools are persisted to config
- `henri tool-call grep` subcommand for direct testing of the grep tool
- `henri mcp add <name> <command...>` and `henri mcp remove [name]` CLI
  commands to manage MCP server configuration
- Status line shows `[MCP: X]` indicator when MCP servers are running
- `/mcp` command to manage MCP server connections - servers can be
  started/stopped on demand via interactive menu in both TUI and CLI
- `/help` command now works in both CLI and TUI modes with colored output
  showing available commands, shell commands, and keyboard shortcuts
- `/new` command as alias for `/clear` to start a new conversation
- Unknown slash commands in TUI now show an error message instead of being
  sent to the model
- Prompt caching for Anthropic provider: system prompt and tool
  definitions are now cached to reduce token usage and latency
- CLI: Syntax highlighting for code blocks and diffs in streaming output

### Changed

- CLI: Shift+Tab (cycle models) and Ctrl+T (toggle thinking) now update the
  status bar in place instead of printing a new prompt line
- `/mcp` now auto-triggers on tab completion (like `/model` and `/settings`)
- CLI `/mcp` menu now uses MultiSelect for toggling multiple servers at once
- TUI MCP menu uses markdown-style checkboxes `[x]`/`[ ]`
- TUI MCP toggle is now non-blocking with optimistic UI updates
- MCP servers are now disabled by default on startup - use `/mcp` to
  enable them as needed
- Simplified startup message to "Type /help for help."
- `Message::Text` in TUI now renders as markdown, enabling formatted help output
- CLI: Custom commands from slash menu now fill in with a trailing space
  on Enter, allowing arguments to be typed (matching TUI behavior)
- Diff display now uses subtle background colors (dark green/red) for
  added/removed lines instead of changing foreground colors, preserving
  syntax highlighting visibility
- When switching providers, thinking blocks are now transformed to
  `<thinking>` tagged text instead of being stripped entirely,
  preserving reasoning context for the new model
- Claude thinking now supports budget levels (off/low/medium/high) instead
  of simple on/off toggle
- Claude default model updated to claude-haiku-4-5

### Internal

- Refactored syntax highlighting into shared `syntax` module used by both
  TUI and CLI
- Removed tree-sitter syntax highlighting, now using syntect only

### Fixed

- MCP server stderr output no longer corrupts TUI display
- TUI now displays tool error messages instead of just showing the
  failure indicator (✗) with no explanation
- Session restore now uses the saved model when no model is specified
  on the command line (both TUI and CLI modes)
- The `-c` flag now works correctly with subcommands: both `henri -c cli`
  and `henri cli -c` will resume the previous session

## [0.2.0] - 2025-12-28

### Added

- File path completion in TUI and CLI prompts: press Tab on words
  starting with `./`, `../`, or `/` to complete file and directory
  names
- Enhanced diff display with syntax highlighting and line numbers
- Allow marking models as favourites
- `upgrade` subcommand to check for new releases on GitHub
- `fetch` tool for fetching URLs with automatic HTML-to-Markdown and
  JSON pretty-printing
- `grep` tool for searching patterns in files using ripgrep (with grep
  fallback)
- Interrupt handling for bash tool: Ctrl+C now kills running commands
- Add Antigravity as a model provider
- Settings toggle for todo tools (`/settings` → "Todo Tools")
- Auto-compaction: automatically compacts context when usage exceeds
  threshold (configurable via `auto-compact` in config)

### Changed

- Todo list display now uses markdown-style checkboxes: `[ ]` pending,
  `[-]` in-progress, `[✓]` completed

### Fixed

- Switching providers mid-conversation (e.g., from antigravity to claude) no
  longer fails with "Invalid signature in thinking block" errors
- Claude provider token refresh failing after idle periods when another client
  instance (e.g., rate limit check) had already refreshed the tokens

## [0.1.0] - 2025-12-26

Initial release.
