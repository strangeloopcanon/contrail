"""Selective recovery from retained metadata, without cloud downloads or app writes."""

import os
from pathlib import Path
import re
import tempfile


def _restore_helper(name):
    # The installed Rust CLI concatenates this module before native_history.
    # Checkout execution imports it normally; defer the import to avoid a cycle.
    if name in globals():
        return globals()[name]
    import native_history
    return getattr(native_history, name)


def _restore_json(path):
    import json
    return json.loads(_restore_helper('regular_path')(path).read_text())


def _restore_size(value, positive=False):
    if type(value) is not int or value < (1 if positive else 0):
        raise ValueError('invalid restore manifest size')
    return value


def _restore_hash(value):
    if not isinstance(value, str) or re.fullmatch(r'[0-9a-f]{64}', value) is None:
        raise ValueError('invalid restore manifest SHA256')
    return value


def _restore_name(value):
    if not isinstance(value, str):
        raise ValueError('invalid restore member name')
    return _restore_helper('safe_name')(value)


def _restore_context(run, members):
    if isinstance(members, (str, bytes)) or members is None:
        raise ValueError('select at least one exact archive member name')
    members = list(members)
    if not members:
        raise ValueError('select at least one exact archive member name')
    for name in members:
        _restore_name(name)
    if len(set(members)) != len(members):
        raise ValueError('duplicate restore selection')
    try:
        chain, catalog = _restore_helper('archive_chain')(run)
        origins = {}
        for directory, manifest in chain:
            origin = manifest.get('run_id', directory.name)
            _restore_name(origin)
            if '/' in origin or origin != directory.name or origin in origins:
                raise ValueError('invalid or duplicate archive origin run ID')
            records = manifest['files']
            if not isinstance(records, list):
                raise ValueError('invalid restore manifest files')
            names = set()
            for record in records:
                _restore_name(record['name'])
                if record['name'] in names or type(record['sqlite']) is not bool:
                    raise ValueError('duplicate or invalid restore manifest member')
                names.add(record['name'])
                _restore_size(record['bytes'])
                _restore_hash(record['sha256'])
            parts = manifest['parts']
            if not isinstance(parts, list) or not parts:
                raise ValueError('missing archive parts')
            for index, part in enumerate(parts, 1):
                if (part['name'] != f'history.tar.gz.part{index:05d}'
                        or _restore_size(part['bytes'], positive=True) > _restore_helper('PART_LIMIT')):
                    raise ValueError('invalid part order or size')
                _restore_hash(part['sha256'])
            if sum(part['bytes'] for part in parts) != _restore_size(manifest['archive_bytes']):
                raise ValueError('invalid archive byte total')
            _restore_hash(manifest['archive_sha256'])
            origins[origin] = (directory, manifest)
        unknown = sorted(set(members) - set(catalog))
        if unknown:
            raise ValueError('unknown archive member: ' + ', '.join(unknown))
        selected = [catalog[name] for name in sorted(members)]
        for record in selected:
            origin = record['origin_run_id']
            if origin not in origins:
                raise ValueError('selected member has unknown archive origin')
            own = [item for item in origins[origin][1]['files'] if item['name'] == record['name']]
            if len(own) != 1 or {**own[0], 'origin_run_id': origin} != record:
                raise ValueError('selected member does not match its origin manifest')
        return chain, origins, selected
    except (KeyError, TypeError, AttributeError) as error:
        raise ValueError('malformed restore manifest or catalog') from error


def _restore_remote_parts(directory, manifest):
    """Read historical location hints, never imply current remote verification."""
    receipt_path = directory / 'cloud-receipt.json'
    if not receipt_path.exists() and not receipt_path.is_symlink():
        return {}
    receipt = _restore_json(receipt_path)
    try:
        if (receipt.get('run_id') != manifest.get('run_id', directory.name)
                or not isinstance(receipt.get('folder_id'), str) or not receipt['folder_id']
                or receipt.get('private') is not True):
            raise ValueError('cloud receipt location does not match archive origin')
        files = receipt['files']
        if not isinstance(files, list):
            raise ValueError('invalid cloud receipt files')
        by_name = {}
        for item in files:
            name = item['name']
            _restore_name(name)
            if name in by_name:
                raise ValueError('duplicate cloud receipt file')
            by_name[name] = item
        result = {}
        for part in manifest['parts']:
            item = by_name.get(part['name'])
            if item is None:
                continue
            if (not isinstance(item.get('id'), str) or not item['id']
                    or item.get('bytes') != part['bytes']
                    or item.get('parent_id') != receipt['folder_id']
                    or item.get('private') is not True
                    or (item.get('sha256') and item['sha256'] != part['sha256'])
                    or (item.get('md5') and item['md5'] != part.get('md5'))):
                raise ValueError('cloud receipt part location does not match manifest')
            result[part['name']] = {'cloud_file_id': item['id'], 'cloud_folder_id': receipt['folder_id']}
        return result
    except (KeyError, TypeError, AttributeError) as error:
        raise ValueError('malformed cloud receipt location metadata') from error


def restore_plan(run, members):
    """Resolve exact latest members and required archives using local metadata only.

    A gzip archive must be read in full even when only one file is requested.
    ``expanded_bytes`` is output size; ``verification_expanded_bytes`` includes
    every member in the required origin archives and governs extraction budget.
    Cloud IDs are historical receipt hints, not evidence of current availability.
    """
    chain, _, selected = _restore_context(run, members)
    wanted_origins = {record['origin_run_id'] for record in selected}
    required = []
    unavailable = []
    for directory, manifest in chain:
        origin = manifest.get('run_id', directory.name)
        if origin not in wanted_origins:
            continue
        remote = _restore_remote_parts(directory, manifest)
        parts = []
        for part in manifest['parts']:
            path = directory / part['name']
            available = path.exists() or path.is_symlink()
            if available:
                path = _restore_helper('regular_path')(path)
                if path.stat().st_size != part['bytes']:
                    raise ValueError('local restore part size mismatch')
            entry = {**part, 'path': str(path), 'available_locally': available,
                     **remote.get(part['name'], {})}
            parts.append(entry)
            if not available:
                unavailable.append({'run_id': origin, **entry})
        required.append({'run_id': origin, 'path': str(directory),
                         'archive_sha256': manifest['archive_sha256'],
                         'archive_bytes': manifest['archive_bytes'],
                         'expanded_bytes': sum(item['bytes'] for item in manifest['files']),
                         'parts': parts})
    return {'run_id': chain[-1][1].get('run_id', chain[-1][0].name),
            'files': len(selected), 'selected_files': selected,
            'expanded_bytes': sum(record['bytes'] for record in selected),
            'verification_expanded_bytes': sum(origin['expanded_bytes'] for origin in required),
            'archive_bytes': sum(origin['archive_bytes'] for origin in required),
            'required_runs': required, 'unavailable_local_parts': unavailable,
            'downloads_performed': 0, 'native_import_performed': False,
            'remote_locations': 'historical cloud receipt hints; current availability and permissions unverified',
            'verification': 'manifest chain and catalog checked; archive payload not read',
            'recovery': 'restore selected files into a fresh directory; native app import and resume untested'}


def extract_selected(run, destination, members, max_bytes=64 * 1024**3):
    """Validate required origin archives and atomically expose selected files.

    The budget covers all expanded member bytes across required archives, not
    only selected output. This bounds work when selecting from a large archive.
    Missing payloads must be supplied explicitly; recovery never downloads them.
    """
    _restore_size(max_bytes, positive=True)
    destination = Path(destination).absolute()
    for ancestor in [destination, *destination.parents]:
        if ancestor.is_symlink():
            raise ValueError('symlink extraction destination is unsupported')
    if destination.exists():
        raise ValueError('extraction destination must not exist')
    plan = restore_plan(run, members)
    if plan['verification_expanded_bytes'] > max_bytes:
        raise ValueError('expanded origin archives exceed restore verification budget')
    if plan['unavailable_local_parts']:
        raise ValueError('required archive parts are unavailable locally; inspect restore-plan and supply only its required parts')
    destination.parent.mkdir(parents=True, exist_ok=True)
    verified = []
    with tempfile.TemporaryDirectory(prefix='.contrail-select-', dir=destination.parent) as temporary:
        staging = Path(temporary) / 'restored'
        staging.mkdir(mode=0o700)
        for origin in plan['required_runs']:
            names = {record['name'] for record in plan['selected_files']
                     if record['origin_run_id'] == origin['run_id']}
            receipt = _restore_helper('verify_archive')(
                origin['path'], extract_to=staging, max_bytes=max_bytes, members=names)
            if receipt.get('archive_sha256') != origin['archive_sha256']:
                raise ValueError('verified archive no longer matches restore plan')
            verified.append({'run_id': origin['run_id'], **receipt})
        # Bind staged bytes to the original selection, including a manifest that
        # changed during verification and was subsequently put back.
        for record in plan['selected_files']:
            target = _restore_helper('regular_path')(staging / record['name'])
            if (target.stat().st_size != record['bytes']
                    or _restore_helper('digest_file')(target) != record['sha256']):
                raise ValueError('staged member no longer matches restore plan')
        # Catalog metadata and selection must still agree after archive reads.
        if restore_plan(run, [record['name'] for record in plan['selected_files']]) != plan:
            raise ValueError('restore metadata changed during extraction')
        for ancestor in [destination, *destination.parents]:
            if ancestor.is_symlink():
                raise ValueError('symlink extraction destination is unsupported')
        if destination.exists():
            raise ValueError('extraction destination appeared during verification')
        # mkdir is an atomic no-replace reservation. A plain exists+rename can
        # otherwise replace a concurrently created empty destination directory.
        destination.mkdir(mode=0o700)
        reserved = destination.stat()
        published = False
        try:
            current = destination.lstat()
            if (current.st_dev, current.st_ino) != (reserved.st_dev, reserved.st_ino):
                raise ValueError('extraction destination reservation changed')
            os.rename(staging, destination)
            published = True
        finally:
            if not published:
                # Only remove our own empty reservation, never another writer's
                # directory, symlink, or any contents added after reservation.
                try:
                    current = destination.lstat()
                    if (current.st_dev, current.st_ino) == (reserved.st_dev, reserved.st_ino):
                        destination.rmdir()
                except OSError:
                    pass
    return {'status': 'extracted', 'destination': str(destination),
            'files': plan['files'], 'expanded_bytes': plan['expanded_bytes'],
            'verification_expanded_bytes': plan['verification_expanded_bytes'],
            'verified_runs': verified, 'downloads_performed': 0,
            'native_import_performed': False,
            'verification': 'required origin archives verified; selected latest file bytes restored; native app import and resume untested'}
