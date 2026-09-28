"""Regression for SQLite snapshots while a native store remains busy."""
import hashlib
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest import mock

import native_history as history


class _ConnectionProxy:
    """Inject real writer commits between incremental online-backup steps."""

    def __init__(self, connection, on_progress=None, suppress_pin=False):
        self.connection = connection
        self.on_progress = on_progress
        self.suppress_pin = suppress_pin

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def close(self):
        self.connection.close()

    def execute(self, *args, **kwargs):
        statement = args[0].strip().upper() if args and isinstance(args[0], str) else ""
        if self.suppress_pin and statement in {
            "BEGIN",
            "SELECT COUNT(*) FROM SQLITE_MASTER",
        }:
            # Recreate the pre-fix path: a standalone schema read starts a
            # read transaction too, so both pinning statements must be skipped.
            return _UnusedCursor()
        return self.connection.execute(*args, **kwargs)

    def backup(self, destination, *, progress=None, **kwargs):
        def advance(status, remaining, total):
            if progress:
                progress(status, remaining, total)
            if self.on_progress:
                self.on_progress()

        return self.connection.backup(
            destination, progress=advance, **kwargs
        )


class _UnusedCursor:
    def fetchone(self):
        return None


class BusySQLiteSnapshotTests(unittest.TestCase):
    def _run_snapshot(self, root, suppress_pin):
        source_path = root / ("busy-unpinned.sqlite" if suppress_pin else "busy-pinned.sqlite")
        destination = (root / ("snapshot-unpinned.sqlite" if suppress_pin else "snapshot-pinned.sqlite")).resolve()
        setup = sqlite3.connect(source_path)
        setup.execute("PRAGMA journal_mode=WAL")
        setup.execute("CREATE TABLE state (counter INTEGER NOT NULL)")
        setup.execute("INSERT INTO state VALUES (0)")
        setup.execute("CREATE TABLE pages (payload BLOB NOT NULL)")
        payload = b"x" * 3000
        setup.executemany(
            "INSERT INTO pages VALUES (?)", ((payload,) for _ in range(1400))
        )
        setup.commit()
        setup.close()

        before = hashlib.sha256(source_path.read_bytes()).digest()
        writer = sqlite3.connect(source_path, timeout=5)
        self.addCleanup(writer.close)
        commits = 0

        def commit_writer_update():
            nonlocal commits
            writer.execute("UPDATE state SET counter = counter + 1")
            writer.commit()
            commits += 1

        real_connect = sqlite3.connect

        def connect(*args, **kwargs):
            connection = real_connect(*args, **kwargs)
            if args and str(args[0]).startswith(source_path.as_uri()):
                return _ConnectionProxy(
                    connection, commit_writer_update, suppress_pin=suppress_pin
                )
            return connection

        source = {
            "path": str(source_path.resolve()),
            "relative": "busy.sqlite",
            "app": "fixture",
            "sqlite": True,
        }
        with mock.patch.object(history.sqlite3, "connect", side_effect=connect):
            history.snapshot_file(
                source, destination, 16 * 1024 * 1024,
                deadline_seconds=0.4 if suppress_pin else 60,
            )

        self.assertGreater(commits, 0)
        self.assertEqual(writer.execute("SELECT counter FROM state").fetchone(), (commits,))
        # Writer activity was confined to SQLite's WAL; snapshotting did not
        # rewrite the source database file.
        self.assertEqual(hashlib.sha256(source_path.read_bytes()).digest(), before)
        return destination, commits

    def test_unpinned_backup_times_out_but_pinned_snapshot_is_coherent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            with self.assertRaisesRegex(ValueError, "time limit"):
                self._run_snapshot(root, suppress_pin=True)

            destination, commits = self._run_snapshot(root, suppress_pin=False)
            snapshot = sqlite3.connect(destination)
            try:
                self.assertEqual(snapshot.execute("PRAGMA integrity_check").fetchall(), [("ok",)])
                self.assertEqual(snapshot.execute("SELECT counter FROM state").fetchone(), (0,))
            finally:
                snapshot.close()
            self.assertGreater(commits, 0)


if __name__ == "__main__":
    unittest.main()
