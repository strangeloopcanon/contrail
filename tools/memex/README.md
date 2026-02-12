# memex

Session context layer for coding agents. Syncs past session transcripts from Cursor, Codex, Claude Code, and Gemini into a `.context/` folder in your repo. The next agent reads them and picks up where the last one left off.

## Quick start

```bash
# Stable release
cargo install contrail-memex --bin memex

# Or latest unreleased from GitHub main
# cargo install --git https://github.com/strangeloopcanon/contrail --package contrail-memex --bin memex

# Or from a local clone of this repo
# cargo install --path tools/memex --locked

cd /path/to/your-project
memex init
memex sync
```

`memex init` creates `.context/sessions/`, detects which agents you've used in this repo, and wires them up (AGENTS.md, CLAUDE.md, .cursor/rules/, GEMINI.md) with a short instruction to check past sessions for context.
It also hardens `.gitignore` so plaintext session files stay local by default.

`memex sync` pulls recent transcripts from agent storage into `.context/sessions/` as readable markdown files. One file per session.

After that, any agent you start in this repo can read or grep `.context/sessions/` for context about previous work.

## What it does

Each coding agent already saves its own session transcripts to disk:

- **Cursor**: SQLite in `~/Library/Application Support/Cursor/User/workspaceStorage/`
- **Codex CLI/Desktop**: JSONL in `~/.codex/sessions/`
- **Claude Code**: JSONL in `~/.claude/projects/` and `~/.claude/history.jsonl`
- **Gemini/Antigravity**: Files in `~/.gemini/antigravity/brain/`

`memex sync` reads from these locations, filters to sessions for the current repo, renders them as markdown, redacts secrets, and writes them to `.context/sessions/`. The next agent greps the folder and figures out what matters.

## Commands

### `memex init`

Run once per repo. Creates the `.context/` directory and tells agents about it.

What it writes depends on which agents have been used in this repo (auto-detected):

| Agent | File created | What it does |
|-------|-------------|--------------|
| Codex | Appends to `AGENTS.md`, writes `.codex/config.toml` | Points Codex at the context folder and compact prompt |
| Claude Code | Creates/appends to `CLAUDE.md` | Points Claude at the context folder |
| Cursor | Creates `.cursor/rules/memex.mdc` | Points Cursor at the context folder |
| Gemini | Creates/appends to `GEMINI.md` | Points Gemini at the context folder |

Also writes:
- `.context/compact_prompt.md` -- a compaction policy that teaches agents to compress context while leaving search keys pointing back to `.context/sessions/`
- `.context/LEARNINGS.md` -- a shared file where agents append decisions, pitfalls, and patterns
- `.gitignore` entries for `.context/sessions/*.md` and `.context/LEARNINGS.md` (local plaintext only; share via `vault.age`)
- A local-only repo-root alias list under `.context/.memex/` so renames/moves don't break `memex sync` (gitignored via `.git/info/exclude`)

Idempotent: won't overwrite existing files.

### `memex sync`

Pulls recent sessions into `.context/sessions/`.

```bash
memex sync              # last 30 days (default)
memex sync --days 90    # last 90 days
```

Skips sessions that are already synced (by filename). Secrets are redacted before writing.

If you move/rename the repo folder, `memex sync` automatically records the new repo root locally and continues matching old sessions from agent storage.

### `memex link-commit`

Records a best-effort link between the current `HEAD` commit and agent sessions that were active around commit time.

This is normally invoked automatically by the `post-commit` git hook installed by `memex init`.

It appends JSONL records to:
- `.context/commits.jsonl`

### `memex explain <commit-ish>`

Explain a commit by showing the agent sessions that were active when it was made.

```bash
memex explain HEAD
memex explain 4d4e12d
memex explain main~1
```

If session files are missing locally, run:
- `memex sync` (to regenerate `.context/sessions/` from local agent storage), or
- `memex unlock` (if your team shares `.context/vault.age`).

### `memex search <query>`

Greppable search across `.context/sessions/*.md` + `.context/LEARNINGS.md`.

```bash
memex search "migrate"
memex search "panic" --days 7
memex search "TODO" --files
```

### `memex share-session <session.md>`

Encrypt a single session transcript into a portable bundle under `.context/bundles/`.

```bash
memex share-session 2026-02-10T12-00-00_codex-cli_abc123.md --passphrase "..."
```

This prints a short Bundle ID you can share. Teammates can import by ID:

```bash
memex import <bundle-id>
memex import <bundle-id> --passphrase "..."
```

### When does sync run?

Three options, all compatible:

1. **The agent runs it.** The AGENTS.md instruction says "run `memex sync` if sessions look stale." Agents that can execute shell commands will do this.

2. **Git hooks from `memex init`.**
   - `post-checkout`: runs `memex sync --quiet` when you switch branches.
   - `pre-commit`: blocks staged plaintext `.context/sessions/*.md` and `.context/LEARNINGS.md`.
   - `post-commit`: records commit-to-session links with `memex link-commit --quiet`.
   - Optional auto-share: set `MEMEX_PASSPHRASE` to have `pre-commit` run `memex share --passphrase-env MEMEX_PASSPHRASE` and stage `.context/vault.age`.
   - Disable all memex hooks with `MEMEX_HOOK=0`.

3. **Manually.** Just run `memex sync` whenever you want.

## What `.context/` looks like

```
.context/
  sessions/
    2026-02-09T14-30_cursor.md
    2026-02-09T10-15_codex-cli.md
    2026-02-08T16-00_claude-code.md
  bundles/
    a3b2c4d5e6f7.age
  compact_prompt.md
  LEARNINGS.md
  commits.jsonl
  vault.age
```

Each session file is plain markdown:

```markdown
# Session: 2026-02-09 14:30 UTC
Tool: cursor | Branch: feat/context-layer | Duration: ~23 min

## user
How do I add a new watcher to the daemon?

## assistant
Looking at core_daemon/src/main.rs, you spawn a new task...
```

## Compact prompt

`.context/compact_prompt.md` is a compaction policy. For Codex, it's automatically wired via `.codex/config.toml`. For other agents, it's a reference document -- you can tell the agent "use `.context/compact_prompt.md` when compressing context."

The prompt teaches the agent to preserve search keys pointing back to `.context/sessions/` when compacting, so detail can be recovered later by grepping the archive.

### `memex share`

Encrypts session transcripts and LEARNINGS.md into a single file (`.context/vault.age`) for sharing via git.

```bash
memex share --passphrase "..."
# or read from env var
MEMEX_PASSPHRASE="..." memex share --passphrase-env MEMEX_PASSPHRASE
```

What it does:
- Packs all `.context/sessions/*.md` + `.context/LEARNINGS.md` into JSON, encrypts with the passphrase using [age](https://age-encryption.org/) (scrypt KDF).
- Writes `.context/vault.age`.
- Ensures `.context/sessions/*.md` and `.context/LEARNINGS.md` are in `.gitignore` so only the encrypted vault gets committed.
- The compact prompt stays unencrypted and committed (it's a template, not session data).

Run it again after `memex sync` to re-encrypt with new sessions.

### `memex unlock`

Decrypts `.context/vault.age` back into readable sessions and learnings.

```bash
memex unlock --passphrase "..."    # use the same passphrase used for `memex share`
```

A teammate clones the repo, runs `memex unlock` with the passphrase, and gets the full session history locally. The vault.age file is standard age format, so it can also be decrypted with the `age` CLI (`age -d -o out.json vault.age`) if memex isn't installed.

## Privacy and security

- All data stays local. No network calls, no cloud, no accounts.
- Secrets (API keys, tokens) are redacted before writing to `.context/sessions/`.
- Plaintext `.context/sessions/*.md` and `.context/LEARNINGS.md` are gitignored by default after `memex init`.
- Commit `.context/vault.age` for team sharing; teammates use `memex unlock` with the passphrase.
- `memex share` encrypts sessions with a passphrase. Only people with the passphrase can read them.

## Part of Contrail

memex lives in the [Contrail](../../README.md) workspace and shares its session parsers and DLP/redaction logic. Contrail is a separate tool (a telemetry daemon for AI coding sessions). They're independent -- memex works without Contrail, Contrail works without memex.

## Install

From crates.io (stable):

```bash
cargo install contrail-memex --bin memex
```

From GitHub `main` (latest unreleased):

```bash
cargo install --git https://github.com/strangeloopcanon/contrail --package contrail-memex --bin memex
```

From a local Contrail workspace:

```bash
cargo install --path tools/memex --locked
```
