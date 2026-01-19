# Git Guidelines

## General Principles

- **Only commit when explicitly requested** by the user. If unclear, ask first.
- Never commit changes proactively.
- Commit messages should explain the **purpose** of changes, not just describe them.
- Be careful not to stage/commit irrelevant files (avoid `git add .` blindly).
- If there are no changes to commit, do not create an empty commit.

## Safety Rules

**NEVER:**
- Update the git config
- Run destructive/irreversible commands (`push --force`, hard reset) unless explicitly requested
- Skip hooks (`--no-verify`, `--no-gpg-sign`) unless explicitly requested
- Force push to main/master — warn the user if they request it
- Use interactive flags (`-i`) like `git rebase -i` or `git add -i` — they require interactive input
- Commit files containing secrets (`.env`, `credentials.json`, etc.) — warn user if they request it
- Push to the remote repository unless explicitly requested

**Avoid:**
- `git commit --amend` unless: (1) user explicitly requested, or (2) adding edits from a pre-commit hook
- Before amending: always verify authorship with `git log -1 --format='%an %ae'`

## Commit Process

1. **Gather context** — run these commands in parallel:
   - `git status` — see all untracked files
   - `git diff` — see both staged and unstaged changes
   - `git log` — see recent commit messages to match repository style

2. **Analyze and draft commit message:**
   - Review previous commits (`git log`) to match the repository's commit style
   - List the files that have been changed or added
   - Summarize the nature of the changes (new feature, enhancement, bug fix, refactoring, test, docs, etc.)
   - Consider the purpose or motivation behind the changes
   - Assess impact on the overall project
   - Check for sensitive information that shouldn't be committed
   - Draft a concise message focusing on the **"why"** rather than the "what"
   - Use accurate verbs: "add" = wholly new feature, "update" = enhancement, "fix" = bug fix
   - Avoid generic messages (no "Update" or "Fix" without context)

## Commit Message Formatting

- **Title line (first line):** Must not exceed 72 characters. May extend to 80 characters only with explicit user approval.
- **Body:** Use bullet points (starting with `-`) to list changes. Each point should be concise.
- Wrap lines at 80 characters, respecting word boundaries and whitespace.
- Use a blank line to separate the title from the body.

3. **Stage and commit:**
   - Add relevant untracked files to staging
   - Create the commit with the drafted message
   - Use HEREDOC format for proper multi-line message formatting

4. **Verify:**
   - Run `git status` after commit to confirm success

## Pre-commit Hook Handling

- If commit fails due to pre-commit hook changes, retry **once** to include the automated changes
- If commit succeeds but files were modified by the hook:
  1. Check HEAD commit: `git log -1 --format='[%h] (%an <%ae>) %s'` — verify it matches your commit
  2. Check not pushed: `git status` shows "Your branch is ahead"
  3. If both true: amend your commit. Otherwise: create a **new** commit (never amend other developers' commits)
