# Changelog

## [Unreleased]

### Added

- `/sessions` command to list and switch between previous sessions for the
  current directory - sessions are now stored separately instead of
  overwriting each other
- Landlock sandbox for bash tool: restricts write access to cwd and temp
  directories. Disable with `--no-sandbox` flag or `/sandbox` command.
  - Allows writes to /dev/null and /dev/tty for git and other tools
  - Supports git worktrees by allowing writes to the actual git directory
  - Sandbox enforcement for `file_write`, `file_edit`, and `file_delete` tools
    restricting writes to cwd and safe temporary directories
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

- Word navigation (Alt+B/F) and word deletion (Alt+D) now treat `/` and `-`
  as word boundaries, improving navigation through file paths and hyphenated
  names in both CLI and TUI modes
- Session storage format changed: sessions are now stored in per-directory
  folders with individual files per session. **Breaking:** existing sessions
  from before this change will no longer be accessible.
- CLI: Shift+Tab (cycle models) and Ctrl+T (toggle thinking) now update the
  status bar in place instead of printing a new prompt line
- `/mcp` now auto-triggers on tab completion (like `/model` and `/settings`)
- CLI `/mcp` menu now uses MultiSelect for toggling multiple servers at once
- TUI MCP menu uses markdown-style checkboxes `[x]`/`[ ]`
- TUI MCP toggle is now non-blocking with optimistic UI updates
- MCP servers are now disabled by default on startup - use `/mcp` to
  enable them as needed
- Simplified startup message to "Type /help for help."
- TUI: Welcome message now shows sandbox status (enabled/disabled/unavailable) matching CLI behavior
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
- TUI: Suppressed detailed error output for bash commands in conversation history
  (e.g., test failures) to reduce noise, showing only the status indicator (✗)
- Thinking toggle visibility in UI is now model-aware: hidden for models where
  thinking cannot be disabled (e.g., minimax-m2.1-free, grok-code in Zen provider)
- TUI: Markdown tables that would exceed terminal width are now left unformatted
  to prevent horizontal overflow

### Internal

- Compaction now uses XML format for conversation history, preserving
  full content and enabling cross-model compaction
- Refactored syntax highlighting into shared `syntax` module used by both
  TUI and CLI
- Removed tree-sitter syntax highlighting, now using syntect only

### Fixed

- CLI: Removed extra blank lines in startup message
- TUI compaction now correctly preserves the summary instead of
  overwriting it with the summarization request/response
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
