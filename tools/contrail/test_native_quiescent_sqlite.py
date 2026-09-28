"""Regression tests for immutable fallback on closed, quiescent WAL databases."""
import contextlib
import hashlib
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest import mock

import native_history as history


class _FailFirstRead:
    """Keep SQLite real except for the first query on the live read-only DB."""

    def __init__(self, connection):
        self.connection = connection

    def __getattr__(self, name):
        return getattr(self.connection, name)

    def execute(self, sql, *args, **kwargs):
        raise sqlite3.OperationalError("unable to open database file")


class QuiescentSQLiteSnapshotTests(unittest.TestCase):
    def _closed_wal_header(self, path):
        with contextlib.closing(sqlite3.connect(path)) as db:
            self.assertEqual(db.execute("PRAGMA journal_mode=WAL").fetchone(), ("wal",))
            db.execute("CREATE TABLE fixture (value TEXT NOT NULL)")
            db.execute("INSERT INTO fixture VALUES ('preserved')")
            db.commit()
            self.assertEqual(db.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()[0], 0)
        # Some SQLite builds retain empty WAL/SHM files after the last WAL
        # connection closes. The checkpoint above moved all fixture content
        # into the database; remove only these sidecars beside our private DB.
        for suffix in ("-wal", "-shm", "-journal"):
            Path(str(path) + suffix).unlink(missing_ok=True)
        # A closed WAL database has a WAL-format header, but no active sidecars.
        for suffix in ("-wal", "-shm", "-journal"):
            self.assertFalse(Path(str(path) + suffix).exists())
        with path.open("rb") as stream:
            self.assertEqual(stream.read(20)[18:20], b"\x02\x02")

    def _source(self, path):
        return {"path": str(path), "relative": "fixture.sqlite", "app": "fixture", "sqlite": True}

    def _mock_first_live_query(self, path):
        real_connect = sqlite3.connect
        failed = False
        databases = []

        def connect(database, *args, **kwargs):
            nonlocal failed
            databases.append(str(database))
            connection = real_connect(database, *args, **kwargs)
            if not failed and str(database).startswith(path.as_uri()) and kwargs.get("uri"):
                failed = True
                return _FailFirstRead(connection)
            return connection

        return real_connect, connect, databases

    def test_falls_back_to_private_immutable_copy_only_for_quiescent_wal_header(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source_path = root / "source.sqlite"
            destination = root / "snapshot.sqlite"
            self._closed_wal_header(source_path)
            before = source_path.read_bytes()
            real_connect, connect, databases = self._mock_first_live_query(source_path)
            temp_dirs = []
            real_temp_directory = tempfile.TemporaryDirectory

            def temporary_directory(*args, **kwargs):
                result = real_temp_directory(*args, **kwargs)
                temp_dirs.append(Path(result.name))
                return result

            with mock.patch.object(history.sqlite3, "connect", side_effect=connect), \
                 mock.patch.object(history.tempfile, "TemporaryDirectory", side_effect=temporary_directory):
                history.snapshot_file(self._source(source_path), destination, 4 * 1024 * 1024)

            with contextlib.closing(real_connect(destination)) as snapshot:
                self.assertEqual(snapshot.execute("SELECT value FROM fixture").fetchall(), [("preserved",)])
                self.assertEqual(snapshot.execute("PRAGMA integrity_check").fetchall(), [("ok",)])
            self.assertEqual(source_path.read_bytes(), before)
            for suffix in ("-wal", "-shm", "-journal"):
                self.assertFalse(Path(str(source_path) + suffix).exists())
            self.assertTrue(temp_dirs)
            self.assertTrue(all(not directory.exists() for directory in temp_dirs))
            live_uris = [item for item in databases if item.startswith(source_path.as_uri())]
            immutable_uris = [item for item in databases if "immutable=1" in item]
            self.assertTrue(live_uris)
            self.assertTrue(immutable_uris)
            self.assertTrue(all("immutable=1" not in item for item in live_uris))
            self.assertTrue(all(any(item.startswith(directory.as_uri()) for directory in temp_dirs)
                                for item in immutable_uris))

    def test_a_sidecar_appearing_during_stable_copy_rejects_fallback(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source_path = root / "source.sqlite"
            destination = root / "snapshot.sqlite"
            self._closed_wal_header(source_path)
            before = hashlib.sha256(source_path.read_bytes()).digest()
            real_connect, connect, _databases = self._mock_first_live_query(source_path)
            real_snapshot_file = history.snapshot_file
            copied = False

            def snapshot_file(source, target, max_bytes, *args, **kwargs):
                nonlocal copied
                result = real_snapshot_file(source, target, max_bytes, *args, **kwargs)
                if not source["sqlite"] and Path(source["path"]) == source_path:
                    copied = True
                    Path(str(source_path) + "-journal").write_bytes(b"appeared during copy")
                return result

            with mock.patch.object(history.sqlite3, "connect", side_effect=connect), \
                 mock.patch.object(history, "snapshot_file", side_effect=snapshot_file):
                with self.assertRaises((ValueError, sqlite3.OperationalError)):
                    history.snapshot_file(self._source(source_path), destination, 4 * 1024 * 1024)

            self.assertFalse(destination.exists())
            self.assertTrue(copied)
            self.assertEqual(hashlib.sha256(source_path.read_bytes()).digest(), before)
            self.assertTrue(Path(str(source_path) + "-journal").exists())


if __name__ == "__main__":
    unittest.main()
