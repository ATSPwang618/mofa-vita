"""Shared fail-closed boundaries for recovery builds and texture caches.

A historical output hash is not a source recipe. The currently implemented BASE
recovery generator has no update/mod production chain, so no JSON assertion can
promote its output to a source-reproducible release.
"""
import hashlib
import json
from pathlib import Path


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def recovery_scope(contract, allow_candidate=False):
    if not allow_candidate:
        raise ValueError(
            'SOURCE_REPRODUCTION_UNPROVEN: BASE recovery does not implement the '
            'BASE+update+MOD production chain or reproduce the historical image recipe. '
            'No extraction started. For an independent experimental candidate only, '
            'explicitly use --allow-recovery-candidate. This is not a release override.')
    return dict(schema='knife-reproduction-scope-v1',
                status='recovery-candidate-only',
                source_reproducible_release=False,
                historical_visual_equivalence_verified=False,
                source_manifest_sha256=contract.get('source_manifest_sha256'),
                missing=['update/mod production chain', 'historical image recipe closure'])


def python_sources(directory):
    directory = Path(directory)
    return {p.relative_to(directory).as_posix(): digest(p)
            for p in sorted(directory.rglob('*.py')) if '__pycache__' not in p.parts}


def verify_cached_payload(data, record):
    if (len(data)!=record.get('payload_bytes') or
            hashlib.sha256(data).hexdigest()!=record.get('payload_sha256')):
        raise ValueError('Cached texture payload missing identity or corrupted')


def locked_cache(root, identity):
    """Separate caches by the complete encoder recipe; never consume legacy entries."""
    encoded = (json.dumps(identity, sort_keys=True, separators=(',', ':'))+'\n').encode()
    namespace = hashlib.sha256(encoded).hexdigest()
    root = Path(root).resolve()
    directory = root/namespace
    if directory.is_symlink() or (hasattr(directory, 'is_junction') and directory.is_junction()):
        raise ValueError('Cache namespace must not be a link')
    marker = directory/'recipe.json'
    if directory.exists():
        if not marker.is_file() or marker.read_bytes() != encoded:
            raise ValueError('Cache recipe missing or modified: '+str(directory))
    else:
        directory.mkdir(parents=True)
        marker.write_bytes(encoded)
    return directory, namespace
