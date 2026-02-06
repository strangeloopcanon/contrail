# Contrail ✈️
**AI Telemetry & Shadow AI Defense Daemon**

Contrail is a local-first background service that acts as a "Flight Recorder" for your AI coding sessions. It universally captures interactions from various AI tools, normalizes them into a structured log, and provides "Blackbox" telemetry like file effects, interruptions, and clipboard leaks.

## 🌟 Features

*   **Universal Capture:** Monitors **Cursor**, **OpenAI Codex CLI/Desktop sessions**, **Claude Code**, and **Antigravity**.
*   **Blackbox Telemetry:**
    *   **File Effects:** Automatically runs `git status` after every session to link AI advice to actual code changes.
    *   **Interruption Detection:** Detects if an AI session was cut short or crashed.
    *   **Clipboard Monitor:** Flags if AI-generated code was copied to the system clipboard (potential leak detection).
    *   **Git Context:** Logs branch/repo/folder context (when available) for every interaction.
*   **Privacy First:** All data is stored locally in `~/.contrail/logs/master_log.jsonl`.
*   **DLP (Data Loss Prevention):** Basic regex-based redaction for secrets (API keys) before logging.

## 🎁 AI Year in Code (Wrapup)

Generate a beautiful, shareable "Spotify Wrapped" style report of your AI coding usage. Includes stats like **"The Marathon"** (longest session), **"Books Written"** (token count), and your unique **Coding Persona**.

### Quick Start
1.  **Install & Build** (see [Installation](#-installation) below).
2.  **Import History** (collects your past logs):
    ```bash
    cargo run -p importer
    ```
3.  **Generate Report**:
    ```bash
    # Rolling last 30 days (includes Cursor token totals via Cursor API)
    cargo run -p wrapup -- --last-days 30 --cursor-usage --out export/wrapup_last_30d.json --html export/wrapup_last_30d.html

    # Calendar month (example: Dec 2025)
    cargo run -p wrapup -- --start 2025-12-01 --end 2025-12-31 --cursor-usage --out export/wrapup_2025-12.json --html export/wrapup_2025-12.html

    # Full year (Codex/Claude/Antigravity from local logs; Cursor usage uses Cursor API over the same observed date range)
    cargo run -p wrapup -- --year 2025 --cursor-usage --out export/wrapup_2025.json --html export/wrapup_2025.html
    ```
    *   Opens a vibrant HTML dashboard (e.g. `export/wrapup_last_30d.html`).
    *   Download your "Vibrant Bento" share card directly from the UI.
    *   `--cursor-usage` is optional. Cursor’s local workspace DB does not expose token counts, so totals come from Cursor’s backend usage API (requires that you’re logged into Cursor on this machine).

## 🚀 Installation

### Prerequisites
*   **macOS** (Currently optimized for macOS paths and AppKit notifications)
*   **Rust Toolchain** (Install via [rustup.rs](https://rustup.rs))
*   **Git**
*   AI tools at their default locations:
    * Cursor: `~/Library/Application Support/Cursor/User/workspaceStorage`
    * Codex CLI/Desktop sessions: `~/.codex/sessions`
    * Claude Code: `~/.claude/history.jsonl`
    * Antigravity: `~/.gemini/antigravity/brain`

### Setup Steps

1.  **Clone the Repository:**
    ```bash
    git clone https://github.com/yourusername/contrail.git
    cd contrail
    ```

2.  **Build the Daemon:**
    ```bash
    cargo build --release -p core_daemon
    ```

3.  **Run the Daemon:**
    You can run it directly:
    ```bash
    ./target/release/core_daemon
    ```
    
    *Optional: Run in background*
    ```bash
    nohup ./target/release/core_daemon > /dev/null 2>&1 &
    ```

## 📂 Data & Logs

All logs are stored in **JSONL** format at:
`~/.contrail/logs/master_log.jsonl`

### Quickstart: Live + Historical
If you already have a pile of projects and past AI sessions:

1) Build binaries (first run only):
```bash
cargo build --release
```

2) Start live capture (also backfills history on first run):
```bash
cargo run -p core_daemon
```
On the first run, `core_daemon` will backfill historical Codex (CLI/Desktop)/Claude logs into `~/.contrail/logs/master_log.jsonl` (DLP + schema validation + dedupe), then switch to live capture.  
To re-run the one-time backfill, delete `~/.contrail/state/history_import_done.json` and restart the daemon.

3) View the dashboard (live tail):
```bash
cargo run -p dashboard
# open http://127.0.0.1:3000  (use ?all=true to fetch the full log file)
```

To restart later, just run steps 2–3 as needed (skip rebuild unless deps change).

### Quickstart (capture → view → analyze)

1. **Live capture:** `cargo run -p core_daemon` (writes to `~/.contrail/logs/master_log.jsonl`).  
2. **View dashboard:** `cargo run -p dashboard` then open `http://127.0.0.1:3000`.  
3. **Analyze (local ADE):** `cargo run -p analysis` then open `http://127.0.0.1:3210/` (optional GPT features need `OPENAI_API_KEY` or a key file at `~/.config/openai/api_key`).  
4. **Historical backfill (optional/manual):** `cargo run -p importer` to pull past Codex (CLI/Desktop)/Claude history into the master log (the daemon also does a one-time backfill on first run).

### Export a curated dataset (seed for fine-tuning)

After you’ve collected logs, produce a trimmed JSONL for training:
```bash
cargo run -p exporter
```
This writes `export/curated_dataset.jsonl`, with:
- Only supported sources (codex-cli, cursor, claude-code, antigravity)
- Basic DLP/redaction already applied by the daemon/importer
- Giant payloads dropped and oversized fields truncated
- Deduped entries (same session + content hash)

Use the exported file as your starting point for fine-tuning or further filtering.

**Log Structure Example:**
```json
{
  "event_id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2025-11-22T10:00:00Z",
  "source_tool": "cursor",
  "project_context": "/Users/rohit/dev/my-app",
  "session_id": "7a125a...", 
  "interaction": {
    "role": "assistant",
    "content": "Here is the fix for your bug..."
  },
  "metadata": {
    "conversation_id": "019c31ca-c7b9-73d2-8707-61eb9ae9e0c1",
    "user": "rohit",
    "hostname": "MacBook-Pro",
    "git_branch": "feature/login",
    "git_repository_url": "https://github.com/example/repo.git",
    "file_effects": ["M src/main.rs"],
    "copied_to_clipboard": true
  }
}
```

### Analysis service (ADE + memories/probing)

An optional analysis surface lives in the `analysis` crate. It reads the existing `~/.contrail/logs/master_log.jsonl`, serves a local “ADE” UI, scores sessions/turns for salience, and exposes a small API for building “memories” and probe prompts (no embeddings). It stores analysis artifacts separately under `~/.contrail/analysis/`.

Run it locally:
```bash
cargo run -p analysis
# env overrides: CONTRAIL_LOG_PATH=... ANALYSIS_BIND=127.0.0.1:3210
```

Endpoints:
- `/` — local ADE UI (sessions browser, probe, context pack, memory blocks).
- `/api/sessions?day=YYYY-MM-DD&sort=recent&tool=codex-cli&limit=200` — session summaries (supports `sort`, `tool`, `limit`, `offset`).
- `/api/session_events?source_tool=codex-cli&session_id=...` — full event list for a single session (for browsing conversations).
- `/api/salient?limit=5&day=YYYY-MM-DD` — top sessions with salient turns.
- `/api/probe?q=question&limit=12&day=YYYY-MM-DD` — lexical probe over turns; returns matching snippets plus a suggested LLM prompt for GPT-5.1 Responses API.
- `/api/context_pack?format=text&session_limit=5&memory_limit=5` — redacted, size-bounded “paste into your next session” context bundle.
- `/api/memory_blocks` — GET/POST editable memory blocks stored at `~/.contrail/analysis/memory_blocks.json` (env override: `CONTRAIL_MEMORY_BLOCKS_PATH`).
- `/api/memories` — GET to list stored memory records; POST `{ "q": "...", "limit": N, "llm_response": {...} }` to persist a probe + (optional) LLM output.
- `/api/memories/autoprobe` — POST `{ "q": "...", "limit": N, "model": "gpt-5.1", "temperature": 0 }`; requires `OPENAI_API_KEY`, calls GPT with the suggested prompt, and stores the response.
- `/api/memories/autoprobe/defaults` — POST to run a default (or custom) set of probes in one shot and store GPT-backed memories. Body: `{ "queries": ["error","interrupted",...], "limit": N, "model": "gpt-5.1", "temperature": 0 }`. If `queries` is omitted, uses the built-in defaults (errors, interruptions, patch failures, rate limits, tool-call failures). This is opt-in; trigger it when you want a turnkey daily/adhoc sweep.

### End-to-end flow (arms-length analytics)
- **Capture live:** `cargo run -p core_daemon` to tail active sessions (writes `~/.contrail/logs/master_log.jsonl`; does a one-time historical backfill on first run).
- **Analyze/browse:** `cargo run -p analysis` then open `http://127.0.0.1:3210/` for sessions, probes, context packs, and memories.
- **(Optional) Live UI:** `cargo run -p dashboard` at `http://127.0.0.1:3000`.
- **(Optional/manual) Historical backfill:** `cargo run -p importer` to append past Codex (CLI/Desktop)/Claude logs into `~/.contrail/logs/master_log.jsonl` (runs DLP/redaction on ingest).


## 🔧 Supported Tools Configuration

Contrail automatically watches standard paths. Ensure your tools are installed in their default locations:

*   **Cursor:** `~/Library/Application Support/Cursor/User/workspaceStorage`
*   **Codex CLI/Desktop sessions:** `~/.codex/sessions`
*   **Claude Code:** `~/.claude/history.jsonl`
*   **Antigravity:** `~/.gemini/antigravity/brain`

## 🛡️ Security

*   **Local Only (by default):** Capture + analysis read/write only local files.  
    *Exception:* `wrapup --cursor-usage` calls Cursor’s backend usage API to fetch token totals (still no upload of your local Contrail logs).
*   **Redaction:** Basic patterns (like `sk-...`) are redacted from logs automatically.

## 🤝 Contributing

1.  Fork the repo
2.  Create your feature branch (`git checkout -b feature/amazing-feature`)
3.  Commit your changes (`git commit -m 'Add some amazing feature'`)
4.  Push to the branch (`git push origin feature/amazing-feature`)
5.  Open a Pull Request
