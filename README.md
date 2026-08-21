# Contrail

Local-first flight recorder for AI coding sessions, plus a per-repo context layer. Records sessions from Codex, Claude Code, Cursor, Gemini, and DeepSeek Harness into a single timeline with secret/PII redaction. Nothing is uploaded anywhere.

## Install

For normal use, install the latest published bundle from crates.io:

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

Use GitHub `main` when you need a fix that has merged but is not published to crates.io yet:

```bash
cargo install --git https://github.com/strangeloopcanon/contrail --package contrails
```

Use a local clone when you are actively developing Contrail or want the exact binaries from your checkout:

```bash
./install.sh
# equivalent core command:
cargo install --path contrails --locked --force
```

For a single binary from a local clone, install the package directly, for example:

```bash
cargo install --path tools/contrail --locked --force
```

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

### DeepSeek Harness capture

The daemon watches DeepSeek Harness's native rc.8 session root and reads both
the default append-only `session.jsonl.zstd` format and uncompressed
`session.jsonl`. It records canonical user and assistant messages, exact
provider/model request provenance, tool call/result pairs, token counters, and
turn outcomes. Native session IDs, lineage, turn/step numbers, and event
sequence numbers stay available as metadata for downstream trajectory
compilers. Credentials and complete request headers are never copied; captured
message and tool payloads pass through the normal secret/PII redactor.

The live watcher resumes from durable sequence watermarks in the current and
rotated Contrail logs, so it also captures events written while the daemon was
stopped. To backfill explicitly, run `contrail import-history`; re-running it
is safe because DSH records deduplicate by source instance, session ID, and
native event sequence. Contrail retains canonical messages and lifecycle
events rather than private reasoning or packed streaming deltas.

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
contrail down
contrail merge-log ~/Desktop/contrail-export.jsonl
```

Re-running merge is safe -- it deduplicates by event ID and content fingerprint.
`merge-log` refuses to run while the Contrail daemon is active through either
the macOS LaunchAgent or `contrail up`.

## Privacy

Everything is local. Redaction covers common API keys, tokens, JWTs, and emails, but treat logs as sensitive anyway. `memex init` gitignores plaintext sessions; use `memex share` / `memex unlock` for encrypted team sharing via `.context/vault.age`.

## License

MIT. See [LICENSE](LICENSE).

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
`CONTRAIL_LOG_PATH` (default `~/.contrail/logs/master_log.jsonl`), `CONTRAIL_CURSOR_STORAGE`, `CONTRAIL_CODEX_ROOT`, `CONTRAIL_CLAUDE_HISTORY`, `CONTRAIL_CLAUDE_PROJECTS`, `CONTRAIL_ANTIGRAVITY_BRAIN`, `CONTRAIL_DSH_SESSIONS` (default `$DSH_HOME/sessions`, otherwise `~/.dsh/sessions`)

**Feature flags:**
`CONTRAIL_ENABLE_CURSOR`, `CONTRAIL_ENABLE_CODEX`, `CONTRAIL_ENABLE_CLAUDE`, `CONTRAIL_ENABLE_ANTIGRAVITY`, `CONTRAIL_ENABLE_DSH` (all default `true`)

**Timing:**
`CONTRAIL_CURSOR_SILENCE_SECS` (5), `CONTRAIL_CODEX_SILENCE_SECS` (3), `CONTRAIL_CLAUDE_SILENCE_SECS` (5)

**Rotation:**
`CONTRAIL_LOG_MAX_BYTES` (524288000), `CONTRAIL_LOG_KEEP_FILES` (5)

When `master_log.jsonl` exceeds the max size at daemon startup, Contrail rotates it to `master_log.<UTC timestamp>.jsonl` and prunes older archives to the keep count.

**Service lifecycle overrides (`contrail up/down/status`):**
`CONTRAIL_CORE_DAEMON_BIN`, `CONTRAIL_DASHBOARD_BIN`, `CONTRAIL_ANALYSIS_BIN`

**Logging:** `RUST_LOG=info` (or `debug`, etc.)

**Default watch locations (macOS):**
Cursor (`~/Library/Application Support/Cursor/User/workspaceStorage`), Codex (`~/.codex/sessions`), Claude (`~/.claude`), Gemini/Antigravity (`~/.gemini/antigravity/brain`), DeepSeek Harness (`$DSH_HOME/sessions` or `~/.dsh/sessions`)

</details>

<details>
<summary>Workspace tools</summary>

- `contrail` / `importer`: history import + export/merge
- `exporter`: trimmed dataset export
- `wrapup`: "AI year in code" report
- `analysis`: session browser/scorer UI at `http://127.0.0.1:3210`

</details>
