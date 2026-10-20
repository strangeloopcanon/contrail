"""Allowlisted native history inventory; never traverse symbolic links.

This preserves history and its local dependencies, not complete application profiles.
The archive runner must recheck each path before opening it: discovery is not a lock.
"""
import os
from pathlib import Path
import plistlib
import re
import stat
import tempfile


_EXCLUSIONS = [
    "Credentials, auth, settings, plugins, extensions, caches, logs, shell environments, and source workspaces are excluded.",
    "SQLite WAL/SHM sidecars are excluded; databases use online backup. Codex, Cursor desktop and Antigravity snapshots are history projections with auth/settings namespaces removed, not byte-identical full profiles.",
    "Cursor retrieval indexes, MCP state/auth, worker logs, terminals, and workspace trust markers are excluded.",
    "Codex generated worktrees, automations, memories, and application configuration are excluded.",
    "Antigravity account/config, MCP, binaries, implicit context, code tracker and general app state are excluded; derived search indexes are omitted from history projections.",
]


def _checked(path):
    """lstat every ancestor before touching a candidate, including the home itself."""
    for part in reversed([path, *path.parents]):
        try:
            info = part.lstat()
        except FileNotFoundError:
            return None
        if stat.S_ISLNK(info.st_mode):
            raise ValueError("symlink excluded: " + str(part))
    return info


def _children(path, exclusions):
    try:
        info = _checked(path)
        if info is None:
            return []
        if not stat.S_ISDIR(info.st_mode):
            raise ValueError("expected directory: " + str(path))
    except ValueError as error:
        exclusions.append(str(error))
        return []
    # Do not suppress permission/I/O errors: inaccessible is not absent.
    with os.scandir(path) as entries:
        children = sorted(entries, key=lambda entry: entry.name)
    result = []
    for entry in children:
        info = entry.stat(follow_symlinks=False)
        if stat.S_ISLNK(info.st_mode):
            exclusions.append("symlink excluded: " + str(Path(entry.path)))
        elif stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode):
            result.append((Path(entry.path), info))
        else:
            exclusions.append("special file excluded: " + str(Path(entry.path)))
    return result


def discover(home: Path):
    """Return deterministic file sources relative to an absolute, non-symlink home.

    Missing apps are ordinary. Permission failures abort discovery rather than
    silently producing an incomplete archive. Unknown native layout files are
    reported in unsupported; filenames and extensions do not authenticate formats.
    """
    home = Path(home)
    if not home.is_absolute() or ".." in home.parts:
        raise ValueError("home must be an absolute path without '..'")
    home_info = _checked(home)
    if home_info is None or not stat.S_ISDIR(home_info.st_mode):
        raise ValueError("home must be an existing directory")
    inventory = {"sources": [], "exclusions": list(_EXCLUSIONS), "unsupported": [],
                 "versions": {"codex": None, "cursor-cli": None, "cursor-desktop": None,
                              "claude-code": None, "antigravity": None}}
    exclusions = inventory["exclusions"]
    unsupported = inventory["unsupported"]
    seen = set()

    def add(app, path, sqlite=False):
        try:
            info = _checked(path)
        except ValueError as error:
            exclusions.append(str(error))
            return
        if info is None:
            return
        if not stat.S_ISREG(info.st_mode):
            unsupported.append("expected regular history file: " + str(path))
            return
        relative = str(path.relative_to(home))
        if relative not in seen:
            seen.add(relative)
            inventory["sources"].append({"app": app, "path": str(path),
                                         "relative": relative, "sqlite": sqlite})

    def tree(app, root, accept, report_unknown=True):
        pending = [root]
        while pending:
            for path, info in _children(pending.pop(), exclusions):
                if stat.S_ISDIR(info.st_mode):
                    pending.append(path)
                elif accept(path):
                    add(app, path)
                elif report_unknown:
                    unsupported.append("unrecognized history file excluded: " + str(path))

    # Codex transcript trees and versioned native metadata/projection databases.
    codex = home / ".codex"
    for directory in ("sessions", "archived_sessions"):
        tree("codex", codex / directory,
             lambda path: path.name.endswith((".jsonl", ".jsonl.zst")))
    for name in ("session_index.jsonl", "history.jsonl"):
        add("codex", codex / name)
    for path, info in _children(codex, exclusions):
        if re.fullmatch(r"(?:state|thread_history)_\d+\.sqlite", path.name):
            add("codex", path, True)
        elif path.suffix == ".sqlite" and not path.name.startswith("logs_"):
            unsupported.append("unrecognized Codex database excluded: " + str(path))

    cursor = home / ".cursor"
    # store.db + meta.json preserve chat payloads and identities; never copy a
    # whole Cursor project, which can contain MCP credentials.
    pending = [(cursor / "chats", False), (cursor / "acp-sessions", True)]
    while pending:
        directory, acp = pending.pop()
        for path, info in _children(directory, exclusions):
            if stat.S_ISDIR(info.st_mode):
                pending.append((path, acp))
            elif path.name == "store.db":
                add("cursor-cli", path, True)
            elif path.name == "meta.json" or path.suffix == ".jsonl" or (
                acp and re.fullmatch(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\.json", path.name)
            ):
                add("cursor-cli", path)
            elif not path.name.endswith(("-wal", "-shm")):
                unsupported.append("unrecognized Cursor chat file excluded: " + str(path))
    for project, info in _children(cursor / "projects", exclusions):
        if not stat.S_ISDIR(info.st_mode):
            continue
        add("cursor-cli", project / "repo.json")
        tree("cursor-cli", project / "agent-transcripts",
             lambda path: path.suffix in (".jsonl", ".txt", ".md"))
        for dependency in ("agent-tools", "assets", "attachments", "uploads"):
            tree("cursor-cli", project / dependency, lambda path: True)
        # These are not established native history formats; preserve the
        # limitation rather than silently including arbitrary project content.
        for unknown in ("canvases", "snapshots"):
            try:
                if _checked(project / unknown) is not None:
                    unsupported.append("Cursor project dependency not supported: " + str(project / unknown))
            except ValueError as error:
                exclusions.append(str(error))

    # Desktop stores may live under different OS profile roots. Exact known
    # databases, mapping metadata, and attached images are allowlisted.
    desktop_roots = [home / "Library/Application Support/Cursor/User",
                     home / ".config/Cursor/User", home / "AppData/Roaming/Cursor/User"]
    for user in desktop_roots:
        for name in ("state.vscdb", "state.vscdb.backup"):
            add("cursor-desktop", user / "globalStorage" / name, True)
        for workspace, info in _children(user / "workspaceStorage", exclusions):
            if not stat.S_ISDIR(info.st_mode):
                continue
            for name in ("state.vscdb", "state.vscdb.backup"):
                add("cursor-desktop", workspace / name, True)
            add("cursor-desktop", workspace / "workspace.json")
            tree("cursor-desktop", workspace / "images", lambda path: True)
        tree("cursor-desktop", user / "History", lambda path: True)

    claude = home / ".claude"
    add("claude-code", claude / "history.jsonl")
    for project, info in _children(claude / "projects", exclusions):
        if not stat.S_ISDIR(info.st_mode):
            continue
        # Session JSONL, subagent JSONL, and session-local tool results. Do not
        # sweep the entire profile or include environment/shell snapshots.
        for path, child_info in _children(project, exclusions):
            if stat.S_ISREG(child_info.st_mode):
                if path.suffix == ".jsonl" or path.name == "sessions-index.json":
                    add("claude-code", path)
                else:
                    unsupported.append("unrecognized Claude project file excluded: " + str(path))
            elif stat.S_ISDIR(child_info.st_mode):
                tree("claude-code", path / "subagents", lambda item: item.suffix == ".jsonl")
                tree("claude-code", path / "tool-results", lambda item: True)
    for dependency in ("file-history", "todos", "tasks"):
        tree("claude-code", claude / dependency,
             lambda path, dep=dependency: dep == "file-history" or path.suffix == ".json")

    # Only named native profiles qualify; copied backup directories do not.
    # History directories are separate from account/config/runtime state.
    identity = r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}"
    for profile in ("antigravity", "antigravity-cli", "antigravity-ide"):
        antigravity = home / ".gemini" / profile
        for path, info in _children(antigravity / "conversations", exclusions):
            if stat.S_ISREG(info.st_mode) and re.fullmatch(identity + r"\.(?:db|pb)", path.name):
                add("antigravity", path, path.suffix == ".db")
            elif not path.name.endswith(("-wal", "-shm")):
                unsupported.append("unrecognized Antigravity conversation excluded: " + str(path))
        for name in ("conversation_summaries.db", "agyhub_summaries_proto.pb", "jetbox_summaries_proto.pb"):
            add("antigravity", antigravity / name, name.endswith(".db"))
        # Arbitrary conversation-generated artifacts remain private and unredacted.
        # Only native conversation-ID roots qualify; unrelated profile files do not.
        for directory in ("brain", "browser_recordings"):
            for path, info in _children(antigravity / directory, exclusions):
                if stat.S_ISDIR(info.st_mode) and re.fullmatch(identity, path.name):
                    tree("antigravity", path, lambda item: item.name != ".DS_Store")
                elif path.name != ".DS_Store":
                    unsupported.append("unrecognized Antigravity artifact excluded: " + str(path))
        for path, info in _children(antigravity / "annotations", exclusions):
            if stat.S_ISREG(info.st_mode) and re.fullmatch(identity + r"\.pbtxt", path.name):
                add("antigravity", path)
            else:
                unsupported.append("unrecognized Antigravity annotation excluded: " + str(path))

    # Reading bundle metadata avoids starting apps or executing an untrusted CLI.
    # CLI versions remain unknown; a history layout is not an app version.
    for app, bundle in (("codex", "Codex.app"), ("cursor-desktop", "Cursor.app"),
                        ("antigravity", "Antigravity.app")):
        for apps in (home / "Applications", Path("/Applications")):
            metadata = apps / bundle / "Contents/Info.plist"
            try:
                info = _checked(metadata)
                if info is not None and stat.S_ISREG(info.st_mode):
                    with metadata.open("rb") as stream:
                        version = plistlib.load(stream).get("CFBundleShortVersionString")
                    if isinstance(version, str):
                        inventory["versions"][app] = version
                        break
            except (ValueError, plistlib.InvalidFileException):
                unsupported.append("application version metadata unavailable: " + str(metadata))
    if any(source["app"] == "codex" for source in inventory["sources"]):
        unsupported.append("Codex managed attachment blobs/assets are not discovered; native attachment payload metadata alone may not restore media.")
    inventory["sources"].sort(key=lambda source: source["relative"])
    inventory["exclusions"] = sorted(set(exclusions))
    inventory["unsupported"] = sorted(set(unsupported))
    return inventory


# Observed Cursor native history namespaces. Unknown keys are excluded even when
# their names contain "chat" or "composer"; those words also occur in settings.
_DESKTOP_ITEM_KEYS = (
    "aiService.prompts", "aiService.generations", "composer.composerData",
    "composer.composerHeaders", "composer.composerHeaders.version",
    "composer.composerHeaders.tableGateEnabled",
    "workbench.backgroundComposer.persistentData",
    "workbench.backgroundComposer.workspacePersistentData",
)
_DESKTOP_DISK_PREFIXES = (
    "composerData:", "bubbleId:", "composer.content.", "agentKv:",
    "checkpointId:", "codeBlockDiff:", "codeBlockPartialInlineDiffFates:",
    "inlineDiff:", "ofsContent:", "patch-graph:",
)
_DESKTOP_HEADER_COLUMNS = {
    "composerId", "workspaceId", "createdAt", "lastUpdatedAt", "isArchived",
    "isSubagent", "recency", "checkpointAt", "value", "subagentTypeName",
}


def _offline_snapshot_path(path, max_bytes=None):
    path = Path(path)
    if not path.is_absolute():
        raise ValueError("snapshot path must be absolute")
    info = _checked(path)
    if info is None or not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
        raise ValueError("snapshot must be a regular, unlinked private copy")
    if max_bytes is not None and info.st_size > max_bytes:
        raise ValueError("SQLite snapshot exceeds scratch limit")
    parent = path.parent.stat()
    if parent.st_uid != os.geteuid() or stat.S_IMODE(parent.st_mode) & 0o077:
        raise ValueError("snapshot parent must be private")
    for suffix in ("-wal", "-shm", "-journal"):
        if _checked(Path(str(path) + suffix)) is not None:
            raise ValueError("snapshot has active SQLite sidecar: " + suffix)
    return path


def _compact_snapshot(connection, path, max_bytes=None):
    """Replace a committed private snapshot only after its compact copy passes."""
    import sqlite3

    _offline_snapshot_path(path, max_bytes)
    limit = path.stat().st_size if max_bytes is None else max_bytes
    if path.stat().st_size > limit:
        raise ValueError("SQLite snapshot exceeds scratch limit")
    descriptor, name = tempfile.mkstemp(prefix="compact-", suffix=".sqlite", dir=path.parent)
    temporary = Path(name)
    os.close(descriptor)
    try:
        # VACUUM INTO accepts an existing empty file, so retain mkstemp's
        # exclusive creation and 0600 permissions throughout the rebuild.
        connection.execute("VACUUM INTO ?", (str(temporary),))
        info = _checked(temporary)
        if (info is None or not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or stat.S_IMODE(info.st_mode) != 0o600):
            raise ValueError("compact snapshot must be a regular, unlinked private copy")
        if info.st_size > limit:
            raise ValueError("SQLite snapshot exceeds scratch limit")
        checked = sqlite3.connect(temporary.as_uri() + "?mode=ro", uri=True)
        try:
            checks = checked.execute("PRAGMA quick_check").fetchall()
            freelist = checked.execute("PRAGMA freelist_count").fetchone()[0]
            if checks != [("ok",)] or freelist != 0:
                raise ValueError("sanitized snapshot failed SQLite integrity check")
        finally:
            checked.close()
        # The input database must be closed before its pathname is replaced.
        connection.close()
        os.replace(temporary, path)
        return "ok", freelist
    finally:
        for suffix in ("", "-wal", "-shm", "-journal"):
            Path(str(temporary) + suffix).unlink(missing_ok=True)


def sanitize_desktop_snapshot(path: Path, max_bytes=None):
    """Project a caller-owned OFFLINE SQLite snapshot onto known history rows.

    Never call this on native/live stores. This removes auth/settings namespaces
    and unknown schema artifacts; history values remain verbatim and can contain
    secrets supplied in conversation/tool output. This is not general DLP, a full
    profile backup, or a promise of automatic native session resumption.
    """
    import sqlite3

    path = _offline_snapshot_path(path, max_bytes)
    report = {
        "scope": "Cursor native history projection; auth/settings and unknown schema excluded. History payloads are not content-redacted; automatic app resumption is unverified.",
        "tables": {}, "removed_schema": {},
        "preserved_disk_prefixes": list(_DESKTOP_DISK_PREFIXES),
    }
    quote = lambda name: '"' + name.replace('"', '""') + '"'
    connection = sqlite3.connect(path.as_uri() + "?mode=rw", uri=True)
    try:
        connection.execute("PRAGMA trusted_schema=OFF")
        connection.execute("PRAGMA foreign_keys=OFF")
        schema = connection.execute(
            "SELECT type,name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name"
        ).fetchall()
        for kind, name in schema:
            if kind != "table":
                continue
            columns = {row[1] for row in connection.execute("PRAGMA table_info(" + quote(name) + ")")}
            if name in ("ItemTable", "cursorDiskKV") and columns != {"key", "value"}:
                raise ValueError("unrecognized Cursor history schema: " + name)
            if name == "composerHeaders" and (
                    not {"composerId", "value"}.issubset(columns) or not columns.issubset(_DESKTOP_HEADER_COLUMNS)):
                raise ValueError("unrecognized Cursor history schema: composerHeaders")
        # Only disposable offline copies may omit the rollback journal. Failed
        # projection can damage this copy; the backup must discard the run.
        if connection.execute("PRAGMA journal_mode=OFF").fetchone()[0] != "off":
            raise ValueError("offline SQLite runtime does not support journal_mode=OFF; use a compatible Python 3 runtime")
        connection.execute("PRAGMA secure_delete=ON")
        connection.execute("BEGIN IMMEDIATE")
        # Prevent unknown triggers from executing while deleting rows. Explicit
        # indexes/views also carry SQL constants and unrelated profile structure.
        for kind, name in schema:
            if kind in ("trigger", "view", "index"):
                connection.execute("DROP " + kind.upper() + " IF EXISTS " + quote(name))
                report["removed_schema"][kind] = report["removed_schema"].get(kind, 0) + 1
        for kind, name in schema:
            if kind != "table":
                continue
            if connection.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)).fetchone() is None:
                continue
            table = quote(name)
            before = connection.execute("SELECT count(*) FROM " + table).fetchone()[0]
            if name in ("ItemTable", "cursorDiskKV"):
                if name == "ItemTable":
                    clause = "key IN (" + ",".join("?" for _ in _DESKTOP_ITEM_KEYS) + ")"
                    params = _DESKTOP_ITEM_KEYS
                else:
                    clause = " OR ".join("substr(key,1,?)=?" for _ in _DESKTOP_DISK_PREFIXES)
                    params = tuple(item for prefix in _DESKTOP_DISK_PREFIXES for item in (len(prefix), prefix))
                connection.execute("DELETE FROM " + table + " WHERE typeof(key)!='text' OR NOT (" + clause + ")", params)
                kept = connection.execute("SELECT count(*) FROM " + table).fetchone()[0]
            elif name == "composerHeaders":
                kept = before
            else:
                connection.execute("DROP TABLE " + table)
                kept = 0
            report["tables"][name] = {"before": before, "kept": kept, "removed": before - kept}
        connection.commit()
        # Finish projection before allocating the compact output. At most the
        # private input and one database-sized copy coexist; no journal exists.
        report["quick_check"], report["freelist_pages"] = _compact_snapshot(connection, path, max_bytes)
    finally:
        connection.close()
    return report


_ANTIGRAVITY_COLUMNS = {
    "trajectory_meta": "trajectory_id cascade_id trajectory_type source",
    "steps": "idx step_type status has_subtrajectory metadata error_details permissions task_details render_info step_payload step_format",
    "gen_metadata": "idx data size",
    "executor_metadata": "idx data",
    "parent_references": "idx data",
    "trajectory_metadata_blob": "id data",
    "battle_mode_infos": "idx data",
    "conversation_summaries": "conversation_id title preview step_count last_modified_time workspace_uris status source project_id agent_name parent_conversation_id nesting_depth battle_id winning_conversation_id not_fully_idle killed last_user_input_time last_user_input_step_index app_data_dir raw_summary group_id",
    "search_schema_version": "version",
}


def sanitize_antigravity_snapshot(path: Path, max_bytes=None):
    """Project an offline copy onto observed native history tables only.

    Derived search indexes and account/config tables are excluded. Unknown
    columns fail before mutation. Conversation content is not redacted.
    """
    import sqlite3

    path = _offline_snapshot_path(path, max_bytes)
    quote = lambda name: '"' + name.replace('"', '""') + '"'
    report = {"scope": "Antigravity native history projection; account/config and derived search indexes excluded; native resumption unverified.",
              "tables": {}, "excluded_tables": [], "removed_schema": {}}
    connection = sqlite3.connect(path.as_uri() + "?mode=rw", uri=True)
    try:
        connection.execute("PRAGMA trusted_schema=OFF")
        connection.execute("PRAGMA foreign_keys=OFF")
        schema = connection.execute("SELECT type,name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").fetchall()
        tables = {name for kind, name, _ in schema if kind == "table"}
        if {"trajectory_meta", "steps"}.issubset(tables):
            permitted = set(_ANTIGRAVITY_COLUMNS) - {"conversation_summaries", "search_schema_version"}
            report["layout"] = "trajectory"
        elif "conversation_summaries" in tables:
            permitted = {"conversation_summaries", "search_schema_version"}
            report["layout"] = "summaries"
        else:
            raise ValueError("unrecognized Antigravity history database layout")
        for name in tables & permitted:
            column_info = connection.execute("PRAGMA table_xinfo(" + quote(name) + ")").fetchall()
            columns = {row[1] for row in column_info}
            if (columns != set(_ANTIGRAVITY_COLUMNS[name].split())
                    or any(row[6] != 0 for row in column_info)):
                raise ValueError("unrecognized Antigravity history schema: " + name)
            definition = next(sql for kind, table, sql in schema if kind == "table" and table == name)
            if definition and "VIRTUAL TABLE" in definition.upper():
                raise ValueError("unrecognized Antigravity virtual history table: " + name)
        if connection.execute("PRAGMA journal_mode=OFF").fetchone()[0] != "off":
            raise ValueError("offline SQLite runtime does not support journal_mode=OFF; use a compatible Python 3 runtime")
        connection.execute("PRAGMA secure_delete=ON")
        connection.execute("BEGIN IMMEDIATE")
        for kind, name, _ in schema:
            if kind in ("trigger", "view", "index"):
                connection.execute("DROP " + kind.upper() + " IF EXISTS " + quote(name))
                report["removed_schema"][kind] = report["removed_schema"].get(kind, 0) + 1
        # Virtual tables must be dropped before their shadow tables.
        ordered = sorted((item for item in schema if item[0] == "table"),
                         key=lambda item: not (item[2] and "VIRTUAL TABLE" in item[2].upper()))
        for _, name, _ in ordered:
            if connection.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)).fetchone() is None:
                continue
            if name in permitted:
                count = connection.execute("SELECT count(*) FROM " + quote(name)).fetchone()[0]
                report["tables"][name] = {"kept": count}
            else:
                connection.execute("DROP TABLE " + quote(name))
                report["excluded_tables"].append(name)
        connection.commit()
        report["quick_check"], report["freelist_pages"] = _compact_snapshot(connection, path, max_bytes)
    finally:
        connection.close()
    return report


_CODEX_TABLE_COLUMNS = {
    "_sqlx_migrations": "version description installed_on success checksum execution_time",
    "backfill_state": "id status last_watermark last_success_at updated_at",
    "project_idempotency_keys": "key project_id created_at_ms",
    "project_roots": "project_id position path",
    "projects": "id name metadata position created_at_ms updated_at_ms",
    "rollout_migration_skipped_rollouts": "migration_id rollout_path rollout_size_bytes rollout_modified_at_ns skip_reason skipped_at",
    "rollout_migration_state": "migration_id last_checked_thread_created_at last_checked_thread_id updated_at",
    "thread_attachments": "id thread_id attachment_type identity_key payload created_at",
    "thread_dynamic_tools": "thread_id position name description input_schema defer_loading namespace",
    "thread_sections": "id name appearance",
    "thread_spawn_edges": "parent_thread_id child_thread_id status",
    "threads": "id rollout_path created_at updated_at source model_provider cwd title sandbox_policy approval_mode tokens_used has_user_event archived archived_at git_sha git_branch git_origin_url cli_version first_user_message agent_nickname agent_role memory_mode model reasoning_effort agent_path created_at_ms updated_at_ms thread_source preview recency_at recency_at_ms history_mode name is_pinned thread_section_id section_position section_entered_at_ms project_id originator daybreak_enabled creator_user_id creator_account_id",
    "thread_history_projection_state": "thread_id next_rollout_byte_offset next_rollout_ordinal",
    "thread_items": "thread_id turn_id item_id rollout_ordinal created_at_ms item_json item_type updated_at_ordinal started_at_ms completed_at_ms",
    "thread_realtime_items": "thread_id item_id rollout_ordinal created_at_ms item_type item_json",
    "thread_turns": "thread_id turn_id rollout_ordinal status error_json started_at completed_at duration_ms first_user_item_id final_agent_item_id rollout_byte_offset rollout_end_ordinal rollout_end_byte_offset root_turn_id",
}


def sanitize_codex_snapshot(path: Path, max_bytes=None):
    """Remove non-history tables from a caller-owned closed Codex SQLite copy.

    This is a native history projection, not a profile backup or content DLP.
    Credentials present inside transcript/tool payloads are not examined/redacted.
    Only established state/history schemas are accepted; unknown layouts fail.
    """
    import sqlite3

    path = _offline_snapshot_path(path, max_bytes)
    quote = lambda name: '"' + name.replace('"', '""') + '"'
    report = {
        "scope": "Codex native history projection; enrollment/config/auth and unknown schema excluded. History payloads are not content-redacted. Managed blobs/assets are not discovered; automatic native resumption is unverified.",
        "tables": {}, "excluded_tables": [], "removed_schema": {},
    }
    connection = sqlite3.connect(path.as_uri() + "?mode=rw", uri=True)
    try:
        connection.execute("PRAGMA trusted_schema=OFF")
        connection.execute("PRAGMA foreign_keys=OFF")
        schema = connection.execute(
            "SELECT type,name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name"
        ).fetchall()
        tables = {name for kind, name in schema if kind == "table"}
        if "threads" in tables:
            report["layout"] = "state"
            permitted = set(_CODEX_TABLE_COLUMNS) - {
                "thread_history_projection_state", "thread_items", "thread_realtime_items", "thread_turns",
            }
        elif "thread_items" in tables or "thread_turns" in tables:
            report["layout"] = "thread_history"
            permitted = {"_sqlx_migrations", "thread_history_projection_state", "thread_items", "thread_realtime_items", "thread_turns"}
        else:
            raise ValueError("unrecognized Codex history database layout")
        # Validate before any mutation. A familiar table name with extra columns
        # could be a future credential-bearing schema and must not pass silently.
        for name in tables & permitted:
            columns = {row[1] for row in connection.execute("PRAGMA table_info(" + quote(name) + ")")}
            expected = set(_CODEX_TABLE_COLUMNS[name].split())
            required = {"id", "rollout_path"} if name == "threads" else {"parent_thread_id", "child_thread_id"} if name == "thread_spawn_edges" else {"thread_id"} if name.startswith("thread_") and name != "thread_sections" else set()
            if not columns or not columns.issubset(expected) or not required.issubset(columns):
                raise ValueError("unrecognized Codex history schema: " + name)
        # This is disposable scratch, never a live store. Projection failure
        # may damage it without a journal; backup discards the incomplete run.
        if connection.execute("PRAGMA journal_mode=OFF").fetchone()[0] != "off":
            raise ValueError("offline SQLite runtime does not support journal_mode=OFF; use a compatible Python 3 runtime")
        connection.execute("PRAGMA secure_delete=ON")
        connection.execute("BEGIN IMMEDIATE")
        for kind, name in schema:
            if kind in ("trigger", "view", "index"):
                connection.execute("DROP " + kind.upper() + " IF EXISTS " + quote(name))
                report["removed_schema"][kind] = report["removed_schema"].get(kind, 0) + 1
        for kind, name in schema:
            if kind != "table":
                continue
            if connection.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?", (name,)).fetchone() is None:
                continue
            table = quote(name)
            before = connection.execute("SELECT count(*) FROM " + table).fetchone()[0]
            kept = before if name in permitted else 0
            if name not in permitted:
                connection.execute("DROP TABLE " + table)
                report["excluded_tables"].append(name)
            report["tables"][name] = {"before": before, "kept": kept, "removed": before - kept}
        connection.commit()
        report["quick_check"], report["freelist_pages"] = _compact_snapshot(connection, path, max_bytes)
    finally:
        connection.close()
    return report
