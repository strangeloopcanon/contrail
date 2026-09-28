# Preserve coding history without keeping every backup on your laptop

Contrail can preserve supported native histories from Codex, Cursor and Claude Code as immutable snapshots. Later runs store only new or changed files. Upload the snapshots to storage you control through an external authenticated connector workflow, then release generated local archive parts under your chosen verification policy. Small local catalogs and receipts support the next comparison and recovery planning.

Catalog browsing, restore planning and selected extraction arrive in `contrail-cli` 0.1.7 and `contrails` 0.1.6. Older installed releases do not have these commands. The archive format is unchanged, and whole-chain `verify` and `extract` work as before.

These snapshots are not the redacted Contrail capture log. They keep original transcripts and databases, which can contain pasted secrets, and Contrail does not encrypt them.

## Back up, upload, release local parts

```sh
contrail history backup --output /path/to/snapshots
contrail history backup --output /path/to/snapshots \
  --base-run /path/to/snapshots/PREVIOUS-RUN
```

The first run is a full baseline; `--base-run` stores only files that are new or changed since a sibling run. With a private Drive folder configured, each run writes `cloud-handoff.json`, the list of files to upload. Contrail has no built-in cloud login. An authenticated connector uploads those files and writes a receipt, which `contrail history record-cloud` validates.

`contrail history release-local --run RUN` then removes that run's generated archive parts. By default, it requires remote checksums for every uploaded file. Adding `--accept-size-only` explicitly relies on provider upload integrity once size, parent folder and private permissions match; the release receipt records that policy. Manifests, receipts and the catalog stay local. Backup and upload still need temporary local scratch space and the current run's parts until release. Keep every referenced remote run: later incrementals and recovery depend on them.

## Recover only what you need

Browse the latest retained file catalog without opening archive payloads:

```sh
contrail history catalog --run /path/to/snapshots/LATEST-RUN \
  --app claude-code --contains SESSION-ID
```

Copy an exact file name from the result and ask what recovery would require:

```sh
contrail history restore-plan --run /path/to/snapshots/LATEST-RUN \
  --member .claude/projects/PROJECT/SESSION-ID.jsonl
```

The plan reports the runs and parts needed for that file's latest archived version, their sizes and expected hashes. Planning does not download anything. Keep the manifest chain locally; fetch the listed payload parts only when you want to recover data. A required run's entire compressed stream is needed, even for one file. Saved remote file IDs help locate uploads but do not prove current remote availability.

After fetching those parts into their original run folders:

```sh
contrail history extract --run /path/to/snapshots/LATEST-RUN \
  --member .claude/projects/PROJECT/SESSION-ID.jsonl \
  --destination /path/to/new-recovery-folder
```

Repeat `--member` for additional files and known dependencies; Contrail does not infer tool outputs, subagents, indexes or attachments a transcript needs. Extraction validates the required archives and writes selected files into a new folder. It does not overwrite a running app's profile or import a conversation, and importing or resuming restored files in the native app is untested. SQLite stores may contain many conversations; file selection is not chat selection.

## What works, and what comes next

| Capability | Current behavior |
| --- | --- |
| Native capture | Supported default-profile histories, coherent SQLite snapshots and documented coverage gaps |
| Incremental storage | Exact file comparison; changed files stored whole; earlier versions preserved |
| Cloud-only archive payloads | External authenticated upload workflow; explicit policy for releasing generated local parts |
| Find and recover files | Metadata-only catalog and restore plan; selected extraction into a new folder |
| Scheduling | External scheduler or connected workflow; no built-in unattended cloud login |
| Automatic native-history cleanup | Disabled; activity/protection and app recovery are not yet sufficiently established |

## Development plan

1. **Prove recovery in each application.** Use disposable profiles with transcripts, tool results, subagents, attachments and SQLite records. Archive, remove only fixture history, restore through a documented app-specific procedure, then resume and exercise checkpoints where supported. Record app versions and limitations. This is the prerequisite for offering native cleanup.
2. **Define protected retention per adapter.** Establish running-session, pin, automation and fork/dependency protection through supported interfaces. Unknown protection blocks removal. Preview the exact items and bytes; never remove history needed by retained sessions.
3. **Integrate one unattended upload workflow.** Store credentials outside history archives. Persist upload IDs, resume partial runs, prevent duplicate uploads and overlapping schedules, and report actionable failures. Support an explicit remote verification policy without silently downloading backups.
4. **Offer a local storage budget.** Once recovery and protection pass, archive eligible inactive sessions before cleanup, retain a searchable local catalog, and restore on demand. Detect missing remote evidence before removing any source history. Encryption and chunk deduplication deserve evaluation before scaling large, frequently changing databases.

The acceptance test is an archived conversation that can be restored and resumed after local removal. A successful upload or a valid SQLite file alone does not establish that behavior.

## Related tools

[SpecStory](https://docs.specstory.com/cloud/sync-and-store) captures conversations and syncs them for cloud search while retaining local Markdown. [SessionVault](https://github.com/rush-skills/sessionvault) documents scheduled incremental native-history uploads to user-owned storage. [restic](https://github.com/restic/restic) provides encrypted, deduplicated backup snapshots across storage backends. These establish that automatic backup is already a category; Contrail's proposed extension is application-aware retention and recovery across coding tools.

Read [coverage, verification policies and recovery limits](NATIVE-HISTORY.md) before relying on an archive. Native transcripts can contain pasted secrets; the history backup does not redact their original contents.
