# Contrail

Local-first flight recorder for AI coding sessions, plus a per-repo context layer.

## What You Get

Contrail has two pieces:

1. **Flight recorder daemon (`core_daemon`)**: captures sessions from Codex (CLI/Desktop), Claude Code, Cursor, and Gemini/Antigravity from local storage, normalizes them into a single JSONL schema, and applies basic secret/PII redaction before writing to disk.
2. **memex (`tools/memex`)**: per-repo `.context/` folder that syncs past session transcripts across agents into readable markdown, includes a compacting prompt to slow context rot ("RLM at home"), and supports optional encrypted sharing for teams.

Both tools are local-first. You decide what (if anything) to commit or share.

## Install

```bash
# Install memex (per-repo context layer)
cargo install --git https://github.com/strangeloopcanon/contrail --package memex --bin memex

# Install contrail CLI (history import + cross-machine export/merge)
cargo install --git https://github.com/strangeloopcanon/contrail --package contrail --bin contrail

# Optional backward-compatible binary name
cargo install --git https://github.com/strangeloopcanon/contrail --package importer --bin importer

# Install the flight recorder daemon + UIs
cargo install --git https://github.com/strangeloopcanon/contrail --package core_daemon --bin core_daemon
cargo install --git https://github.com/strangeloopcanon/contrail --package dashboard --bin dashboard
cargo install --git https://github.com/strangeloopcanon/contrail --package analysis --bin analysis
```

Or install everything locally in one command:

```bash
./install.sh
```

If you built locally, the binaries will be in `./target/release/` (e.g. `./target/release/memex`). The Quickstart commands below assume the tools are on your `PATH` (as they are after `cargo install`); otherwise, prefix them with `./target/release/`.

## Quickstart

### Per-Repo Memory (memex)

In any repo:

```bash
memex init
memex sync
```

`memex init` creates `.context/`, wires detected agents (Codex/Claude/Cursor/Gemini) to look there for prior context, and installs git hooks for commit linkage. `memex sync` pulls recent sessions from native storage into `.context/sessions/` as markdown, with redaction.

### Live Capture

```bash
core_daemon
```

On first run, `core_daemon` does a one-time historical backfill and then switches to live watchers. To re-run backfill, delete `~/.contrail/state/history_import_done.json` and restart the daemon.

### View Logs

```bash
dashboard
# open http://127.0.0.1:3000
```

### Explain a Commit

```bash
memex explain abc123
```

Shows which agent sessions were active when a commit was made — the reasoning behind the diff.

## Data Model

By default, Contrail writes an append-only JSONL log to:

- `~/.contrail/logs/master_log.jsonl`

Each line is a schema-validated `MasterLog` record:

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

## Default Locations (macOS)

Contrail watches these locations by default (overrideable via env vars):

- Cursor: `~/Library/Application Support/Cursor/User/workspaceStorage`
- Codex CLI/Desktop sessions: `~/.codex/sessions`
- Claude Code: `~/.claude/history.jsonl` and `~/.claude/projects`
- Gemini/Antigravity: `~/.gemini/antigravity/brain`

## Configuration

Paths:

- `CONTRAIL_LOG_PATH` (default `~/.contrail/logs/master_log.jsonl`)
- `CONTRAIL_CURSOR_STORAGE`
- `CONTRAIL_CODEX_ROOT`
- `CONTRAIL_CLAUDE_HISTORY`
- `CONTRAIL_CLAUDE_PROJECTS`
- `CONTRAIL_ANTIGRAVITY_BRAIN`

Feature flags:

- `CONTRAIL_ENABLE_CURSOR` (default `true`)
- `CONTRAIL_ENABLE_CODEX` (default `true`)
- `CONTRAIL_ENABLE_CLAUDE` (default `true`)
- `CONTRAIL_ENABLE_ANTIGRAVITY` (default `true`)

Timing:

- `CONTRAIL_CURSOR_SILENCE_SECS` (default `5`)
- `CONTRAIL_CODEX_SILENCE_SECS` (default `3`)
- `CONTRAIL_CLAUDE_SILENCE_SECS` (default `5`)

Logging:

- `RUST_LOG=info` (or `debug`, etc) controls daemon/importer logging via `tracing_subscriber`.

## Privacy And Security Notes

- **Local-only by default:** Contrail and memex read/write local files. Nothing is uploaded.
- **Redaction is best-effort:** current patterns cover common API keys/tokens, JWT-like strings, and emails. Treat logs and `.context/` as sensitive anyway.
- **Encrypted sharing (optional):** `memex share` encrypts `.context/sessions/*.md` and `.context/LEARNINGS.md` into `.context/vault.age` for committing/sharing; `memex unlock` decrypts locally.

## Cross-Machine Merge

If you use contrail on multiple computers, each machine builds its own master log. The `importer` tool can export from one and merge into another.

**On machine A** (the one you want to export from):

```bash
# Export everything
contrail export-log -o ~/Desktop/contrail-export.jsonl

# Or filter: only events after a date, or from a specific tool
contrail export-log -o ~/Desktop/contrail-export.jsonl --after 2026-01-01T00:00:00Z --tool cursor
```

**Transfer the file** to machine B however you like (AirDrop, USB, shared folder, etc.).

**On machine B** (stop the daemon first to avoid write conflicts):

```bash
launchctl unload ~/Library/LaunchAgents/com.contrail.daemon.plist
contrail merge-log ~/Desktop/contrail-export.jsonl
launchctl load ~/Library/LaunchAgents/com.contrail.daemon.plist
```

`contrail merge-log` now checks `com.contrail.daemon` and exits early if it appears to be running.

Merge deduplicates in two passes: first by `event_id` UUID, then by a content fingerprint that catches the same underlying event ingested independently on both machines (e.g. if both ran `import-history` from the same Codex/Claude files). Re-running merge with the same file is safe — it's idempotent.

For **per-repo session sharing** (`.context/sessions/`), use the existing memex workflow instead: `memex share` on one machine, commit the vault, `memex unlock` on the other. See the memex README for details.

## Related Tools In This Workspace

- `contrail`/`importer`: history import + cross-machine export/merge (`cargo run -p importer -- --help`)
- `exporter`: writes a trimmed dataset (`cargo run -p exporter`)
- `wrapup`: generates an "AI year in code" report (`cargo run -p wrapup`)
- `analysis`: local UI for browsing/scoring/probing sessions (`cargo run -p analysis`)
