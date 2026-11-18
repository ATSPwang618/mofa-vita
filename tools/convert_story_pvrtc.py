"""Full-resolution, quality-gated PVRTC2 4bpp candidate from a native DXT pack.

Only appended native story variants are eligible. Original entries, fallback
images and hash-bound coverage proofs remain byte-identical. Requires Pillow,
numpy and local psp2pvrt64/psp2gxt tools; binaries are never redistributed.
"""
import argparse
from collections import deque
from concurrent.futures import ProcessPoolExecutor
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import time

from resize_story_variants import Reader, Writer, ROOT, sha, stored, unpack


def read_cache(path):
    for attempt in range(100):
        try:
            return path.read_bytes()
        except PermissionError:
            time.sleep(.05)
    raise RuntimeError(f'Cannot read cache {path}')


def publish_cache(temp, destination):
    # Separate workers may finish an identical texture simultaneously. Windows
    # denies replacement while a peer reads it; identical published bytes win.
    expected = temp.read_bytes()
    for attempt in range(100):
        try:
            if destination.exists():
                if read_cache(destination) != expected:
                    raise RuntimeError(f'Non-deterministic cache {destination}')
                temp.unlink()
                return
            os.replace(temp,destination)
            return
        except PermissionError:
            time.sleep(.05)
    raise RuntimeError(f'Cannot publish cache {destination}')


def init_worker(source, base, volumes, cache, encoder, packer, protected):
    global reader, work, pvrt, gxttool, proofs, codec, Image, np
    import numpy as np
    from PIL import Image
    reader = Reader(Path(source), base, volumes)
    work, pvrt, gxttool, proofs = Path(cache), encoder, packer, set(protected)
    spec = importlib.util.spec_from_file_location('codec', ROOT/'tools/migrate-vita-native-color.py')
    codec = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(codec)


def encode(index, protected=False):
    data = reader.read(index)
    raw = unpack(data)
    assert raw[:4] == b'KIMG'
    _, version, lw, lh, w, h, encoding, length = struct.unpack_from('<4s7I', raw)
    assert version == 1 and encoding == 9
    original = raw[32:32+length]
    fmt = struct.unpack_from('<I', original, 52)[0]
    desc = (w, h, fmt, struct.unpack_from('<I', original, 16)[0])
    keep = (data, reader.rows[index][2], desc)
    if protected or sha(original) in proofs or sha(raw[:32+length]) in proofs:
        return keep, {'reason': 'protected'}
    if fmt != 0x87000000 or min(w,h) < 8:
        return keep, {'reason': 'format_or_small'}
    key = sha(original)
    record_path = work/(key+'.json')
    payload_path = work/(key+'.gxt')
    if record_path.exists():
        record = json.loads(read_cache(record_path))
        if record.get('source_sha256')!=key:
            raise ValueError('Cached texture source identity mismatch')
    else:
        job = work/('worker-'+str(os.getpid()))
        job.mkdir(exist_ok=True)
        im = codec.gxt_decode(original)[0].crop((0,0,w,h))
        a = np.asarray(im)
        bw,bh = codec.pow2(w),codec.pow2(h)
        Image.fromarray(np.pad(a,((0,bh-h),(0,bw-w),(0,0)),mode='edge')).save(job/'input.tga')
        commands = [
            [pvrt,'-i','input.tga','-o','output.pvr','-d','decoded.dds',
             '-f','SCE_GXM_TEXTURE_FORMAT_PVRTII4BPP_ABGR','-it','4','-th','1','-m','0'],
            [gxttool,'-i','output.pvr','-o','output.gxt','-w',str(w),'-h',str(h)]]
        for command in commands:
            result = subprocess.run(command,cwd=job,capture_output=True,timeout=180)
            if result.returncode:
                raise RuntimeError(result.stdout.decode(errors='replace')+result.stderr.decode(errors='replace'))
        candidate = bytearray((job/'output.gxt').read_bytes())
        assert struct.unpack_from('<I',candidate,52)[0] == 0x83000000
        assert struct.unpack_from('<HH',candidate,56) == (w,h)
        assert len(candidate) == 64+bw*bh//2
        struct.pack_into('<I',candidate,48,0xa0000000)
        b = np.asarray(Image.open(job/'decoded.dds').convert('RGBA').crop((0,0,w,h))).astype(np.float32)
        a = a.astype(np.float32)
        aa,ba = a[:,:,3:]/255,b[:,:,3:]/255
        differences = [a[:,:,:3]*aa-b[:,:,:3]*ba,
                       a[:,:,:3]*aa+255*(1-aa)-b[:,:,:3]*ba-255*(1-ba)]
        record = dict(alpha_rmse=float(np.sqrt(np.mean((a[:,:,3]-b[:,:,3])**2))),
                      composite_rmse=max(float(np.sqrt(np.mean(d*d))) for d in differences),
                      composite_p99=max(float(np.percentile(abs(d),99)) for d in differences))
        record['reason'] = ('accepted' if record['alpha_rmse'] <= 4
            and record['composite_rmse'] <= 5 and record['composite_p99'] <= 24 else 'quality')
        if record['reason'] == 'accepted':
            record.update(payload_sha256=sha(candidate),payload_bytes=len(candidate))
            tmp = work/(key+'.'+str(os.getpid())+'.tmp')
            tmp.write_bytes(candidate)
            publish_cache(tmp,payload_path)
        record['source_sha256']=key
        tmp = work/(key+'.'+str(os.getpid())+'.json.tmp')
        tmp.write_text(json.dumps(record),'utf8')
        publish_cache(tmp,record_path)
    if record['reason'] != 'accepted':
        return keep, record
    candidate = read_cache(payload_path)
    from asset_reproducibility import verify_cached_payload
    verify_cached_payload(candidate,record)
    envelope = struct.pack('<4s7I',b'KIMG',1,lw,lh,w,h,9,len(candidate))+candidate
    result, decoded = stored(envelope, data[:4] == b'KIMG')
    if len(result) >= len(data):
        return keep, {**record,'reason':'no_storage_saving'}
    return (result,decoded,(w,h,0x83000000,len(candidate)-64)), record


def convert(task):
    index, protected = task
    raw = unpack(reader.read(index))
    if raw[:4] == b'KIMG':
        block, record = encode(index,protected)
        return index,[block],[record]
    assert raw[:4] == b'KQTL'
    cols,rows = struct.unpack_from('<II',raw,24)
    blocks,records = [],[]
    for slot in range(cols*rows+1):
        ref = struct.unpack_from('<I',raw,32+slot*20)[0]
        assert ref == index+1+slot
        block,record = encode(ref,protected or slot == 0)
        blocks.append(block);records.append(record)
    return index,blocks,records


def digest_file(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f,'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--cache',type=Path,required=True)
    parser.add_argument('--encoder',required=True)
    parser.add_argument('--packer',required=True)
    parser.add_argument('--workers',type=int,default=8)
    parser.add_argument('--resume-incomplete',action='store_true',help='Reassemble only this incomplete candidate using the encoding cache')
    args = parser.parse_args()
    assert args.source.resolve() != args.output.resolve()
    assert not args.output.exists() or (args.resume_incomplete and not (args.output/'vita-540p-assets.json').exists())
    assert 1 <= args.workers <= 8
    from asset_reproducibility import digest, python_sources, locked_cache
    import numpy as np
    import PIL
    args.encoder=str(Path(args.encoder).resolve());args.packer=str(Path(args.packer).resolve())
    cache_identity=dict(schema='knife-pvrtc-cache-recipe-v1',
                        encoder=digest(args.encoder),packer=digest(args.packer),
                        sources=python_sources(ROOT/'tools'),
                        python=sys.version, numpy=np.__version__, pillow=PIL.__version__)
    args.cache,cache_namespace=locked_cache(args.cache,cache_identity)
    args.output.mkdir(parents=True,exist_ok=args.resume_incomplete)
    marker = (args.source/'vita-540p-assets.json').read_bytes()
    manifest = json.loads(marker)
    originals = manifest['story_gxt_variants']['original_entries']
    protected,proofs = set(),set()
    for path in (ROOT/'knife/knife-render/src').glob('vita*.rs'):
        text = path.read_text('utf8')
        protected.update(re.findall(r'entry: "(KQ[^\"]+)"',text))
        proofs.update(re.findall(r'b"([a-f0-9]{64})"',text))
    report = dict(schema='knife-pvrtc2-4bpp-candidate-v1',source_manifest_sha256=sha(marker),
                  cache_recipe_sha256=cache_namespace,cache_recipe=cache_identity,
                  scale=1.0,format='PVRTC2 4bpp',quality_gate=dict(alpha_rmse=4,composite_rmse=5,composite_p99=24),
                  archives={},counts={},accepted=[],tiles_checked=0)
    started = time.monotonic()
    for base,count in originals.items():
        volumes = manifest.get('story_archive_volumes',{}).get(base,1)
        source = Reader(args.source,base,volumes)
        writer = Writer(args.output,base,overwrite_incomplete=args.resume_incomplete)
        for i in range(count):
            writer.write(source.name(i),source.read(i),source.rows[i][2])
        writer.names[:] = source.names[:count*32]
        tasks=[];i=count
        while i < len(source.rows)-2:
            tasks.append((i,source.name(i) in protected))
            raw=unpack(source.read(i))
            i += (struct.unpack_from('<I',raw,24)[0]*struct.unpack_from('<I',raw,28)[0]+2) if raw[:4]==b'KQTL' else 1
        print(base,'variants',len(tasks),flush=True)
        def consume(result):
            old,blocks,records=result
            assert len(writer.rows)==old
            raw=unpack(source.read(old))
            if raw[:4]==b'KQTL':
                head=raw[:32]+b''.join(struct.pack('<5I',old+1+s,*b[2]) for s,b in enumerate(blocks))
                writer.write(source.name(old),*stored(head,True))
                for s,b in enumerate(blocks):
                    writer.write(source.name(old+1+s),b[0],b[1])
            else:
                writer.write(source.name(old),blocks[0][0],blocks[0][1])
            for slot,record in enumerate(records):
                reason=record['reason'];report['counts'][reason]=report['counts'].get(reason,0)+1
                if reason=='accepted':
                    ref=old+1+slot if raw[:4]==b'KQTL' else old
                    report['accepted'].append(dict(archive=base,index=ref,name=source.name(ref),**record))
                report['tiles_checked']+=1
        with ProcessPoolExecutor(max_workers=args.workers,initializer=init_worker,
            initargs=(str(args.source),base,volumes,str(args.cache.resolve()),args.encoder,args.packer,list(proofs))) as pool:
            pending=deque()
            done=0
            for task in tasks:
                pending.append(pool.submit(convert,task))
                if len(pending)>=args.workers*2:
                    consume(pending.popleft().result());done+=1
                    if done%100==0:print(base,done,'/',len(tasks),report['counts'],'seconds',round(time.monotonic()-started),flush=True)
            while pending:consume(pending.popleft().result())
        writer.finish()
        assert len(writer.names) == len(source.names)
        for ordinal in range(len(source.rows)-1):
            assert writer.names[ordinal*32:(ordinal+1)*32].split(b'\0')[0].decode('ascii') == source.name(ordinal)
        # Source NAM uses CRLF in the last bytes of each record. Preserve those
        # bytes too; ordinals and names do not change in this conversion.
        (args.output/(base+'.nam')).write_bytes(source.names)
        source.volume.__exit__(None,None,None)
        report['archives'][base]=dict(bytes=writer.position,volumes=writer.volume_index+1,entries=len(writer.rows)-1)
        (args.output.parent/'pvrtc-report.json').write_text(json.dumps(report,indent=2),'utf8')
    for source in args.source.rglob('*'):
        if not source.is_file():continue
        rel=source.relative_to(args.source)
        if rel.name=='vita-540p-assets.json' or any(re.fullmatch(re.escape(b)+r'\.(hed|nam|mrg(?:\.\d{3})?)',str(rel)) for b in originals):continue
        target=args.output/rel;target.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(source,target)
    for archive in manifest['archives']:
        if archive['name'] in report['archives']:archive['bytes']=report['archives'][archive['name']]['bytes']
    manifest['story_archive_volumes']={b:r['volumes'] for b,r in report['archives'].items() if r['volumes']>1}
    if (digest(args.encoder)!=cache_identity['encoder'] or
            digest(args.packer)!=cache_identity['packer'] or
            python_sources(ROOT/'tools')!=cache_identity['sources']):
        raise RuntimeError('Encoder or recipe changed during conversion; no final manifest published')
    # Provenance belongs in the sidecar receipt. Do not change legacy runtime
    # manifest bytes merely by adding diagnostic/cache metadata.
    manifest['story_pvrtc_trial']={k:v for k,v in report.items()
                                 if k not in ('accepted','cache_recipe','cache_recipe_sha256')}
    manifest['files']=[r for r in manifest['files'] if not any(r['path'].startswith(b+'.') for b in originals)]
    for path in sorted(args.output.iterdir()):
        if path.is_file() and any(path.name.startswith(b+'.') for b in originals):
            manifest['files'].append(dict(path=path.name,bytes=path.stat().st_size,sha256=digest_file(path)))
    manifest['files'].sort(key=lambda r:r['path'])
    (args.output/'vita-540p-assets.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n','utf8')
    report['seconds']=round(time.monotonic()-started)
    report['source_bytes']=sum(p.stat().st_size for p in args.source.rglob('*') if p.is_file())
    report['candidate_bytes']=sum(p.stat().st_size for p in args.output.rglob('*') if p.is_file())
    (args.output.parent/'pvrtc-report.json').write_text(json.dumps(report,indent=2),'utf8')
    print('DONE',report['counts'],report['candidate_bytes'],flush=True)


if __name__=='__main__':main()
