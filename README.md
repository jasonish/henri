# Henri

This is an LLM coding assistant. Mostly an experiment to see if I
could vibe code a vibe coding tool, and it turns out you can.  Named
after my Golden Retriever after hearing that AI coding agents were
much like letting a Golden Retrieve code.

There is no way this tool can keep up with the progress of Claude Code
or OpenCode, but sometimes I just prefer its simplicity, especially in
CLI mode.

## Installation

At least for now, the only way is with Cargo:

```
cargo install --git https://github.com/jasonish/henri
```

This will install to `~/.cargo/bin`, so be sure to have that in your
path, or after install do something like:

```
mv ~/.cargo/bin/henri ~/.local/bin/henri
```

## Running

Henri defaults to TUI mode (terminal UI with ratatui):

```
henri
```

For a traditional REPL/shell interface:

```
henri cli
```

On first start, a free model will be used, however connecting to a
provider is recommended.

## Adding a Provider

Henri supports multiple AI providers. Add one with:

```
henri provider add
```

This launches an interactive menu to configure:

- **GitHub Copilot** - OAuth device flow authentication
- **Claude (Max/Pro)** - OAuth authentication for Anthropic Claude
- **OpenAI** - OAuth authentication
- **OpenCode Zen** - API key authentication
- **OpenRouter** - API key with model selection
- **OpenAI Compatible** - For local servers (Ollama, etc.) or other OpenAI-compatible APIs

To remove a provider:

```
henri provider remove
```

Configuration is stored in `~/.config/henri/config.toml`.
