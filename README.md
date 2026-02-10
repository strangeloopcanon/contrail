# Contrail

Local-first flight recorder for AI coding sessions, plus a per-repo context layer.

## What You Get

Contrail has two pieces:

1. **Flight recorder daemon (`core_daemon`)**: captures sessions from Codex (CLI/Desktop), Claude Code, Cursor, and Gemini/Antigravity from local storage, normalizes them into a single JSONL schema, and applies basic secret/PII redaction before writing to disk.
2. **memex (`tools/memex`)**: per-repo `.context/` folder that syncs past session transcripts across agents into readable markdown, includes a compacting prompt to slow context rot ("RLM at home"), and supports optional encrypted sharing for teams.

Both tools are local-first. You decide what (if anything) to commit or share.

## Quickstart

### Build

```bash
./install.sh
```

### Run Live Capture

```bash
./target/release/core_daemon
```

On first run, `core_daemon` does a one-time historical backfill and then switches to live watchers. To re-run backfill, delete `~/.contrail/state/history_import_done.json` and restart the daemon.

### View Logs

```bash
./target/release/dashboard
# open http://127.0.0.1:3000
```

### Enable Per-Repo Memory (memex)

In any repo:

```bash
cargo install --path tools/memex
memex init
memex sync
```

`memex init` creates `.context/` and wires detected agents (Codex/Claude/Cursor/Gemini) to look there for prior context. `memex sync` pulls recent sessions from native storage into `.context/sessions/` as markdown, with redaction.

For more detail, see `tools/memex/README.md`.

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

## Related Tools In This Workspace

- `importer`: manual historical import (`cargo run -p importer`)
- `exporter`: writes a trimmed dataset (`cargo run -p exporter`)
- `wrapup`: generates an "AI year in code" report (`cargo run -p wrapup`)
- `analysis`: local UI for browsing/scoring/probing sessions (`cargo run -p analysis`)
