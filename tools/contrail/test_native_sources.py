"""Fixture coverage for native history discovery; never touch real histories."""
import json
import contextlib
import os
from pathlib import Path
import tempfile
import sqlite3
import unittest
from unittest import mock
from unittest.mock import patch

from native_sources import discover, sanitize_desktop_snapshot, sanitize_codex_snapshot
import native_sources


class NativeSourcesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name).resolve()

    def file(self, relative, content=b"fixture"):
        path = self.home / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path

    def test_absent_apps_produce_empty_sources(self):
        result = discover(self.home)
        self.assertEqual(result["sources"], [])
        self.assertEqual(result["unsupported"], [])
        json.dumps(result)

    def test_allowlist_includes_history_and_excludes_credentials(self):
        included = {
            ".codex/sessions/2026/01/rollout.jsonl": False,
            ".codex/archived_sessions/rollout.jsonl": False,
            ".codex/state_5.sqlite": True,
            ".codex/thread_history_1.sqlite": True,
            ".codex/history.jsonl": False,
            ".codex/session_index.jsonl": False,
            ".cursor/chats/workspace/session/store.db": True,
            ".cursor/chats/workspace/session/meta.json": False,
            ".cursor/acp-sessions/session.jsonl": False,
            ".cursor/acp-sessions/12345678-1234-1234-1234-123456789012.json": False,
            ".cursor/projects/project/agent-transcripts/session/session.jsonl": False,
            ".cursor/projects/project/agent-tools/output.txt": False,
            ".cursor/projects/project/attachments/image.png": False,
            "Library/Application Support/Cursor/User/globalStorage/state.vscdb": True,
            "Library/Application Support/Cursor/User/workspaceStorage/project/state.vscdb": True,
            "Library/Application Support/Cursor/User/workspaceStorage/project/state.vscdb.backup": True,
            "Library/Application Support/Cursor/User/workspaceStorage/project/workspace.json": False,
            "Library/Application Support/Cursor/User/workspaceStorage/project/images/image.png": False,
            "Library/Application Support/Cursor/User/History/edit/entries.json": False,
            ".claude/history.jsonl": False,
            ".claude/projects/project/session.jsonl": False,
            ".claude/projects/project/sessions-index.json": False,
            ".claude/projects/project/session/subagents/agent.jsonl": False,
            ".claude/projects/project/session/tool-results/result.txt": False,
            ".claude/file-history/session/file@v1": False,
            ".claude/todos/session.json": False,
            ".claude/tasks/session/1.json": False,
        }
        excluded = [
            ".codex/auth.json", ".codex/logs_2.sqlite", ".codex/state_5.sqlite-wal",
            ".cursor/projects/project/mcp-auth.json", ".cursor/projects/project/worker.log",
            ".cursor/projects/project/mcps/server/auth.json",
            ".cursor/chats/workspace/session/store.db-wal",
            ".cursor/chats/workspace/session/auth.json",
            ".cursor/acp-sessions/auth.json",
            "Library/Application Support/Cursor/User/workspaceStorage/project/anysphere.cursor-retrieval/cache.txt",
            ".claude/settings.json", ".claude/.credentials.json",
            ".claude/shell-snapshots/env.sh",
        ]
        for path in [*included, *excluded]:
            self.file(path)
        result = discover(self.home)
        sources = result["sources"]
        self.assertEqual({source["relative"]: source["sqlite"] for source in sources}, included)
        self.assertEqual([source["relative"] for source in sources], sorted(included))
        for source in sources:
            self.assertEqual(source["path"], str(self.home / source["relative"]))

    def test_unknown_database_and_transcript_files_are_reported(self):
        self.file(".codex/state_future.sqlite")
        self.file(".codex/sessions/unknown.bin")
        result = discover(self.home)
        self.assertEqual(result["sources"], [])
        self.assertEqual(len(result["unsupported"]), 2)

    def test_symlink_files_directories_and_ancestors_are_not_followed(self):
        target = self.file("outside/secret.jsonl")
        sessions = self.home / ".codex/sessions"
        sessions.mkdir(parents=True)
        (sessions / "linked.jsonl").symlink_to(target)
        (sessions / "linked-directory").symlink_to(target.parent, target_is_directory=True)
        (self.home / ".claude").symlink_to(target.parent, target_is_directory=True)
        cursor = self.home / ".cursor"
        cursor.mkdir()
        (cursor / "chats").symlink_to(target.parent, target_is_directory=True)
        result = discover(self.home)
        self.assertEqual(result["sources"], [])
        self.assertEqual(sum("symlink excluded" in item for item in result["exclusions"]), 4)

    def test_home_cannot_be_a_symlink(self):
        alias = self.home / "alias"
        alias.symlink_to(self.home, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "symlink excluded"):
            discover(alias)

    def test_permission_errors_are_not_reported_as_absent(self):
        (self.home / ".codex/sessions").mkdir(parents=True)
        with patch("native_sources.os.scandir", side_effect=PermissionError("blocked")):
            with self.assertRaises(PermissionError):
                discover(self.home)

    def test_unknown_versions_are_explicit(self):
        result = discover(self.home)
        self.assertIsNone(result["versions"]["cursor-cli"])
        self.assertIsNone(result["versions"]["claude-code"])


class DesktopSanitizerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name).resolve() / "snapshot.sqlite"

    def test_only_history_rows_survive_and_removed_secret_bytes_are_erased(self):
        marker = "SECRET_SENTINEL_FOR_AUTH_PROJECTION_"
        secret = marker * 400
        with sqlite3.connect(self.path) as connection:
            connection.executescript("""
                CREATE TABLE ItemTable(key TEXT PRIMARY KEY,value BLOB);
                CREATE TABLE cursorDiskKV(key TEXT PRIMARY KEY,value BLOB);
                CREATE TABLE composerHeaders(composerId TEXT PRIMARY KEY,value BLOB);
                CREATE TABLE credentials(value TEXT);
                CREATE VIEW settingsView AS SELECT * FROM credentials;
                CREATE INDEX itemIdx ON ItemTable(value);
                CREATE TRIGGER leakAuth BEFORE DELETE ON ItemTable
                BEGIN INSERT INTO credentials VALUES(old.value); END;
            """)
            connection.executemany("INSERT INTO ItemTable VALUES(?,?)", [
                ("composer.composerData", "history"),
                ("aiService.prompts", "prompts"),
                ("cursorAuth/accessToken", secret),
                ("secret://provider", secret),
                ("mcpOAuth.secret.provider", secret),
                ("composer.unrelatedSetting", secret),
            ])
            connection.executemany("INSERT INTO cursorDiskKV VALUES(?,?)", [
                ("composerData:session", "history"),
                ("bubbleId:session:message", "bubble"),
                ("composer.content.hash", "history"),
                ("agentKv:blob", "dependency"),
                ("unrecognized:credential", secret),
                ("messageRequestContext:request", secret),
            ])
            connection.execute("INSERT INTO credentials VALUES(?)", (secret,))
            connection.execute("INSERT INTO composerHeaders VALUES('session','header')")
        self.assertTrue(marker.encode() in self.path.read_bytes())
        report = sanitize_desktop_snapshot(self.path)
        self.assertEqual(report["tables"]["ItemTable"], {"before": 6, "kept": 2, "removed": 4})
        self.assertEqual(report["tables"]["cursorDiskKV"], {"before": 6, "kept": 4, "removed": 2})
        self.assertEqual(report["tables"]["credentials"]["removed"], 1)
        self.assertEqual(report["removed_schema"], {"index": 1, "trigger": 1, "view": 1})
        self.assertEqual(report["quick_check"], "ok")
        self.assertEqual(report["freelist_pages"], 0)
        self.assertFalse(marker.encode() in self.path.read_bytes())
        with sqlite3.connect(self.path) as connection:
            self.assertEqual(connection.execute("SELECT value FROM composerHeaders").fetchone()[0], "header")
            self.assertEqual(connection.execute("SELECT value FROM ItemTable WHERE key='composer.composerData'").fetchone()[0], "history")

    def test_unrecognized_history_schema_fails_closed(self):
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TABLE ItemTable(key TEXT,value TEXT,credential TEXT)")
        with self.assertRaisesRegex(ValueError, "unrecognized Cursor history schema"):
            sanitize_desktop_snapshot(self.path)

    def test_active_sidecars_and_hardlinks_are_rejected(self):
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TABLE ItemTable(key TEXT,value TEXT)")
        sidecar = Path(str(self.path) + "-wal")
        sidecar.write_bytes(b"active")
        with self.assertRaisesRegex(ValueError, "active SQLite sidecar"):
            sanitize_desktop_snapshot(self.path)
        sidecar.unlink()
        linked = self.path.parent / "linked.sqlite"
        linked.hardlink_to(self.path)
        with self.assertRaisesRegex(ValueError, "private copy"):
            sanitize_desktop_snapshot(linked)


class CompactSnapshotTests(unittest.TestCase):
    def fixture(self, directory, sanitizer):
        path = Path(directory).resolve() / "snapshot.sqlite"
        with contextlib.closing(sqlite3.connect(path)) as db:
            if sanitizer == sanitize_desktop_snapshot:
                db.execute("CREATE TABLE ItemTable(key TEXT,value TEXT)")
                db.execute("INSERT INTO ItemTable VALUES('composer.composerData','history')")
            else:
                db.execute("CREATE TABLE threads(id TEXT,rollout_path TEXT,title TEXT)")
                db.execute("INSERT INTO threads VALUES('session','history.jsonl','history')")
            db.execute("CREATE TABLE credentials(secret TEXT)")
            db.execute("INSERT INTO credentials VALUES(?)", ("auth-canary" * 4000,))
            db.execute("CREATE TABLE old_credentials(secret TEXT)")
            db.execute("INSERT INTO old_credentials VALUES(?)", ("freepage-canary" * 4000,))
            db.commit()
            db.execute("DROP TABLE old_credentials")
            db.commit()
            self.assertGreater(db.execute("PRAGMA freelist_count").fetchone()[0], 0)
        self.assertIn(b"freepage-canary", path.read_bytes())
        return path

    def test_private_compact_file_replaces_only_after_input_is_closed(self):
        for sanitizer in (sanitize_desktop_snapshot, sanitize_codex_snapshot):
            with self.subTest(sanitizer=sanitizer.__name__), tempfile.TemporaryDirectory() as directory:
                path = self.fixture(directory, sanitizer)
                before_size = path.stat().st_size
                original_connect, original_replace = sqlite3.connect, os.replace
                input_connections = []
                outputs = []
                test = self

                class Connection:
                    def __init__(self, db):
                        self.db, self.closed = db, False

                    def execute(self, statement, *args):
                        if statement.startswith(("DELETE ", "DROP ")):
                            test.assertEqual(self.db.execute("PRAGMA journal_mode").fetchone(), ("off",))
                            test.assertFalse(Path(str(path) + "-journal").exists())
                        if statement == "VACUUM INTO ?":
                            temporary = Path(args[0][0])
                            outputs.append(temporary)
                            test.assertEqual(temporary.parent, path.parent)
                            test.assertEqual(temporary.stat().st_mode & 0o777, 0o600)
                            test.assertEqual(temporary.stat().st_nlink, 1)
                            test.assertEqual(temporary.stat().st_size, 0)
                            test.assertFalse(self.db.in_transaction)
                            test.assertFalse(Path(str(path) + "-journal").exists())
                        result = self.db.execute(statement, *args)
                        test.assertFalse(Path(str(path) + "-journal").exists())
                        return result

                    def commit(self):
                        self.db.commit()

                    def close(self):
                        self.db.close()
                        self.closed = True

                def connect(database, *args, **kwargs):
                    db = original_connect(database, *args, **kwargs)
                    if str(database).startswith(path.as_uri()):
                        db = Connection(db)
                        input_connections.append(db)
                    return db

                def replace(source, destination):
                    self.assertTrue(all(db.closed for db in input_connections))
                    self.assertLessEqual(Path(source).stat().st_size + path.stat().st_size, 2 * before_size)
                    original_replace(source, destination)

                with patch.object(native_sources.os, "replace", side_effect=replace), \
                     patch.object(sqlite3, "connect", side_effect=connect):
                    report = sanitizer(path, max_bytes=before_size)
                self.assertEqual(report["quick_check"], "ok")
                self.assertEqual(report["freelist_pages"], 0)
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertNotIn(b"auth-canary", path.read_bytes())
                self.assertNotIn(b"freepage-canary", path.read_bytes())
                self.assertEqual(list(path.parent.iterdir()), [path])
                self.assertTrue(outputs)

    def test_failed_rebuild_check_or_replace_preserves_valid_input_and_cleans_temporary_files(self):
        for sanitizer in (sanitize_desktop_snapshot, sanitize_codex_snapshot):
            for failure in ("vacuum", "check", "freelist", "budget", "replace"):
                with self.subTest(sanitizer=sanitizer.__name__, failure=failure), tempfile.TemporaryDirectory() as directory:
                    path = self.fixture(directory, sanitizer)
                    inode = path.stat().st_ino
                    original_connect = sqlite3.connect

                    class Connection:
                        def __init__(self, db, compact):
                            self.db, self.compact = db, compact

                        def execute(self, statement, *args):
                            if failure == "vacuum" and statement == "VACUUM INTO ?":
                                temporary = Path(args[0][0])
                                for suffix in ("", "-journal", "-wal", "-shm"):
                                    Path(str(temporary) + suffix).write_bytes(b"partial")
                                raise sqlite3.OperationalError("fixture compaction failure")
                            if failure == "check" and self.compact and statement == "PRAGMA quick_check":
                                return mock.Mock(fetchall=lambda: [("fixture invalid",)])
                            if failure == "freelist" and self.compact and statement == "PRAGMA freelist_count":
                                return mock.Mock(fetchone=lambda: (1,))
                            result = self.db.execute(statement, *args)
                            if failure == "budget" and statement == "VACUUM INTO ?":
                                with Path(args[0][0]).open("ab") as output:
                                    output.write(b"x" * path.stat().st_size)
                            return result

                        def commit(self):
                            self.db.commit()

                        def close(self):
                            self.db.close()

                    def connect(database, *args, **kwargs):
                        return Connection(original_connect(database, *args, **kwargs), "compact-" in str(database))

                    replace = patch.object(native_sources.os, "replace", side_effect=OSError("fixture replace failure"))
                    with patch.object(sqlite3, "connect", side_effect=connect), \
                         replace if failure == "replace" else contextlib.nullcontext():
                        with self.assertRaises((ValueError, OSError, sqlite3.OperationalError)):
                            sanitizer(path)
                    self.assertEqual(path.stat().st_ino, inode)
                    self.assertEqual(list(path.parent.iterdir()), [path])
                    with contextlib.closing(original_connect(path)) as db:
                        self.assertEqual(db.execute("PRAGMA quick_check").fetchall(), [("ok",)])
                        self.assertIsNone(db.execute("SELECT name FROM sqlite_master WHERE name='credentials'").fetchone())

    def test_nonprivate_parent_and_undersized_budget_fail_before_mutation(self):
        for sanitizer in (sanitize_desktop_snapshot, sanitize_codex_snapshot):
            with self.subTest(sanitizer=sanitizer.__name__), tempfile.TemporaryDirectory() as directory:
                path = self.fixture(directory, sanitizer)
                before = path.read_bytes()
                with self.assertRaisesRegex(ValueError, "scratch limit"):
                    sanitizer(path, max_bytes=len(before) - 1)
                path.parent.chmod(0o755)
                try:
                    with self.assertRaisesRegex(ValueError, "parent must be private"):
                        sanitizer(path)
                finally:
                    path.parent.chmod(0o700)
                self.assertEqual(path.read_bytes(), before)
                self.assertEqual(list(path.parent.iterdir()), [path])

    def test_cursor_schema_validation_precedes_any_projection_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory).resolve() / "snapshot.sqlite"
            with contextlib.closing(sqlite3.connect(path)) as db:
                db.executescript("""
                    CREATE TABLE ItemTable(key TEXT,value TEXT,credential TEXT);
                    CREATE TABLE credentials(secret TEXT);
                    CREATE VIEW secret_view AS SELECT * FROM credentials;
                """)
            before = path.read_bytes()
            with self.assertRaisesRegex(ValueError, "unrecognized Cursor history schema"):
                sanitize_desktop_snapshot(path)
            self.assertEqual(path.read_bytes(), before)

    @unittest.skipUnless(hasattr(sqlite3.Connection, "setconfig"), "SQLite defensive config requires Python 3.12+")
    def test_runtime_that_prohibits_journal_off_fails_before_mutation(self):
        real_connect = sqlite3.connect

        def defensive_connect(*args, **kwargs):
            db = real_connect(*args, **kwargs)
            db.setconfig(sqlite3.SQLITE_DBCONFIG_DEFENSIVE, True)
            return db

        for sanitizer in (sanitize_desktop_snapshot, sanitize_codex_snapshot):
            with self.subTest(sanitizer=sanitizer.__name__), tempfile.TemporaryDirectory() as directory:
                path = self.fixture(directory, sanitizer)
                before = path.read_bytes()
                with patch.object(sqlite3, "connect", side_effect=defensive_connect):
                    with self.assertRaisesRegex(ValueError, "runtime does not support journal_mode=OFF"):
                        sanitizer(path)
                self.assertEqual(path.read_bytes(), before)
                self.assertEqual(list(path.parent.iterdir()), [path])


class CodexSanitizerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name).resolve() / "snapshot.sqlite"

    def test_state_auth_tables_and_removed_bytes_are_excluded(self):
        marker = "CODEX_ENROLLMENT_SECRET_SENTINEL"
        with sqlite3.connect(self.path) as connection:
            connection.executescript("""
                CREATE TABLE threads(id TEXT PRIMARY KEY,rollout_path TEXT,title TEXT);
                CREATE TABLE thread_spawn_edges(parent_thread_id TEXT,child_thread_id TEXT,status TEXT);
                CREATE TABLE remote_control_enrollments(token TEXT);
                CREATE TABLE external_agent_config_imports(credential TEXT);
                CREATE TABLE __probe(secret TEXT);
                CREATE VIEW enrollment_view AS SELECT * FROM remote_control_enrollments;
                CREATE INDEX title_index ON threads(title);
            """)
            connection.execute("INSERT INTO threads VALUES('session','sessions/history.jsonl','history')")
            connection.execute("INSERT INTO thread_spawn_edges VALUES('parent','session','complete')")
            for table in ("remote_control_enrollments", "external_agent_config_imports", "__probe"):
                connection.execute('INSERT INTO "' + table + '" VALUES(?)', (marker * 400,))
        self.assertTrue(marker.encode() in self.path.read_bytes())
        report = sanitize_codex_snapshot(self.path)
        self.assertEqual(report["layout"], "state")
        self.assertEqual(set(report["excluded_tables"]), {"remote_control_enrollments", "external_agent_config_imports", "__probe"})
        self.assertEqual(report["tables"]["threads"], {"before": 1, "kept": 1, "removed": 0})
        self.assertFalse(marker.encode() in self.path.read_bytes())
        self.assertEqual(report["quick_check"], "ok")
        self.assertEqual(report["freelist_pages"], 0)
        with sqlite3.connect(self.path) as connection:
            self.assertEqual(connection.execute("SELECT title FROM threads").fetchone()[0], "history")

    def test_thread_history_payloads_survive_and_unknown_tables_are_excluded(self):
        with sqlite3.connect(self.path) as connection:
            connection.executescript("""
                CREATE TABLE thread_items(thread_id TEXT,item_json TEXT,item_id TEXT);
                CREATE TABLE thread_turns(thread_id TEXT,turn_id TEXT,status TEXT,root_turn_id TEXT);
                CREATE TABLE future_credentials(secret TEXT);
            """)
            connection.execute("INSERT INTO thread_items VALUES('session','history payload','item')")
            connection.execute("INSERT INTO thread_turns VALUES('session','turn','complete','root-turn')")
            connection.execute("INSERT INTO future_credentials VALUES('credential')")
        report = sanitize_codex_snapshot(self.path)
        self.assertEqual(report["layout"], "thread_history")
        self.assertEqual(report["excluded_tables"], ["future_credentials"])
        with sqlite3.connect(self.path) as connection:
            self.assertEqual(connection.execute("SELECT item_json FROM thread_items").fetchone()[0], "history payload")
            self.assertEqual(connection.execute("SELECT root_turn_id FROM thread_turns").fetchone()[0], "root-turn")

    def test_thread_history_unknown_credential_column_fails_before_mutation(self):
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TABLE thread_turns(thread_id TEXT,root_turn_id TEXT,credential TEXT)")
            connection.execute("INSERT INTO thread_turns VALUES('session','root-turn','secret')")
        before = self.path.read_bytes()
        with self.assertRaisesRegex(ValueError, "unrecognized Codex history schema: thread_turns"):
            sanitize_codex_snapshot(self.path)
        self.assertEqual(self.path.read_bytes(), before)

    def test_unknown_layout_or_columns_fail_closed(self):
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TABLE unknown(secret TEXT)")
        with self.assertRaisesRegex(ValueError, "unrecognized Codex history database layout"):
            sanitize_codex_snapshot(self.path)
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TABLE threads(id TEXT,rollout_path TEXT,credential TEXT)")
        with self.assertRaisesRegex(ValueError, "unrecognized Codex history schema"):
            sanitize_codex_snapshot(self.path)


class AntigravitySourcesTests(unittest.TestCase):
    setUp = NativeSourcesTests.setUp
    file = NativeSourcesTests.file

    def test_antigravity_allowlist_and_profile_exclusions(self):
        identity = "12345678-1234-1234-1234-123456789012"
        root = ".gemini/antigravity/"
        included = {
            root + "conversations/" + identity + ".pb": False,
            root + "conversations/" + identity + ".db": True,
            root + "brain/" + identity + "/artifacts/task.md.resolved.1": False,
            root + "browser_recordings/" + identity + "/capture.jpg": False,
            root + "annotations/" + identity + ".pbtxt": False,
            root + "conversation_summaries.db": True,
            root + "agyhub_summaries_proto.pb": False,
        }
        excluded = [root + name for name in (
            "auth.json", "config.json", "mcp_config.json", "generalappstate.vscdb",
            "implicit/cache.pb", "code_tracker/state.pb", "bin/runtime",
            "brain/auth.json", "brain/unknown/file.txt", "conversations/auth.db",
            "conversations/" + identity + ".db-wal",
        )]
        for name in [*included, *excluded]:
            self.file(name)
        result = discover(self.home)
        self.assertEqual({s["relative"]: s["sqlite"] for s in result["sources"]}, included)
        self.assertTrue(all(s["app"] == "antigravity" for s in result["sources"]))
        self.assertIn("antigravity", result["versions"])
        outside = self.file("outside/secret")
        (self.home / root / "brain" / identity / "link").symlink_to(outside)
        self.assertTrue(any("symlink" in item for item in discover(self.home)["exclusions"]))


class AntigravityProjectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name).resolve()
        self.directory.chmod(0o700)
        self.path = self.directory / "snapshot.db"

    def database(self, names):
        with sqlite3.connect(self.path) as connection:
            for name in names:
                columns = native_sources._ANTIGRAVITY_COLUMNS[name].split()
                connection.execute('CREATE TABLE "' + name + '" (' + ','.join('"' + c + '" BLOB' for c in columns) + ')')
                connection.execute('INSERT INTO "' + name + '" VALUES (' + ','.join('?' for _ in columns) + ')', tuple(b'history' for _ in columns))

    def test_trajectory_preserves_payload_and_removes_credential_schema(self):
        names = set(native_sources._ANTIGRAVITY_COLUMNS) - {"conversation_summaries", "search_schema_version"}
        self.database(names)
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TABLE account_credentials(token TEXT)")
            connection.execute("INSERT INTO account_credentials VALUES ('credential-marker')")
            connection.execute("CREATE TRIGGER steal AFTER DELETE ON steps BEGIN INSERT INTO account_credentials VALUES ('trigger-marker'); END")
        report = native_sources.sanitize_antigravity_snapshot(self.path)
        self.assertEqual(report["layout"], "trajectory")
        self.assertEqual(report["quick_check"], "ok")
        with sqlite3.connect(self.path) as connection:
            self.assertEqual(connection.execute("SELECT step_payload FROM steps").fetchone(), (b"history",))
            self.assertEqual({r[0] for r in connection.execute("SELECT name FROM sqlite_master WHERE type='table'")}, names)
        self.assertNotIn(b"credential-marker", self.path.read_bytes())

    def test_summary_preserves_history_but_excludes_derived_fts(self):
        self.database(("conversation_summaries", "search_schema_version"))
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE VIRTUAL TABLE conversations_fts USING fts5(user_body,agent_body)")
            connection.execute("INSERT INTO conversations_fts VALUES ('derived','text')")
        native_sources.sanitize_antigravity_snapshot(self.path)
        with sqlite3.connect(self.path) as connection:
            self.assertEqual(connection.execute("SELECT raw_summary FROM conversation_summaries").fetchone(), (b"history",))
            self.assertFalse(connection.execute("SELECT 1 FROM sqlite_master WHERE name LIKE 'conversations_fts%'").fetchall())

    def test_generated_credential_column_fails_before_mutation(self):
        self.database(("trajectory_meta", "steps"))
        with sqlite3.connect(self.path) as connection:
            connection.execute("ALTER TABLE steps ADD COLUMN auth_config TEXT GENERATED ALWAYS AS ('credential-marker') VIRTUAL")
        before = self.path.read_bytes()
        with self.assertRaisesRegex(ValueError, "unrecognized Antigravity history schema: steps"):
            native_sources.sanitize_antigravity_snapshot(self.path)
        self.assertEqual(self.path.read_bytes(), before)

    def test_unknown_layout_or_columns_fail_before_mutation(self):
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TABLE account_credentials(token TEXT)")
        before = self.path.read_bytes()
        with self.assertRaisesRegex(ValueError, "unrecognized Antigravity history database layout"):
            native_sources.sanitize_antigravity_snapshot(self.path)
        self.assertEqual(self.path.read_bytes(), before)
        self.database(("trajectory_meta", "steps"))
        with sqlite3.connect(self.path) as connection:
            connection.execute("ALTER TABLE steps ADD COLUMN auth_token TEXT")
        before = self.path.read_bytes()
        with self.assertRaisesRegex(ValueError, "unrecognized Antigravity history schema: steps"):
            native_sources.sanitize_antigravity_snapshot(self.path)
        self.assertEqual(self.path.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
