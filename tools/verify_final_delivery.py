"""Verify physical assets against the pinned final delivery; never rewrite hashes.

The lock is an acceptance oracle, not evidence of a working source build recipe.
Programs, screenshots and reports outside assets/ are deliberately not compared.
"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath

from asset_reproducibility import digest

ROOT=Path(__file__).resolve().parents[1]
EXPECTED_MANIFEST='7c08cff36060b27bf7079917b4f984fe98c5dffe8cd6f9fd9859ee10ef59759d'
EXPECTED_LOCK='3544c657b314d7d54151066c7eab4268077e11ddde8a1be8bb3f34055a00b944'


def verify(assets, lock, *, expected_manifest=EXPECTED_MANIFEST, expected_lock=EXPECTED_LOCK):
    canonical=json.dumps(lock,sort_keys=True,separators=(',',':'),ensure_ascii=True).encode()
    if expected_lock is not None and hashlib.sha256(canonical).hexdigest()!=expected_lock:
        raise ValueError('Delivery lock changed; cannot replace the pinned acceptance baseline')
    if lock.get('schema')!='knife-final-assets-lock-v1':raise ValueError('Unsupported delivery lock')
    files=lock['files']
    if not files or files.get('vita-540p-assets.json',{}).get('sha256')!=expected_manifest:
        raise ValueError('Lock does not describe the pinned final delivery')
    for name in files:
        p=PurePosixPath(name)
        if p.is_absolute() or '..' in p.parts or '\\' in name or ':' in name:
            raise ValueError('Unsafe lock path')
    # A different candidate can be rejected by reading only the small manifest.
    if digest(assets/'vita-540p-assets.json')!=expected_manifest:
        raise ValueError('FINAL_DELIVERY_MISMATCH: manifest differs; not the requested final pack')
    actual=set()
    for p in assets.rglob('*'):
        if p.is_symlink() or (hasattr(p,'is_junction') and p.is_junction()):
            raise ValueError('Asset links are not allowed in exact delivery verification')
        if p.is_file():actual.add(p.relative_to(assets).as_posix())
    if actual!=set(files):
        raise ValueError('FINAL_DELIVERY_MISMATCH: file set changed; missing='+
                         str(sorted(set(files)-actual))+' extra='+str(sorted(actual-set(files))))
    for name,row in sorted(files.items()):
        p=assets/name
        if p.stat().st_size!=row['bytes'] or digest(p)!=row['sha256']:
            raise ValueError('FINAL_DELIVERY_MISMATCH: '+name)
    return dict(physical_files=len(files),physical_bytes=sum(r['bytes'] for r in files.values()),
                exact_delivery_match=True,source_recipe_verified=False)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets',type=Path,required=True)
    parser.add_argument('--lock',type=Path,default=ROOT/'doc/evidence/59-final-assets-lock.json')
    args=parser.parse_args()
    print(json.dumps(verify(args.assets,json.loads(args.lock.read_bytes())),indent=2))
