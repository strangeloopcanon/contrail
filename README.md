# Contrail

Local-first flight recorder for AI coding sessions, plus a per-repo context layer. Records sessions from Codex, Claude Code, Cursor, and Gemini into a single timeline with secret/PII redaction. Nothing is uploaded anywhere.

## Install

```bash
cargo install contrails
```

This single command installs all eight binaries: `contrail`, `memex`, `core_daemon`, `dashboard`, `analysis`, `importer`, `exporter`, `wrapup`.

<details>
<summary>Other install methods</summary>

Individual packages (if you only need one tool):

```bash
cargo install contrail-cli          # just the CLI
cargo install contrail-memex        # just memex
cargo install core_daemon           # just the daemon
cargo install contrail-dashboard    # just the dashboard
cargo install analysis              # just the analysis UI
cargo install importer              # backward-compatible CLI alias
cargo install exporter              # dataset export
cargo install wrapup                # year-in-code report
```

From GitHub `main` (unreleased):

```bash
cargo install --git https://github.com/strangeloopcanon/contrail --package contrails
```

From a local clone:
- `./install.sh` for an all-in-one workflow, or
- package-specific installs such as `cargo install --path tools/contrail --locked`

</details>

## Quickstart

**Per-repo memory** -- in any repo:

```bash
memex init      # creates .context/, wires agents, installs hooks
memex sync      # pulls recent sessions into .context/sessions/
```

**Start Contrail services (from anywhere):**

```bash
contrail up       # starts core_daemon + dashboard + analysis
contrail status   # check running state
contrail down     # stop all three
```

If your PATH has conflicting binary names, set explicit paths before `contrail up`:

```bash
export CONTRAIL_CORE_DAEMON_BIN="$HOME/.cargo/bin/core_daemon"
export CONTRAIL_DASHBOARD_BIN="$HOME/.cargo/bin/dashboard"
export CONTRAIL_ANALYSIS_BIN="$HOME/.cargo/bin/analysis"
```

**Individual service entrypoints (optional):**

```bash
core_daemon     # one-time backfill on first run, then live watchers
dashboard       # http://127.0.0.1:3000
analysis        # http://127.0.0.1:3210
```

Dashboard lookback modes:
- `Live` (SSE stream with polling fallback)
- `Last 24h`, `Last 7d`, `Last 30d`, `Last 365d`, `All Time`

Historical windows read both `master_log.jsonl` and rotated archives, so older data stays visible after rotation.

**Explain a commit:**

```bash
memex explain abc123    # which sessions produced this diff?
```

## Dev Workflow

Use interface contract targets:

```bash
make setup
make check
make test
make all
```

Local orchestration helper:

```bash
./scripts/dev.sh start    # build + launch core_daemon/dashboard/analysis
./scripts/dev.sh status
./scripts/dev.sh logs
./scripts/dev.sh stop
./scripts/dev.sh check
```

## Release Workflow

Use this before publishing a release to crates.io:

```bash
./scripts/release-bump.sh patch   # or minor / major
make check
make test
```

The bump script updates:
- package versions in workspace `Cargo.toml` files
- internal workspace dependency pins (`contrail-types`, `scrapers`, `importer`, `contrail-memex`, `contrail-cli`)

Publish in dependency order:

```bash
cargo publish --package contrail-types
cargo publish --package scrapers
cargo publish --package importer
cargo publish --package contrail-memex
cargo publish --package contrail-cli
cargo publish --package core_daemon
cargo publish --package contrail-dashboard
cargo publish --package analysis
cargo publish --package exporter
cargo publish --package wrapup
cargo publish --package contrails
```

## Claude Profile Import

Migrate your Claude Code setup to Codex in one command. Safe to re-run.

```bash
contrail import-claude                                          # global
contrail import-claude --repo-root /path/to/repo                # one repo
contrail import-claude --repo-root /path/to/repo --include-global
contrail import-claude --dry-run                                # preview only
```

| Claude source | Codex destination |
|---|---|
| `CLAUDE.md`, `.clauderc` | `~/AGENTS.md` or `<repo>/AGENTS.md` |
| `commands/*.md` | `~/.agents/skills/` or `<repo>/.agents/skills/` |
| `agents/*.md` | `~/.agents/skills/` or `<repo>/.agents/skills/` |
| `history.jsonl`, `projects/*.jsonl` | Contrail master log (deduplicated) |
| `settings.json`, `todos/`, `plugins/` | Archived under `.codex/imports/claude/` |

Instructions are appended with idempotent markers. Commands and agents become Codex `SKILL.md` files with parsed frontmatter. Use `--scope broad|full` to widen what's included, or `--source /path` to override the Claude profile location.

## Cross-Machine Merge

Each machine builds its own log. To combine them:

```bash
# Machine A: export
contrail export-log -o ~/Desktop/contrail-export.jsonl

# Machine B: merge (stop daemon first)
contrail merge-log ~/Desktop/contrail-export.jsonl
```

Re-running merge is safe -- it deduplicates by event ID and content fingerprint.

## Privacy

Everything is local. Redaction covers common API keys, tokens, JWTs, and emails, but treat logs as sensitive anyway. `memex init` gitignores plaintext sessions; use `memex share` / `memex unlock` for encrypted team sharing via `.context/vault.age`.

<details>
<summary>Data model</summary>

Contrail writes an append-only JSONL log to `~/.contrail/logs/master_log.jsonl`. Each line:

```json
{
  "event_id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2025-11-22T10:00:00Z",
  "source_tool": "cursor",
  "project_context": "/Users/rohit/dev/my-app",
  "session_id": "7a125a...",
  "interaction": { "role": "assistant", "content": "..." },
  "security_flags": { "has_pii": true, "redacted_secrets": ["openai_key"] },
  "metadata": { "git_branch": "feature/login", "file_effects": ["M src/main.rs"] }
}
```

</details>

<details>
<summary>Configuration</summary>

All paths and behaviour are overrideable via environment variables.

**Paths:**
`CONTRAIL_LOG_PATH` (default `~/.contrail/logs/master_log.jsonl`), `CONTRAIL_CURSOR_STORAGE`, `CONTRAIL_CODEX_ROOT`, `CONTRAIL_CLAUDE_HISTORY`, `CONTRAIL_CLAUDE_PROJECTS`, `CONTRAIL_ANTIGRAVITY_BRAIN`

**Feature flags:**
`CONTRAIL_ENABLE_CURSOR`, `CONTRAIL_ENABLE_CODEX`, `CONTRAIL_ENABLE_CLAUDE`, `CONTRAIL_ENABLE_ANTIGRAVITY` (all default `true`)

**Timing:**
`CONTRAIL_CURSOR_SILENCE_SECS` (5), `CONTRAIL_CODEX_SILENCE_SECS` (3), `CONTRAIL_CLAUDE_SILENCE_SECS` (5)

**Rotation:**
`CONTRAIL_LOG_MAX_BYTES` (524288000), `CONTRAIL_LOG_KEEP_FILES` (5)

When `master_log.jsonl` exceeds the max size at daemon startup, Contrail rotates it to `master_log.<UTC timestamp>.jsonl` and prunes older archives to the keep count.

**Service lifecycle overrides (`contrail up/down/status`):**
`CONTRAIL_CORE_DAEMON_BIN`, `CONTRAIL_DASHBOARD_BIN`, `CONTRAIL_ANALYSIS_BIN`

**Logging:** `RUST_LOG=info` (or `debug`, etc.)

**Default watch locations (macOS):**
Cursor (`~/Library/Application Support/Cursor/User/workspaceStorage`), Codex (`~/.codex/sessions`), Claude (`~/.claude`), Gemini/Antigravity (`~/.gemini/antigravity/brain`)

</details>

<details>
<summary>Workspace tools</summary>

- `contrail` / `importer`: history import + export/merge
- `exporter`: trimmed dataset export
- `wrapup`: "AI year in code" report
- `analysis`: session browser/scorer UI at `http://127.0.0.1:3210`

</details>
