# Changelog

## [Unreleased]

### Added

- Prompt caching for Anthropic provider: system prompt and tool
  definitions are now cached to reduce token usage and latency

### Changed

- Diff display now uses subtle background colors (dark green/red) for
  added/removed lines instead of changing foreground colors, preserving
  syntax highlighting visibility
- When switching providers, thinking blocks are now transformed to
  `<thinking>` tagged text instead of being stripped entirely,
  preserving reasoning context for the new model
- Claude thinking now supports budget levels (off/low/medium/high) instead
  of simple on/off toggle
- Claude default model updated to claude-haiku-4-5

### Fixed

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
