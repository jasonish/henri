# Henri 🐕

This is an LLM coding assistant. Mostly an experiment to see if I
could vibe code a vibe coding tool, and it turns out you can. Named
after my Golden Retriever after hearing that AI coding agents were
much like letting a Golden Retriever code.

There is no way this tool can keep up with the progress of Claude Code
or OpenCode, but sometimes I just prefer its simplicity.

Henri remains simple. It executes one task at a time serially, providing
verbose output so you can see exactly what's going on. It uses no
sub-agents or background tasks. In some ways, I prefer this to
what the more advanced agents are doing these days.

## Features

### Multi Provider / Multi Model

Built-in support for the following providers:

- GitHub Copilot
- Anthropic Claude Pro/Max
- OpenAI ChatGPT Pro
- Google Antigravity
- OpenCode Zen
- OpenAI Compatible APIs (like Z.ai)
- OpenRouter

### Sandboxing

Henri has three security modes:

- **Read-Write (RW)**: Tools and shell commands can write only inside
  the current directory (and children), plus `/tmp`. This is the
  default.
- **Read-Only (RO)**: Tools and shell commands cannot write files at
  all.
- **YOLO**: No sandbox restrictions—tools and shell can write anywhere.

Switch modes with `/read-write`, `/read-only`, `/yolo`, or `Ctrl+X` to
cycle through them. Start in read-only mode with `henri cli --read-only`.

Sandboxing uses Linux Landlock; on unsupported systems, restrictions
are best-effort.

## Installation

Currently, the only way to install is with Cargo:

```
cargo install --locked henri
```

This will install to `~/.cargo/bin`. Ensure that directory is in your
`PATH`, or move the binary after installation:

```
mv ~/.cargo/bin/henri ~/.local/bin/henri
```

## Running

```
henri
```

On first start, you must configure a provider/model with the "/provider"
command.

## Adding a Provider

Henri supports multiple AI providers. Add one with:

```
henri provider add
```

To remove a provider:

```
henri provider remove
```

Configuration is stored in `~/.config/henri/config.toml`.
