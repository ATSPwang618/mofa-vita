"""Bounded reconstruction smoke test using actual NSP-derived RomFS inputs.

Does not assert visual equivalence or produce a playable pack. Records selected
base/variant/thumbnail encoding checks and one audio decode/re-encode per bank.
"""
import argparse
import json
from pathlib import Path
import time

import build_recovered_runtime as build
from vita_image_metadata import image_info


def run(args):
    kit=json.loads((args.kit/'kit.json').read_bytes())
    recipe=json.loads((args.kit/'recipe.json').read_bytes())
    args.output.mkdir(parents=True,exist_ok=False)
    build.initialize(args.romfs,args.kit,str(args.python_deps) if args.python_deps else None)
    results=dict(originals=[],variants=[],previews=[],audio=[])
    def record(name,data,start):
        raw=build.texture.unpack(data)
        info=image_info(raw) if raw[:4]==b'KIMG' else dict(magic=raw[:8].hex())
        return dict(name=name,bytes=len(data),seconds=round(time.monotonic()-start,4),info=info)
    for base,archive in recipe['archives'].items():
        seen=set()
        for i,r in enumerate(archive['originals']):
            group=(r.get('codec'),r.get('png_color_type'),r.get('format'),
                   r['name'].split('.')[0] in build.original.strip_names())
            if group in seen:continue
            seen.add(group);start=time.monotonic()
            name,data,_=build.original_job((base,i,r))
            item=record(name,data,start)
            if 'physical' in item['info']:assert item['info']['physical']==r['physical']
            results['originals'].append(dict(archive=base,index=i,**item))
        seen=set()
        for r in archive['variants']:
            group=(r['tiled'],r['language'],bool(r['grade']))
            if group in seen:continue
            seen.add(group);start=time.monotonic()
            _,blocks,proof=build.variant_job((base,r,kit['proofs'].get(base+':'+r['name'])))
            for data,_,desc in blocks:
                raw=build.texture.unpack(data)
                if raw[:4]==b'KIMG':image_info(raw)
            results['variants'].append(dict(archive=base,name=r['name'],blocks=len(blocks),proof=proof,
                                           seconds=round(time.monotonic()-start,4)))
        print(base,'probed',flush=True)
    seen=set()
    for r in kit['previews']:
        stem=r['name'].split('.')[0][3:]
        group=(r['name'][0],r['name'][1:3]!='00','private_sha256' in r,stem in build.thumbs)
        if group in seen:continue
        seen.add(group);start=time.monotonic();name,data,_=build.preview_job(r)
        results['previews'].append(record(name,data,start))
    for bank,names in kit['banks'].items():
        name=next(n for n in names if n.endswith('.ogg'))
        source=args.romfs/'stream'/Path(name).with_suffix('.opus')
        target=args.output/name;start=time.monotonic()
        build.audio_job((source,target,args.ffmpeg))
        results['audio'].append(dict(bank=bank,name=name,bytes=target.stat().st_size,
                                    seconds=round(time.monotonic()-start,4)))
    (args.output/'probe.json').write_text(json.dumps(results,indent=2)+'\n','utf8')
    print({k:len(v) for k,v in results.items()})


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('romfs','kit','output'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--python-deps',type=Path)
    p.add_argument('--ffmpeg',default='ffmpeg')
    run(p.parse_args())
