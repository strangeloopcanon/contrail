"""Exercise checkout and installed-engine command dispatch on disposable data."""
import gzip
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import native_history as history


class NativeRecoveryCliTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.home = self.root / 'home'
        self.home.mkdir()
        self.source = self.home / '.claude/projects/p/session.jsonl'
        self.source.parent.mkdir(parents=True)
        self.source.write_bytes(b'original\n')
        sources = [{'path': str(self.source), 'relative': '.claude/projects/p/session.jsonl',
                    'app': 'claude-code', 'sqlite': False}]
        inventory = {'sources': sources, 'scope': 'all installed default-profile apps',
                     'unsupported': [], 'exclusions': []}
        options = {'inventory': inventory, 'scratch_bytes': 1024**2, 'output_bytes': 1024**2}
        base = Path(history.backup(self.home, self.root / 'backups', **options)['run'])
        self.source.write_bytes(b'latest\n')
        self.run = Path(history.backup(self.home, self.root / 'backups', base_run=base, **options)['run'])
        for part in json.loads((base / 'manifest.json').read_text())['parts']:
            (base / part['name']).unlink()
        directory = Path(__file__).parent
        # Match the Rust include_str! ordering. Functions must resolve shared
        # helpers without relying on a checkout import at runtime.
        engine = '\n'.join((directory / name).read_text() for name in (
            'native_sources.py', 'native_retention.py', 'native_restore.py', 'native_history.py'))
        self.launchers = {
            'checkout': [sys.executable, str(directory / 'native_history.py')],
            'embedded': [sys.executable, '-c', engine],
        }

    def invoke(self, launcher, *args):
        return subprocess.run(self.launchers[launcher] + list(args), cwd=self.root,
                              capture_output=True, text=True, timeout=30)

    def test_metadata_commands_and_selected_extraction_with_released_parent(self):
        for launcher in self.launchers:
            with self.subTest(launcher=launcher):
                result = self.invoke(launcher, 'catalog', '--run', str(self.run),
                                     '--app', 'claude-code', '--contains', 'session')
                self.assertEqual(result.returncode, 0, result.stderr)
                catalog = json.loads(result.stdout)
                self.assertEqual(catalog['matched_files'], 1)
                member = catalog['files'][0]['name']
                self.assertEqual(member, '.claude/projects/p/session.jsonl')
                self.assertEqual(catalog['downloads_performed'], 0)
                result = self.invoke(launcher, 'restore-plan', '--run', str(self.run), '--member', member)
                self.assertEqual(result.returncode, 0, result.stderr)
                plan = json.loads(result.stdout)
                self.assertEqual([run['run_id'] for run in plan['required_runs']], [self.run.name])
                self.assertEqual(plan['downloads_performed'], 0)
                destination = self.root / ('recovered-' + launcher)
                result = self.invoke(launcher, 'extract', '--run', str(self.run),
                                     '--member', member, '--destination', str(destination))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((destination / member).read_bytes(), b'latest\n')
                self.assertEqual(json.loads(result.stdout)['native_import_performed'], False)
                self.assertEqual(self.source.read_bytes(), b'latest\n')

    def test_invalid_selection_and_budget_fail_without_destination(self):
        for launcher in self.launchers:
            with self.subTest(launcher=launcher):
                result = self.invoke(launcher, 'restore-plan', '--run', str(self.run), '--member', '../escape')
                self.assertNotEqual(result.returncode, 0)
                destination = self.root / ('blocked-' + launcher)
                result = self.invoke(launcher, 'extract', '--run', str(self.run),
                                     '--member', '.claude/projects/p/session.jsonl',
                                     '--max-expanded-mib', '0', '--destination', str(destination))
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(destination.exists())

    def test_compressed_padding_cannot_bypass_expansion_budget(self):
        manifest = json.loads((self.run / 'manifest.json').read_text())
        self.assertEqual(len(manifest['parts']), 1)
        part = manifest['parts'][0]
        path = self.run / part['name']
        # Valid member bytes and matching archive hashes, but excessive data
        # after TAR termination. The verifier must bound actual decompression.
        payload = gzip.compress(gzip.decompress(path.read_bytes()) + b'\0' * (2 * 1024**2))
        path.write_bytes(payload)
        part.update({'bytes': len(payload), 'sha256': hashlib.sha256(payload).hexdigest(),
                     'md5': hashlib.md5(payload).hexdigest()})
        manifest.update({'archive_bytes': len(payload), 'archive_sha256': part['sha256']})
        history.write_json(self.run / 'manifest.json', manifest)
        destination = self.root / 'blocked-padding'
        with self.assertRaisesRegex(ValueError, 'bounded TAR allowance'):
            history.extract_selected(self.run, destination, ['.claude/projects/p/session.jsonl'], max_bytes=1024**2)
        self.assertFalse(destination.exists())


if __name__ == '__main__':
    unittest.main()
