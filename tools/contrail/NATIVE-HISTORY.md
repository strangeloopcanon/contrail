# Native history backup

`contrail history` preserves native coding-app history without running the daemon or importing another master log. Python 3 with SQLite is required; no third-party Python packages or new Rust dependencies are needed. The engine is embedded in the Rust binary. `catalog`, `restore-plan` and `extract --member` require `contrail-cli` 0.1.7 or `contrails` 0.1.6 or later; source operation is `python3 tools/contrail/native_history.py` with the same arguments. Unlike daemon capture, these archives are unredacted originals.

```sh
contrail history inventory
contrail history dry-run
contrail history backup --output /absolute/path/outside-live-profiles
contrail history backup --output /absolute/path/outside-live-profiles --base-run /absolute/path/outside-live-profiles/PREVIOUS-RUN
contrail history verify --run /absolute/path/UTC-RUN
contrail history extract --run /absolute/path/UTC-RUN --destination /absolute/path/new-folder
contrail history catalog --run /absolute/path/UTC-RUN --app claude-code --contains projects/
contrail history restore-plan --run /absolute/path/UTC-RUN --member .claude/projects/PROJECT/SESSION.jsonl
contrail history extract --run /absolute/path/UTC-RUN --destination /absolute/path/new-folder --member .claude/projects/PROJECT/SESSION.jsonl
contrail history retention-plan
```

`--home` selects another or disposable home. Backup `--app claude-code` (repeatable) makes an explicitly partial backup; omit it for all installed default-profile apps. Budgets default to 12 GiB output, 18 GiB snapshot scratch (including projection journals/VACUUM), 80 MiB parts; lower them with `--output-mib`, `--scratch-mib`, `--part-mib`. Budget failures remove the incomplete run. Hard kills leave a lock and partial run: prove the lock PID has ended, inspect the abandoned generated run, then remove only that run and lock before retrying. No live histories are changed.

Each unique UTC run contains numbered gzip/tar parts, file/part/full-archive SHA256, part MD5 for Drive comparison, manifest, restore instructions and verification receipt. Configured cloud destinations also produce a cloud handoff. There is no concatenated archive on disk. Scratch contains one snapshot at a time; Codex and desktop projection reserve three times the snapshot size for the database and temporary journal/VACUUM files within the scratch budget. Configured scratch plus output budgets must fit free space before a run. Verification streams all parts and extracts one SQLite member at a time. Source files are checked for stable identity/size/mtime/ctime with three attempts; SQLite online backup includes committed WAL transactions and normalizes only the snapshot to portable journal mode. A closed WAL-header store that cannot open read-only is copied only while all live sidecars are absent before and after a stable copy; immutable mode is used only on that private copy. Other SQLite scratch budgets reserve a copy plus backup. Boundaries are per-file/per-database, not globally atomic. Do not replace source paths during backup.

| Adapter | Preserved | Limits |
| --- | --- | --- |
| Codex | Session/archived JSONL, prompt/session indexes, known history tables in online state/thread-history snapshots | Default home; auth/enrollment/config state excluded; managed attachment blobs unsupported; worktrees, automations, memories/config excluded |
| Cursor CLI | Chat stores/meta, ACP formats, project transcripts/tool/media dependencies | MCP auth/state/caches and source excluded; canvases/snapshots unsupported |
| Cursor desktop | Online state snapshots projected to known history records, workspace mappings/history/images | Credentials and unrelated database state excluded from offline snapshots; unknown layouts reported |
| Claude Code | Sessions/subagents/indexes, tool results, file-history/tasks/todos | Default profile; custom config locations and shell snapshots excluded |

Read manifest `exclusions`, `unsupported`, `versions`, and `scope` before claiming coverage. Permission failures abort rather than masquerading as absent apps. Transcripts preserve original contents and may contain user-provided secrets; this does not redact transcripts. Codex/Cursor SQLite projections preserve selected native logical history records, not exact live database bytes; unknown history schemas fail closed. Native per-chat import and whole-profile recovery remain untested. Extraction rejects links/special files/traversal and creates a new destination, never an app profile. Preserve dependencies together. Before version-specific profile recovery, preserve current state and quit all writers; never mix restored databases with current WAL/SHM. Re-authentication may be necessary.

## Incremental backup

Make one full backup, then pass `--base-run PREVIOUS-RUN` for subsequent runs. The previous run must be a sibling in the same output folder with the same home and app scope. Contrail snapshots and hashes sources, compares SHA256/size/app/type against the cumulative catalog, and stores only new or changed files. It reports new, changed, unchanged and missing-local-but-preserved counts. This reduces compression and upload, but still reads/snapshots sources to establish exact equality. Changed files, including SQLite databases, are complete replacement snapshots; this is file-level incremental storage, not byte-tail or database-row merging.

Earlier archives are immutable. Missing local files stay in the cumulative catalog so native retention cannot erase already archived history. Each delta binds its parent's manifest SHA256 and records where every latest file originated. Keep all referenced runs together; download them into sibling folders named by run ID. `verify` checks the entire chain; `extract` assembles the latest retained version of every file in a new destination, applying changes only in private staging. Corrupt parts, changed parent manifests, missing parents and incompatible scopes are rejected.

Creation accepts a manifest-bound verification receipt for its base without rereading old archives, allowing earlier parts to have been uploaded and released locally. Legacy receipts without manifest binding require a full base verification once. Receipts are trusted local evidence; a delta alone is not proof that remote parent bytes still exist. Upload the new run's handoff files and retain all prior remote runs: they are required for recovery. To start a self-contained new baseline, omit `--base-run`; it covers current local sources, not history already deleted locally.

## Selective recovery

`catalog` lists the latest retained file metadata, including files no longer present in live storage. It reads only the local manifest chain. Filter by `--app` or a case-sensitive `--contains` substring; paginate with `--limit` (1–1000, default 100) and `--offset`. Copy exact `name` values into repeatable `--member` arguments.

`restore-plan --member NAME` identifies the origin runs, numbered archive parts, expected hashes and download sizes needed for the selected latest versions. It works after local archive release and performs no downloads. Remote IDs, when available, are locations from saved upload receipts, not a fresh check that remote bytes still exist. Preserve the complete manifest chain in sibling run folders; only the selected origin runs need archive payloads for selective extraction.

Download payloads only when you explicitly want recovery. One run is a single gzip/tar stream split into parts, so every part of a required origin run is needed even for one small file; this is not a random-access archive format. `extract --member NAME` verifies all part, archive and member hashes and SQLite integrity in those origin runs, then writes only selected files into a new private destination. It never downloads automatically. `--max-expanded-mib` (default 65536) bounds the aggregate expanded member data in required origin runs, including unselected files that must be checked. Decompression also has a bounded allowance for TAR headers and padding. The archive format is unchanged; `verify`, and `extract` without `--member`, keep their existing whole-chain behavior. Do not modify archive metadata, parts or destination parents during recovery; the checks do not defend against a hostile actor with access to the same local account.

Selection is by file, not by native conversation: a SQLite database can hold many chats, and transcripts can reference tool outputs, subagents, indexes and attachments. Contrail does not infer a complete dependency closure or import files into an application. Include known dependencies explicitly and consult adapter limits. Restoring one file proves its bytes are recoverable; it does not prove a conversation can resume in its original app.

## Connected private Drive handoff

The authenticated Google Drive connector supplies upload authority. Contrail stores no OAuth credentials. Configure your private folder and expected owner with `--cloud-parent-id FOLDER_ID --cloud-owner OWNER_EMAIL` on backup or cloud-handoff. You can also set `CONTRAIL_HISTORY_CLOUD_PARENT` and `CONTRAIL_HISTORY_CLOUD_OWNER`. Without this configuration, backups remain local until you generate a handoff. No personal destination is built into the package.

1. Run backup and read `cloud-handoff.json`, the ordered upload list. Verify the exact private parent and owner. Create a dated child named for the run, or reuse the verified child recorded for that run.
2. Upload each listed part, manifest, restore guide and verification receipt with the connector. Journal each returned ID immediately. Retry by reading the saved ID and checking name/parent/bytes/checksum; never blindly create duplicates.
3. Read back owner, permissions, parents and size. Compare provider SHA256/MD5 if exposed. If unavailable, retain local parts or explicitly choose the size-only policy below. Downloads are not an automatic verification step. Size alone is not checksum verification.
4. Save authenticated evidence in the receipt shape below. Run `contrail history record-cloud --run RUN --receipt FILE`. It validates one-to-one complete coverage, local correspondence and remote checksum fields; remote evidence remains connector/operator supplied.
5. Upload `cloud-receipt.json`. Run `contrail history release-local --run RUN`, then upload `local-release.json`. Release removes only generated archive parts; manifests, receipts and catalog metadata remain, and no native history is deleted. By default, release requires remote checksum coverage of every handoff file. If you explicitly choose to rely on provider upload confirmations instead, use `release-local --accept-size-only`. This requires complete matching upload-size/parent/private-permission evidence, records the lower-assurance `provider-upload-integrity` policy in a bound release receipt, and allows later incrementals to use the released cloud parent. The flag does not claim byte-level remote verification.

```json
{
  "run_id": "UTC-RUN", "parent_folder_id": "PRIVATE_PARENT_ID",
  "owner": "owner@example.invalid", "private": true, "folder_id": "verified-child-ID",
  "files": [
    {"name": "history.tar.gz.part00001", "id": "verified-file-ID",
     "parent_id": "verified-child-ID", "private": true, "bytes": 123,
     "sha256": "verified downloaded/provider SHA256"}
  ]
}
```

Include every handoff file in the receipt. Missing checksums yield size-only evidence; wrong checksums are rejected even when size-only release is accepted. Receipts are trusted connector/operator evidence, not independent cryptographic attestations. This orchestration is the cloud backend; this is not a standalone unattended OAuth client. Incremental runs omit unchanged files when given a compatible verified native-history base; older unrelated backup formats are not supported.

## Retention and migration

Policy is **30 days since last activity**, protecting running, pinned, automation-linked and fork-dependent sessions. Codex planning combines native metadata, every rollout event time and conservative file mtime. Unknown/malformed activity or protection blocks deletion. Cursor/Claude authoritative retention APIs remain unsupported. `retention-apply` fails closed: historical experimental helpers do not establish a supported guarded interface in the installed app release. Approval cannot bypass missing protection checks.

For recurring backup, maintain one full baseline followed by `--base-run` incrementals. Scan all supported histories each time: a rolling date filter can miss updates to older conversations. A narrower source list with the same app scope retains omitted baseline files, but filtering the contents of a transcript or database can discard older records from its latest restored version. Contrail does not merge rows from separately generated rolling-window exports.

Keep all referenced remote runs. After authenticated remote checksum verification, release local archive parts; retain manifests, verification receipts, cloud handoffs and upload receipts for comparison and recovery. Released parents are validated using their checksum receipts and bound metadata, so subsequent incrementals do not require downloading their archive parts. Temporary local scratch and current-run parts are required during creation, upload and verification; cloud-only means no permanent local archive copy. Size-only upload evidence blocks release by default. Explicit `--accept-size-only` relies on provider upload integrity, records the policy in a bound release receipt, and avoids download readback. Do not download archives just to compare their size; use authenticated metadata. Required metadata stays local; it is much smaller than payload archives but can grow with the catalog.

`make check` supplies formatting/clippy; `make test` runs disposable Python fixtures through a Rust test. LLM live tests are not applicable to this archive path. Native cleanup remains disabled until a supported protection-aware deletion interface exists.
