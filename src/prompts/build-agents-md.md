Generate or update the AGENTS.md file for this project.

You are tasked with creating or updating an AGENTS.md file for this project. AGENTS.md is a standardized file that provides guidance to AI coding agents working with this codebase - think of it as a "README for agents."

## Your Task

1. **Analyze the project** by examining:
   - Build configuration files (Cargo.toml, package.json, Makefile, pyproject.toml, go.mod, etc.)
   - Existing documentation (README.md, CONTRIBUTING.md, docs/, etc.)
   - Test infrastructure and how to run tests
   - Code style patterns already in use
   - Project structure and architecture

2. **Generate the AGENTS.md file** with these sections:

### Required Sections:

**Build Commands**
- How to build/compile the project
- How to run the development server (if applicable)
- How to run tests (all tests, single test)
- How to lint and format code
- Any pre-commit hooks or checks

**Code Style**
- Language-specific conventions observed in the codebase
- Import ordering patterns
- Naming conventions (files, functions, variables, types)
- Error handling patterns
- Any header requirements (license headers, etc.)

**Rules** (optional but recommended)
- Important constraints or gotchas
- Things to avoid
- Required checks before committing

**Architecture** (for larger projects)
- High-level overview of the codebase structure
- Key modules/packages and their responsibilities
- Important patterns or design decisions

### Guidelines:

- Keep it **concise and actionable** - agents need quick, specific guidance
- Focus on information that **isn't obvious** from reading the code
- Include **exact commands** that can be copy-pasted
- Mention **conventions that differ** from language defaults
- If an existing AGENTS.md exists, **update it** while preserving custom sections

### Format:

```markdown
# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

## Build Commands
- `<command>` - <description>
...

## Code Style
- <convention>
...

## Rules
- <rule>
...

## Architecture
<overview>
...
```

Now, analyze this project and create/update the AGENTS.md file.
