# Contrail Project Tasks

- [x] **Project Initialization**
    - [x] Create Cargo workspace (`core_daemon`, `scrapers`)
    - [x] Update `AGENTS.md` with Rust instructions
    - [x] Add dependencies (`notify`, `tokio`, `rusqlite`, etc.)

- [x] **Phase 1: The Harvester (File Watcher)**
    - [x] Create `~/.contrail` directory structure (in `main.rs`)
    - [x] Implement `CursorWatcher`
        - [x] Find `workspaceStorage`
        - [x] Watch `state.vscdb`
        - [x] Detect silence (> 5s)
    - [x] Implement `CodexWatcher`
        - [x] Find date-sharded sessions (`YYYY/MM/DD`)
        - [x] Watch latest `.jsonl`
        - [x] Tail content
        - [x] Detect silence (> 3s)
    - [x] **Implement `AntigravityWatcher`**
        - [x] Find `~/.gemini/antigravity/brain`
        - [x] Watch `task.md` in latest session
    - [x] **Implement `ClaudeWatcher`**
        - [x] Find `~/.claude/history.jsonl`
        - [x] Tail content
    - [x] **Implement JSONL Storage**
        - [x] Write `MasterLog` to `~/.contrail/logs/master_log.jsonl`
    - [x] Implement `Notifier` module
    - [x] Implement `Sentry` (DLP) module

- [ ] **Phase 2: The Observer (Window Scraper & Blackbox)**
    - [ ] **Interruption Detection**
        - [ ] Detect "Cut-off" streams (incomplete JSON/text)
        - [ ] Detect "Rapid Re-prompt" (< 1s after stop)
    - [ ] **File Effects (The "Walk")**
        - [ ] Run `git status --short` after session ends
        - [ ] Log modified/created files in `metadata.effect`
    - [ ] **Clipboard Monitor**
        - [ ] Watch system clipboard
        - [ ] Match against AI output (Exfiltration detection)
    - [ ] Request Accessibility Permissions (for Window Title)
    - [ ] Implement Window Title detection

- [x] **Phase 3: The Dashboard (Web UI)**
    - [x] Create `dashboard` crate (Axum/Tokio)
    - [x] Implement Log Reader API
    - [x] Build Simple/Clean Web UI (No flashiness)
    - [x] Real-time updates (Polling)

- [x] **Phase 4: Distribution & Tools**
    - [x] Create `install.sh` script
    - [x] Implement `import-history` command (Backfill)
    - [x] Add Launch Agent (Auto-start on login)
        - [x] Create `.plist` file
        - [x] Document start/stop commands
    - [x] Full DLP integration (redact before write)

- [x] **Phase 5: Side Quest - Antigravity Decode**
    - [x] Analyze `.pb` file structure (Encrypted/Custom)
    - [x] Attempt decompression (Zstd, Gzip, Brotli, LZ4, Snappy)
    - [x] Search for encryption keys (None found)
    - [x] ~~Reverse engineer Protobuf schema~~ (Not feasible)

- [x] **Phase 6: Dashboard Improvements**
    - [x] Group logs by session_id
    - [x] Display as conversations (User/Assistant alternating)
    - [x] Better metadata display (file_effects, interrupted, copied_to_clipboard)
    - [x] Filter by tool (Codex, Cursor, Antigravity, Claude)
    - [x] Session timeline view

- [ ] **Phase 7: Refinement**
    - [x] Connect Harvester to `MasterLog` writer (Done in Phase 1)
    - [ ] Implement SQLite storage for logs (Optional for now)

- [ ] **Phase 6: Refinement**
    - [ ] Add error handling for file locking
    - [ ] Add configuration file support
