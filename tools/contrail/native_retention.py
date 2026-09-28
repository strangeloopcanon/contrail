"""Read-only native history retention planning; deletion deliberately fails closed.

Codex state metadata and rollout event timestamps establish inactivity. A native
database cannot establish whether a desktop/CLI writer is running, or rule out
fork references in another Codex home. Those protections remain unknown. Past
experimental ``thread/delete`` receipts do not establish a supported, guarded
deletion interface in the installed release. No SQLite mutation or unlink is
performed by this module. Cursor deletion is also unsupported.
"""

import datetime
import math
import re
import sqlite3
import time
from pathlib import Path


def _retention_epoch(value):
    """Accept finite Unix seconds/milliseconds or timezone-qualified ISO times."""
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        stamp = float(value)
        if not math.isfinite(stamp) or stamp <= 0:
            return None
        return stamp / 1000 if stamp >= 100_000_000_000 else stamp
    if isinstance(value, str):
        try:
            parsed = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
            return parsed.timestamp() if parsed.tzinfo is not None else None
        except (ValueError, OverflowError):
            return None
    return None


def _retention_iso(stamp):
    return datetime.datetime.fromtimestamp(stamp, datetime.timezone.utc).isoformat()


def _retention_within(path, home):
    """Symlinks are not accepted as deletion targets, including parent links."""
    try:
        path = Path(path)
        if not path.is_absolute() or path != path.resolve():
            return False
        path.relative_to(home)
        return True
    except (ValueError, OSError, RuntimeError):
        return False


def _retention_rollout(path):
    """Read activity without retaining prompt/output text or trusting filenames."""
    import json

    latest = None
    fork_ids = set()
    errors = []
    try:
        before = path.stat()
        with path.open("r", encoding="utf-8") as stream:
            for line in stream:
                if not line.strip():
                    continue
                try:
                    event = json.loads(line)
                except (ValueError, UnicodeError, RecursionError):
                    errors.append("malformed_rollout")
                    break
                if not isinstance(event, dict):
                    errors.append("malformed_rollout")
                    break
                stamp = _retention_epoch(event.get("timestamp"))
                if stamp is None:
                    errors.append("unknown_event_activity")
                else:
                    latest = max(latest or stamp, stamp)
                if event.get("type") == "session_meta":
                    payload = event.get("payload")
                    if isinstance(payload, dict):
                        for key in ("forked_from_id", "forked_from_thread_id", "parent_thread_id"):
                            if isinstance(payload.get(key), str) and payload[key]:
                                fork_ids.add(payload[key])
        after = path.stat()
        if (before.st_size, before.st_mtime_ns, before.st_ino) != (
            after.st_size, after.st_mtime_ns, after.st_ino
        ):
            errors.append("rollout_changed_during_scan")
        # Touching a file only makes the planner more conservative.
        latest = max(latest or 0, after.st_mtime)
        return latest, after.st_size, fork_ids, sorted(set(errors))
    except (OSError, UnicodeError):
        return None, 0, fork_ids, ["unreadable_rollout"]


def _retention_automation_targets(codex_root):
    """Recognize target links while withholding an all-clear on unreadable config."""
    targets = set()
    errors = []
    root = codex_root / "automations"
    if not root.exists():
        return targets, errors
    if root.is_symlink() or not root.is_dir():
        return targets, ["automation_inventory_unknown"]
    try:
        configs = list(root.glob("*/automation.toml"))
        for config in configs:
            if config.is_symlink():
                errors.append("automation_inventory_unknown")
                continue
            text = config.read_text(encoding="utf-8")
            # This extraction is protective only: a successful match can retain
            # a thread, but cannot prove absence of other/native link storage.
            for match in re.finditer(
                r'(?:target_thread_id|targetThreadId|thread_id|threadId)\s*=\s*[\"\']([^\"\']+)[\"\']',
                text,
            ):
                targets.add(match.group(1))
    except (OSError, UnicodeError):
        errors.append("automation_inventory_unknown")
    return targets, errors


def _retention_codex_rows(database):
    """Read one SQLite transaction through a read-only URI; never create a DB."""
    connection = sqlite3.connect(database.as_uri() + "?mode=ro", uri=True, timeout=2)
    try:
        connection.execute("PRAGMA query_only=ON")
        connection.execute("BEGIN")
        required = {"id", "rollout_path", "updated_at", "recency_at", "is_pinned"}
        columns = {row[1] for row in connection.execute("PRAGMA table_info(threads)")}
        if not required.issubset(columns):
            raise ValueError("unsupported_codex_thread_schema")
        rows = connection.execute(
            "SELECT id, rollout_path, updated_at, recency_at, is_pinned FROM threads"
        ).fetchall()
        tables = {
            row[0] for row in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")
        }
        if "thread_spawn_edges" not in tables:
            return rows, set(), ["fork_dependency_inventory_unknown"]
        edge_columns = {
            row[1] for row in connection.execute("PRAGMA table_info(thread_spawn_edges)")
        }
        if not {"parent_thread_id", "child_thread_id"}.issubset(edge_columns):
            return rows, set(), ["fork_dependency_inventory_unknown"]
        endpoints = set()
        for parent, child in connection.execute(
            "SELECT parent_thread_id, child_thread_id FROM thread_spawn_edges"
        ):
            endpoints.update((parent, child))
        return rows, endpoints, []
    finally:
        connection.close()


def retention_plan(home: Path, inventory: dict, days=30) -> dict:
    """Produce reviewable age candidates with explicit protection/apply blockers.

    ``inventory['sources']`` contains app/path/sqlite records from discovery.
    A candidate means only old enough by last activity, never safe to delete.
    Every native delete remains blocked until a release-supported guarded API,
    verified archive, and fresh authoritative protection state are established.
    """
    if isinstance(days, bool) or not isinstance(days, int) or days <= 0:
        raise ValueError("retention days must be a positive integer")
    home = Path(home).absolute().resolve()
    now = time.time()
    cutoff = now - days * 86400
    entries = []
    coverage = []
    seen = set()
    codex_sources = []
    other_apps = set()
    for source in inventory.get("sources", []):
        app = str(source.get("app", "unknown"))
        path = Path(str(source.get("path", "")))
        if "codex" in app.lower() and source.get("sqlite") and re.fullmatch(
            r"state_\d+\.sqlite", path.name
        ):
            if _retention_within(path, home):
                codex_sources.append(path)
            else:
                coverage.append({"app": app, "status": "blocked", "reason": "unsafe_state_path"})
        elif "codex" not in app.lower():
            other_apps.add(app)
    for database in sorted(set(codex_sources)):
        root = database.parent
        automation_targets, automation_errors = _retention_automation_targets(root)
        try:
            rows, endpoints, edge_errors = _retention_codex_rows(database)
        except (sqlite3.Error, ValueError, OSError):
            coverage.append({"app": "codex", "source": str(database), "status": "blocked", "reason": "state_unreadable_or_schema_unsupported"})
            continue
        records = []
        fork_targets = set(endpoints)
        for thread_id, rollout, updated, recency, pinned in rows:
            key = (str(database), str(thread_id))
            if key in seen:
                continue
            seen.add(key)
            entry = {"app": "codex", "id": str(thread_id), "state_source": str(database), "path": rollout, "reasons": []}
            activity = [_retention_epoch(updated), _retention_epoch(recency)]
            blockers = list(automation_errors + edge_errors)
            if pinned not in (0, 1):
                blockers.append("pin_state_unknown")
            if 1 == pinned:
                entry["reasons"].append("pinned")
            if thread_id in automation_targets:
                entry["reasons"].append("automation_linked")
            path = Path(rollout) if isinstance(rollout, str) else None
            if path is None or not _retention_within(path, home):
                blockers.append("unsafe_or_unknown_rollout_path")
            elif not path.is_file():
                blockers.append("rollout_missing")
            else:
                latest, size, references, scan_errors = _retention_rollout(path)
                entry["bytes"] = size
                activity.append(latest)
                blockers.extend(scan_errors)
                if references:
                    fork_targets.add(thread_id)
                    fork_targets.update(references)
                if path.is_relative_to(root) and "automations" in path.relative_to(root).parts:
                    entry["reasons"].append("automation_home")
            if any(stamp is None for stamp in activity):
                blockers.append("last_activity_unknown")
            stamps = [stamp for stamp in activity if stamp is not None]
            last = max(stamps) if stamps else None
            if last is not None:
                try:
                    entry["last_activity"] = _retention_iso(last)
                    entry["last_activity_epoch"] = last
                except (OverflowError, OSError, ValueError):
                    blockers.append("last_activity_invalid")
                    last = None
            records.append((entry, blockers, last))
        for entry, blockers, last in records:
            if entry["id"] in fork_targets:
                entry["reasons"].append("fork_dependency")
            if entry["reasons"]:
                entry["status"] = "retained_protected"
            elif last is not None and last >= cutoff:
                entry["status"] = "retained_recent"
                entry["reasons"].append("within_retention_window")
            elif blockers:
                entry["status"] = "blocked"
            else:
                entry["status"] = "candidate_blocked"
            entry["blockers"] = sorted(set(blockers + [
                "running_state_unknown", "cross_home_fork_references_unknown",
                "native_automation_links_unknown", "verified_archive_required",
                "supported_guarded_native_delete_unavailable",
            ]))
            entries.append(entry)
        coverage.append({"app": "codex", "source": str(database), "status": "partial_read_only", "threads": len(records), "activity": "max(state updated_at, state recency_at, all rollout event timestamps, file mtime)"})
    if not codex_sources:
        coverage.append({"app": "codex", "status": "blocked", "reason": "native_state_not_discovered"})
    for app in sorted(other_apps):
        coverage.append({"app": app, "status": "unsupported", "reason": "authoritative_activity_and_native_protection_interface_unavailable"})
    counts = {}
    for entry in entries:
        counts[entry["status"]] = counts.get(entry["status"], 0) + 1
    return {
        "schema_version": 1, "mode": "dry_run", "days": days,
        "planned_at": _retention_iso(now), "cutoff_epoch": cutoff,
        "policy": "Retain sessions with any activity in the window; protect running, pinned, automation-linked and fork-dependent sessions; unknown protection blocks deletion.",
        "native_apply_supported": False, "deletion_count": 0,
        "counts": counts, "entries": entries, "coverage": coverage,
        "candidate_bytes": sum(entry.get("bytes", 0) for entry in entries if entry["status"] == "candidate_blocked"),
        "requirements_before_apply": [
            "Verify a restorable archive and exact source hashes for every affected native record/file.",
            "Obtain fresh authoritative running, pin, automation and cross-home fork protection state.",
            "Establish a release-supported native delete interface with atomic writer/reference guards and full affected-member preflight.",
            "Obtain explicit user approval for the concrete completed deletion plan; archive/hide/unsubscribe is not deletion.",
        ],
    }


def retention_apply(home: Path, plan: dict, *, approved=False, archive_manifest=None) -> dict:
    """Report why native apply cannot run; never mutate native storage.

    Approval and a caller-supplied manifest cannot replace a verified supported
    interface or atomic protection checks. Kept as an explicit API boundary so
    a CLI cannot silently turn dry-run candidates into filesystem deletions.
    """
    blockers = ["supported_guarded_native_delete_unavailable"]
    if approved is not True:
        blockers.append("explicit_approval_required")
    blockers.extend(["verified_archive_required", "fresh_native_protection_required"])
    return {"mode": "apply", "status": "blocked", "native_apply_supported": False, "deleted": [], "blockers": blockers}
