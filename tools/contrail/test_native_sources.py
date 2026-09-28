"""Fixture coverage for native history discovery; never touch real histories."""
import json
from pathlib import Path
import tempfile
import sqlite3
import unittest
from unittest.mock import patch

from native_sources import discover, sanitize_desktop_snapshot, sanitize_codex_snapshot


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
                CREATE TABLE thread_turns(thread_id TEXT,turn_id TEXT,status TEXT);
                CREATE TABLE future_credentials(secret TEXT);
            """)
            connection.execute("INSERT INTO thread_items VALUES('session','history payload','item')")
            connection.execute("INSERT INTO thread_turns VALUES('session','turn','complete')")
            connection.execute("INSERT INTO future_credentials VALUES('credential')")
        report = sanitize_codex_snapshot(self.path)
        self.assertEqual(report["layout"], "thread_history")
        self.assertEqual(report["excluded_tables"], ["future_credentials"])
        with sqlite3.connect(self.path) as connection:
            self.assertEqual(connection.execute("SELECT item_json FROM thread_items").fetchone()[0], "history payload")

    def test_unknown_layout_or_columns_fail_closed(self):
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TABLE unknown(secret TEXT)")
        with self.assertRaisesRegex(ValueError, "unrecognized Codex history database layout"):
            sanitize_codex_snapshot(self.path)
        with sqlite3.connect(self.path) as connection:
            connection.execute("CREATE TABLE threads(id TEXT,rollout_path TEXT,credential TEXT)")
        with self.assertRaisesRegex(ValueError, "unrecognized Codex history schema"):
            sanitize_codex_snapshot(self.path)


if __name__ == "__main__":
    unittest.main()
