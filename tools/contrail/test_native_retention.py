"""Disposable native-history fixtures; no developer history is read or deleted."""

import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

_SPEC = importlib.util.spec_from_file_location(
    "native_retention", Path(__file__).with_name("native_retention.py")
)
retention = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(retention)


class NativeRetentionTests(unittest.TestCase):
    NOW = 1_790_560_000
    OLD = NOW - 60 * 86400

    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.home = Path(self.scratch.name).resolve()
        self.root = self.home / ".codex"
        self.root.mkdir()
        self.database = self.root / "state_5.sqlite"
        with sqlite3.connect(self.database) as connection:
            connection.execute(
                "CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, "
                "updated_at INTEGER, recency_at INTEGER, is_pinned INTEGER)"
            )
            connection.execute(
                "CREATE TABLE thread_spawn_edges (parent_thread_id TEXT, child_thread_id TEXT)"
            )
        self.inventory = {"sources": [{"app": "codex", "path": str(self.database), "sqlite": True}]}

    def add_thread(self, name="old", *, pinned=0, updated=None, recency=None, event=None, fork=None):
        path = self.root / "sessions" / (name + ".jsonl")
        path.parent.mkdir(exist_ok=True)
        timestamp = self.OLD if event is None else event
        payload = {"id": name}
        if fork:
            payload["forked_from_id"] = fork
        path.write_text(json.dumps({"timestamp": timestamp, "type": "session_meta", "payload": payload}) + "\n")
        os.utime(path, (self.OLD, self.OLD))
        with sqlite3.connect(self.database) as connection:
            connection.execute("INSERT INTO threads VALUES (?, ?, ?, ?, ?)", (
                name, str(path), self.OLD if updated is None else updated,
                self.OLD if recency is None else recency, pinned,
            ))
        return path

    def plan(self, days=30):
        with patch.object(retention.time, "time", return_value=self.NOW):
            return retention.retention_plan(self.home, self.inventory, days)

    def test_old_session_is_only_a_blocked_age_candidate(self):
        self.add_thread()
        result = self.plan()
        self.assertEqual(result["counts"], {"candidate_blocked": 1})
        self.assertFalse(result["native_apply_supported"])
        self.assertEqual(result["deletion_count"], 0)
        self.assertIn("running_state_unknown", result["entries"][0]["blockers"])
        self.assertIn("cross_home_fork_references_unknown", result["entries"][0]["blockers"])

    def test_activity_uses_latest_event_not_creation_or_database_only(self):
        path = self.add_thread()
        with path.open("a") as stream:
            stream.write(json.dumps({"timestamp": self.NOW - 100, "type": "event_msg"}) + "\n")
        os.utime(path, (self.OLD, self.OLD))
        entry = self.plan()["entries"][0]
        self.assertEqual(entry["status"], "retained_recent")
        self.assertEqual(entry["last_activity_epoch"], self.NOW - 100)

    def test_recent_metadata_or_file_mtime_keeps_old_event(self):
        self.add_thread("recency", recency=self.NOW - 10)
        path = self.add_thread("mtime")
        os.utime(path, (self.NOW - 5, self.NOW - 5))
        entries = {entry["id"]: entry for entry in self.plan()["entries"]}
        self.assertEqual(entries["recency"]["status"], "retained_recent")
        self.assertEqual(entries["mtime"]["status"], "retained_recent")

    def test_committed_wal_activity_is_read_without_checkpointing(self):
        self.add_thread()
        connection = sqlite3.connect(self.database)
        self.addCleanup(connection.close)
        connection.execute("PRAGMA journal_mode=WAL")
        connection.execute("UPDATE threads SET updated_at = ?", (self.NOW - 2,))
        connection.commit()
        wal = self.database.with_name(self.database.name + "-wal")
        before = wal.read_bytes()
        entry = self.plan()["entries"][0]
        self.assertEqual(entry["status"], "retained_recent")
        self.assertEqual(entry["last_activity_epoch"], self.NOW - 2)
        self.assertEqual(before, wal.read_bytes())

    def test_exact_cutoff_is_retained(self):
        self.add_thread(event=self.NOW - 30 * 86400)
        self.assertEqual(self.plan()["entries"][0]["status"], "retained_recent")

    def test_pins_and_both_fork_endpoints_are_protected(self):
        self.add_thread("pin", pinned=1)
        self.add_thread("parent")
        self.add_thread("child")
        with sqlite3.connect(self.database) as connection:
            connection.execute("INSERT INTO thread_spawn_edges VALUES ('parent', 'child')")
        entries = {entry["id"]: entry for entry in self.plan()["entries"]}
        self.assertIn("pinned", entries["pin"]["reasons"])
        for name in ("parent", "child"):
            self.assertEqual(entries[name]["status"], "retained_protected")
            self.assertIn("fork_dependency", entries[name]["reasons"])

    def test_rollout_fork_metadata_preserves_parent_and_child(self):
        self.add_thread("parent")
        self.add_thread("child", fork="parent")
        self.assertTrue(all(entry["status"] == "retained_protected" for entry in self.plan()["entries"]))

    def test_automation_link_retains_thread(self):
        self.add_thread()
        config = self.root / "automations" / "monitor" / "automation.toml"
        config.parent.mkdir(parents=True)
        config.write_text('target_thread_id = "old"\n')
        entry = self.plan()["entries"][0]
        self.assertEqual(entry["status"], "retained_protected")
        self.assertIn("automation_linked", entry["reasons"])

    def test_malformed_rollout_and_missing_metadata_block_age_candidate(self):
        path = self.add_thread()
        path.write_text('{"timestamp":\n')
        os.utime(path, (self.OLD, self.OLD))
        with sqlite3.connect(self.database) as connection:
            connection.execute("UPDATE threads SET recency_at = NULL")
        entry = self.plan()["entries"][0]
        self.assertEqual(entry["status"], "blocked")
        self.assertIn("malformed_rollout", entry["blockers"])
        self.assertIn("last_activity_unknown", entry["blockers"])

    def test_symlinked_rollout_is_never_a_candidate(self):
        path = self.add_thread()
        saved = path.with_name("saved.jsonl")
        path.rename(saved)
        path.symlink_to(saved)
        entry = self.plan()["entries"][0]
        self.assertEqual(entry["status"], "blocked")
        self.assertIn("unsafe_or_unknown_rollout_path", entry["blockers"])

    def test_missing_fork_schema_fails_closed(self):
        self.add_thread()
        with sqlite3.connect(self.database) as connection:
            connection.execute("DROP TABLE thread_spawn_edges")
        entry = self.plan()["entries"][0]
        self.assertEqual(entry["status"], "blocked")
        self.assertIn("fork_dependency_inventory_unknown", entry["blockers"])

    def test_unknown_state_schema_and_cursor_report_unsupported(self):
        self.inventory["sources"].append({"app": "cursor", "path": str(self.home / "cursor.db"), "sqlite": True})
        with sqlite3.connect(self.database) as connection:
            connection.execute("DROP TABLE threads")
        result = self.plan()
        self.assertEqual(result["entries"], [])
        self.assertTrue(any(item.get("reason") == "state_unreadable_or_schema_unsupported" for item in result["coverage"]))
        self.assertTrue(any(item["app"] == "cursor" and item["status"] == "unsupported" for item in result["coverage"]))

    def test_apply_cannot_delete_even_with_approval_and_manifest(self):
        path = self.add_thread()
        before = {p: p.read_bytes() for p in (path, self.database)}
        result = retention.retention_apply(self.home, self.plan(), approved=True, archive_manifest={"verified": True})
        self.assertEqual(result["status"], "blocked")
        self.assertEqual(result["deleted"], [])
        self.assertEqual(before, {p: p.read_bytes() for p in before})

    def test_timestamp_formats_and_invalid_days(self):
        self.assertEqual(retention._retention_epoch(self.NOW * 1000), self.NOW)
        self.assertEqual(retention._retention_epoch("2026-01-01T00:00:00Z"), 1_767_225_600)
        for stamp in (True, float("nan"), float("inf"), "2026-01-01T00:00:00"):
            self.assertIsNone(retention._retention_epoch(stamp))
        for days in (0, -1, True, 30.0):
            with self.assertRaises(ValueError):
                self.plan(days)


if __name__ == "__main__":
    unittest.main()
