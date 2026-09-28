"""Selective recovery fixtures never access real histories or cloud storage."""

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
import native_restore as restore


class NativeRestoreTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.home = self.root / 'home'
        self.home.mkdir()
        self.output = self.root / 'backups'

    def source(self, name, content=b'fixture', sqlite=False, app='codex'):
        path = self.home / name
        path.parent.mkdir(parents=True, exist_ok=True)
        if not sqlite:
            path.write_bytes(content)
        return {'path': str(path), 'relative': name, 'app': app, 'sqlite': sqlite}

    def backup(self, sources, base=None):
        result = history.backup(
            self.home, self.output, base_run=base,
            inventory={'sources': sources, 'unsupported': [], 'exclusions': []},
            scratch_bytes=1024 * 1024, output_bytes=2 * 1024 * 1024,
            cloud_parent_id='fixture-parent', cloud_owner='owner@example.invalid')
        return Path(result['run'])

    def manifest(self, run):
        return json.loads((run / 'manifest.json').read_text())

    def change_manifest(self, run, change):
        data = self.manifest(run)
        change(data)
        (run / 'manifest.json').write_text(json.dumps(data))

    def remove_parts(self, run):
        for part in self.manifest(run)['parts']:
            (run / part['name']).unlink()

    def pair(self):
        old = self.source('.codex/sessions/old.jsonl', b'old retained')
        changed = self.source('.codex/sessions/changed.jsonl', b'before')
        base = self.backup([old, changed])
        Path(changed['path']).write_bytes(b'after')
        delta = self.backup([changed], base=base)
        return old, changed, base, delta

    def test_plan_only_requires_latest_selected_origin_even_without_old_parts(self):
        old, changed, base, delta = self.pair()
        self.remove_parts(base)
        self.remove_parts(delta)
        with mock.patch.object(history, 'manifest_parts', side_effect=AssertionError('payload read')):
            plan = restore.restore_plan(delta, [changed['relative']])
        self.assertEqual([item['run_id'] for item in plan['required_runs']], [delta.name])
        self.assertEqual(plan['files'], 1)
        self.assertEqual(plan['expanded_bytes'], len(b'after'))
        self.assertEqual(len(plan['unavailable_local_parts']), 1)
        self.assertEqual(plan['downloads_performed'], 0)
        self.assertFalse(plan['native_import_performed'])
        retained = restore.restore_plan(delta, [old['relative']])
        self.assertEqual(retained['selected_files'][0]['origin_run_id'], base.name)

    def test_selective_extract_ignores_unselected_missing_origin_payload(self):
        old, changed, base, delta = self.pair()
        self.remove_parts(base)
        destination = self.root / 'recovered'
        result = restore.extract_selected(delta, destination, [changed['relative']])
        self.assertEqual(result['files'], 1)
        self.assertEqual((destination / changed['relative']).read_bytes(), b'after')
        self.assertFalse((destination / old['relative']).exists())
        self.assertEqual([item['run_id'] for item in result['verified_runs']], [delta.name])
        self.assertEqual(Path(old['path']).read_bytes(), b'old retained')

    def test_extract_latest_and_retained_files_from_two_origins(self):
        old, changed, base, delta = self.pair()
        destination = self.root / 'recovered'
        result = restore.extract_selected(delta, destination, [old['relative'], changed['relative']])
        self.assertEqual(result['files'], 2)
        self.assertEqual((destination / old['relative']).read_bytes(), b'old retained')
        self.assertEqual((destination / changed['relative']).read_bytes(), b'after')
        self.assertEqual(len(result['verified_runs']), 2)

    def test_repeated_unchanged_increment_resolves_to_original_run(self):
        source = self.source('.codex/sessions/a.jsonl')
        base = self.backup([source])
        delta = self.backup([source], base=base)
        self.remove_parts(delta)
        plan = restore.restore_plan(delta, [source['relative']])
        self.assertEqual([item['run_id'] for item in plan['required_runs']], [base.name])
        destination = self.root / 'recovered'
        restore.extract_selected(delta, destination, [source['relative']])
        self.assertEqual((destination / source['relative']).read_bytes(), b'fixture')

    def test_missing_required_parts_fail_without_creating_destination(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        self.remove_parts(run)
        destination = self.root / 'absent-parent' / 'recovered'
        with self.assertRaisesRegex(ValueError, 'unavailable locally'):
            restore.extract_selected(run, destination, [source['relative']])
        self.assertFalse(destination.parent.exists())

    def test_empty_duplicate_unknown_and_unsafe_selections_fail(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        for members in ([], None, source['relative'], [source['relative']] * 2,
                        ['unknown'], ['../escape'], ['/absolute'], ['a\\b'], ['a//b'], [42]):
            with self.subTest(members=members), self.assertRaises(ValueError):
                restore.restore_plan(run, members)

    def test_budget_covers_all_expanded_origin_bytes_not_only_selection(self):
        small = self.source('.codex/sessions/a.jsonl', b'a')
        big = self.source('.codex/sessions/b.jsonl', b'b' * 8192)
        run = self.backup([small, big])
        plan = restore.restore_plan(run, [small['relative']])
        self.assertEqual(plan['expanded_bytes'], 1)
        self.assertEqual(plan['verification_expanded_bytes'], 8193)
        for budget in (1, 8192, 0, -1, True):
            with self.subTest(budget=budget), self.assertRaises(ValueError):
                restore.extract_selected(run, self.root / 'recovered', [small['relative']], budget)
        self.assertFalse((self.root / 'recovered').exists())

    def test_budget_is_aggregate_across_selected_origins(self):
        old, changed, base, delta = self.pair()
        names = [old['relative'], changed['relative']]
        plan = restore.restore_plan(delta, names)
        largest_origin = max(item['expanded_bytes'] for item in plan['required_runs'])
        self.assertGreater(plan['verification_expanded_bytes'], largest_origin)
        with self.assertRaisesRegex(ValueError, 'verification budget'):
            restore.extract_selected(delta, self.root / 'recovered', names, largest_origin)

    def test_existing_destination_and_symlinks_are_rejected(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        destination = self.root / 'recovered'
        destination.mkdir()
        with self.assertRaisesRegex(ValueError, 'must not exist'):
            restore.extract_selected(run, destination, [source['relative']])
        link = self.root / 'link'
        link.symlink_to(destination, target_is_directory=True)
        dangling = self.root / 'dangling'
        dangling.symlink_to(self.root / 'missing')
        for path in (link, link / 'nested', dangling):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, 'symlink'):
                restore.extract_selected(run, path, [source['relative']])
        part = run / self.manifest(run)['parts'][0]['name']
        saved = self.root / 'saved-part'
        part.rename(saved)
        part.symlink_to(saved)
        with self.assertRaisesRegex(ValueError, 'symlink'):
            restore.restore_plan(run, [source['relative']])

    def test_part_corruption_never_exposes_partial_output(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        part = run / self.manifest(run)['parts'][0]['name']
        data = part.read_bytes()
        part.write_bytes(bytes([data[0] ^ 1]) + data[1:])
        destination = self.root / 'recovered'
        with self.assertRaisesRegex(ValueError, 'checksum'):
            restore.extract_selected(run, destination, [source['relative']])
        self.assertFalse(destination.exists())
        self.assertFalse(list(self.root.glob('.contrail-select-*')))

    def test_plan_rejects_malformed_manifest_parts_origins_and_catalog(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        original = (run / 'manifest.json').read_text()
        changes = [lambda data: data['parts'][0].update(name='../part'),
                   lambda data: data['parts'][0].update(bytes=-1),
                   lambda data: data['parts'][0].update(sha256='x' * 64),
                   lambda data: data.update(archive_bytes=42),
                   lambda data: data.update(run_id='other'),
                   lambda data: data['catalog'][0].update(origin_run_id='elsewhere'),
                   lambda data: data['files'][0].update(bytes=True),
                   lambda data: data['files'][0].update(sqlite='false')]
        for index, change in enumerate(changes):
            (run / 'manifest.json').write_text(original)
            self.change_manifest(run, change)
            with self.subTest(index=index), self.assertRaises(ValueError):
                restore.restore_plan(run, [source['relative']])

    def test_cloud_receipt_locations_are_metadata_only(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        part = self.manifest(run)['parts'][0]
        receipt = {'run_id': run.name, 'folder_id': 'cloud-folder', 'private': True,
                   'files': [{'name': part['name'], 'id': 'cloud-part', 'parent_id': 'cloud-folder',
                              'bytes': part['bytes'], 'private': True}]}
        (run / 'cloud-receipt.json').write_text(json.dumps(receipt))
        self.remove_parts(run)
        plan = restore.restore_plan(run, [source['relative']])
        self.assertEqual(plan['unavailable_local_parts'][0]['cloud_file_id'], 'cloud-part')
        self.assertIn('unverified', plan['remote_locations'])
        receipt['files'][0]['bytes'] += 1
        (run / 'cloud-receipt.json').write_text(json.dumps(receipt))
        with self.assertRaisesRegex(ValueError, 'does not match'):
            restore.restore_plan(run, [source['relative']])

    def test_latest_sqlite_preserves_committed_rows(self):
        source = self.source('.codex/custom.sqlite', sqlite=True, app='fixture')
        with contextlib.closing(sqlite3.connect(source['path'])) as database:
            database.execute('CREATE TABLE events (id INTEGER PRIMARY KEY, body TEXT)')
            database.execute("INSERT INTO events VALUES (1, 'before')")
            database.commit()
        base = self.backup([source])
        with contextlib.closing(sqlite3.connect(source['path'])) as database:
            database.execute("INSERT INTO events VALUES (2, 'after')")
            database.commit()
        delta = self.backup([source], base=base)
        self.remove_parts(base)
        destination = self.root / 'recovered'
        restore.extract_selected(delta, destination, [source['relative']])
        with contextlib.closing(sqlite3.connect(destination / source['relative'])) as database:
            self.assertEqual(database.execute('SELECT body FROM events ORDER BY id').fetchall(),
                             [('before',), ('after',)])
            self.assertEqual(database.execute('PRAGMA integrity_check').fetchall(), [('ok',)])

    def test_long_unicode_path_roundtrip_with_pax_headers(self):
        name = '.codex/sessions/' + '/'.join(['long-' + 'x' * 160] * 3)
        name += '/' + '\u03bb' * 80 + '.jsonl'
        source = self.source(name, b'long path fixture')
        run = self.backup([source])
        destination = self.root / 'recovered'
        restore.extract_selected(run, destination, [name])
        self.assertEqual((destination / name).read_bytes(), b'long path fixture')

    def test_unselected_sqlite_is_verified_without_materializing_it(self):
        text = self.source('.codex/sessions/a.jsonl')
        database = self.source('.codex/custom.sqlite', sqlite=True, app='fixture')
        with contextlib.closing(sqlite3.connect(database['path'])) as connection:
            connection.execute('CREATE TABLE events (body TEXT)')
            connection.commit()
        run = self.backup([text, database])
        destination = self.root / 'recovered'
        restore.extract_selected(run, destination, [text['relative']])
        self.assertFalse((destination / database['relative']).exists())
        self.assertEqual((destination / text['relative']).read_bytes(), b'fixture')
        self.assertFalse(list(run.glob('verify-*')))

    def malicious_archive(self, sqlite=False, symlink=False):
        run = self.output / 'fixture'
        run.mkdir(parents=True)
        records = []
        raw = io.BytesIO()
        with tarfile.open(fileobj=raw, mode='w') as archive:
            for name, contents in [('chosen', b'chosen'), ('unselected', b'not sqlite')]:
                info = tarfile.TarInfo(name)
                if symlink and name == 'unselected':
                    info.type = tarfile.SYMTYPE
                    info.linkname = '../../escape'
                    archive.addfile(info)
                else:
                    info.size = len(contents)
                    archive.addfile(info, io.BytesIO(contents))
                records.append({'name': name, 'bytes': len(contents),
                                'sha256': hashlib.sha256(contents).hexdigest(),
                                'sqlite': sqlite and name == 'unselected', 'app': 'codex'})
        data = gzip.compress(raw.getvalue(), mtime=0)
        part = 'history.tar.gz.part00001'
        (run / part).write_bytes(data)
        manifest = {'format': 'contrail-native-history-v1', 'run_id': run.name, 'files': records,
                    'parts': [{'name': part, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}],
                    'archive_bytes': len(data), 'archive_sha256': hashlib.sha256(data).hexdigest()}
        (run / 'manifest.json').write_text(json.dumps(manifest))
        return run

    def test_unselected_member_checksum_is_still_required(self):
        run = self.malicious_archive()
        self.change_manifest(run, lambda data: data['files'][1].update(sha256='0' * 64))
        with self.assertRaisesRegex(ValueError, 'member checksum'):
            restore.extract_selected(run, self.root / 'recovered', ['chosen'])
        self.assertFalse((self.root / 'recovered').exists())

    def test_unselected_symlink_is_rejected(self):
        run = self.malicious_archive(symlink=True)
        with self.assertRaisesRegex(ValueError, 'non-regular'):
            restore.extract_selected(run, self.root / 'recovered', ['chosen'])
        self.assertFalse((self.root / 'recovered').exists())

    def test_unselected_invalid_sqlite_is_rejected(self):
        run = self.malicious_archive(sqlite=True)
        with self.assertRaises((ValueError, sqlite3.DatabaseError)):
            restore.extract_selected(run, self.root / 'recovered', ['chosen'])
        self.assertFalse((self.root / 'recovered').exists())

    def test_changed_metadata_during_verification_prevents_publication(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        original = history.verify_archive

        def mutate_after_verify(*args, **kwargs):
            result = original(*args, **kwargs)
            self.change_manifest(run, lambda data: data.update(archive_sha256='0' * 64))
            return result

        with mock.patch.object(history, 'verify_archive', side_effect=mutate_after_verify):
            with self.assertRaisesRegex(ValueError, 'metadata changed'):
                restore.extract_selected(run, self.root / 'recovered', [source['relative']])
        self.assertFalse((self.root / 'recovered').exists())

    def test_staged_bytes_are_bound_to_original_plan(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        original = history.verify_archive

        def change_staged_after_verify(*args, **kwargs):
            result = original(*args, **kwargs)
            (Path(kwargs['extract_to']) / source['relative']).write_bytes(b'changed')
            return result

        with mock.patch.object(history, 'verify_archive', side_effect=change_staged_after_verify):
            with self.assertRaisesRegex(ValueError, 'staged member'):
                restore.extract_selected(run, self.root / 'recovered', [source['relative']])
        self.assertFalse((self.root / 'recovered').exists())

    def test_verification_receipt_is_bound_to_original_archive(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        original = history.verify_archive

        def change_receipt_after_verify(*args, **kwargs):
            result = original(*args, **kwargs)
            result['archive_sha256'] = '0' * 64
            return result

        with mock.patch.object(history, 'verify_archive', side_effect=change_receipt_after_verify):
            with self.assertRaisesRegex(ValueError, 'archive no longer matches'):
                restore.extract_selected(run, self.root / 'recovered', [source['relative']])
        self.assertFalse((self.root / 'recovered').exists())

    def test_atomic_reservation_preserves_concurrently_created_directory(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        destination = self.root / 'recovered'
        original = Path.mkdir

        def create_before_reservation(path, *args, **kwargs):
            if path == destination:
                original(path)
            return original(path, *args, **kwargs)

        with mock.patch.object(Path, 'mkdir', new=create_before_reservation):
            with self.assertRaises(FileExistsError):
                restore.extract_selected(run, destination, [source['relative']])
        self.assertTrue(destination.is_dir())
        self.assertEqual(list(destination.iterdir()), [])

    def test_failed_publication_removes_only_own_empty_reservation(self):
        source = self.source('.codex/sessions/a.jsonl')
        run = self.backup([source])
        destination = self.root / 'recovered'
        with mock.patch.object(restore.os, 'rename', side_effect=OSError('fixture failure')):
            with self.assertRaisesRegex(OSError, 'fixture failure'):
                restore.extract_selected(run, destination, [source['relative']])
        self.assertFalse(destination.exists())

        def add_external_file_then_fail(*args):
            (destination / 'external').write_text('preserve')
            raise OSError('fixture race')

        with mock.patch.object(restore.os, 'rename', side_effect=add_external_file_then_fail):
            with self.assertRaisesRegex(OSError, 'fixture race'):
                restore.extract_selected(run, destination, [source['relative']])
        self.assertEqual((destination / 'external').read_text(), 'preserve')

    def test_catalog_latest_metadata_filters_and_pagination_without_parts(self):
        old, changed, base, delta = self.pair()
        claude = self.source('.claude/projects/p/a.jsonl', b'claude', app='claude-code')
        latest = self.backup([changed, claude], base=delta)
        for run in (base, delta, latest):
            self.remove_parts(run)
        page = history.list_catalog(latest, limit=1)
        self.assertEqual(page['matched_files'], 3)
        self.assertEqual(page['next_offset'], 1)
        self.assertEqual(page['downloads_performed'], 0)
        second = history.list_catalog(latest, limit=1, offset=1)
        self.assertNotEqual(page['files'][0]['name'], second['files'][0]['name'])
        all_codex = history.list_catalog(latest, app='codex')
        self.assertEqual(all_codex['matched_files'], 2)
        self.assertEqual({item['name'] for item in all_codex['files']}, {old['relative'], changed['relative']})
        match = history.list_catalog(latest, contains='changed')
        self.assertEqual(match['files'][0]['origin_run_id'], delta.name)
        self.assertEqual(match['files'][0]['bytes'], len(b'after'))
        self.assertEqual(history.list_catalog(latest, offset=100)['files'], [])
        self.assertIsNone(history.list_catalog(latest, offset=100)['next_offset'])
        for options in ({'limit': 0}, {'limit': 1001}, {'limit': True}, {'offset': -1}, {'offset': False}):
            with self.subTest(options=options), self.assertRaises(ValueError):
                history.list_catalog(latest, **options)


if __name__ == '__main__':
    unittest.main()
