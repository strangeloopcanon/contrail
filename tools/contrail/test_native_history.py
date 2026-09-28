"""Bounded archive fixtures; no real app history or remote service access."""
import contextlib
import gzip
import hashlib
import io
import json
from pathlib import Path
import sqlite3
import tarfile
import tempfile
import unittest
from unittest import mock

import native_history as history


class NativeHistoryTests(unittest.TestCase):
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

    def backup(self, sources=(), **kwargs):
        kwargs.setdefault("cloud_parent_id", "fixture-parent")
        kwargs.setdefault("cloud_owner", "owner@example.invalid")
        result = history.backup(
            self.home, self.output,
            inventory={"sources": list(sources), "unsupported": [], "exclusions": []},
            scratch_bytes=1024 * 1024, output_bytes=2 * 1024 * 1024, **kwargs,
        )
        return Path(result["run"])

    def manifest(self, run):
        return json.loads((run / "manifest.json").read_text())

    def parts(self, run):
        return [run / part["name"] for part in self.manifest(run)["parts"]]

    def cloud_receipt(self, run):
        handoff = json.loads((run / "cloud-handoff.json").read_text())
        return {
            "run_id": handoff["run_id"], "parent_folder_id": handoff["parent_folder_id"],
            "owner": handoff["owner"], "private": True, "folder_id": "fixture-folder",
            "files": [
                {"name": item["name"], "id": "fixture-" + str(index),
                 "parent_id": "fixture-folder", "bytes": item["bytes"],
                 "private": True, "sha256": item["sha256"]}
                for index, item in enumerate(handoff["files"])
            ],
        }

    def record(self, run, receipt):
        path = self.root / "remote-receipt.json"
        path.write_text(json.dumps(receipt))
        return history.record_cloud(run, path)

    def size_only_receipt(self, run):
        receipt = self.cloud_receipt(run)
        for item in receipt["files"]:
            item.pop("sha256")
        return receipt

    def archive_fixture(self, members, records=None, truncate=0):
        """Write internally checksummed hostile archives to exercise content checks."""
        run = self.root / "hostile"
        run.mkdir()
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode="w") as archive:
            for name, content, kind in members:
                info = tarfile.TarInfo(name)
                info.type = kind
                if kind == tarfile.REGTYPE:
                    info.size = len(content)
                    archive.addfile(info, io.BytesIO(content))
                else:
                    info.linkname = "../../outside"
                    archive.addfile(info)
        data = gzip.compress(raw.getvalue(), mtime=0)
        if truncate:
            data = data[:-truncate]
        name = "history.tar.gz.part00001"
        (run / name).write_bytes(data)
        if records is None:
            records = [{"name": name, "bytes": len(content), "sqlite": False,
                        "sha256": hashlib.sha256(content).hexdigest()}
                       for name, content, _ in members]
        manifest = {
            "format": "contrail-native-history-v1", "files": records,
            "parts": [{"name": name, "bytes": len(data),
                       "sha256": hashlib.sha256(data).hexdigest()}],
            "archive_bytes": len(data), "archive_sha256": hashlib.sha256(data).hexdigest(),
        }
        (run / "manifest.json").write_text(json.dumps(manifest))
        return run

    def test_roundtrip_preserves_native_bytes_and_never_overwrites(self):
        contents = {
            ".codex/sessions/session.jsonl": b'{"message":"fixture"}\n\x00\xff',
            ".claude/projects/project/media.bin": bytes(range(256)),
            ".cursor/chats/workspace/session/meta.json": b"",
        }
        sources = [self.source(name, data) for name, data in contents.items()]
        run = self.backup(sources)
        destination = self.root / "restored"
        receipt = history.extract(run, destination)
        self.assertEqual(receipt["files"], len(contents))
        for name, data in contents.items():
            self.assertEqual((destination / name).read_bytes(), data)
            self.assertEqual((self.home / name).read_bytes(), data)
        with self.assertRaisesRegex(ValueError, "must not exist"):
            history.extract(run, destination)

    def test_online_sqlite_backup_includes_committed_wal_without_changing_source(self):
        source = self.source(".codex/state_5.sqlite", sqlite=True)
        path = Path(source["path"])
        with contextlib.closing(sqlite3.connect(path)) as live:
            self.assertEqual(live.execute("PRAGMA journal_mode=WAL").fetchone(), ("wal",))
            live.execute("PRAGMA wal_autocheckpoint=0")
            live.execute("CREATE TABLE messages (body TEXT)")
            live.execute("INSERT INTO messages VALUES ('committed fixture')")
            live.commit()
            wal = Path(str(path) + "-wal")
            self.assertGreater(wal.stat().st_size, 0)
            before = {item: item.read_bytes() for item in (path, wal)}
            run = self.backup([source])
            restored = self.root / "restored"
            history.extract(run, restored)
            self.assertEqual({item: item.read_bytes() for item in before}, before)
            with contextlib.closing(sqlite3.connect(restored / source["relative"])) as db:
                self.assertEqual(db.execute("SELECT body FROM messages").fetchall(), [("committed fixture",)])
                self.assertEqual(db.execute("PRAGMA integrity_check").fetchall(), [("ok",)])

    def test_desktop_wal_snapshot_is_projected_without_touching_live_auth(self):
        source = self.source("Library/Application Support/Cursor/User/globalStorage/state.vscdb", sqlite=True)
        source["app"] = "cursor-desktop"
        path = Path(source["path"])
        marker = "PRIVATE_AUTH_FIXTURE_NOT_HISTORY"
        with contextlib.closing(sqlite3.connect(path)) as live:
            live.execute("PRAGMA journal_mode=WAL")
            live.execute("PRAGMA wal_autocheckpoint=0")
            live.execute("CREATE TABLE ItemTable(key TEXT,value TEXT)")
            live.executemany("INSERT INTO ItemTable VALUES (?,?)", [
                ("composer.composerData", "history fixture"), ("cursorAuth/accessToken", marker),
            ])
            live.commit()
            wal = Path(str(path) + "-wal")
            before = {item: item.read_bytes() for item in (path, wal)}
            run = self.backup([source])
            restored = self.root / "restored"
            history.extract(run, restored)
            restored_db = restored / source["relative"]
            self.assertNotIn(marker.encode(), restored_db.read_bytes())
            with contextlib.closing(sqlite3.connect(restored_db)) as db:
                self.assertEqual(db.execute("SELECT key,value FROM ItemTable").fetchall(), [
                    ("composer.composerData", "history fixture"),
                ])
            self.assertEqual({item: item.read_bytes() for item in before}, before)
            self.assertEqual(live.execute("SELECT value FROM ItemTable WHERE key='cursorAuth/accessToken'").fetchone(), (marker,))

    def test_archive_is_deterministic_sorted_and_bounded_in_numbered_parts(self):
        sources = [self.source("z.bin", bytes(range(256)) * 8), self.source("a.txt", b"first")]
        first = self.backup(sources, part_bytes=127)
        second = self.backup(list(reversed(sources)), part_bytes=127)
        left, right = self.manifest(first), self.manifest(second)
        self.assertEqual(left["archive_sha256"], right["archive_sha256"])
        self.assertEqual(left["parts"], right["parts"])
        self.assertEqual([item["name"] for item in left["files"]], ["a.txt", "z.bin"])
        self.assertGreater(len(left["parts"]), 1)
        for index, part in enumerate(left["parts"], 1):
            self.assertEqual(part["name"], "history.tar.gz.part%05d" % index)
            self.assertLessEqual(part["bytes"], 127)
            self.assertLessEqual(part["bytes"], 80 * 1024 * 1024)
            self.assertEqual(part["sha256"], history.digest_file(first / part["name"]))
        with self.assertRaisesRegex(ValueError, "80 MiB"):
            self.backup(sources, part_bytes=history.PART_LIMIT + 1)
        left["parts"].reverse()
        (first / "manifest.json").write_text(json.dumps(left))
        with self.assertRaisesRegex(ValueError, "order"):
            history.verify(first)

    def test_absent_apps_have_a_valid_empty_backup(self):
        run = self.backup()
        self.assertEqual(history.verify(run)["files"], 0)
        self.assertEqual(self.manifest(run)["files"], [])
        history.extract(run, self.root / "restored")
        self.assertEqual(list((self.root / "restored").iterdir()), [])

    def test_corrupted_part_fails_without_creating_restore(self):
        run = self.backup([self.source("session.jsonl")])
        part = self.parts(run)[0]
        data = bytearray(part.read_bytes())
        data[len(data) // 2] ^= 1
        part.write_bytes(data)
        destination = self.root / "restored"
        with self.assertRaisesRegex(ValueError, "checksum"):
            history.extract(run, destination)
        self.assertFalse(destination.exists())

    def test_truncated_gzip_footer_is_rejected_even_with_matching_part_hashes(self):
        run = self.archive_fixture([("safe.txt", b"fixture", tarfile.REGTYPE)], truncate=4)
        with self.assertRaises((EOFError, OSError, tarfile.TarError, ValueError)):
            history.verify(run)

    def test_manifest_and_tar_path_traversal_are_rejected(self):
        for name in ("../outside", "/absolute", "dir/../outside", "dir\\outside", "dir//file"):
            with self.subTest(name=name):
                with self.assertRaisesRegex(ValueError, "unsafe"):
                    history.safe_name(name)
        records = [{"name": "safe.txt", "bytes": 7, "sqlite": False,
                    "sha256": hashlib.sha256(b"fixture").hexdigest()}]
        run = self.archive_fixture([("../outside", b"fixture", tarfile.REGTYPE)], records)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            history.extract(run, self.root / "restored")
        self.assertFalse((self.root / "outside").exists())
        self.assertFalse((self.root / "restored").exists())

    def test_duplicate_and_link_members_are_rejected(self):
        for kind in (tarfile.REGTYPE, tarfile.SYMTYPE, tarfile.LNKTYPE):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory(dir=self.root) as directory:
                old_root, self.root = self.root, Path(directory)
                try:
                    members = [("safe.txt", b"fixture", kind)]
                    if kind == tarfile.REGTYPE:
                        members.append(members[0])
                    record = {"name": "safe.txt", "bytes": 7, "sqlite": False,
                              "sha256": hashlib.sha256(b"fixture").hexdigest()}
                    run = self.archive_fixture(members, [record])
                    with self.assertRaisesRegex(ValueError, "duplicate|non-regular"):
                        history.verify(run)
                finally:
                    self.root = old_root

    def test_duplicate_manifest_members_are_rejected(self):
        run = self.archive_fixture([("safe.txt", b"fixture", tarfile.REGTYPE)])
        manifest = self.manifest(run)
        manifest["files"].append(manifest["files"][0])
        (run / "manifest.json").write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            history.verify(run)

    def test_symlink_source_part_and_extract_parent_are_rejected(self):
        source = self.source("safe.txt")
        alias = self.home / "alias.txt"
        alias.symlink_to(source["path"])
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.backup([{**source, "path": str(alias), "relative": "alias.txt"}])
        run = self.backup([source])
        parent = self.root / "alias-parent"
        parent.symlink_to(self.home, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "symlink"):
            history.extract(run, parent / "restored")
        part = self.parts(run)[0]
        original = part.with_suffix(".saved")
        part.rename(original)
        part.symlink_to(original)
        with self.assertRaisesRegex(ValueError, "symlink"):
            history.verify(run)

    def test_symlink_sqlite_sidecars_are_rejected_without_reading_target(self):
        source = self.source("state.sqlite", sqlite=True)
        path = Path(source["path"])
        with contextlib.closing(sqlite3.connect(path)) as db:
            db.execute("CREATE TABLE fixture (value TEXT)")
        target = self.root / "unrelated"
        target.write_bytes(b"private fixture bytes")
        for suffix in ("-wal", "-shm", "-journal"):
            with self.subTest(suffix=suffix):
                sidecar = Path(str(path) + suffix)
                sidecar.symlink_to(target)
                try:
                    with self.assertRaisesRegex(ValueError, "symlink SQLite sidecar"):
                        self.backup([source])
                finally:
                    sidecar.unlink()
                self.assertEqual(target.read_bytes(), b"private fixture bytes")
                self.assertEqual(list(self.output.iterdir()), [])

    def test_changed_source_retries_and_keeps_only_stable_bytes(self):
        source = self.source("session.jsonl", b"old")
        path = Path(source["path"])
        original = history.fingerprint
        calls = []
        def change_after_first_copy(candidate):
            calls.append(candidate)
            if len(calls) == 3:
                path.write_bytes(b"stable replacement")
            return original(candidate)
        destination = self.root / "snapshot"
        with mock.patch.object(history, "fingerprint", side_effect=change_after_first_copy):
            history.snapshot_file(source, destination, 1024)
        self.assertEqual(destination.read_bytes(), b"stable replacement")
        self.assertEqual(len(calls), 6)

    def test_changing_source_exhausts_three_attempts_and_cleans_run(self):
        source = self.source("session.jsonl", b"old")
        original = history.fingerprint
        count = [0]
        def unstable(candidate):
            count[0] += 1
            value = original(candidate)
            return (*value[:-1], count[0])
        with mock.patch.object(history, "fingerprint", side_effect=unstable):
            with self.assertRaisesRegex(ValueError, "three snapshot attempts"):
                self.backup([source])
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertEqual(Path(source["path"]).read_bytes(), b"old")

    def test_interrupted_backup_cleans_partial_run_and_can_retry(self):
        source = self.source("session.jsonl")
        with mock.patch.object(history, "snapshot_file", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                self.backup([source])
        self.assertEqual(list(self.output.iterdir()), [])
        self.assertEqual(history.verify(self.backup([source]))["files"], 1)

    def test_budget_failures_leave_sources_intact_and_no_partial_run(self):
        source = self.source("session.jsonl", bytes(range(256)))
        for budget, limit in (("scratch_bytes", 16), ("output_bytes", 16)):
            with self.subTest(budget=budget):
                options = {"scratch_bytes": 1024 * 1024, "output_bytes": 1024 * 1024}
                options[budget] = limit
                with self.assertRaisesRegex(ValueError, "limit|budget"):
                    history.backup(self.home, self.output, inventory={"sources": [source]}, **options)
                self.assertEqual(list(self.output.iterdir()), [])
                self.assertEqual(Path(source["path"]).read_bytes(), bytes(range(256)))

    def test_lock_and_retry_preserve_completed_backup(self):
        source = self.source("session.jsonl")
        run = self.backup([source])
        before = {part.name: part.read_bytes() for part in self.parts(run)}
        lock = self.output / ".native-history.lock"
        lock.write_text("fixture active writer")
        with self.assertRaises(FileExistsError):
            self.backup([source])
        self.assertEqual(lock.read_text(), "fixture active writer")
        lock.unlink()
        next_run = self.backup([source])
        self.assertNotEqual(run, next_run)
        self.assertEqual({part.name: part.read_bytes() for part in self.parts(run)}, before)
        self.assertEqual(history.verify(run)["files"], 1)
        self.assertEqual(history.handoff(run), history.handoff(run))
        self.assertEqual(len([path for path in self.output.iterdir() if path.is_dir()]), 2)

    def test_cloud_receipt_mismatches_cannot_release_parts(self):
        run = self.backup([self.source("session.jsonl")])
        valid = self.cloud_receipt(run)
        for field, value in (("run_id", "other-run"), ("owner", "other-owner"),
                             ("parent_folder_id", "other-parent"), ("private", False)):
            with self.subTest(field=field):
                receipt = {**valid, field: value}
                with self.assertRaises(ValueError):
                    self.record(run, receipt)
                self.assertTrue(all(part.exists() for part in self.parts(run)))
        receipt = self.cloud_receipt(run)
        receipt["files"][0]["bytes"] += 1
        with self.assertRaisesRegex(ValueError, "metadata"):
            self.record(run, receipt)
        self.assertFalse((run / "cloud-receipt.json").exists())

    def test_local_backup_needs_no_cloud_configuration(self):
        with mock.patch.dict(history.os.environ, {}, clear=True):
            run = self.backup([self.source("session.jsonl")], cloud_parent_id=None, cloud_owner=None)
            self.assertNotIn("cloud_destination", self.manifest(run))
            self.assertFalse((run / "cloud-handoff.json").exists())
            self.assertEqual(history.verify(run)["files"], 1)
            with self.assertRaisesRegex(ValueError, "configure.*cloud-parent-id"):
                history.handoff(run)
            handoff = history.handoff(run, cloud_parent_id="other-fixture-parent",
                                      cloud_owner="archive-owner@example.invalid")
        self.assertEqual(handoff["parent_folder_id"], "other-fixture-parent")
        self.assertEqual(handoff["owner"], "archive-owner@example.invalid")
        self.record(run, self.cloud_receipt(run))
        self.assertEqual(history.release_local(run)["verification_level"], "sha256")

    def test_cloud_configuration_environment_and_pair_validation(self):
        with mock.patch.dict(history.os.environ, {"CONTRAIL_HISTORY_CLOUD_PARENT": "environment-parent",
                                                  "CONTRAIL_HISTORY_CLOUD_OWNER": "environment@example.invalid"}, clear=True):
            run = self.backup(cloud_parent_id=None, cloud_owner=None)
            self.assertEqual(self.manifest(run)["cloud_destination"],
                             {"parent_id": "environment-parent", "owner": "environment@example.invalid"})
        with mock.patch.dict(history.os.environ, {}, clear=True):
            with self.assertRaisesRegex(ValueError, "both be configured"):
                self.backup(cloud_owner=None)

    def test_legacy_recorded_cloud_configuration_is_preserved(self):
        run = self.backup([self.source("session.jsonl")])
        manifest = self.manifest(run)
        manifest.pop("cloud_destination")
        history.write_json(run / "manifest.json", manifest)
        verification = history.verify(run)
        verification.pop("manifest_sha256")  # Original receipts did not bind this field.
        history.write_json(run / "verification.json", verification)
        with mock.patch.dict(history.os.environ, {}, clear=True):
            handoff = history.handoff(run)
        self.assertEqual(handoff["parent_folder_id"], "fixture-parent")
        self.assertEqual(handoff["owner"], "owner@example.invalid")
        self.record(run, self.cloud_receipt(run))
        self.assertEqual(history.release_local(run)["verification_level"], "sha256")

    def test_handoff_and_release_delta_with_released_parent(self):
        source = self.source("session.jsonl", b"base")
        base = self.backup([source])
        self.record(base, self.cloud_receipt(base))
        history.release_local(base)
        Path(source["path"]).write_bytes(b"delta")
        run = self.backup([source], base_run=base)
        self.assertEqual(history.handoff(run)["required_base_runs"], [base.name])
        self.record(run, self.cloud_receipt(run))
        self.assertEqual(history.release_local(run)["verification_level"], "sha256")
        self.assertFalse(any(part.exists() for part in self.parts(run)))
        self.assertEqual(Path(source["path"]).read_bytes(), b"delta")

    def test_incremental_backup_with_legacy_released_parent(self):
        source = self.source("session.jsonl", b"base")
        base = self.backup([source])
        manifest = self.manifest(base)
        manifest.pop("catalog")
        manifest.pop("backup_kind")
        manifest.pop("comparison")
        history.write_json(base / "manifest.json", manifest)
        verification = history.verify(base)
        verification.pop("manifest_sha256")
        history.write_json(base / "verification.json", verification)
        history.handoff(base)
        self.record(base, self.cloud_receipt(base))
        history.release_local(base)
        retained = {name: (base / name).read_bytes() for name in ("manifest.json", "verification.json", "cloud-handoff.json")}
        Path(source["path"]).write_bytes(b"delta")
        run = self.backup([source], base_run=base)
        self.assertEqual(history.handoff(run)["required_base_runs"], [base.name])
        self.record(run, self.cloud_receipt(run))
        self.assertEqual(history.release_local(run)["verification_level"], "sha256")
        self.assertEqual(retained, {name: (base / name).read_bytes() for name in retained})

    def test_released_legacy_base_requires_checksum_evidence_for_new_backup(self):
        source = self.source("session.jsonl", b"base")
        base = self.backup([source])
        verification = history.verify(base)
        verification.pop("manifest_sha256")
        history.write_json(base / "verification.json", verification)
        history.handoff(base)
        self.record(base, self.cloud_receipt(base))
        history.release_local(base)
        receipt_path = base / "cloud-receipt.json"
        receipt = json.loads(receipt_path.read_text())
        for item in receipt["files"]:
            item.pop("sha256")
        receipt_path.write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, "checksum verified"):
            self.backup([source], base_run=base)

    def test_released_parent_remote_evidence_revalidated_for_delta(self):
        source = self.source("session.jsonl", b"base")
        base = self.backup([source])
        self.record(base, self.cloud_receipt(base))
        history.release_local(base)
        Path(source["path"]).write_bytes(b"delta")
        run = self.backup([source], base_run=base)
        self.record(run, self.cloud_receipt(run))
        receipt_path = base / "cloud-receipt.json"
        original = json.loads(receipt_path.read_text())
        for checksum in (None, "wrong-checksum"):
            with self.subTest(checksum=checksum):
                receipt = json.loads(json.dumps(original))
                for item in receipt["files"]:
                    if checksum is None:
                        item.pop("sha256")
                    else:
                        item["sha256"] = checksum
                receipt_path.write_text(json.dumps(receipt))
                for operation in (history.handoff, history.release_local):
                    with self.assertRaisesRegex(ValueError, "checksum"):
                        operation(run)
                self.assertTrue(all(part.exists() for part in self.parts(run)))
        receipt_path.write_text(json.dumps(original))
        manifest_path = base / "manifest.json"
        manifest = self.manifest(base)
        manifest["parts"][0]["sha256"] = "wrong-manifest-checksum"
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "base manifest checksum mismatch"):
            history.release_local(run)
        self.assertTrue(all(part.exists() for part in self.parts(run)))

    def test_size_only_and_wrong_checksums_retain_local_parts(self):
        run = self.backup([self.source("session.jsonl")])
        for checksum in (None, "wrong-checksum"):
            with self.subTest(checksum=checksum):
                receipt = self.cloud_receipt(run)
                for item in receipt["files"]:
                    if checksum is None:
                        item.pop("sha256")
                    else:
                        item["sha256"] = checksum
                if checksum is None:
                    result = self.record(run, receipt)
                    self.assertEqual(result["verification_level"], "size-only")
                    with self.assertRaisesRegex(ValueError, "checksum verified"):
                        history.release_local(run)
                else:
                    with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                        self.record(run, receipt)
                self.assertTrue(all(part.exists() for part in self.parts(run)))
        self.assertFalse((run / "local-release.json").exists())

    def test_each_supplied_cloud_checksum_must_match_even_if_other_is_correct(self):
        run = self.backup([self.source("session.jsonl")])
        handoff = json.loads((run / "cloud-handoff.json").read_text())
        for algorithm in ("sha256", "md5"):
            with self.subTest(algorithm=algorithm):
                receipt = self.cloud_receipt(run)
                for remote, local in zip(receipt["files"], handoff["files"]):
                    remote["md5"] = local["md5"]
                receipt["files"][0][algorithm] = "wrong-checksum"
                with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    self.record(run, receipt)
                self.assertTrue(all(part.exists() for part in self.parts(run)))
        self.assertFalse((run / "cloud-receipt.json").exists())

    def test_size_only_release_requires_explicit_opt_in_and_records_bindings(self):
        source = self.source("session.jsonl")
        run = self.backup([source])
        self.record(run, self.size_only_receipt(run))
        with self.assertRaisesRegex(ValueError, "checksum verified"):
            history.release_local(run)
        self.assertTrue(all(part.exists() for part in self.parts(run)))
        with mock.patch.object(history, "verify", side_effect=AssertionError("release repeated archive extraction")):
            result = history.release_local(run, accept_size_only=True)
        self.assertEqual(result["verification_level"], "size-only")
        self.assertEqual(result["release_policy"], "provider-upload-integrity")
        self.assertTrue(result["accept_size_only"])
        self.assertEqual(result["bindings"], history.release_bindings(run))
        self.assertFalse(any(part.exists() for part in self.parts(run)))
        self.assertEqual(Path(source["path"]).read_bytes(), b"fixture")
        self.assertEqual(history.release_local(run), result)

    def test_incremental_continuation_from_authorized_size_only_released_base(self):
        source = self.source("session.jsonl", b"base")
        base = self.backup([source])
        self.record(base, self.size_only_receipt(base))
        history.release_local(base, accept_size_only=True)
        Path(source["path"]).write_bytes(b"delta")
        run = self.backup([source], base_run=base)
        self.assertEqual(history.handoff(run)["required_base_runs"], [base.name])
        self.record(run, self.cloud_receipt(run))
        with self.assertRaisesRegex(ValueError, "checksum verified"):
            history.release_local(run)
        self.assertTrue(all(part.exists() for part in self.parts(run)))
        result = history.release_local(run, accept_size_only=True)
        self.assertEqual(result["release_policy"], "provider-upload-integrity")
        self.assertFalse(any(part.exists() for part in self.parts(run)))

    def test_size_only_released_base_requires_unchanged_authorization_bindings(self):
        source = self.source("session.jsonl", b"base")
        base = self.backup([source])
        self.record(base, self.size_only_receipt(base))
        history.release_local(base, accept_size_only=True)
        Path(source["path"]).write_bytes(b"delta")
        run = self.backup([source], base_run=base)
        self.record(run, self.cloud_receipt(run))
        release_path = base / "local-release.json"
        original = json.loads(release_path.read_text())
        release = json.loads(json.dumps(original))
        release["bindings"]["manifest.json"] = "changed-binding"
        release_path.write_text(json.dumps(release))
        for operation in (lambda: self.backup([source], base_run=base),
                          lambda: history.handoff(run),
                          lambda: history.release_local(run, accept_size_only=True)):
            with self.assertRaisesRegex(ValueError, "authorization evidence changed"):
                operation()
        self.assertTrue(all(part.exists() for part in self.parts(run)))
        release_path.write_text(json.dumps(original))
        receipt_path = base / "cloud-receipt.json"
        receipt = json.loads(receipt_path.read_text())
        receipt["recorded_utc"] = "changed"
        receipt_path.write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, "authorization evidence changed"):
            history.handoff(run)
        self.assertTrue(all(part.exists() for part in self.parts(run)))

    def test_missing_base_parts_cannot_use_unapproved_size_only_receipt(self):
        source = self.source("session.jsonl", b"base")
        base = self.backup([source])
        self.record(base, self.size_only_receipt(base))
        for part in self.parts(base):
            part.unlink()
        with self.assertRaisesRegex(ValueError, "checksum verified"):
            self.backup([source], base_run=base)

    def test_interrupted_size_only_release_requires_same_explicit_policy(self):
        run = self.backup([self.source("session.jsonl", bytes(range(256)) * 4)], part_bytes=64)
        parts = self.parts(run)
        self.record(run, self.size_only_receipt(run))
        original = Path.unlink
        def interrupt_second_part(path, *args, **kwargs):
            if path == parts[1]:
                raise KeyboardInterrupt("fixture interruption")
            return original(path, *args, **kwargs)
        with mock.patch.object(Path, "unlink", interrupt_second_part):
            with self.assertRaises(KeyboardInterrupt):
                history.release_local(run, accept_size_only=True)
        self.assertFalse(parts[0].exists())
        with self.assertRaisesRegex(ValueError, "same explicit release policy"):
            history.release_local(run)
        self.assertTrue(parts[1].exists())
        result = history.release_local(run, accept_size_only=True)
        self.assertEqual(result["verification_level"], "size-only")
        self.assertFalse(any(part.exists() for part in parts))

    def test_release_recomputes_cloud_evidence_instead_of_trusting_stale_level(self):
        source = self.source("session.jsonl")
        run = self.backup([source])
        self.record(run, self.cloud_receipt(run))
        receipt_path = run / "cloud-receipt.json"
        receipt = json.loads(receipt_path.read_text())
        for item in receipt["files"]:
            item.pop("sha256")
        receipt_path.write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, "checksum verified"):
            history.release_local(run)
        self.assertTrue(all(part.exists() for part in self.parts(run)))
        self.assertEqual(Path(source["path"]).read_bytes(), b"fixture")

    def test_matching_but_stale_handoff_and_receipt_cannot_release_current_bytes(self):
        run = self.backup([self.source("session.jsonl")])
        receipt = self.cloud_receipt(run)
        handoff_path = run / "cloud-handoff.json"
        handoff = json.loads(handoff_path.read_text())
        handoff["files"][0]["sha256"] = "stale-checksum"
        receipt["files"][0]["sha256"] = "stale-checksum"
        handoff_path.write_text(json.dumps(handoff))
        with self.assertRaisesRegex(ValueError, "handoff.*local bytes"):
            self.record(run, receipt)
        self.assertTrue(all(part.exists() for part in self.parts(run)))
        self.assertFalse((run / "cloud-receipt.json").exists())

    def test_interrupted_release_can_resume_after_one_part_was_removed(self):
        source = self.source("session.jsonl", bytes(range(256)) * 4)
        run = self.backup([source], part_bytes=64)
        parts = self.parts(run)
        self.assertGreater(len(parts), 2)
        self.record(run, self.cloud_receipt(run))
        original = Path.unlink
        def interrupt_second_part(path, *args, **kwargs):
            if path == parts[1]:
                raise KeyboardInterrupt("fixture interrupted release")
            return original(path, *args, **kwargs)
        with mock.patch.object(Path, "unlink", interrupt_second_part):
            with self.assertRaises(KeyboardInterrupt):
                history.release_local(run)
        self.assertFalse(parts[0].exists())
        self.assertTrue(parts[1].exists())
        result = history.release_local(run)
        self.assertEqual(set(result["removed_parts"]), {part.name for part in parts})
        self.assertFalse(any(part.exists() for part in parts))
        self.assertEqual(Path(source["path"]).read_bytes(), bytes(range(256)) * 4)

    def test_failed_cloud_receipt_write_can_be_retried_without_stale_temp(self):
        run = self.backup([self.source("session.jsonl")])
        receipt = self.cloud_receipt(run)
        with mock.patch.object(history.os, "replace", side_effect=OSError("fixture interrupted metadata write")):
            with self.assertRaisesRegex(OSError, "interrupted metadata write"):
                self.record(run, receipt)
        self.assertTrue(all(part.exists() for part in self.parts(run)))
        self.assertEqual(list(run.glob("cloud-receipt.json*.tmp")), [])
        self.assertEqual(self.record(run, receipt)["verification_level"], "sha256")

    def test_verified_cloud_release_removes_only_archive_parts(self):
        source = self.source("session.jsonl")
        run = self.backup([source])
        parts = self.parts(run)
        self.record(run, self.cloud_receipt(run))
        result = history.release_local(run)
        self.assertEqual(result["native_histories_deleted"], 0)
        self.assertFalse(any(part.exists() for part in parts))
        self.assertTrue((run / "manifest.json").exists())
        self.assertTrue((run / "cloud-receipt.json").exists())
        self.assertEqual(Path(source["path"]).read_bytes(), b"fixture")
        self.assertEqual(history.release_local(run), result)


if __name__ == "__main__":
    unittest.main()
