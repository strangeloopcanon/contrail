"""Native snapshots: no importer, daemon, credentials, or live-store mutation."""
import argparse
import contextlib
import datetime
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import sqlite3
import stat
import sys
import tarfile
import tempfile
import time
import uuid

# The Rust binary concatenates adapters before this module. Direct source
# execution remains useful for tests and connector orchestration from a checkout.
if 'discover' not in globals():
    from native_sources import discover, sanitize_desktop_snapshot, sanitize_codex_snapshot
    from native_retention import retention_plan

PART_LIMIT = 80 * 1024 * 1024


def cloud_destination(parent_id=None, owner=None, existing=None):
    existing = existing or {}
    parent_id = parent_id or existing.get('parent_id') or os.environ.get('CONTRAIL_HISTORY_CLOUD_PARENT')
    owner = owner or existing.get('owner') or os.environ.get('CONTRAIL_HISTORY_CLOUD_OWNER')
    if bool(parent_id) != bool(owner):
        raise ValueError('cloud parent folder ID and owner must both be configured')
    return {'parent_id': parent_id, 'owner': owner} if parent_id else None


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def progress_event(message, **context):
    print(json.dumps({'timestamp': utc(), 'level': 'info', 'message': message,
                      'context': context}), file=sys.stderr, flush=True)


def digest_file(path, algorithm='sha256'):
    digest = hashlib.new(algorithm)
    with open(path, 'rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def write_json(path, data):
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    created = False
    try:
        with open(temporary, 'x', encoding='utf-8') as stream:
            created = True
            json.dump(data, stream, indent=2, sort_keys=True)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if created:
            temporary.unlink(missing_ok=True)


def safe_name(name):
    path = PurePosixPath(name)
    if (not name or '\\' in name or '\x00' in name or path.is_absolute()
            or any(p in ('', '.', '..') for p in name.split('/'))):
        raise ValueError('unsafe archive member name')
    return path


def regular_path(path):
    path = Path(path).absolute()
    for ancestor in [path, *path.parents]:
        if ancestor.is_symlink():
            raise ValueError('symlink source or destination is unsupported')
    if not stat.S_ISREG(path.stat().st_mode):
        raise ValueError('source is not a regular file')
    return path


def fingerprint(path):
    info = path.stat()
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


@contextlib.contextmanager
def sqlite_reader(path, destination, max_bytes):
    """Open live read-only SQLite, or a stable private copy of a closed store."""
    connection = sqlite3.connect(path.as_uri() + '?mode=ro', uri=True, timeout=5)
    try:
        connection.execute('PRAGMA journal_mode').fetchone()
    except sqlite3.OperationalError as error:
        connection.close()
        # Some closed WAL-header stores cannot be queried read-only without
        # creating live sidecars. Never use immutable mode on a live database.
        sidecars = [Path(str(path) + suffix) for suffix in ('-wal', '-shm', '-journal')]
        def absent():
            return all(not item.exists() and not item.is_symlink() for item in sidecars)
        if str(error) != 'unable to open database file' or not absent():
            raise
        with tempfile.TemporaryDirectory(prefix='closed-store-', dir=destination.parent) as folder:
            copied = Path(folder) / 'store.sqlite'
            snapshot_file({'path': str(path), 'app': 'private-copy', 'sqlite': False}, copied, max_bytes)
            if not absent():
                raise ValueError('SQLite sidecar appeared during closed-store copy')
            with contextlib.closing(sqlite3.connect(copied.as_uri() + '?mode=ro&immutable=1', uri=True)) as reader:
                yield reader, True
    else:
        with contextlib.closing(connection):
            yield connection, False


def snapshot_file(source, destination, max_bytes, deadline_seconds=60):
    path = regular_path(source['path'])
    observed_stat = path.stat()
    # In-place VACUUM may need two additional database-sized temporary files.
    if source['sqlite'] and source['app'] in ('cursor-desktop', 'codex'):
        max_bytes //= 3
    elif source['sqlite']:
        max_bytes //= 2  # Closed-store fallback holds a copy plus its backup.
    if path.stat().st_size > max_bytes:
        raise ValueError('source exceeds single snapshot scratch limit')
    started = utc()
    if source['sqlite']:
        for suffix in ('-wal', '-shm', '-journal'):
            sidecar = Path(str(path) + suffix)
            if sidecar.is_symlink():
                raise ValueError('symlink SQLite sidecar is unsupported')
        deadline = time.monotonic() + deadline_seconds
        page_size = 4096
        def progress(_status, remaining, total):
            if total * page_size > max_bytes:
                raise ValueError('SQLite snapshot exceeds scratch limit')
            if time.monotonic() > deadline:
                raise ValueError('SQLite online backup exceeded time limit')
        # mode=ro prevents creation or recovery writes to the live store.
        with sqlite_reader(path, destination, max_bytes) as (src, quiescent_copy):
            # A WAL reader can pin a committed version without blocking writers.
            # Without a read transaction, each concurrent commit may restart a
            # large online backup indefinitely. Rollback-journal stores retain
            # the incremental backup API's short locks instead.
            pinned = src.execute('PRAGMA journal_mode').fetchone()[0] == 'wal'
            if pinned:
                src.execute('BEGIN')
                src.execute('SELECT count(*) FROM sqlite_master').fetchone()
            page_size = src.execute('PRAGMA page_size').fetchone()[0]
            pages = src.execute('PRAGMA page_count').fetchone()[0]
            if pages * page_size > max_bytes:
                raise ValueError('SQLite snapshot exceeds scratch limit')
            with contextlib.closing(sqlite3.connect(destination)) as dst:
                src.backup(dst, pages=256, progress=progress, sleep=0.05)
                if pinned:
                    # Release the live WAL version before offline integrity and
                    # credential projection work, which can take much longer.
                    src.execute('ROLLBACK')
                if dst.execute('PRAGMA journal_mode=DELETE').fetchone()[0] != 'delete':
                    raise ValueError('offline snapshot journal normalization failed')
                result = dst.execute('PRAGMA integrity_check').fetchall()
                if result != [('ok',)]:
                    raise ValueError('SQLite integrity check failed')
        # Some SQLite versions leave SHM after closing a WAL-origin backup.
        # DELETE normalization succeeded and both connections are closed. These
        # sidecars belong solely to this private disposable destination.
        for suffix in ('-wal', '-shm', '-journal'):
            sidecar = Path(str(destination) + suffix)
            if sidecar.exists() or sidecar.is_symlink():
                regular_path(sidecar)
                if sidecar.stat().st_nlink != 1:
                    raise ValueError('linked offline snapshot sidecar is unsupported')
                sidecar.unlink()
        projection = None
        if source['app'] == 'cursor-desktop':
            projection = sanitize_desktop_snapshot(destination)
        elif source['app'] == 'codex':
            projection = sanitize_codex_snapshot(destination)
        if destination.stat().st_size > max_bytes:
            raise ValueError('SQLite snapshot exceeds scratch limit')
        boundary = ('Stable closed-store copy; live sidecars absent before and after; private SQLite backup'
                    if quiescent_copy else 'Pinned WAL read transaction and SQLite online backup; committed transactions; per-database boundary'
                    if pinned else 'SQLite online backup; committed transactions; per-database boundary')
    else:
        for attempt in range(3):
            before = fingerprint(path)
            with open(path, 'rb') as src, open(destination, 'wb') as dst:
                if fingerprint(path) != before or os.fstat(src.fileno()).st_ino != before[1]:
                    continue
                copied = 0
                for block in iter(lambda: src.read(1024 * 1024), b''):
                    copied += len(block)
                    if copied > max_bytes:
                        raise ValueError('growing source exceeds scratch limit')
                    dst.write(block)
            if before == fingerprint(path) and copied == before[2]:
                break
        else:
            raise ValueError('source changed during all three snapshot attempts')
        boundary = 'stable per-file copy; source identity/size/mtime/ctime checked before and after'
    result = {'started_utc': started, 'finished_utc': utc(), 'boundary': boundary,
              'source_mtime_ns': observed_stat.st_mtime_ns if source['sqlite'] else before[3],
              'source_mode': stat.S_IMODE(observed_stat.st_mode)}
    if source['sqlite'] and projection is not None:
        result['history_projection'] = projection
    return result


class PartWriter:
    def __init__(self, folder, limit, max_bytes):
        self.folder, self.limit, self.max_bytes = folder, limit, max_bytes
        self.stream = None
        self.parts = []
        self.total = 0
        self.archive_hash = hashlib.sha256()

    def write(self, data):
        if self.total + len(data) > self.max_bytes:
            raise ValueError('archive exceeds output budget')
        self.archive_hash.update(data)
        self.total += len(data)
        view = memoryview(data)
        while view:
            if self.stream is None:
                name = f'history.tar.gz.part{len(self.parts) + 1:05d}'
                self.stream = open(self.folder / name, 'xb')
                self.parts.append({'name': name, 'bytes': 0})
            part = self.parts[-1]
            count = min(len(view), self.limit - part['bytes'])
            self.stream.write(view[:count])
            part['bytes'] += count
            view = view[count:]
            if part['bytes'] == self.limit:
                self.finish_part()
        return len(data)

    def flush(self):
        if self.stream:
            self.stream.flush()

    def finish_part(self):
        if self.stream:
            self.stream.flush()
            os.fsync(self.stream.fileno())
            self.stream.close()
            self.stream = None
            part = self.parts[-1]
            part['sha256'] = digest_file(self.folder / part['name'])
            part['md5'] = digest_file(self.folder / part['name'], 'md5')


class PartReader(io.RawIOBase):
    def __init__(self, paths):
        self.paths = iter(paths)
        self.stream = None

    def readable(self):
        return True

    def readinto(self, buffer):
        while True:
            if self.stream is None:
                try:
                    self.stream = open(next(self.paths), 'rb')
                except StopIteration:
                    return 0
            count = self.stream.readinto(buffer)
            if count:
                return count
            self.stream.close()
            self.stream = None

    def close(self):
        if self.stream:
            self.stream.close()
        super().close()


def manifest_parts(folder, manifest):
    if manifest.get('format') != 'contrail-native-history-v1':
        raise ValueError('unsupported manifest format')
    parts = manifest['parts']
    if not parts:
        raise ValueError('missing archive parts')
    full_hash = hashlib.sha256()
    paths = []
    for index, part in enumerate(parts, 1):
        expected = f'history.tar.gz.part{index:05d}'
        if part['name'] != expected or not 0 < part['bytes'] <= PART_LIMIT:
            raise ValueError('invalid part order or size')
        path = regular_path(folder / expected)
        if path.stat().st_size != part['bytes'] or digest_file(path) != part['sha256']:
            raise ValueError('part size/checksum mismatch')
        with open(path, 'rb') as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                full_hash.update(chunk)
        paths.append(path)
    if sum(p['bytes'] for p in parts) != manifest['archive_bytes'] or full_hash.hexdigest() != manifest['archive_sha256']:
        raise ValueError('full archive checksum mismatch')
    return paths


class _ExpandedReader:
    """Bound decompression including TAR headers and trailing padding."""

    def __init__(self, stream, limit):
        self.stream = stream
        self.remaining = limit

    def read(self, size=-1):
        wanted = self.remaining + 1 if size < 0 else min(size, self.remaining + 1)
        data = self.stream.read(wanted)
        self.remaining -= len(data)
        if self.remaining < 0:
            raise ValueError('decompressed archive exceeds bounded TAR allowance')
        return data


def verify_archive(folder, extract_to=None, max_bytes=64 * 1024**3, overlay=False, members=None):
    folder = Path(folder).absolute()
    manifest = json.loads(regular_path(folder / 'manifest.json').read_text())
    paths = manifest_parts(folder, manifest)
    expected = {}
    for member in manifest['files']:
        safe_name(member['name'])
        if member['name'] in expected or member['bytes'] < 0:
            raise ValueError('duplicate or invalid manifest member')
        expected[member['name']] = member
    if members is not None:
        members = set(members)
        if not members or not members.issubset(expected):
            raise ValueError('selected archive members must exist in manifest')
    if sum(m['bytes'] for m in expected.values()) > max_bytes:
        raise ValueError('expanded archive exceeds verification budget')
    # Each file allows regular and PAX headers, rounded file data, and a
    # path-sized PAX payload. Archive record padding is bounded separately.
    # The manifest byte budget must not permit an unbounded gzip padding tail.
    tar_allowance = 10240 + sum(2048 + 512 * ((len(name.encode('utf-8')) + 256 + 511) // 512)
                                for name in expected)
    seen = set()
    last_progress = time.monotonic()
    if len(expected) > 1000:
        progress_event('native_archive_verification_started', total_files=len(expected))
    # Scratch is one SQLite member at a time; never a second full snapshot.
    with tempfile.TemporaryDirectory(prefix='verify-', dir=folder) as scratch:
        with io.BufferedReader(PartReader(paths)) as raw:
            with gzip.GzipFile(fileobj=raw, mode='rb') as compressed:
                expanded = _ExpandedReader(compressed, sum(m['bytes'] for m in expected.values()) + tar_allowance)
                with tarfile.open(fileobj=expanded, mode='r|') as archive:
                    for member in archive:
                        safe_name(member.name)
                        if not member.isfile() or member.name in seen or member.name not in expected:
                            raise ValueError('unexpected, duplicate, or non-regular archive member')
                        record = expected[member.name]
                        if member.size != record['bytes']:
                            raise ValueError('member size mismatch')
                        seen.add(member.name)
                        target = None
                        materialize = extract_to is not None and (members is None or member.name in members)
                        if materialize:
                            target = Path(extract_to).joinpath(*safe_name(member.name).parts)
                            target.parent.mkdir(parents=True, exist_ok=True)
                            if overlay and target.exists():
                                regular_path(target).unlink()
                        elif record['sqlite']:
                            target = Path(scratch) / 'snapshot.sqlite'
                        digest = hashlib.sha256()
                        with contextlib.ExitStack() as stack:
                            data = stack.enter_context(archive.extractfile(member))
                            out = stack.enter_context(open(target, 'xb')) if target else None
                            for chunk in iter(lambda: data.read(1024 * 1024), b''):
                                digest.update(chunk)
                                if out:
                                    out.write(chunk)
                        if digest.hexdigest() != record['sha256']:
                            raise ValueError('member checksum mismatch')
                        if record['sqlite']:
                            with contextlib.closing(sqlite3.connect(target.as_uri() + '?mode=ro', uri=True)) as db:
                                if db.execute('PRAGMA integrity_check').fetchall() != [('ok',)]:
                                    raise ValueError('restored SQLite integrity failure')
                            if not materialize:
                                target.unlink()
                        if len(expected) > 1000 and time.monotonic() - last_progress >= 30:
                            progress_event('native_archive_verification_progress', verified_files=len(seen), total_files=len(expected))
                            last_progress = time.monotonic()
                # Consume gzip footer; detect corrupt/truncated data after the tar terminator.
                while expanded.read(1024 * 1024):
                    pass
    if seen != set(expected):
        raise ValueError('missing archive members')
    return {'status': 'verified', 'verified_utc': utc(), 'files': len(seen),
            'archive_sha256': manifest['archive_sha256'], 'sqlite_integrity': 'ok',
            'verification': 'all parts/full archive/member SHA256 plus SQLite integrity; native app import untested'}


def archive_chain(folder):
    """Resolve immutable parent manifests from sibling run directories."""
    current = Path(folder).absolute()
    chain, seen = [], set()
    while True:
        if current in seen or len(chain) >= 1024:
            raise ValueError('cyclic or excessive backup chain')
        seen.add(current)
        manifest_path = regular_path(current / 'manifest.json')
        manifest = json.loads(manifest_path.read_text())
        if manifest.get('format') != 'contrail-native-history-v1':
            raise ValueError('unsupported manifest format')
        chain.append((current, manifest))
        base = manifest.get('base')
        if not base:
            break
        safe_name(base['run_id'])
        if '/' in base['run_id']:
            raise ValueError('invalid base run ID')
        parent = current.parent / base['run_id']
        if digest_file(regular_path(parent / 'manifest.json')) != base['manifest_sha256']:
            raise ValueError('base manifest checksum mismatch')
        current = parent
    chain.reverse()
    catalog = {}
    initial = chain[0][1]
    for directory, manifest in chain:
        if manifest.get('home') != initial.get('home') or manifest.get('inventory', {}).get('scope') != initial.get('inventory', {}).get('scope'):
            raise ValueError('backup chain home/scope mismatch')
        if manifest.get('base') and manifest.get('run_id') != directory.name:
            raise ValueError('incremental run directory must match run ID')
        own_names = set()
        for record in manifest['files']:
            safe_name(record['name'])
            if record['name'] in own_names or record['bytes'] < 0:
                raise ValueError('duplicate or invalid manifest member')
            own_names.add(record['name'])
            catalog[record['name']] = {**record, 'origin_run_id': manifest.get('run_id', directory.name)}
        if 'catalog' in manifest and manifest['catalog'] != sorted(catalog.values(), key=lambda r: r['name']):
            raise ValueError('incremental catalog does not match backup chain')
    return chain, catalog


def verified_base(folder):
    chain, catalog = archive_chain(folder)
    folder, manifest = chain[-1]
    receipt_path = folder / 'verification.json'
    receipt = json.loads(regular_path(receipt_path).read_text()) if receipt_path.exists() else {}
    bound = (receipt.get('status') == 'verified'
             and receipt.get('manifest_sha256') == digest_file(folder / 'manifest.json')
             and receipt.get('archive_sha256') == manifest['archive_sha256'])
    if not all((folder / part['name']).is_file() for part in manifest['parts']):
        # Released baselines need remote checksums tied to the retained manifest
        # and verification bytes, including older receipts without manifest_sha256.
        verified_cloud_archive(folder, manifest, allow_released_size_only=True)
    elif not bound:
        # Verify legacy local bytes without changing metadata already uploaded.
        check_cloud_parents(folder, require_remote=False)
        receipt = verify(folder, include_base=False)
    for directory, parent_manifest in chain[:-1]:
        if not all((directory / part['name']).is_file() for part in parent_manifest['parts']):
            verified_cloud_archive(directory, parent_manifest, allow_released_size_only=True)
    return manifest, catalog


def verify(folder, extract_to=None, max_bytes=64 * 1024**3, include_base=True):
    chain, catalog = archive_chain(folder)
    if sum(record['bytes'] for record in catalog.values()) > max_bytes:
        raise ValueError('expanded backup chain exceeds verification budget')
    if extract_to:
        target = Path(extract_to).absolute()
        for ancestor in [target, *target.parents]:
            if ancestor.is_symlink():
                raise ValueError('symlink extraction destination is unsupported')
        if target.exists() and any(target.iterdir()):
            raise ValueError('chain extraction destination must be empty')
    selected = chain if include_base else chain[-1:]
    for index, (directory, manifest) in enumerate(selected):
        receipt = verify_archive(directory, extract_to, max_bytes, overlay=index > 0)
    receipt.update({'files': len(catalog), 'stored_files': len(chain[-1][1]['files']),
                    'chain_runs': len(chain), 'base_archives_checked': include_base,
                    'manifest_sha256': digest_file(Path(folder).absolute() / 'manifest.json')})
    return receipt


def backup(home, output, inventory=None, part_bytes=PART_LIMIT, scratch_bytes=18 * 1024**3, output_bytes=12 * 1024**3, base_run=None,
           cloud_parent_id=None, cloud_owner=None):
    if not 0 < part_bytes <= PART_LIMIT:
        raise ValueError('parts must be between 1 byte and 80 MiB')
    destination = cloud_destination(cloud_parent_id, cloud_owner)
    home, output = Path(home).absolute(), Path(output).absolute()
    # Do not allow a backup destination within a live app store.
    for live in (home / '.codex', home / '.cursor', home / '.claude', home / 'Library/Application Support/Cursor'):
        if output == live or live in output.parents:
            raise ValueError('output must be outside live app stores')
    for ancestor in [output, *output.parents]:
        if ancestor.is_symlink():
            raise ValueError('symlink output is unsupported')
    output.mkdir(parents=True, exist_ok=True)
    lock = output / '.native-history.lock'
    descriptor = os.open(lock, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.write(descriptor, json.dumps({'pid': os.getpid(), 'started_utc': utc()}).encode())
    os.close(descriptor)
    run = None
    completed = False
    writer = None
    try:
        inventory = inventory if inventory is not None else discover(home)
        prior, catalog = verified_base(base_run) if base_run else (None, {})
        if prior and (prior['home'] != str(home) or prior.get('inventory', {}).get('scope') != inventory.get('scope')):
            raise ValueError('incremental base home/scope mismatch')
        if base_run and Path(base_run).absolute().parent != output:
            raise ValueError('incremental base must be a sibling of output runs')
        prior_names = set(catalog)
        run_id = datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H%M%S.%fZ') + '-' + uuid.uuid4().hex[:8]
        run = output / run_id
        run.mkdir(mode=0o700)
        records = []
        sources = sorted(inventory['sources'], key=lambda s: s['relative'])
        names = [s['relative'] for s in sources]
        if len(names) != len(set(names)):
            raise ValueError('duplicate source paths')
        free = shutil.disk_usage(output).free
        if free < scratch_bytes + output_bytes + 256 * 1024**2:
            raise ValueError('insufficient disk headroom for configured scratch/output budgets; lower budgets or free space')
        writer = PartWriter(run, part_bytes, output_bytes)
        last_progress = time.monotonic()
        if len(sources) > 1000:
            progress_event('native_snapshot_started', total_files=len(sources))
        with tempfile.TemporaryDirectory(prefix='snapshot-', dir=run) as scratch:
            with gzip.GzipFile(fileobj=writer, mode='wb', filename='', mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode='w|', format=tarfile.PAX_FORMAT) as archive:
                    for source in sources:
                        safe_name(source['relative'])
                        snapshot = Path(scratch) / 'file'
                        try:
                            boundary = snapshot_file(source, snapshot, scratch_bytes)
                        except (OSError, ValueError, sqlite3.Error) as error:
                            raise ValueError('snapshot failed for ' + source['app'] + ' ' + source['relative'] + ': ' + str(error)) from error
                        record = {'name': source['relative'], 'source': source['path'], 'app': source['app'],
                                  'sqlite': source['sqlite'], 'bytes': snapshot.stat().st_size,
                                  'sha256': digest_file(snapshot), **boundary}
                        previous = catalog.get(record['name'])
                        if previous and all(previous[key] == record[key] for key in ('sha256', 'bytes', 'app', 'sqlite')):
                            snapshot.unlink()
                            continue
                        catalog[record['name']] = {**record, 'origin_run_id': run_id}
                        info = tarfile.TarInfo(record['name'])
                        info.size, info.mode, info.mtime = record['bytes'], 0o600, 0
                        with open(snapshot, 'rb') as data:
                            archive.addfile(info, data)
                        records.append(record)
                        snapshot.unlink()
                        if len(sources) > 1000 and time.monotonic() - last_progress >= 30:
                            progress_event('native_snapshot_progress', app=source['app'], completed_files=len(records), total_files=len(sources), archive_bytes=writer.total)
                            last_progress = time.monotonic()
        writer.finish_part()
        manifest = {'format': 'contrail-native-history-v1', 'run_id': run_id, 'created_utc': utc(),
                    'home': str(home), 'files': records, 'parts': writer.parts, 'archive_bytes': writer.total,
                    'archive_sha256': writer.archive_hash.hexdigest(), 'inventory': inventory,
                    'boundary': 'per-file/per-database snapshots, not globally atomic; discovered paths frozen before snapshot',
                    'retention': '30 days since last activity; native apply separately guarded'}
        if destination:
            manifest['cloud_destination'] = destination
        manifest['comparison'] = {'new_files': sum(r['name'] not in prior_names for r in records),
                                  'changed_files': sum(r['name'] in prior_names for r in records),
                                  'unchanged_files': len(sources) - len(records),
                                  'missing_local_preserved': len(prior_names - set(names))}
        manifest['backup_kind'] = 'incremental' if prior else 'full'
        manifest['catalog'] = sorted(catalog.values(), key=lambda r: r['name'])
        if prior:
            manifest['base'] = {'run_id': prior['run_id'], 'manifest_sha256': digest_file(Path(base_run) / 'manifest.json')}
        write_json(run / 'manifest.json', manifest)
        (run / 'RESTORE.md').write_text(
            '# Native history recovery\n\nFor full recovery, download every referenced base run and this run into sibling folders named by run ID, with each run’s numbered parts and manifest.json together. '
            'Run `contrail history verify --run FOLDER`, then '
            '`contrail history extract --run FOLDER --destination NEW_EMPTY_PATH`. '
            'Extraction verifies every byte and SQLite integrity. It never overwrites a profile.\n\n'
            'For selected files, use `contrail history catalog --run FOLDER`, then '
            '`contrail history restore-plan --run FOLDER --member EXACT_NAME`. Planning reads only metadata and does not download. '
            'Keep the complete manifest chain, but fetch only the origin runs listed by the plan, with all their parts. '
            '`contrail history extract --run FOLDER --member EXACT_NAME --destination NEW_EMPTY_PATH` '
            'verifies those archives and extracts selected latest files. Repeat --member for known dependencies. '
            'A database can contain multiple chats; selection is by file, not conversation.\n\n'
            'Files are located under their original home-relative paths. Preserve all files together, '
            'including transcript tools/media dependencies and indexes. Native per-chat import and full '
            'profile recovery have NOT been tested. For version-specific profile recovery, first preserve '
            'current state and quit every app writer; never mix restored databases with current WAL/SHM. '
            'Credentials are excluded; re-authentication may be required. Consult manifest inventory '
            'for exclusions and unsupported sources. Snapshot versions/boundaries are recorded per run.\n')
        receipt = verify(run, include_base=False)
        write_json(run / 'verification.json', receipt)
        if destination:
            handoff(run, include_base=False)
        completed = True
        return {'run': str(run), **receipt, 'comparison': manifest['comparison'],
                'cloud_status': 'pending upload; local parts retained' if destination else 'cloud destination unconfigured; local parts retained'}
    finally:
        if writer:
            writer.finish_part()
        if run and not completed:
            shutil.rmtree(run)
        lock.unlink(missing_ok=True)


def release_bindings(directory):
    return {name: digest_file(regular_path(directory / name))
            for name in ('manifest.json', 'cloud-receipt.json', 'cloud-handoff.json')}


def authorized_size_only_release(directory, manifest):
    path = directory / 'local-release.json'
    if not path.exists():
        return False
    release = json.loads(regular_path(path).read_text())
    if (release.get('release_policy') != 'provider-upload-integrity'
            or release.get('accept_size_only') is not True
            or release.get('verification_level') != 'size-only'):
        return False
    if (release.get('bindings') != release_bindings(directory)
            or release.get('removed_parts') != [part['name'] for part in manifest['parts']]):
        raise ValueError('size-only release authorization evidence changed')
    return True


def check_local_verification(directory, manifest):
    verification = json.loads(regular_path(directory / 'verification.json').read_text())
    if (verification.get('status') != 'verified'
            or verification.get('archive_sha256') != manifest['archive_sha256']
            or ('manifest_sha256' in verification
                and verification['manifest_sha256'] != digest_file(directory / 'manifest.json'))):
        raise ValueError('verification does not match manifest')


def verified_cloud_archive(directory, manifest, accept_size_only=False, allow_released_size_only=False):
    receipt = json.loads(regular_path(directory / 'cloud-receipt.json').read_text())
    validated = validate_cloud(directory, receipt, allow_missing_parts=True)
    if validated['verification_level'] not in ('md5', 'sha256'):
        missing_parts = any(not (directory / part['name']).is_file() for part in manifest['parts'])
        authorized = authorized_size_only_release(directory, manifest)
        if (not (accept_size_only or (allow_released_size_only and authorized))
                or (missing_parts and not authorized)):
            raise ValueError('base remote bytes not checksum verified; retain local parts or require bound size-only authorization')
    check_local_verification(directory, manifest)


def check_cloud_parents(run, require_remote=True, accept_size_only=False):
    """Revalidate parent remote bytes, or available local bytes for a handoff."""
    for directory, manifest in archive_chain(run)[0][:-1]:
        if not require_remote and all((directory / p['name']).is_file() for p in manifest['parts']):
            verify_archive(directory)
            continue
        verified_cloud_archive(directory, manifest, accept_size_only=accept_size_only,
                               allow_released_size_only=not require_remote)


def handoff(run, include_base=True, cloud_parent_id=None, cloud_owner=None):
    run = Path(run).absolute()
    manifest = json.loads(regular_path(run / 'manifest.json').read_text())
    existing = manifest.get('cloud_destination')
    # Older runs recorded their configuration in the handoff. Preserve it when
    # regenerating evidence without requiring private defaults in the source.
    if not existing and (run / 'cloud-handoff.json').exists():
        previous = json.loads(regular_path(run / 'cloud-handoff.json').read_text())
        existing = {'parent_id': previous.get('parent_folder_id'), 'owner': previous.get('owner')}
    destination = cloud_destination(cloud_parent_id, cloud_owner, existing)
    if not destination:
        raise ValueError('configure --cloud-parent-id and --cloud-owner for cloud handoff')
    verify(run, include_base=False)
    if include_base:
        check_cloud_parents(run, require_remote=False)
    names = [p['name'] for p in manifest['parts']] + ['manifest.json', 'RESTORE.md', 'verification.json']
    files = [{'name': name, 'path': str(run / name), 'bytes': regular_path(run / name).stat().st_size,
              'sha256': digest_file(run / name), 'md5': digest_file(run / name, 'md5')} for name in names]
    data = {'run_id': manifest['run_id'], 'parent_folder_id': destination['parent_id'], 'owner': destination['owner'],
            'files': files, 'required_base_runs': [m['run_id'] for _, m in archive_chain(run)[0][:-1]], 'workflow': 'Verify private parent owner; create dated child; upload in listed order; '
            'read back parents/permissions/size/checksums; write cloud receipt. Retry by remote ID and checksum, '
            'never blindly duplicate a completed upload. Keep local parts until byte-level cloud verification.'}
    write_json(run / 'cloud-handoff.json', data)
    return data


def validate_cloud(run, receipt, allow_missing_parts=False):
    run = Path(run).absolute()
    handoff_data = json.loads(regular_path(run / 'cloud-handoff.json').read_text())
    if (not handoff_data.get('parent_folder_id') or not handoff_data.get('owner')
            or receipt.get('parent_folder_id') != handoff_data['parent_folder_id']
            or receipt.get('owner') != handoff_data['owner'] or receipt.get('run_id') != handoff_data['run_id']):
        raise ValueError('cloud receipt destination/owner/run mismatch')
    if receipt.get('private') is not True or not receipt.get('folder_id'):
        raise ValueError('private dated folder verification missing')
    expected = {f['name']: f for f in handoff_data['files']}
    manifest = json.loads(regular_path(run / 'manifest.json').read_text())
    names = {p['name'] for p in manifest['parts']} | {'manifest.json', 'RESTORE.md', 'verification.json'}
    if (set(expected) != names or len(expected) != len(handoff_data['files'])
            or handoff_data['run_id'] != manifest['run_id']):
        raise ValueError('cloud handoff does not correspond to manifest')
    parts = {p['name']: p for p in manifest['parts']}
    for name, local in expected.items():
        safe_name(name)
        part = parts.get(name)
        if part and any(local.get(key) != part.get(key) for key in ('bytes', 'sha256', 'md5')):
            raise ValueError('cloud handoff no longer matches local bytes')
        path = run / name
        if allow_missing_parts and part and not path.exists() and not path.is_symlink():
            continue
        path = regular_path(path)
        if path.stat().st_size != local['bytes'] or digest_file(path) != local['sha256'] or digest_file(path, 'md5') != local['md5']:
            raise ValueError('cloud handoff no longer matches local bytes')
    remote = receipt.get('files', [])
    if len(remote) != len(expected) or {f['name'] for f in remote} != set(expected):
        raise ValueError('incomplete or duplicate cloud receipt')
    levels = []
    for item in remote:
        local = expected[item['name']]
        if not item.get('id') or item.get('parent_id') != receipt['folder_id'] or item.get('bytes') != local['bytes'] or item.get('private') is not True:
            raise ValueError('cloud file metadata mismatch')
        for algorithm in ('sha256', 'md5'):
            if item.get(algorithm) and item[algorithm] != local[algorithm]:
                raise ValueError('remote checksum mismatch')
        if item.get('sha256') == local['sha256']:
            levels.append('sha256')
        elif item.get('md5') == local['md5']:
            levels.append('md5')
        else:
            if item.get('sha256') or item.get('md5'):
                raise ValueError('remote checksum mismatch')
            levels.append('size-only')
    receipt['verification_level'] = 'size-only' if 'size-only' in levels else ('md5' if 'md5' in levels else 'sha256')
    return receipt


def record_cloud(run, receipt_path):
    run = Path(run).absolute()
    receipt = validate_cloud(run, json.loads(regular_path(receipt_path).read_text()))
    receipt['recorded_utc'] = utc()
    receipt['authority'] = 'connector/operator supplied remote evidence; Contrail validates completeness and correspondence'
    write_json(run / 'cloud-receipt.json', receipt)
    return receipt


def release_local(run, accept_size_only=False):
    run = Path(run).absolute()
    policy = 'provider-upload-integrity' if accept_size_only else 'checksums-required'
    if (run / 'local-release.json').is_file():
        result = json.loads(regular_path(run / 'local-release.json').read_text())
        if any((run / name).exists() for name in result['removed_parts']):
            raise ValueError('released archive parts unexpectedly reappeared')
        if result.get('verification_level') == 'size-only':
            manifest = json.loads(regular_path(run / 'manifest.json').read_text())
            if not authorized_size_only_release(run, manifest):
                raise ValueError('size-only release authorization missing')
        return result
    manifest = json.loads((run / 'manifest.json').read_text())
    pending = run / 'local-release.pending.json'
    if pending.exists():
        intent = json.loads(regular_path(pending).read_text())
        if intent.get('release_policy', 'checksums-required') != policy:
            raise ValueError('interrupted release requires the same explicit release policy')
        if intent['bindings'] != release_bindings(run):
            raise ValueError('release evidence changed after interruption')
        if intent['parts'] != manifest['parts']:
            raise ValueError('release intent does not match manifest')
        receipt = validate_cloud(run, json.loads(regular_path(run / 'cloud-receipt.json').read_text()), allow_missing_parts=True)
        check_cloud_parents(run, accept_size_only=accept_size_only)
    else:
        receipt = record_cloud(run, run / 'cloud-receipt.json')
        if receipt.get('verification_level') not in ('md5', 'sha256') and not accept_size_only:
            raise ValueError('remote bytes not checksum verified; retain local parts')
        check_cloud_parents(run, accept_size_only=accept_size_only)
        # Handoff validation already binds the verification receipt, manifest,
        # and every part; avoid repeating SQLite extraction before removal.
        check_local_verification(run, manifest)
        intent = {'parts': manifest['parts'], 'bindings': release_bindings(run),
                  'release_policy': policy, 'accept_size_only': accept_size_only}
        write_json(pending, intent)
    if receipt.get('verification_level') not in ('md5', 'sha256') and not accept_size_only:
        raise ValueError('remote bytes not checksum verified; retain local parts')
    removed = []
    for part in manifest['parts']:
        safe_name(part['name'])
        if '/' in part['name'] or not part['name'].startswith('history.tar.gz.part'):
            raise ValueError('invalid release part name')
        path = run / part['name']
        if path.exists() or path.is_symlink():
            regular_path(path)
            if path.stat().st_size != part['bytes'] or digest_file(path) != part['sha256']:
                raise ValueError('remaining release part changed')
            path.unlink()
        removed.append(part['name'])
    data = {'released_utc': utc(), 'removed_parts': removed, 'cloud_receipt': 'cloud-receipt.json',
            'verification_level': receipt['verification_level'], 'native_histories_deleted': 0,
            'release_policy': policy, 'accept_size_only': accept_size_only, 'bindings': intent['bindings']}
    write_json(run / 'local-release.json', data)
    pending.unlink()
    return data


def extract(run, destination):
    destination = Path(destination).absolute()
    if destination.exists():
        raise ValueError('extraction destination must not exist')
    for ancestor in destination.parents:
        if ancestor.is_symlink():
            raise ValueError('symlink extraction parent is unsupported')
    destination.parent.mkdir(parents=True, exist_ok=True)
    verify(run)
    with tempfile.TemporaryDirectory(prefix='.contrail-extract-', dir=destination.parent) as temporary:
        staging = Path(temporary) / 'restored'
        staging.mkdir(mode=0o700)
        receipt = verify(run, extract_to=staging)
        # rename is atomic on the same filesystem; destination existence rechecked.
        if destination.exists():
            raise ValueError('extraction destination appeared during verification')
        os.rename(staging, destination)
    return {'destination': str(destination), **receipt}


def list_catalog(run, app=None, contains=None, limit=100, offset=0):
    """Browse retained file metadata without opening archives or live histories."""
    if type(limit) is not int or not 1 <= limit <= 1000 or type(offset) is not int or offset < 0:
        raise ValueError('catalog limit must be 1..1000 and offset must be nonnegative')
    chain, catalog = archive_chain(run)
    records = sorted((record for record in catalog.values()
                      if (app is None or record['app'] == app)
                      and (contains is None or contains in record['name'])),
                     key=lambda record: record['name'])
    page = records[offset:offset + limit]
    return {'run_id': chain[-1][1]['run_id'], 'chain_runs': len(chain),
            'matched_files': len(records), 'offset': offset, 'limit': limit,
            'next_offset': offset + len(page) if offset + len(page) < len(records) else None,
            'files': [{key: record[key] for key in ('name', 'app', 'bytes', 'sha256', 'sqlite', 'origin_run_id')}
                      for record in page],
            'verification': 'manifest chain correspondence only; archive payloads not read',
            'downloads_performed': 0}


def main():
    parser = argparse.ArgumentParser(prog='contrail history', description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ('inventory', 'dry-run', 'backup', 'retention-plan'):
        command = commands.add_parser(name)
        command.add_argument('--home', type=Path, default=Path.home())
        if name == 'inventory':
            command.add_argument('--summary', action='store_true')
        if name == 'backup':
            command.add_argument('--output', type=Path, required=True)
            command.add_argument('--base-run', type=Path, help='prior verified sibling run; store only new or changed files')
            command.add_argument('--scratch-mib', type=int, default=18432)
            command.add_argument('--output-mib', type=int, default=12288)
            command.add_argument('--part-mib', type=int, default=80)
            command.add_argument('--cloud-parent-id', help='private Drive parent folder ID; defaults to CONTRAIL_HISTORY_CLOUD_PARENT')
            command.add_argument('--cloud-owner', help='expected private Drive owner; defaults to CONTRAIL_HISTORY_CLOUD_OWNER')
            command.add_argument('--app', action='append', choices=('codex', 'cursor-cli', 'cursor-desktop', 'claude-code'), help='explicit partial app scope; omit for all installed apps')
    for name in ('verify', 'extract', 'restore-plan', 'catalog', 'cloud-handoff', 'record-cloud', 'release-local'):
        command = commands.add_parser(name)
        command.add_argument('--run', type=Path, required=True)
        if name == 'extract':
            command.add_argument('--destination', type=Path, required=True)
            command.add_argument('--member', action='append', help='exact catalog name; repeat to extract selected files only')
            command.add_argument('--max-expanded-mib', type=int, default=65536,
                                 help='selected recovery: expanded byte budget across required origin archives')
        if name == 'restore-plan':
            command.add_argument('--member', action='append', required=True,
                                 help='exact catalog name; repeat to plan selected files without downloading')
        if name == 'catalog':
            command.add_argument('--app', choices=('codex', 'cursor-cli', 'cursor-desktop', 'claude-code'))
            command.add_argument('--contains', help='case-sensitive substring of home-relative file name')
            command.add_argument('--limit', type=int, default=100)
            command.add_argument('--offset', type=int, default=0)
        if name == 'cloud-handoff':
            command.add_argument('--cloud-parent-id', help='private Drive parent folder ID; defaults to recorded configuration or CONTRAIL_HISTORY_CLOUD_PARENT')
            command.add_argument('--cloud-owner', help='expected private Drive owner; defaults to recorded configuration or CONTRAIL_HISTORY_CLOUD_OWNER')
        if name == 'record-cloud':
            command.add_argument('--receipt', type=Path, required=True)
        if name == 'release-local':
            command.add_argument('--accept-size-only', action='store_true',
                                 help='explicitly trust provider upload integrity after private metadata and size confirmation')
    commands.add_parser('retention-apply', help='fails closed until native protection/apply support is available')
    args = parser.parse_args()
    if args.command in ('inventory', 'dry-run'):
        result = discover(args.home)
        result['source_bytes'] = sum(Path(s['path']).stat().st_size for s in result['sources'])
        result['source_count'] = len(result['sources'])
        result['app_counts'] = {app: sum(s['app'] == app for s in result['sources']) for app in ('codex', 'cursor-cli', 'cursor-desktop', 'claude-code')}
        if args.command == 'dry-run' or args.summary:
            result.pop('sources')
    elif args.command == 'backup':
        inventory = discover(args.home)
        inventory['scope'] = args.app or 'all installed default-profile apps'
        if args.app:
            inventory['sources'] = [s for s in inventory['sources'] if s['app'] in args.app]
            inventory['unsupported'].append('Partial backup: unselected apps are not covered by this run')
        result = backup(args.home, args.output, inventory, part_bytes=args.part_mib * 1024**2,
                        scratch_bytes=args.scratch_mib * 1024**2, output_bytes=args.output_mib * 1024**2, base_run=args.base_run,
                        cloud_parent_id=args.cloud_parent_id, cloud_owner=args.cloud_owner)
    elif args.command == 'retention-plan':
        result = retention_plan(args.home, discover(args.home))
    elif args.command == 'retention-apply':
        raise ValueError('native apply blocked: cannot prove running/pinned/automation/fork protection through supported interfaces; no histories deleted')
    elif args.command == 'verify':
        result = verify(args.run)
    elif args.command == 'extract':
        if args.member:
            result = extract_selected(args.run, args.destination, args.member,
                                      max_bytes=args.max_expanded_mib * 1024**2)
        else:
            if args.max_expanded_mib != 65536:
                raise ValueError('--max-expanded-mib requires --member')
            result = extract(args.run, args.destination)
    elif args.command == 'restore-plan':
        result = restore_plan(args.run, args.member)
    elif args.command == 'catalog':
        result = list_catalog(args.run, args.app, args.contains, args.limit, args.offset)
    elif args.command == 'cloud-handoff':
        result = handoff(args.run, cloud_parent_id=args.cloud_parent_id, cloud_owner=args.cloud_owner)
    elif args.command == 'record-cloud':
        result = record_cloud(args.run, args.receipt)
    else:
        result = release_local(args.run, accept_size_only=args.accept_size_only)
    print(json.dumps(result, indent=2, sort_keys=True))


if 'restore_plan' not in globals():
    from native_restore import restore_plan, extract_selected


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, sqlite3.Error, tarfile.TarError, EOFError, KeyError, TypeError) as error:
        print(json.dumps({'status': 'failed', 'error': str(error)}), file=sys.stderr)
        sys.exit(1)
