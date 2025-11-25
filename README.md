# Contrail ✈️
**AI Telemetry & Shadow AI Defense Daemon**

Contrail is a local-first background service that acts as a "Flight Recorder" for your AI coding sessions. It universally captures interactions from various AI tools, normalizes them into a structured log, and provides "Blackbox" telemetry like file effects, interruptions, and clipboard leaks.

## 🌟 Features

*   **Universal Capture:** Monitors **Cursor**, **OpenAI Codex CLI**, **Claude Code**, and **Antigravity**.
*   **Blackbox Telemetry:**
    *   **File Effects:** Automatically runs `git status` after every session to link AI advice to actual code changes.
    *   **Interruption Detection:** Detects if an AI session was cut short or crashed.
    *   **Clipboard Monitor:** Flags if AI-generated code was copied to the system clipboard (potential leak detection).
    *   **Git Context:** Logs the active branch and project folder for every interaction.
*   **Privacy First:** All data is stored locally in `~/.contrail/logs/master_log.jsonl`.
*   **DLP (Data Loss Prevention):** Basic regex-based redaction for secrets (API keys) before logging.

## 🚀 Installation

### Prerequisites
*   **macOS** (Currently optimized for macOS paths and AppKit notifications)
*   **Rust Toolchain** (Install via [rustup.rs](https://rustup.rs))
*   **Git**
*   AI tools at their default locations:
    * Cursor: `~/Library/Application Support/Cursor/User/workspaceStorage`
    * Codex CLI: `~/.codex/sessions`
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

2) Backfill historical Codex/Claude logs:
```bash
cargo run -p importer
```
This runs DLP + schema validation and appends to `~/.contrail/logs/master_log.jsonl`.

3) Start live capture:
```bash
cargo run -p core_daemon
```

4) View the dashboard:
```bash
cargo run -p dashboard
# open http://127.0.0.1:3000  (use ?all=true to fetch the full log file)
```

To restart later, just run steps 2–4 as needed (skip rebuild unless deps change).

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
    "user": "rohit",
    "hostname": "MacBook-Pro",
    "git_branch": "feature/login",
    "file_effects": ["M src/main.rs"],
    "copied_to_clipboard": true
  }
}
```

## 🔧 Supported Tools Configuration

Contrail automatically watches standard paths. Ensure your tools are installed in their default locations:

*   **Cursor:** `~/Library/Application Support/Cursor/User/workspaceStorage`
*   **Codex CLI:** `~/.codex/sessions`
*   **Claude Code:** `~/.claude/history.jsonl`
*   **Antigravity:** `~/.gemini/antigravity/brain`

## 🛡️ Security

*   **Local Only:** No data is sent to the cloud.
*   **Redaction:** Basic patterns (like `sk-...`) are redacted from logs automatically.

## 🤝 Contributing

1.  Fork the repo
2.  Create your feature branch (`git checkout -b feature/amazing-feature`)
3.  Commit your changes (`git commit -m 'Add some amazing feature'`)
4.  Push to the branch (`git push origin feature/amazing-feature`)
5.  Open a Pull Request
