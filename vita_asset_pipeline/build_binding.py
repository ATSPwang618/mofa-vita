"""Synchronize/check the committed asset manifest and compiled metadata.

Build-time checks require only Python's standard library, without game assets.
"""
import argparse
from hashlib import sha256
import json
from pathlib import Path

MANIFEST='knife/knife/data/vita-540p-assets.json'
RECEIPT='knife/knife/data/vita_asset_binding.json'
GENERATED=('knife/knife-platform-vita/src/asset_profile_540p.rs',
           'knife/knife/data/vita_atlas_index_540p.rs','knife/knife/data/vita_stream_index_540p.txt')


def digest(path):return sha256(path.read_bytes()).hexdigest()


def check(project,assets=None):
    receipt=json.loads((project/RECEIPT).read_bytes());identity=digest(project/MANIFEST)
    if identity!=receipt['manifest_sha256']:
        raise ValueError('Project asset manifest changed; synchronize the complete delivery first')
    if assets is not None and digest(assets/'vita-540p-assets.json')!=identity:
        raise ValueError('Delivered assets and project manifest differ; synchronize before VPK build')
    for name in GENERATED:
        path=project/name
        if digest(path)!=receipt['generated'][name] or identity not in path.read_text(encoding='utf-8'):
            raise ValueError('Stale generated asset metadata: '+name)
    return receipt


def synchronize(assets,project):
    from .production_driver import activate,load_module,MARKER
    activate()
    from generate_vita_asset_profile_540 import generate
    atlas=load_module('mofa_delivery_atlas','generate-vita-atlas-index.py')
    generated=generate(assets);atlas_text,summary=atlas.generate(assets)
    marker=(assets/MARKER).read_bytes();identity=sha256(marker).hexdigest()
    inventory=json.loads(marker)
    names=sorted(row['path'] for row in inventory['files'] if row['path'].startswith('stream/'))
    stream_text=f'# knife-stream-index-v1 {identity}\n'+''.join(name+'\n' for name in names)
    for relative,text in zip(GENERATED,(generated,atlas_text,stream_text)):
        path=project/relative;path.parent.mkdir(parents=True,exist_ok=True)
        if not path.exists() or path.read_bytes()!=text.encode('utf-8'):
            path.write_text(text,encoding='utf-8',newline='\n')
    (project/MANIFEST).write_bytes(marker)
    receipt=dict(schema='mofa-compiled-asset-binding-v1',manifest_sha256=identity,manifest=MANIFEST,
        generated={name:digest(project/name) for name in GENERATED},
        physical_layout='ux0:data/WitchontheHolyNight/assets',audio_layout='voice.mrg + bgm.mrg')
    (project/RECEIPT).write_text(json.dumps(receipt,indent=2)+'\n',encoding='utf-8')
    check(project,assets)
    print('Project manifest and compiled metadata synchronized:',identity,flush=True)
    return summary


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--project',type=Path,default=Path(__file__).resolve().parents[1])
    parser.add_argument('--assets',type=Path);parser.add_argument('--check',action='store_true',required=True)
    args=parser.parse_args();result=check(args.project.resolve(),args.assets.resolve() if args.assets else None)
    print('Asset binding verified:',result['manifest_sha256'])
