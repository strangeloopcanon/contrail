"""Incremental native archive fixtures; never read real app history or cloud data."""
import contextlib
import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

import native_history as history


class NativeIncrementalTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.home = self.root / "home"
        self.home.mkdir()
        self.output = self.root / "backups"

    def source(self, relative, content=b"fixture", sqlite=False):
        path = self.home / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        if not sqlite:
            path.write_bytes(content)
        return {"path": str(path), "relative": relative, "app": "fixture", "sqlite": sqlite}

    def backup(self, sources, base=None, home=None, scope="all installed default-profile apps"):
        options = {"base_run": base} if base is not None else {}
        result = history.backup(
            home or self.home, self.output,
            inventory={"sources": list(sources), "scope": scope, "unsupported": [], "exclusions": []},
            scratch_bytes=1024 * 1024, output_bytes=2 * 1024 * 1024, **options,
        )
        return Path(result["run"])

    def manifest(self, run):
        return json.loads((run / "manifest.json").read_text())

    def names(self, run, field="files"):
        return {record["name"] for record in self.manifest(run)[field]}

    def test_unchanged_files_are_inherited_and_base_manifest_is_bound(self):
        source = self.source(".codex/sessions/one.jsonl", b"unchanged")
        base = self.backup([source])
        delta = self.backup([source], base)
        manifest = self.manifest(delta)
        self.assertEqual(manifest["files"], [])
        self.assertEqual(self.names(delta, "catalog"), {source["relative"]})
        self.assertEqual(manifest["catalog"][0]["origin_run_id"], base.name)
        self.assertEqual(manifest["base"], {
            "run_id": base.name,
            "manifest_sha256": hashlib.sha256((base / "manifest.json").read_bytes()).hexdigest(),
        })
        self.assertEqual(history.verify(delta)["files"], 1)
        destination = self.root / "restored"
        history.extract(delta, destination)
        self.assertEqual((destination / source["relative"]).read_bytes(), b"unchanged")

    def test_only_changed_and_new_files_are_stored_even_with_same_size(self):
        unchanged = self.source(".claude/projects/p/keep.jsonl", b"keep")
        changed = self.source(".codex/sessions/change.jsonl", b"before")
        base = self.backup([unchanged, changed])
        Path(changed["path"]).write_bytes(b"after!")
        new = self.source(".cursor/chats/new.json", b"new")
        delta = self.backup([unchanged, changed, new], base)
        self.assertEqual(self.names(delta), {changed["relative"], new["relative"]})
        self.assertEqual(self.names(delta, "catalog"), {
            unchanged["relative"], changed["relative"], new["relative"],
        })
        origins = {record["name"]: record["origin_run_id"] for record in self.manifest(delta)["catalog"]}
        self.assertEqual(origins[unchanged["relative"]], base.name)
        self.assertEqual(origins[changed["relative"]], delta.name)
        destination = self.root / "restored"
        history.extract(delta, destination)
        for source, expected in ((unchanged, b"keep"), (changed, b"after!"), (new, b"new")):
            self.assertEqual((destination / source["relative"]).read_bytes(), expected)

    def test_deleted_live_files_remain_recoverable(self):
        deleted = self.source(".codex/sessions/deleted.jsonl", b"preserve me")
        base = self.backup([deleted])
        Path(deleted["path"]).unlink()
        delta = self.backup([], base)
        self.assertEqual(self.manifest(delta)["files"], [])
        self.assertEqual(self.names(delta, "catalog"), {deleted["relative"]})
        destination = self.root / "restored"
        history.extract(delta, destination)
        self.assertEqual((destination / deleted["relative"]).read_bytes(), b"preserve me")
        self.assertFalse(Path(deleted["path"]).exists())

    def test_narrower_overlapping_source_list_retains_baseline_and_skips_unchanged(self):
        # The dates describe an externally selected source list. Incrementals
        # compare complete files, independently of how that list was selected.
        older = self.source(".codex/sessions/older.jsonl", b"older conversation")
        deleted = self.source(".claude/projects/p/deleted.jsonl", b"deleted conversation")
        recent = self.source(".codex/sessions/recent.jsonl", b"recent conversation")
        base = self.backup([older, deleted, recent])  # 60-day baseline list.
        Path(deleted["path"]).unlink()
        new = self.source(".codex/sessions/new.jsonl", b"new conversation")
        delta = self.backup([recent, new], base)  # Overlapping 7-day list.

        self.assertEqual(self.names(delta), {new["relative"]})
        self.assertEqual(self.names(delta, "catalog"), {
            older["relative"], deleted["relative"], recent["relative"], new["relative"],
        })
        self.assertEqual(self.manifest(delta)["comparison"], {
            "new_files": 1, "changed_files": 0, "unchanged_files": 1,
            "missing_local_preserved": 2,
        })
        destination = self.root / "restored"
        history.extract(delta, destination)
        for source, expected in (
            (older, b"older conversation"), (deleted, b"deleted conversation"),
            (recent, b"recent conversation"), (new, b"new conversation"),
        ):
            self.assertEqual((destination / source["relative"]).read_bytes(), expected)
        self.assertTrue(Path(older["path"]).exists())
        self.assertFalse(Path(deleted["path"]).exists())

    def test_growing_transcript_is_replaced_whole_and_base_version_stays_immutable(self):
        first = b'{"message":"first"}\n'
        appended = first + b'{"message":"second"}\n'
        source = self.source(".codex/sessions/growing.jsonl", first)
        base = self.backup([source])
        base_manifest = (base / "manifest.json").read_bytes()
        base_parts = {part["name"]: (base / part["name"]).read_bytes()
                      for part in self.manifest(base)["parts"]}
        Path(source["path"]).write_bytes(appended)
        delta = self.backup([source], base)

        self.assertEqual(self.names(delta), {source["relative"]})
        self.assertEqual(self.manifest(delta)["files"][0]["bytes"], len(appended))
        self.assertEqual(self.manifest(delta)["comparison"], {
            "new_files": 0, "changed_files": 1, "unchanged_files": 0,
            "missing_local_preserved": 0,
        })
        self.assertEqual((base / "manifest.json").read_bytes(), base_manifest)
        for name, content in base_parts.items():
            self.assertEqual((base / name).read_bytes(), content)
        previous = self.root / "previous"
        latest = self.root / "latest"
        history.extract(base, previous)
        history.extract(delta, latest)
        self.assertEqual((previous / source["relative"]).read_bytes(), first)
        self.assertEqual((latest / source["relative"]).read_bytes(), appended)
        repeated = self.backup([source], delta)
        self.assertEqual(self.manifest(repeated)["files"], [])
        self.assertEqual(self.manifest(repeated)["catalog"][0]["origin_run_id"], delta.name)

    def test_changed_sqlite_replaces_snapshot_and_preserves_integrity(self):
        source = self.source(".codex/state_5.sqlite", sqlite=True)
        with contextlib.closing(sqlite3.connect(source["path"])) as db:
            db.execute("CREATE TABLE messages (body TEXT)")
            db.execute("INSERT INTO messages VALUES ('first')")
            db.commit()
            base = self.backup([source])
            db.execute("INSERT INTO messages VALUES ('second')")
            db.commit()
            delta = self.backup([source], base)
        self.assertEqual(self.names(delta), {source["relative"]})
        self.assertEqual(history.verify(delta)["sqlite_integrity"], "ok")
        destination = self.root / "restored"
        history.extract(delta, destination)
        with contextlib.closing(sqlite3.connect(destination / source["relative"])) as db:
            self.assertEqual(db.execute("SELECT body FROM messages ORDER BY rowid").fetchall(), [
                ("first",), ("second",),
            ])
            self.assertEqual(db.execute("PRAGMA integrity_check").fetchall(), [("ok",)])

    def test_chained_deltas_restore_latest_content_and_retained_old_files(self):
        changed = self.source(".codex/sessions/change.jsonl", b"first")
        retained = self.source(".claude/projects/p/old.jsonl", b"old")
        base = self.backup([changed, retained])
        Path(changed["path"]).write_bytes(b"second")
        Path(retained["path"]).unlink()
        middle = self.backup([changed], base)
        Path(changed["path"]).write_bytes(b"third")
        new = self.source(".cursor/chats/new.json", b"new")
        latest = self.backup([changed, new], middle)
        self.assertEqual(history.verify(latest)["files"], 3)
        destination = self.root / "restored"
        history.extract(latest, destination)
        for source, expected in ((changed, b"third"), (retained, b"old"), (new, b"new")):
            self.assertEqual((destination / source["relative"]).read_bytes(), expected)
        with self.assertRaisesRegex(ValueError, "must not exist"):
            history.extract(latest, destination)

    def test_parent_manifest_tampering_rejects_verification_and_extraction(self):
        source = self.source(".codex/sessions/one.jsonl")
        base = self.backup([source])
        delta = self.backup([source], base)
        manifest = self.manifest(base)
        manifest["boundary"] = "tampered fixture"
        (base / "manifest.json").write_text(json.dumps(manifest))
        with self.assertRaises(ValueError):
            history.verify(delta)
        destination = self.root / "restored"
        with self.assertRaises(ValueError):
            history.extract(delta, destination)
        self.assertFalse(destination.exists())

    def test_missing_parent_rejects_verification_and_extraction(self):
        source = self.source(".codex/sessions/one.jsonl")
        base = self.backup([source])
        delta = self.backup([source], base)
        base.rename(self.root / "unavailable-parent")
        with self.assertRaises((ValueError, FileNotFoundError)):
            history.verify(delta)
        destination = self.root / "restored"
        with self.assertRaises((ValueError, FileNotFoundError)):
            history.extract(delta, destination)
        self.assertFalse(destination.exists())

    def test_incompatible_home_or_scope_rejects_incremental_creation(self):
        source = self.source(".codex/sessions/one.jsonl")
        base = self.backup([source])
        other_home = self.root / "other-home"
        other_home.mkdir()
        with self.assertRaises(ValueError):
            self.backup([], base, home=other_home)
        with self.assertRaises(ValueError):
            self.backup([source], base, scope=["codex"])

    def test_corrupt_parent_part_rejects_verification_and_extraction(self):
        source = self.source(".codex/sessions/one.jsonl")
        base = self.backup([source])
        delta = self.backup([source], base)
        part = base / self.manifest(base)["parts"][0]["name"]
        data = bytearray(part.read_bytes())
        data[len(data) // 2] ^= 1
        part.write_bytes(data)
        with self.assertRaises(ValueError):
            history.verify(delta)
        destination = self.root / "restored"
        with self.assertRaises(ValueError):
            history.extract(delta, destination)
        self.assertFalse(destination.exists())

    def test_creation_can_use_checksum_verified_cloud_base_after_local_release(self):
        source = self.source(".codex/sessions/one.jsonl")
        base = self.backup([source])
        handoff = history.handoff(base, cloud_parent_id="fixture-parent",
                                  cloud_owner="owner@example.invalid")
        receipt = {
            "run_id": handoff["run_id"], "parent_folder_id": handoff["parent_folder_id"],
            "owner": handoff["owner"], "private": True, "folder_id": "fixture-folder",
            "files": [
                {"name": item["name"], "id": "fixture-" + str(index),
                 "parent_id": "fixture-folder", "bytes": item["bytes"],
                 "private": True, "sha256": item["sha256"]}
                for index, item in enumerate(handoff["files"])
            ],
        }
        receipt_path = self.root / "remote-receipt.json"
        receipt_path.write_text(json.dumps(receipt))
        self.assertEqual(history.record_cloud(base, receipt_path)["verification_level"], "sha256")
        release = history.release_local(base)
        self.assertEqual(release["verification_level"], "sha256")
        self.assertTrue(all(not (base / part["name"]).exists()
                            for part in self.manifest(base)["parts"]))
        delta = self.backup([source], base)
        self.assertEqual(self.manifest(delta)["files"], [])
        self.assertEqual(self.names(delta, "catalog"), {source["relative"]})
        with self.assertRaises((ValueError, FileNotFoundError)):
            history.verify(delta)


if __name__ == "__main__":
    unittest.main()
