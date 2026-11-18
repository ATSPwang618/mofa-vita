"""Rebuild a 100% DXT runtime from original RomFS and an explicit private kit.

The kit supplies metadata plus translated/custom/proof-bound exceptions. All
other story images and previews are decoded from RomFS; audio is regenerated
from original Opus. This does NOT claim a pure NSP-only or byte-identical build.
Outputs never replace existing resources. Use resize_story_variants followed by
convert_story_pvrtc for the final 70% mixed stage.
"""
import argparse
from collections import deque
from concurrent.futures import ProcessPoolExecutor
import hashlib
import importlib.util
import io
import json
import math
from pathlib import Path
import shutil
import struct
import subprocess
import sys

import numpy as np
from PIL import Image

import resize_story_variants as texture
from rebuild_story_images import Originals, read_luts, rebuild, apply_lut, original
from recovery_kit import file_sha
from compact_media import nopus_to_ogg
from pack_audio_bank import pack
from generate_vita_asset_profile_540 import audio_bank_locations
from rebuild_bgm_lead import refresh as refresh_bgm_lead


def initialize(romfs, kit, deps, history_path=None):
    global originals, luts, kit_root, thumbs, history
    if deps:sys.path.insert(0,deps)
    import ispc_texcomp
    spec=importlib.util.spec_from_file_location('native_gxt',Path(__file__).with_name('migrate-vita-native-color.py'))
    codec=importlib.util.module_from_spec(spec);spec.loader.exec_module(codec)
    texture.gxt,texture.Image,texture.np,texture.encoder=codec,Image,np,ispc_texcomp
    originals=Originals(Path(romfs));kit_root=Path(kit);luts=read_luts(kit_root/'grade-luts.bin')
    from historical_images import load
    history=load(Path(history_path)) if history_path else {}
    thumbs={}
    r=originals.readers['jallpac']
    start=next(i for i in range(len(r.rows)-1) if r.name(i).startswith('THUMSP_TOP.'))
    for i in range(start+1,len(r.rows)-1):
        thumbs.setdefault(r.name(i).rsplit('.',1)[0],i)


def source_at(base,index):
    data=originals.readers[base].read(index)
    images=[image for _,leaf in original.leaves(data) for _,_,image,_ in original.textures(leaf)]
    if len(images)!=1:raise ValueError(f'Expected single image: {base}:{index}, found {len(images)}')
    return images[0]


def encode_image(image, contract, force_raw=False):
    if contract['codec']==9:
        return texture.encode(image,contract['logical'],force_raw)
    if contract['codec']==1:
        if 'jpeg_tables' not in contract:raise ValueError('Missing recovered JPEG quantization tables')
        out=io.BytesIO()
        image.convert('RGB').save(out,format='JPEG',qtables=contract['jpeg_tables'],
                                 subsampling=contract['jpeg_subsampling'],optimize=True,progressive=False)
        data=out.getvalue()
        raw=struct.pack('<4s7I',b'KIMG',1,*contract['logical'],*image.size,1,len(data))+data
        block,decoded=texture.stored(raw,force_raw)
        return block,decoded,None
    codec=7 if contract['codec']==7 else 2
    if contract.get('png_color_type') in (0,4):
        image=image.convert('L' if contract['png_color_type']==0 else 'LA')
    out=io.BytesIO();image.save(out,format='PNG',compress_level=9)
    data=out.getvalue()
    raw=struct.pack('<4s7I',b'KIMG',1,*contract['logical'],*image.size,codec,len(data))+data
    block,decoded=texture.stored(raw,force_raw)
    return block,decoded,None


def original_job(task):
    base,index,record=task
    r=originals.readers[base]
    if r.name(index).rsplit('.',1)[0]!=record['name'].rsplit('.',1)[0]:
        raise ValueError(f'Original ordinal/name mismatch {base}:{index}')
    if 'magic' in record:
        data=r.read(index)
        if texture.sha(data)!=record['stored_sha256']:
            raise ValueError(f'Non-image source mismatch {base}:{index}')
        return record['name'],data,r.rows[index][2]
    if (base,index) in history:
        from historical_images import rebuild as historical_rebuild
        return historical_rebuild(base,index,record,history,source_at,encode_image,texture)
    image=source_at(base,index)
    if (base in ('allpacml','allpachdml') or record['name'].rsplit('.',1)[0] in original.strip_names()) and image.height>=4 and record['logical'][1]*2==image.height:
        # Remove ja/zt; resize each retained language independently.
        w,h=record['physical'];target=Image.new('RGBA',(w,h))
        if h%2:raise ValueError('Odd bilingual height')
        for slot in (1,2):
            y=image.height*slot//4
            part=image.crop((0,y,image.width,y+image.height//4))
            target.paste(original.resize(part,(w,h//2)),(0,(slot-1)*h//2))
        image=target
    else:image=original.resize(image,tuple(record['physical']))
    data,decoded,_=encode_image(image,record,record['raw_storage'])
    return record['name'],data,decoded


def variant_job(task):
    base,key,proof=task
    if proof:
        blocks=[]
        for record in proof:
            data=(kit_root/'private/proofs'/record['sha256']).read_bytes()
            if texture.sha(data)!=record['sha256']:raise ValueError('Proof payload changed')
            raw=texture.unpack(data)
            desc=None
            if raw[:4]==b'KIMG':
                w,h=struct.unpack_from('<II',raw,16)
                fmt,payload=struct.unpack_from('<I',raw,84)[0],struct.unpack_from('<I',raw,48)[0]
                desc=(w,h,fmt,payload)
            blocks.append((data,record['decoded'],desc))
        return key,blocks,True
    image,_=rebuild(originals,base,key,luts,associated_alpha=True)
    if not key['tiled']:
        return key,[texture.encode(image,key['logical'],key['raw_storage'])],False
    # Separate low-memory source remains DXT and CPU-decodable.
    fallback_key=dict(key,physical=key['fallback']['physical'])
    fallback,_=rebuild(originals,base,fallback_key,luts,associated_alpha=True)
    blocks=[texture.encode(fallback,key['fallback']['logical'])]
    w,h=image.size
    for y in range(0,h,504):
        for x in range(0,w,504):
            tile=image.crop((max(0,x-4),max(0,y-4),min(w,x+508),min(h,y+508)))
            blocks.append(texture.encode(tile,tile.size))
    return key,blocks,False


def preview_job(record):
    if 'private_sha256' in record:
        data=(kit_root/'private/proofs'/record['private_sha256']).read_bytes()
        if texture.sha(data)!=record['private_sha256']:raise ValueError('Preview supplement changed')
        return record['name'],data,record['decoded']
    name=record['name'];stem=name.rsplit('.',1)[0]
    if stem[0] not in 'AO' or len(stem)<4:raise ValueError('Unknown preview name')
    grade=int(stem[1:3],16);source=stem[3:]
    if source in thumbs:
        image=source_at('jallpac',thumbs[source])
    else:
        base=next((b for b in ('allpachdml','jallpachd','allpacml','jallpac')
                   if source in originals.names[b]),None)
        if base is None:raise ValueError('Missing preview source: '+source)
        image=source_at(base,originals.names[base][source])
    image=original.resize(image,tuple(record['physical']))
    key=dict(grade=grade,bright_bits='3F800000',contrast_bits='3F800000')
    image=apply_lut(image,key,luts)
    if stem[0]=='O':image.putalpha(255)
    data,decoded,_=texture.encode(image,record['logical'])
    return name,data,decoded


def bounded(pool,fn,tasks,workers):
    queue=deque()
    for task in tasks:
        queue.append(pool.submit(fn,task))
        if len(queue)>=workers*2:yield queue.popleft().result()
    while queue:yield queue.popleft().result()


def finish_plain(writer):
    writer.file.close()
    rows=writer.rows+[(0xffffffff,0xffff,0xffff)]
    (writer.root/(writer.base+'.hed')).write_bytes(b''.join(struct.pack('<IHH',*row) for row in rows))
    (writer.root/(writer.base+'.nam')).write_bytes(writer.names+bytes(32))


def audio_job(task):
    source,destination,ffmpeg,*contracts=task
    data,channels,samples=nopus_to_ogg(Path(source).read_bytes())
    command=[ffmpeg,'-nostdin','-v','error','-i','pipe:0','-map_metadata','-1','-c:a','libopus',
             '-b:a','48k' if channels==1 else '80k','-vbr','on','-compression_level','8',
             '-application','audio','-ar','48000','-f','ogg',str(destination)]
    subprocess.run(command,input=data,check=True,capture_output=True,timeout=120)
    if contracts:
        from deterministic_ogg import restore
        destination=Path(destination)
        destination.write_bytes(restore(destination.read_bytes(),contracts[0]))
    return str(destination)


def build(args):
    kit=json.loads((args.kit/'kit.json').read_bytes())
    from asset_reproducibility import recovery_scope
    scope=recovery_scope(kit,getattr(args,'allow_recovery_candidate',False))
    if file_sha(args.kit/'recipe.json')!=kit['recipe_sha256'] or file_sha(args.kit/'grade-luts.bin')!=kit['luts_sha256']:
        raise ValueError('Kit metadata identity mismatch')
    recipe=json.loads((args.kit/'recipe.json').read_bytes())
    resolved={}
    if getattr(args,'mod',None) or getattr(args,'logo',None):
        if not (args.mod and args.logo):raise ValueError('Both --mod and --logo are required')
        from source_inputs import resolve
        resolved=resolve(args.romfs,args.mod,args.logo,kit)
    audio_contract={}
    if getattr(args,'audio_contract',None):
        audio_contract=json.loads(args.audio_contract.read_bytes())['streams']
    if args.output.exists():raise ValueError('Output must not exist')
    if not 1<=args.workers<=8:raise ValueError('workers must be 1..8')
    if shutil.disk_usage(args.output.parent).free<10*1024**3:raise ValueError('Need at least 10 GiB free')
    for rel,record in kit['files'].items():
        if rel in resolved or (audio_contract and rel.startswith('stream/')):continue
        source=(args.romfs if record['source']=='romfs' else args.kit/'private')/rel
        if not source.is_file() or source.stat().st_size!=record['bytes'] or file_sha(source)!=record['sha256']:
            raise ValueError('Missing/mismatched source '+rel)
    args.output.mkdir()
    report=dict(schema='knife-recovered-runtime-v1',complete=False,archives={},private_inputs=True,
                source_recipe_sha256=kit['recipe_sha256'],hardware_tested=False,
                reproduction=scope,byte_identical_to_reference=False)
    report_path=args.output.parent/'rebuild-report.json'
    def save():report_path.write_text(json.dumps(report,indent=2)+'\n','utf8')
    save()
    with ProcessPoolExecutor(max_workers=args.workers,initializer=initialize,
            initargs=(str(args.romfs),str(args.kit),str(args.python_deps) if args.python_deps else None,
                      str(args.image_history) if getattr(args,'image_history',None) else None)) as pool:
        for base,archive in recipe['archives'].items():
            writer=texture.Writer(args.output,base)
            tasks=((base,i,r) for i,r in enumerate(archive['originals']))
            for i,(name,data,decoded) in enumerate(bounded(pool,original_job,tasks,args.workers)):
                writer.write(name,data,decoded)
                if i%500==0:print(base,'original',i,flush=True)
            tasks=((base,k,kit['proofs'].get(base+':'+k['name'])) for k in archive['variants'])
            for n,(key,blocks,proof) in enumerate(bounded(pool,variant_job,tasks,args.workers)):
                if key['tiled']:
                    if proof:blocks=blocks[1:]
                    index=len(writer.rows);w,h=key['physical'];cols,rows=math.ceil(w/504),math.ceil(h/504)
                    raw=struct.pack('<4s7I',b'KQTL',1,*key['logical'],w,h,cols,rows)
                    raw+=b''.join(struct.pack('<5I',index+1+s,*block[2]) for s,block in enumerate(blocks))
                    writer.write(key['name'],*texture.stored(raw,True))
                    for slot,(data,decoded,_) in enumerate(blocks):writer.write(f'KQT{index:07X}{slot:03X}.NXZ',data,decoded)
                else:writer.write(key['name'],blocks[0][0],blocks[0][1])
                if n%250==0:print(base,'variant',n,flush=True)
            writer.finish()
            report['archives'][base]=dict(originals=len(archive['originals']),variants=len(archive['variants']),
                                           bytes=writer.position,volumes=writer.volume_index+1)
            save()
        writer=texture.Writer(args.output,'savethumb')
        for name,data,decoded in bounded(pool,preview_job,kit['previews'],args.workers):writer.write(name,data,decoded)
        finish_plain(writer)
        print('Preview archive complete',len(kit['previews']),flush=True)
        temporary=args.output.parent/'audio-stage';temporary.mkdir(exist_ok=False)
        for bank,names in kit['banks'].items():
            tasks=[]
            for name in names:
                if audio_contract and name.endswith('.bgmstart.mrg'):continue
                if audio_contract:
                    tasks.append((str(args.romfs/'stream'/Path(name).with_suffix('.opus')),
                                  str(temporary/name),args.ffmpeg,audio_contract['stream/'+name]))
                    continue
                if name.endswith('.bgmstart.mrg') or kit['files'].get('stream/'+name,{}).get('source')=='private-supplement':
                    shutil.copyfile(args.kit/'private/stream'/name,temporary/name)
                else:tasks.append((str(args.romfs/'stream'/Path(name).with_suffix('.opus')),str(temporary/name),args.ffmpeg))
            for i,_ in enumerate(bounded(pool,audio_job,tasks,args.workers)):
                if i%500==0:print(bank,'audio',i,flush=True)
            if audio_contract and 'M10.bgmstart.mrg' in names:
                from rebuild_bgm_lead import make_lead
                (temporary/'M10.bgmstart.mrg').write_bytes(make_lead((temporary/'M10.ogg').read_bytes()))
            pack({name:(temporary/name,0,(temporary/name).stat().st_size) for name in names},args.output/bank)
            for name in names:(temporary/name).unlink()
        temporary.rmdir()
    for rel,record in kit['files'].items():
        if rel.startswith('stream/'):continue
        if rel in resolved:
            target=args.output/rel;target.parent.mkdir(parents=True,exist_ok=True)
            target.write_bytes(resolved[rel][0]);continue
        source=(args.romfs if record['source']=='romfs' else args.kit/'private')/rel
        target=args.output/rel;target.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(source,target)
    files=[]
    for p in args.output.rglob('*'):
        if p.is_file() and p.name not in ('voice.mrg','bgm.mrg'):
            files.append(dict(path=p.relative_to(args.output).as_posix(),bytes=p.stat().st_size,sha256=file_sha(p)))
    for name,(path,offset,size) in audio_bank_locations(args.output).items():
        with path.open('rb') as f:f.seek(offset);digest=hashlib.sha256(f.read(size)).hexdigest()
        files.append(dict(path=name,bytes=size,sha256=digest))
    manifest=dict(schema='knife-540p-assets-v1',method='native-540p-encoded-unorm-v2',render=[960,540],display=[960,540],
        composite=[960,540],languages=['en','zc'],codecs=[1,2,7,9],color_contract='encoded-unorm8-v1',
        verification=dict(files_hashed=True,source_preserved=True,resampled=True,runtime_hd_fallback=False),
        files=sorted(files,key=lambda r:r['path']),archives=[dict(name=b,**r) for b,r in report['archives'].items()],
        story_gxt_variants=dict(schema='knife-story-gxt-variants-v1',original_entries={b:len(a['originals']) for b,a in recipe['archives'].items()}),
        story_archive_volumes={b:r['volumes'] for b,r in report['archives'].items() if r['volumes']>1},
        recovered_build=dict(recipe_sha256=kit['recipe_sha256'],private_inputs=True,identical_to_reference=False))
    manifest.update(kit['manifest_contracts'])
    (args.output/'vita-540p-assets.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n','utf8')
    refresh_bgm_lead(args.output)
    report['complete']=True;save()


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ['romfs','kit','output']:p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--workers',type=int,default=4)
    p.add_argument('--python-deps',type=Path)
    p.add_argument('--ffmpeg',default='ffmpeg')
    p.add_argument('--mod',type=Path)
    p.add_argument('--logo',type=Path)
    p.add_argument('--audio-contract',type=Path)
    p.add_argument('--image-history',type=Path)
    p.add_argument('--allow-recovery-candidate',action='store_true')
    build(p.parse_args())
