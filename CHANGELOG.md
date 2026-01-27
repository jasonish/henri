# Changelog

## [Unreleased]

### Added

- CLI: Set terminal title to show 🐕 and current directory
- Zen: Add support for kimi-k2.5 model
- CLI: `/provider` slash command to add/remove providers via interactive menu
- CLI: Bash command output now streams in real-time to a viewport above the
  prompt, showing the last 10 lines with a subtle background for distinction
- CLI: `file_read` now previews the first 3 lines in the tool output viewport
  while still returning the full (numbered) file contents
- CLI: `file_read` preview lines now display with syntax highlighting based on
  file extension, colorizing only the file content (not line numbers)
- CLI: Tool result indicators can include a short summary (e.g. `file_read` and `bash`
  show lines/bytes read)
- CLI: LSP server activation is now visible during tool execution with a cyan
  info message (e.g. `[LSP activated: rust-analyzer (rs)]`) and prompt border
  updates within 50ms to show the current server count
- CLI: Tool output viewport now shows "(... N previous lines)" indicator when
  output has scrolled the first lines out of view
- Custom commands: Also load from `~/.config/opencode/command/`

### Changed

- Providers: Deduplicate `model#variant` parsing/cycling helpers
- Tools: Factor shared LSP diagnostics handling for `file_edit`/`file_write`

- CLI: Requires a configured provider/model on startup (no implicit default);
  prompts entered before configuration now show a helpful error directing users
  to `/provider`.
- LSP: Servers are now started lazily on-demand when a file with a matching
  extension is edited, rather than all at startup. This improves startup time
  when multiple LSP servers are configured.
- Todo tools now appear as a single "todo" entry in the tools menu instead of
  separate `todo_read` and `todo_write` entries
- Todo tool usage guidelines are now only included in the system prompt when
  the todo tools are enabled

### Fixed

- CLI: The `show-diffs` config setting is now respected; file diffs are only
  rendered when enabled (works in both interactive and batch modes)
- Antigravity provider: Strip unsupported JSON Schema fields (like `$schema`) from
  tool parameter schemas to prevent 400 errors when using MCP tools.
- CLI: Queued prompts now remain visible after terminal resize
- CLI: Bandwidth display updates in real time again during streaming responses
  and avoids overlapping the prompt status line
- CLI: Reserve streaming status-line rows before streaming begins to avoid shifting
  or scrolling over the prompt block
- CLI: Keep info/warning/error spacing consistent between live output and session replay
  and render info messages with consistent styling
- CLI: After switching models and running `/clear`, auto-compaction no longer
  triggers immediately due to stale usage data
- CLI: Ctrl+D now behaves like Delete when input is not empty
- CLI: Thinking blocks with no emitted content now render as "Thinking..." on their own line
  (no stray "[thinking with no data]" in the prompt area, and no extra blank lines before text)
- CLI: Claude-only slash commands (like `/claude-count-tokens`) now appear in
  slash menu autocomplete when using a Claude provider

### Changed

- CLI: Improved vertical spacing in streaming output with reactive spacing model
  that eliminates trailing blank lines at end of turns
- Session files now use `.json` extension instead of `.jsonl`
- OpenAI provider: Added `prompt_cache_key` to Codex API requests using session
  ID, enabling prompt caching across requests within the same session
- Session IDs now use UUIDv7 format for time-sortable uniqueness instead of
  timestamp-based format
- CLI: User prompts now render with vertical padding (grey background rows above
  and below) for better visual separation from model responses
- Use "#variant" for all thinking modes across all providers. Some likely still
  need some work.

### Fixed

- CLI: Cancelling during a tool-call loop now properly removes all pending
  messages (assistant responses and tool results) back to the user's original
  prompt, instead of only removing the last message

## [0.4.0] - 2026-01-20

### Added

- TUI: Markdown formatting in thinking messages - bold (`**text**`) and inline
  code (`` `code` ``) are now rendered with appropriate styling
- TUI: Mouse text selection now copies to PRIMARY selection (for middle-click
  paste) in addition to the clipboard (for Ctrl+V paste), matching standard
  terminal behavior
- CLI: Slash commands now supported in non-interactive mode via `henri cli "/command"`.
  Available commands include `/quit`, `/exit`, and `/claude-usage` (when Claude OAuth
  is configured). Unknown commands error with exit code 1.
- TUI: Visual feedback (spinner) when fetching rate limits with `/claude-usage`
  command
- Custom commands now support model specification via frontmatter: add a `model`
  field (e.g., `model: claude/claude-haiku-4-5`) in YAML or TOML frontmatter
  to temporarily switch models when executing a custom command. The original
  model is automatically restored after the chat completes.
- `/undo` command to remove the most recent turn (user message and assistant
  response) from conversation history
- `/forget` command to remove the oldest turn from conversation history,
  useful for managing context window size
- `/truncate` command to keep only the last message and clear the rest of the
  conversation history, useful for starting fresh while preserving context
- Project structure overview in system prompt: automatically includes a
  depth-limited tree of project files (up to depth 2, max 500 entries)
  using `git ls-tree` for git repos or filesystem traversal otherwise.
  Smart trimming prioritizes shallower entries over deeper ones, ensuring
  top-level directories are always visible even in large projects.
- System prompt: When running in read-only mode, include an explicit notice
  instructing the assistant not to attempt file modifications.
- Tilde expansion (`~`) support in all file tools (file_read, file_write,
  file_edit, file_delete, list_dir, glob, grep)
- Line length truncation in file_read tool: lines exceeding 2048 characters
  are now truncated with total length reported, preventing memory issues
  when reading files with extremely long lines
- Zen provider: Added `gpt-5.2` and `gpt-5.2-codex` models
- Antigravity provider: Added "xhigh" thinking mode for Claude models with
  48,000 token budget, complementing existing off/low/medium/high modes
- file_read tool: Added defensive limits (2000 lines or 50KB) with pagination
  hints, preventing unbounded responses when reading large files

### Changed

- Antigravity provider: Internal retry loop with stable request ID for transient
  errors (429s, 5xx, network drops), enabling idempotent retries before falling
  back to the session-level retry loop. Connection pooling disabled to ensure
  retries use fresh connections.

### Fixed

- Custom commands: Fixed parsing of empty YAML (`---\n---`) and TOML
  (`+++\n+++`) front-matter blocks which previously failed to match
- Anthropic OAuth: Updated API request headers and scopes to match Claude Code
  CLI, including `org:create_api_key` scope, `fine-grained-tool-streaming`
  beta feature, and `anthropic-dangerous-direct-browser-access` header
- Anthropic provider: HTTP headers now use lowercase naming and include user-agent
  matching Claude CLI format, ensuring better compatibility with rate limit endpoints
- Anthropic provider: Now uses Claude Code headers unconditionally, removing
  OAuth vs API key distinction that was causing unnecessary complexity
- Anthropic provider: Merge consecutive tool_result messages into a single user
  message to avoid API issues with adjacent user messages
- Anthropic provider: Convert thinking blocks with empty signatures (from aborted
  streams) to text blocks to avoid API rejection
- Anthropic provider: Moved prompt cache control to conversation history (last
  user message) for more effective caching
- TUI: Improved text selection within thinking blocks by properly handling indentation and line wrapping
- TUI: Fixed wrapped lines starting with a space when whitespace caused the wrap;
  whitespace at column 0 after wrapping is now skipped for cleaner text display
- TUI: Removed hanging indent from wrapped list items; continuation lines now
  start at column 0 instead of aligning with list content
- TUI: API error messages that are JSON are now pretty-printed for readability
- TUI: Fixed an issue where an extra blank line was rendered when text fit exactly within the viewport width
- History: Made concurrent appends safe by removing in-process file rewrites
  that could drop entries from other processes. History now appends during
  normal operation and only trims in-memory; file compaction to the
  MAX_HISTORY limit (now 5000) happens on load
- TUI: Usage display (rate limits) rendering had styling bug where first
  character received incorrect styling due to misaligned segment map
- TUI: Added proper spacing above Usage messages for visual separation from
  surrounding content
- TUI: Text after code blocks no longer appears muted/gray. Fixed mismatch
  between syntax highlighter and renderer fence detection causing text to
  incorrectly receive code theme colors instead of normal foreground
- TUI: History navigation state now resets when editing input, preventing
  unexpected history recall after typing
- Context size display now includes cache read tokens for accurate context
  window usage reporting (previously only showed input tokens)
- HTTP errors are now treated as retryable, enabling automatic retry logic for
  transient network issues (not just explicit Retryable errors)
- TUI: Fixed double blank lines appearing between thinking and tool call messages
  caused by empty text events creating spurious spacer messages
- TUI: `kill_to_end` (Ctrl+K) behavior improved for multiline input: now deletes
  to end of line (or deletes newline if at end) instead of truncating entire buffer,
  matching standard Emacs/readline behavior
- Improved prompt caching for Anthropic provider by moving dynamic timestamp
  to end of system prompt, allowing static content to be cached
- Retry notifications during API errors now display as warnings instead of
  errors, preventing the TUI from incorrectly ending the chat session
- Error responses (including 429 rate limits) are now logged to the transaction
  log with headers captured, enabling debugging of API issues
- TUI: Todo list messages now have visual spacing from surrounding content
- TUI: Model override state is now properly restored when chat is interrupted or
  encounters an error, preventing the model selection from being stuck after
  a failed custom command execution

### Internal

- Commit workflow: Move `Co-authored-by: Henri` footer requirement to the repo-local
  `.henri/commands/commit.md` command, so it only applies to this project.

## [0.3.0] - 2025-01-03

### Added

- `/sessions` command to list and switch between previous sessions for the
  current directory - sessions are now stored separately instead of
  overwriting each other
- File path completion now supports tilde expansion (`~`) for home directory
  paths in both TUI and CLI modes
- Landlock sandbox for bash tool: restricts write access to cwd and temp
  directories. Use `--read-only` flag or cycle modes with Ctrl+X.
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
- Automatic retry with exponential backoff for transient API errors (timeouts,
  rate limits, 503/529 overloaded responses) - retries up to 3 times before
  failing

### Changed

- Security modes: replaced `--no-sandbox`/`/sandbox` with three modes cycled via
  Ctrl+X: Read-Write (sandboxed), Read-Only (no file writes), YOLO (no sandbox)
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
- TUI status line now displays context usage during tool loops, not just when
  done - shows input tokens and context limit percentage for real-time feedback
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
