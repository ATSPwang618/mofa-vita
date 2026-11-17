"""Export freshly extracted Switch art to an independent, lossless 540p PNG project.

No Vita pack inputs, texture budgets, palette reduction or lossy output codecs.
Requires Pillow and NumPy. Original archive/index/texture identities are retained.
"""
from __future__ import annotations
import argparse
import concurrent.futures
import hashlib
import io
import json
import math
import re
import struct
import zlib
from functools import lru_cache
from pathlib import Path

import numpy as np
from PIL import Image

ARCHIVES = {'jallpachd': .5, 'allpachdml': .5, 'jallpac': .75,
            'allpacml': .75, 'allui': .5, 'parts': .75}

@lru_cache(maxsize=1)
def strip_names():
    bundle=Path(__file__).resolve().parents[1]/'knife/knife/data/exe_tables_witch.bin'
    raw=zlib.decompress(bundle.read_bytes());assert raw[:4]==b'KXT0'
    pos=40;segments=[]
    for _ in range(struct.unpack_from('<I',raw,36)[0]):
        address,size=struct.unpack_from('<QI',raw,pos);pos+=12
        segments.append((address,raw[pos:pos+size]));pos+=size
    assert pos==len(raw)
    def read(address,size):
        for start,data in segments:
            if start<=address and address+size<=start+len(data):
                return data[address-start:address-start+size]
        raise ValueError('missing production image table')
    return frozenset(read(0x4049a0+32*i,32).split(b'\0')[0].decode() for i in range(10821)
                     if read(0x2f1068+72*i+67,1)[0]&2)

@lru_cache(maxsize=16)
def thumbnail_start(root,archive):
    if not (root/(archive+'.nam')).exists():return 1<<30
    data=(root/(archive+'.nam')).read_bytes()
    names=[data[i:i+32].split(b'\0')[0].split(b'\r')[0] for i in range(0,len(data),32)]
    return next((i for i,n in enumerate(names) if n.startswith(b'THUMSP_TOP.')),len(names))

def image_plan(archive,index,name,size,fmt,thumb_start,language_names):
    """Physical sampling plan; never substitute these sizes for logical coordinates."""
    w,h=size;scale=ARCHIVES[archive]
    reason='1080p-half' if scale==.5 else '720p-three-quarters'
    if name.upper().startswith('RULE_') or min(w,h)<=16 or fmt in ('0x201','0x1d01'):
        scale=1;reason='native-control-or-tiny'
    if index>=thumb_start:
        scale=1;reason='native-thumbnail-index-range'
    if archive=='allui' and Path(name).stem=='BORDER_PARTS':
        scale=1;reason='native-soft-clip-lookup'
    if archive=='parts':
        assert size in ([1280,720],[1920,1080],(1280,720),(1920,1080)), 'unknown boot sizing contract'
        scale=960/w;reason='boot-fullscreen-original-to-540p'
    count=4 if archive in ('jallpac','jallpachd','allpacml','allpachdml') and Path(name).stem in language_names else 1
    assert h>=count
    rows=h//count
    return [dict(crop=[0,k*h//count,w,k*h//count+rows],
        language=('ja','en','zc','zt')[k] if count==4 else None,
        size=[max(1,math.floor(w*scale+.5)),max(1,math.floor(rows*scale+.5))],
        scale=scale,rule=reason,language_count=count,language_slot=k) for k in range(count)]


def sha(data):
    return hashlib.sha256(data).hexdigest()


def file_sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def decode_bc4(linear, w, h):
    """Unsigned BC4 interpolation matching production bcdec_rs fixed weights."""
    blocks=np.frombuffer(linear,dtype=np.uint8).reshape(-1,8)
    a=blocks[:,0].astype(np.int32);b=blocks[:,1].astype(np.int32)
    palette=np.zeros((len(blocks),8),dtype=np.int32);palette[:,0]=a;palette[:,1]=b
    six=[9363,18724,28086,37450,46812,56173];four=[13107,26215,39321,52429]
    for i in range(6):
        palette[:,i+2]=np.where(a>b,(six[5-i]*a+six[i]*b+32768)>>16,
                               (four[3-i]*a+four[i]*b+32768)>>16 if i<4 else 0 if i==4 else 255)
    bits=np.zeros(len(blocks),dtype=np.uint64)
    for i in range(6):bits |= blocks[:,i+2].astype(np.uint64) << (8*i)
    indices=((bits[:,None] >> (3*np.arange(16,dtype=np.uint64))) & 7).astype(np.intp)
    values=np.take_along_axis(palette,indices,axis=1).astype(np.uint8)
    wb,hb=(w+3)//4,(h+3)//4
    pixels=values.reshape(hb,wb,4,4).transpose(0,2,1,3).reshape(hb*4,wb*4)[:h,:w]
    return Image.fromarray(pixels).convert('RGBA')


def unpack(data):
    if data[:4] in (b'NXCX', b'NXGX'):
        count = struct.unpack_from('<I', data, 8)[0]
        assert count <= len(data) - 16
        d = zlib.decompressobj(31 if data[:4] == b'NXGX' else 15)
        result = d.decompress(data[16:16+count], 256*1024*1024)
        assert d.eof and not d.unconsumed_tail
        return result
    return data


def leaves(data, suffix=''):
    data = unpack(data)
    if data[:6] != b'mrgd00':
        yield suffix, data
        return
    count = struct.unpack_from('<H', data, 6)[0]
    base = 8 + count*8
    assert base <= len(data)
    for i in range(count):
        sector, off, sectors, low = struct.unpack_from('<4H', data, 8+i*8)
        if sector == 65535:
            continue
        start = base + sector*2048 + (off & 4095) + ((off >> 12) << 27)
        size = low if not sectors else (sectors-1)//32*65536 + low
        assert start+size <= len(data)
        yield from leaves(data[start:start+size], suffix+f'_m{i:02d}')


def textures(data):
    if data[:4] != b'BNTX':
        if data.startswith((b'\x89PNG', b'\xff\xd8', b'DDS ')):
            yield 0, '', Image.open(io.BytesIO(data)).convert('RGBA'), 'raster'
        return
    assert data[32:36] == b'NX  '
    count = struct.unpack_from('<I', data, 36)[0]
    table = struct.unpack_from('<Q', data, 40)[0]
    for k in range(count):
        p = struct.unpack_from('<Q', data, table+k*8)[0]
        assert data[p:p+4] == b'BRTI'
        fmt = struct.unpack_from('<I', data, p+28)[0]
        w, h = struct.unpack_from('<II', data, p+36)
        assert 0 < w <= 16384 and 0 < h <= 16384
        mode = struct.unpack_from('<H', data, p+18)[0]
        bh = 1 << (struct.unpack_from('<I', data, p+52)[0] & 7)
        nameptr = struct.unpack_from('<Q', data, p+96)[0]
        n = struct.unpack_from('<H', data, nameptr)[0]
        name = data[nameptr+2:nameptr+2+n].decode('utf8', 'replace')
        ptr = struct.unpack_from('<Q', data, p+112)[0]
        off = struct.unpack_from('<Q', data, ptr)[0]
        bpb = {0x0b01:4,0x0b06:4,0x0201:1,0x1a01:8,0x1a06:8,
               0x1c01:16,0x1c06:16,0x1d01:8,0x2001:16,0x2006:16}[fmt]
        block = 1 if fmt in (0x0b01,0x0b06,0x0201) else 4
        wb, hb = (w+block-1)//block, (h+block-1)//block
        if mode == 1:
            linear = data[off:off+wb*hb*bpb]
        else:
            assert mode == 0
            y, x = np.indices((hb,wb), dtype=np.uint32)
            xb, gobs = x*bpb, (wb*bpb+63)//64
            address = ((y//(8*bh))*512*bh*gobs + ((y%(8*bh))//8)*512
                       + (xb//64)*512*bh + ((xb%64)//32)*256
                       + ((y%8)//2)*64 + (y%2)*16 + ((xb%32)//16)*32 + xb%16)
            indices = address[:,:,None] + np.arange(bpb,dtype=np.uint32) + off
            assert int(indices.max()) < len(data)
            linear = np.frombuffer(data,dtype=np.uint8)[indices].tobytes()
        assert len(linear) == wb*hb*bpb
        if block == 1:
            image = Image.frombytes('L' if bpb == 1 else 'RGBA',(w,h),linear).convert('RGBA')
        elif fmt == 0x1d01:
            image = decode_bc4(linear,w,h)
        else:
            fourcc = b'DX10' if fmt in (0x2001,0x2006) else b'ATI1' if fmt == 0x1d01 else b'DXT1' if bpb == 8 else b'DXT5'
            words = [124,0x81007,h,w,len(linear),0,0]+[0]*11+[32,4,int.from_bytes(fourcc,'little'),0,0,0,0,0]+[0x1000,0,0,0,0]
            header = b'DDS '+struct.pack('<31I',*words)
            if fourcc == b'DX10':
                header += struct.pack('<5I',98,3,0,1,0)
            image = Image.open(io.BytesIO(header+linear)).convert('RGBA')
        yield k, name, image, hex(fmt)


def resize(image, size):
    """Float associated-alpha filtering, with hidden RGB retained at zero alpha."""
    if image.size == size:
        return image.copy()
    if image.getextrema()[3] == (255,255):
        return image.resize(size,Image.Resampling.LANCZOS)
    src = np.asarray(image,dtype=np.float32)
    a = src[:,:,3]/255
    def filt(v):
        return np.asarray(Image.fromarray(v).resize(size,Image.Resampling.LANCZOS))
    filtered_alpha = filt(a)
    alpha = np.clip(filtered_alpha,0,1)
    out = np.empty((size[1],size[0],4),dtype=np.uint8)
    for c in range(3):
        color = np.where(alpha > 1/510, filt(src[:,:,c]*a)/np.maximum(filtered_alpha,1e-8), filt(src[:,:,c]))
        out[:,:,c] = np.clip(np.rint(color),0,255).astype(np.uint8)
    out[:,:,3] = np.rint(alpha*255).astype(np.uint8)
    return Image.fromarray(out)


def safe(name):
    return re.sub(r'[^\w.()-]+','_',name).strip('.')[:90] or 'unnamed'


def convert(job):
    root, out, archive, i, row, name = job
    with (root/(archive+'.mrg')).open('rb') as f:
        f.seek(row[0]*2048); raw=f.read(row[1]*2048)
    records=[]; non_images=[]
    for suffix,data in leaves(raw):
        found=False
        for k,label,image,fmt in textures(data):
            found=True
            plans=image_plan(archive,i,name,image.size,fmt,thumbnail_start(root,archive),strip_names())
            for plan in plans:
                part=image.crop(tuple(plan['crop']))
                size=tuple(plan['size']);scale=plan['scale'];reason=plan['rule']
                result=resize(part,size)
                language=plan['language']
                stem=f'{i:05d}_{safe(name)}{suffix}_t{k:03d}_{safe(label)}'
                rel=Path(archive)/(stem+(f'_{language}' if language else '')+'.png')
                target=out/rel;target.parent.mkdir(parents=True,exist_ok=True)
                result.save(target,format='PNG',compress_level=3)
                with Image.open(target) as check:
                    assert check.mode=='RGBA' and check.size==size and check.tobytes()==result.tobytes()
                records.append({'path':rel.as_posix(),'archive':archive,'index':i,'name':name,'member':suffix,'texture':k,'texture_name':label,'format':fmt,'source_entry_sha256':sha(raw),'source_texture_size':list(image.size),**plan,'bytes':target.stat().st_size,'sha256':file_sha(target),'pixel_sha256':sha(result.tobytes())})
        if not found:
            rel=Path('metadata')/archive/f'{i:05d}_{safe(name)}{suffix}.bin'
            (out/rel).parent.mkdir(parents=True,exist_ok=True);(out/rel).write_bytes(data)
            non_images.append({'path':rel.as_posix(),'bytes':len(data),'sha256':sha(data),'magic_hex':data[:8].hex()})
    return records,non_images


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--romfs',type=Path,required=True);p.add_argument('--out',type=Path,required=True)
    p.add_argument('--source-receipt',type=Path,required=True);p.add_argument('--jobs',type=int,default=4)
    args=p.parse_args();root=args.romfs.resolve();out=args.out.resolve()
    assert root != out and not root.is_relative_to(out) and not out.is_relative_to(root)
    assert 1<=args.jobs<=8
    assert not out.exists() or not any(out.iterdir()),'Output must be empty'
    out.mkdir(parents=True,exist_ok=True)
    manifest={'complete':False,'canvas':[960,540],'source':json.loads(args.source_receipt.read_text(encoding='utf-8')),'converter_sha256':file_sha(Path(__file__)),'archives':{},'images':[],'non_images':[]}
    manifest_path=out/'manifest.json'
    for archive in ARCHIVES:
        hed=root/(archive+'.hed');mrg=root/(archive+'.mrg');nam=root/(archive+'.nam')
        rows=list(struct.iter_unpack('<IHH',hed.read_bytes()));names=nam.read_bytes() if nam.exists() else b''
        meta=out/'metadata'/'archive-indices';meta.mkdir(parents=True,exist_ok=True)
        for path in [hed,nam]:
            if path.exists():(meta/path.name).write_bytes(path.read_bytes())
        manifest['archives'][archive]={p.name:{'bytes':p.stat().st_size,'sha256':file_sha(p)} for p in [hed,mrg,nam] if p.exists()}
        jobs=[]
        for i,row in enumerate(rows):
            if row[0]==0xffffffff or row[1]==0:continue
            assert (row[0]+row[1])*2048<=mrg.stat().st_size
            name=names[i*32:(i+1)*32].split(b'\0')[0].split(b'\r')[0].decode('utf8','replace') or str(i)
            jobs.append((root,out,archive,i,row,name))
        with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
            for start in range(0,len(jobs),args.jobs*2):
                for images,non in pool.map(convert,jobs[start:start+args.jobs*2]):
                    manifest['images'].extend(images);manifest['non_images'].extend(non)
                if start%200==0:print(f'{archive}: {start}/{len(jobs)}; total PNG {len(manifest["images"])}',flush=True)
        manifest_path.write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
        print(f'{archive}: complete',flush=True)
    for src in sorted(root.rglob('*.tga')):
        image=Image.open(src).convert('RGBA');rel=Path('standalone')/src.relative_to(root).with_suffix('.png')
        (out/rel).parent.mkdir(parents=True,exist_ok=True);image.save(out/rel)
        manifest['images'].append({'path':rel.as_posix(),'source_file':src.relative_to(root).as_posix(),'source_sha256':file_sha(src),'scale':1,'rule':'native-icon','size':list(image.size),'sha256':file_sha(out/rel),'bytes':(out/rel).stat().st_size,'pixel_sha256':sha(image.tobytes())})
    manifest['complete']=True
    manifest['png_count']=len(manifest['images']);manifest['png_bytes']=sum(x['bytes'] for x in manifest['images'])
    manifest_path.write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    print(f'Complete: {manifest["png_count"]} PNG, {manifest["png_bytes"]} bytes',flush=True)


if __name__=='__main__':
    main()
