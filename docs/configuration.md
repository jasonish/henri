# Configuration

Henri's configuration file is located at `~/.config/henri/config.toml`.

## Configuration File Structure

```toml
# Default model to use on startup
# Use ":last-used" to restore the last selected model, or specify a model directly
default-model = ":last-used"

# Display settings
show-network-stats = true
show-diffs = true

# Provider configurations
[providers.NAME]
type = "provider-type"
# ... provider-specific options

# MCP servers (optional)
[mcp]
servers = [...]

# LSP servers (optional)
[lsp]
servers = [...]
```

## Providers

Providers are AI backends that Henri can use. Each provider is defined under
`[providers.NAME]` where `NAME` is a local identifier you choose. The `type`
field determines which provider backend to use.

### Supported Provider Types

| Type             | Description                           |
|------------------|---------------------------------------|
| `zen`            | OpenCode Zen                          |
| `claude`         | Anthropic Claude (via OAuth)          |
| `github-copilot` | GitHub Copilot                        |
| `openai`         | OpenAI (via OAuth)                    |
| `openai-compat`  | OpenAI-compatible APIs                |
| `openrouter`     | OpenRouter                            |

### Zen Provider

The Zen provider uses the OpenCode Zen API.

```toml
[providers.zen]
type = "zen"
enabled = true
api-key = "your-api-key"
```

| Field     | Required | Description                     |
|-----------|----------|---------------------------------|
| `api-key` | Yes      | Your Zen API key                |
| `enabled` | No       | Enable/disable (default: true)  |

### Claude Provider (Anthropic)

The Claude provider authenticates via OAuth. Authentication tokens are managed
automatically after initial login.

```toml
[providers.claude]
type = "claude"
enabled = true
refresh-token = "..."
access-token = "..."
expires-at = 1234567890
```

| Field           | Required | Description                      |
|-----------------|----------|----------------------------------|
| `refresh-token` | Yes      | OAuth refresh token              |
| `access-token`  | Yes      | OAuth access token               |
| `expires-at`    | Yes      | Token expiration timestamp       |
| `enabled`       | No       | Enable/disable (default: true)   |

**Note:** These fields are typically populated automatically via the `/login`
command.

### GitHub Copilot Provider

The GitHub Copilot provider uses device OAuth flow for authentication.

```toml
[providers.copilot]
type = "github-copilot"
enabled = true
access-token = "..."
refresh-token = "..."
expires-at = 1234567890
copilot-token = "..."
copilot-expires-at = 1234567890
```

| Field               | Required | Description                      |
|---------------------|----------|----------------------------------|
| `access-token`      | Yes      | GitHub OAuth access token        |
| `refresh-token`     | No       | GitHub OAuth refresh token       |
| `expires-at`        | No       | Token expiration timestamp       |
| `copilot-token`     | No       | Copilot-specific token           |
| `copilot-expires-at`| No       | Copilot token expiration         |
| `enabled`           | No       | Enable/disable (default: true)   |

**Note:** These fields are typically populated automatically via the `/login`
command.

### OpenAI Provider

The OpenAI provider authenticates via OAuth.

```toml
[providers.openai]
type = "openai"
enabled = true
refresh-token = "..."
access-token = "..."
expires-at = 1234567890
project-id = "optional-project-id"
```

| Field           | Required | Description                      |
|-----------------|----------|----------------------------------|
| `refresh-token` | Yes      | OAuth refresh token              |
| `access-token`  | Yes      | OAuth access token               |
| `expires-at`    | Yes      | Token expiration timestamp       |
| `project-id`    | No       | OpenAI project ID                |
| `enabled`       | No       | Enable/disable (default: true)   |

**Note:** These fields are typically populated automatically via the `/login`
command.

### OpenAI-Compatible Provider

For self-hosted or third-party OpenAI-compatible APIs (like Ollama, LM Studio,
or other local inference servers).

```toml
[providers.local]
type = "openai-compat"
enabled = true
base-url = "http://localhost:11434/v1"
api-key = ""  # Often not required for local servers
models = ["llama3.2", "codellama"]
```

| Field      | Required | Description                           |
|------------|----------|---------------------------------------|
| `base-url` | Yes      | API base URL                          |
| `api-key`  | No       | API key (if required by the server)   |
| `models`   | No       | List of available model names         |
| `model`    | No       | Detailed model configurations (array) |
| `enabled`  | No       | Enable/disable (default: true)        |

#### Detailed Model Configuration

For more control, use the `[[providers.NAME.model]]` array syntax:

```toml
[providers.local]
type = "openai-compat"
base-url = "http://localhost:11434/v1"

[[providers.local.model]]
name = "llama3.2"
temperature = 0.7
max-tokens = 4096

[[providers.local.model]]
name = "deepseek-r1"
reasoning-effort = "high"
thinking = { type = "enabled", budget_tokens = 10000 }
```

### OpenRouter Provider

OpenRouter provides access to multiple AI models through a single API.

```toml
[providers.openrouter]
type = "openrouter"
enabled = true
api-key = "your-openrouter-api-key"
models = ["anthropic/claude-3.5-sonnet", "openai/gpt-4o"]
```

| Field     | Required | Description                           |
|-----------|----------|---------------------------------------|
| `api-key` | Yes      | Your OpenRouter API key               |
| `models`  | No       | List of available model names         |
| `model`   | No       | Detailed model configurations (array) |
| `enabled` | No       | Enable/disable (default: true)        |

#### Detailed Model Configuration

```toml
[providers.openrouter]
type = "openrouter"
api-key = "your-api-key"

[[providers.openrouter.model]]
name = "anthropic/claude-3.5-sonnet"
max-tokens = 8192

[[providers.openrouter.model]]
name = "openai/o1-preview"
reasoning-effort = "high"
```

## Model Configuration Options

When using detailed model configuration (via the `model` array), these options
are available:

| Field             | Type          | Description                                    |
|-------------------|---------------|------------------------------------------------|
| `name`            | String        | Model name/identifier (required)               |
| `reasoning-effort`| String        | Reasoning level: "low", "medium", or "high"    |
| `thinking`        | Object        | Extended thinking config (provider-specific)   |
| `temperature`     | Float         | Sampling temperature (0.0 - 2.0)               |
| `max-tokens`      | Integer       | Maximum tokens to generate                     |
| `system-prompt`   | String        | Custom system prompt for this model            |
| `stop-sequences`  | String[]      | Stop sequences to end generation               |

### Extended Thinking

For models that support extended thinking (like Claude):

```toml
[[providers.NAME.model]]
name = "claude-3-5-sonnet"
thinking = { type = "enabled" }

[[providers.NAME.model]]
name = "claude-3-5-sonnet-high"
thinking = { type = "enabled", budget_tokens = 10000 }
```

## Model Selection

Models are referenced using the format `provider-name/model-name`. For example:

- `zen/big-pickle`
- `claude/claude-sonnet-4-5`
- `copilot/claude-sonnet-4`
- `openrouter/anthropic/claude-3.5-sonnet`
- `local/llama3.2` (for an openai-compat provider named "local")

### Default Model

Set the default model on startup:

```toml
# Use the last selected model (default behavior)
default-model = ":last-used"

# Or specify a model explicitly
default-model = "claude/claude-sonnet-4-5"
```

The model selection priority is:
1. CLI `--model` flag (highest priority)
2. `default-model` setting
3. Last used model (when `default-model = ":last-used"`)
4. Built-in default (`zen/big-pickle`)

## Multiple Providers of the Same Type

You can configure multiple instances of the same provider type with different
names:

```toml
[providers.local-ollama]
type = "openai-compat"
base-url = "http://localhost:11434/v1"
models = ["llama3.2"]

[providers.local-lmstudio]
type = "openai-compat"
base-url = "http://localhost:1234/v1"
models = ["mistral-7b"]
```

This creates models accessible as `local-ollama/llama3.2` and
`local-lmstudio/mistral-7b`.

## Display Settings

```toml
# Show network statistics after responses (default: true)
show-network-stats = true

# Show file diffs after edits (default: true)
show-diffs = true
```

## Complete Example

```toml
default-model = ":last-used"
show-network-stats = true
show-diffs = true

[providers.zen]
type = "zen"
api-key = "your-zen-api-key"

[providers.claude]
type = "claude"
refresh-token = "..."
access-token = "..."
expires-at = 1234567890

[providers.copilot]
type = "github-copilot"
access-token = "..."

[providers.local]
type = "openai-compat"
base-url = "http://localhost:11434/v1"
models = ["llama3.2", "codellama"]

[[providers.local.model]]
name = "deepseek-r1"
reasoning-effort = "high"
max-tokens = 8192

[providers.openrouter]
type = "openrouter"
api-key = "your-openrouter-key"
models = ["anthropic/claude-3.5-sonnet", "openai/gpt-4o"]

[state]
last-model = "claude/claude-sonnet-4-5"
```
