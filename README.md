# Contrail

Local-first flight recorder for AI coding sessions, plus a per-repo context layer. Records sessions from Codex, Claude Code, Cursor, and Gemini into a single timeline with secret/PII redaction. Nothing is uploaded anywhere.

## Install

```bash
cargo install contrail-memex --bin memex       # per-repo context layer
cargo install contrail-cli --bin contrail       # history import + export/merge
```

<details>
<summary>Other install methods</summary>

From GitHub `main` (unreleased):

```bash
cargo install --git https://github.com/strangeloopcanon/contrail --package contrail-memex --bin memex
cargo install --git https://github.com/strangeloopcanon/contrail --package contrail-cli --bin contrail
cargo install --git https://github.com/strangeloopcanon/contrail --package core_daemon --bin core_daemon
cargo install --git https://github.com/strangeloopcanon/contrail --package dashboard --bin dashboard
cargo install --git https://github.com/strangeloopcanon/contrail --package analysis --bin analysis
```

From a local clone: `./install.sh`

Binaries end up in `./target/release/` when building locally. The commands below assume they're on your `PATH` (as they are after `cargo install`).

</details>

## Quickstart

**Per-repo memory** -- in any repo:

```bash
memex init      # creates .context/, wires agents, installs hooks
memex sync      # pulls recent sessions into .context/sessions/
```

**Live capture** -- records sessions as they happen:

```bash
core_daemon     # one-time backfill on first run, then live watchers
```

**Browse sessions:**

```bash
dashboard       # http://127.0.0.1:3000
```

**Explain a commit:**

```bash
memex explain abc123    # which sessions produced this diff?
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
