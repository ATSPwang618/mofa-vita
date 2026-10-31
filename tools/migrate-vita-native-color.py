#!/usr/bin/env python3
"""Migrate pre-Alter codec8 to encoded-UNORM codec9 without changing source files.

Decode the complete DXT blocks, apply inverse Alter to decoded RGB bytes, then
recompress DXT. This is a second lossy encoding, not recovery of original art.
Vita block Morton order follows vitaGL texture_swizzler.cpp: x bits odd, y even.
"""
from __future__ import annotations
import argparse,collections,concurrent.futures,functools,hashlib,io,json,mmap,struct,time,zlib,os
from pathlib import Path
import numpy as np
from PIL import Image, ImageFile
# Pillow BCN encoder needs a whole output block; default64KiB corrupts larger DDS.
ImageFile.MAXBLOCK = 16 * 1024 * 1024
ARCHIVES=('allui','jallpac','jallpachd','allpacml','allpachdml','parts')
CAP=32*1024*1024
STRICT_UI=False

def sha(data):return hashlib.sha256(data).hexdigest()
def file_sha(p):
 h=hashlib.sha256()
 with p.open('rb')as f:
  for chunk in iter(lambda:f.read(1024*1024),b''):h.update(chunk)
 return h.hexdigest()
def pow2(v):return 1<<(v-1).bit_length()
def decode_srgb(s):return s/12.92 if s<=0.04045 else ((s+0.055)/1.055)**2.4
def encode_srgb(l):return 12.92*l if l<=0.0031308 else 1.055*l**(1/2.4)-0.055
INVERSE=bytes(max(0,min(255,round(255*encode_srgb(decode_srgb(i/255)**2.2))))for i in range(256))

def spread(x):
 x=np.asarray(x,dtype=np.uint32);x=(x|(x<<8))&0x00ff00ff;x=(x|(x<<4))&0x0f0f0f0f;x=(x|(x<<2))&0x33333333;x=(x|(x<<1))&0x55555555;return x
@functools.lru_cache(maxsize=64)
def indices(w,h):
 assert w>=1 and h>=1 and w&(w-1)==0 and h&(h-1)==0
 y,x=np.indices((h,w),dtype=np.uint32);tile=min(w,h)
 idx=((x//tile)+(y//tile)*(w//tile))*tile*tile+(spread(x%tile)<<1)+spread(y%tile)
 assert len(np.unique(idx))==w*h and int(idx.max())==w*h-1
 return idx.reshape(-1)
def unswizzle(data,w,h,bpe):
 return np.frombuffer(data,dtype=np.uint8).reshape(-1,bpe)[indices(w,h)].tobytes()
def swizzle(data,w,h,bpe):
 src=np.frombuffer(data,dtype=np.uint8).reshape(-1,bpe);out=np.empty_like(src);out[indices(w,h)]=src;return out.tobytes()
def dds(payload,w,h,fourcc):
 words=[124,0x81007,h,w,len(payload),0,0]+[0]*11+[32,4,int.from_bytes(fourcc.encode(),'little'),0,0,0,0,0]+[0x1000,0,0,0,0]
 assert len(words)==31
 return b'DDS '+struct.pack('<31I',*words)+payload

def gxt_decode(raw):
 assert len(raw)>=64;magic,version,count,off,length,*_=struct.unpack_from('<8I',raw)
 assert (magic,version,count)==(0x00545847,0x10000003,1)and off+length==len(raw)
 roff,rlen,pal,flags,layout,fmt,w,h,mips=struct.unpack_from('<6IHHB',raw,32)
 assert (roff,rlen,pal,flags,mips)==(off,length,0xffffffff,0,1)and layout in(0,0xa0000000)
 assert fmt in(0x85000000,0x87000000)
 bw,bh=max(4,pow2(w)),max(4,pow2(h));bpe=8 if fmt==0x85000000 else 16
 assert length==bw*bh*bpe//16
 linear=unswizzle(raw[off:],bw//4,bh//4,bpe)
 assert swizzle(linear,bw//4,bh//4,bpe)==raw[off:]
 image=Image.open(io.BytesIO(dds(linear,bw,bh,'DXT1'if bpe==8 else'DXT5'))).convert('RGBA')
 return image,(w,h),(bw,bh),fmt

def rgb565(words):
 words=np.asarray(words,dtype=np.int32)
 return np.stack((((words>>11)&31)*8+(((words>>11)&31)>>2),((words>>5)&63)*4+(((words>>5)&63)>>4),(words&31)*8+((words&31)>>2)),axis=-1)
def quant565(rgb):
 v=np.clip(np.rint(rgb),0,255).astype(np.int32)
 return (((v[...,0]*31+127)//255)<<11)|(((v[...,1]*63+127)//255)<<5)|((v[...,2]*31+127)//255)
@functools.lru_cache(maxsize=1)
def load_refiner():
 import ctypes,subprocess,sys
 source=Path(__file__).with_name('native-color-blocks.c');cache=Path(__file__).resolve().parents[1]/'.work/performance';cache.mkdir(parents=True,exist_ok=True)
 digest=sha(source.read_bytes())[:16];libpath=cache/('native-color-blocks-'+digest+('.dylib'if sys.platform=='darwin'else'.so'))
 if not libpath.exists():
  # Called once on the main thread before the bounded worker pool starts.
  subprocess.run(['cc','-O3','-shared','-fPIC',str(source),'-o',str(libpath),'-lm'],check=True)
 lib=ctypes.CDLL(str(libpath));fn=lib.native_color_refine;fn.restype=ctypes.c_size_t;fn.argtypes=[ctypes.c_void_p,ctypes.c_void_p,ctypes.c_void_p,ctypes.c_size_t,ctypes.c_uint,ctypes.c_uint,ctypes.c_void_p]
 return fn

def refine_rgb_blocks(linear,padded,bpe,old_linear):
 fn=load_refiner()
 blocks=np.frombuffer(linear,dtype=np.uint8).copy();h,w=np.asarray(padded).shape[:2];pixels=np.ascontiguousarray(np.asarray(padded).reshape(h//4,4,w//4,4,4).transpose(0,2,1,3,4).reshape(-1,64));old=np.frombuffer(old_linear,dtype=np.uint8);inverse=np.frombuffer(INVERSE,dtype=np.uint8);count=len(blocks)//bpe
 changed=fn(blocks.ctypes.data,pixels.ctypes.data,old.ctypes.data,count,bpe,len(old)//count,inverse.ctypes.data)
 return blocks.tobytes(),changed

def encode_gxt(image,w,h,alpha_source=None,existing=None):
 bw,bh=max(4,pow2(w)),max(4,pow2(h));opaque=image.getchannel('A').getextrema()==(255,255);codec='DXT1'if opaque else'DXT5';bpe=8 if opaque else 16;fmt=0x85000000 if opaque else 0x87000000
 padded=Image.fromarray(np.pad(np.asarray(image),((0,bh-h),(0,bw-w),(0,0)),mode='edge'))
 if existing is None:
  stream=io.BytesIO();padded.save(stream,format='DDS',pixel_format=codec);encoded=stream.getvalue();assert encoded[84:88]==codec.encode();linear=encoded[128:]
 else:
  exoff,exlength=struct.unpack_from('<II',existing,12);assert exlength==bw*bh*bpe//16;linear=unswizzle(existing[exoff:],bw//4,bh//4,bpe)
 assert len(linear)==bw*bh*bpe//16
 old_off,old_length=struct.unpack_from('<II',alpha_source,12);old_bpe=old_length*16//(bw*bh)
 old_linear=unswizzle(alpha_source[old_off:],bw//4,bh//4,old_bpe)
 linear,changed=refine_rgb_blocks(linear,padded,bpe,old_linear)
 payload=swizzle(linear,bw//4,bh//4,bpe)
 if bpe==16 and alpha_source is not None:
  old_fmt=struct.unpack_from('<I',alpha_source,52)[0];assert old_fmt==0x87000000
  old_off,old_length=struct.unpack_from('<II',alpha_source,12);assert old_length==len(payload)
  blocks=np.frombuffer(payload,dtype=np.uint8).reshape(-1,16).copy()
  blocks[:,:8]=np.frombuffer(alpha_source[old_off:],dtype=np.uint8).reshape(-1,16)[:,:8]
  payload=blocks.tobytes()
 header=struct.pack('<8I',0x00545847,0x10000003,1,64,len(payload),0,0,0)+struct.pack('<6IHHB3s',64,len(payload),0xffffffff,0,0xa0000000,fmt,w,h,1,b'\0'*3)
 assert len(header)==64
 # Decode bytes that will actually ship, including the same swizzle mapping.
 check,physical,_,_=gxt_decode(header+payload);assert physical==(w,h)
 return header+payload,check.crop((0,0,w,h)),codec,changed

def convert_image(raw,metrics=True,existing=None):
 if raw[:4]!=b'KIMG':return raw,None
 v,lw,lh,w,h,codec,n=struct.unpack_from('<7I',raw,4);assert v==1 and len(raw)==32+n
 if codec!=8:return raw,None
 started=time.monotonic();full,physical,backing,fmt=gxt_decode(raw[32:]);assert physical==(w,h)
 image=full.crop((0,0,w,h));r,g,b,a=image.split();target=Image.merge('RGBA',(r.point(INVERSE),g.point(INVERSE),b.point(INVERSE),a))
 gxt,check,code,changed=encode_gxt(target,w,h,raw[32:],existing[32:]if existing and struct.unpack_from('<I',existing,24)[0]==9 else None)
 assert np.array_equal(np.asarray(target)[:,:,3],np.asarray(check)[:,:,3]), "alpha changed"
 report={'width':w,'height':h,'input_format':hex(fmt),'output_format':code,'input_bytes':len(raw),'output_bytes':32+len(gxt),'seconds':time.monotonic()-started,'alpha_exact':True,'refined_rgb_blocks':changed}
 if metrics:
  ref=np.asarray(target,dtype=np.int16);got=np.asarray(check,dtype=np.int16);diff=np.abs(ref-got);report['mae_rgba']=diff.mean(axis=(0,1)).tolist();report['max_rgba']=diff.max(axis=(0,1)).tolist();report['pixels']=w*h;premult=(diff[:,:,:3].astype(np.float32)*ref[:,:,3:4]/255);report['premult_max_rgb']=premult.max(axis=(0,1)).tolist();pixel_error=premult.max(axis=2);report['premult_above32_pixels']=int((pixel_error>32).sum());report['premult_pixel_p99_p999']=np.quantile(pixel_error,[.99,.999]).tolist();report['opaque_max_rgb']=diff[:,:,:3][ref[:,:,3]==255].max(axis=0).tolist()if(ref[:,:,3]==255).any()else None
  for name,bg in [('black',0),('white',255)]:
   ar=ref[:,:,3:4].astype(np.float32)/255;ag=got[:,:,3:4].astype(np.float32)/255
   report['composite_'+name+'_rgb_mae']=np.abs(ref[:,:,:3]*ar+bg*(1-ar)-got[:,:,:3]*ag-bg*(1-ag)).mean(axis=(0,1)).tolist()
 # A few non-linear four-colour blocks cannot represent the inverse curve
 # accurately. Preserve those complete images losslessly rather than shipping
 # a large visible error. This is an explicit codec7 exception, never an
 # endpoint-only inverse approximation or runtime fragment compensation.
 if metrics and max(report['premult_max_rgb'])>32 and (STRICT_UI or report['premult_above32_pixels']>=max(4,(w*h+999)//1000)):
  report['rejected_DXT_quality']={k:report[k]for k in ('mae_rgba','max_rgba','premult_max_rgb','opaque_max_rgb','premult_above32_pixels','premult_pixel_p99_p999')}
  stream=io.BytesIO();target.save(stream,format='PNG',compress_level=6);png=stream.getvalue()
  result=b'KIMG'+struct.pack('<7I',1,lw,lh,w,h,7,len(png))+png
  report.update(output_format='PNG_UNORM',output_bytes=len(result),mae_rgba=[0.0]*4,max_rgba=[0]*4,premult_max_rgb=[0.0]*3,premult_above32_pixels=0,premult_pixel_p99_p999=[0.0]*2,opaque_max_rgb=[0]*3 if report['opaque_max_rgb']is not None else None,composite_black_rgb_mae=[0.0]*3,composite_white_rgb_mae=[0.0]*3)
 else:result=b'KIMG'+struct.pack('<7I',1,lw,lh,w,h,9,len(gxt))+gxt
 return result,report

def unpack(payload):
 if payload[:4]in(b'NXCX',b'NXGX'):
  n,c=struct.unpack_from('<II',payload,4);assert n<=CAP and c+16==len(payload);d=zlib.decompressobj(31 if payload[:4]==b'NXGX'else 15);raw=d.decompress(payload[16:],n+1);assert d.eof and not d.unused_data and not d.unconsumed_tail and len(raw)==n;return raw
 return payload

def entry_payload(mm,row):
 sector,stored,_=row;start=sector*2048;n=stored*2048;assert start+n<=len(mm)
 if mm[start:start+4]in(b'NXCX',b'NXGX'):n=16+struct.unpack_from('<I',mm,start+8)[0]
 return mm[start:start+n]

def convert_entry(payload):
 raw=unpack(payload);reports=[]
 if raw[:4]==b'KIMG':new,report=convert_image(raw);reports +=[report]if report else[]
 elif raw[:4]==b'KATL':
  v,count=struct.unpack_from('<II',raw,4);assert v==1 and count<=4096;p=12;out=bytearray(raw[:12])
  for _ in range(count):
   nl,il=struct.unpack_from('<II',raw,p);p+=8;name=raw[p:p+nl];p+=nl;new_image,report=convert_image(raw[p:p+il]);p+=il
   out+=struct.pack('<II',nl,len(new_image))+name+new_image
   if report:report['member']=name.decode();reports.append(report)
  assert p==len(raw);new=bytes(out)
 else:return payload,[],len(raw)
 if not reports:return payload,[],len(raw)
 compressed=zlib.compress(new,1);wrapped=b'NXCX'+struct.pack('<III',len(new),len(compressed),0)+compressed
 assert len(new)<=CAP and len(wrapped)<=CAP
 return wrapped,reports,len(new)

def probe(root):
 rows=list(struct.iter_unpack('<IHH',(root/'jallpac.hed').read_bytes()))
 with(root/'jallpac.mrg').open('rb')as f,mmap.mmap(f.fileno(),0,access=mmap.ACCESS_READ)as mm:
  before=time.monotonic();payload,reports,_=convert_entry(entry_payload(mm,rows[2926]));result={'source':'jallpac:2926:IMG2326','elapsed_seconds':time.monotonic()-before,'NX_bytes':len(payload),'images':reports}
 print(json.dumps(result,indent=2),flush=True);return result

def batch(root,out,jobs=2):
 global STRICT_UI
 load_refiner()
 assert jobs==2, "bounded worker contract is jobs2"
 assert root.resolve()!=out.resolve() and not root.resolve().is_relative_to(out.resolve())
 out.mkdir(parents=True,exist_ok=True)
 old_marker=(root/'vita-540p-assets.json').read_bytes();manifest=json.loads(old_marker);expected={x['path']:x for x in manifest['files']}
 source_hash=sha(old_marker);assert manifest['method']=='native-540p-preserve-logical-v1'
 receipt={'method':'decode-dxt-inverse-alter-rgb-refit-native-unorm-v2','source_manifest_sha256':source_hash,
  'inverse':'srgb_encode(pow(srgb_decode(decoded_old_DXT_RGB),2.2)); alpha original compressed block bytes preserved',
  'original_unaltered_source_available':False,'lossless_recovery':False,'alpha_exact':True,
  'rgb_fit':'nearest-and-inverse-endpoint-ls-v3-ui32-cg01pct',
  'lossless_fallback_policy':{'allui':'premult max>32','CG':'premult max>32 and count>=max(4,ceil(physicalPixels*0.001))'},
  'layout_reference':'.work/performance/vitaGL-reference/source/utils/texture_swizzler.cpp (x odd bits, y even bits)',
  'jobs':jobs,'archives':[],'quality_scope':'new decoded DXT vs exact inverse LUT of old decoded DXT; physical pixels only, not original artist source'}
 all_start=time.monotonic()
 for base in ARCHIVES:
  STRICT_UI=base=='allui'
  done=out/(base+'.native-color.json')
  if done.exists():
   info=json.loads(done.read_text());assert info['source_manifest_sha256']==source_hash
   for suffix in('hed','mrg'):
    f=out/(base+'.'+suffix);assert file_sha(f)==info['outputs'][f.name]['sha256']
   receipt['archives'].append(info);print(json.dumps({'archive':base,'resume':True}),flush=True);continue
  old_hed=(root/(base+'.hed')).read_bytes();rows=list(struct.iter_unpack('<IHH',old_hed));end=next(i for i,row in enumerate(rows)if row[0]==0xffffffff);rows=rows[:end]
  for suffix in('hed','mrg'):
   src=root/(base+'.'+suffix);assert src.stat().st_size==expected[src.name]['bytes']and file_sha(src)==expected[src.name]['sha256']
  begin=time.monotonic();seen_source={};seen_output={};hed=bytearray();total=0;metrics=[];copied=0;converted=0;aliases=0;weighted=np.zeros(4,dtype=np.float64);max_error=np.zeros(4,dtype=np.int16);pixels=0
  partial=out/(base+'.mrg.part')
  with(root/(base+'.mrg')).open('rb')as src,mmap.mmap(src.fileno(),0,access=mmap.ACCESS_READ)as mm,partial.open('w+b')as dst,concurrent.futures.ThreadPoolExecutor(max_workers=jobs)as pool:
   for start in range(0,len(rows),jobs*2):
    pending={}
    for i in range(start,min(start+jobs*2,len(rows))):
     key=rows[i]
     if key not in seen_source and key not in pending:
      pending[key]=pool.submit(convert_entry,entry_payload(mm,key)) if key[1] else None
    for i in range(start,min(start+jobs*2,len(rows))):
     key=rows[i]
     if key in seen_source:
      record,count=seen_source[key];hed+=record;converted+=count;aliases+=1;continue
     if pending[key]is None:data,reports,n=b'',[],0
     else:data,reports,n=pending[key].result()
     sectors=(len(data)+2047)//2048;unpacked=(n+2047)//2048 if reports else key[2]
     assert sectors<=65535 and unpacked<=65535 and n<=CAP
     identity=(sha(data),len(data),unpacked)
     if identity in seen_output:offset=seen_output[identity]
     else:
      offset=total//2048;dst.seek(total);dst.write(data);dst.write(bytes(sectors*2048-len(data)));total+=sectors*2048;seen_output[identity]=offset
     record=struct.pack('<IHH',offset,sectors,unpacked);seen_source[key]=(record,len(reports));hed+=record;converted+=len(reports);copied+=not bool(reports)
     for report in reports:
      count=report['pixels'];weighted+=np.asarray(report['mae_rgba'])*count;max_error=np.maximum(max_error,report['max_rgba']);pixels+=count
     metrics.extend(reports)
    if start%400<jobs*2:print(json.dumps({'archive':base,'entries':min(start+jobs*2,len(rows)),'total':len(rows),'output_MiB':round(total/1048576,2),'seconds':round(time.monotonic()-begin,2)}),flush=True)
   dst.flush()
  hed+=old_hed[end*8:];(out/(base+'.hed')).write_bytes(hed);partial.rename(out/(base+'.mrg'))
  nam=root/(base+'.nam')
  if nam.exists():(out/nam.name).write_bytes(nam.read_bytes())
  info={'archive':base,'source_manifest_sha256':source_hash,'entries':len(rows),'converted_image_slots':converted,'converted_images_distinct_source_ranges':len(metrics),'unchanged_entry_ranges':copied,'source_range_aliases':aliases,'seconds':round(time.monotonic()-begin,3),'outputs':{},'quality':{'physical_pixels':pixels,'weighted_mae_rgba':(weighted/max(1,pixels)).tolist(),'max_rgba':max_error.tolist(),'alpha_exact':int(max_error[3])==0}}
  for suffix in('hed','mrg'):
   f=out/(base+'.'+suffix);info['outputs'][f.name]={'bytes':f.stat().st_size,'sha256':file_sha(f)}
  done.write_text(json.dumps(info,indent=2)+'\n');(out/(base+'.quality.json')).write_text(json.dumps(metrics,separators=(',',':'))+'\n');receipt['archives'].append(info)
  print(json.dumps(info),flush=True)
 # Keep every unchanged file identity. The delivery is a replacement-file delta;
 # all original non-image files remain installed in the user's existing pack.
 updates={name:value for info in receipt['archives']for name,value in info['outputs'].items()}
 for row in manifest['files']:
  if row['path']in updates:row.update(updates[row['path']])
 manifest['method']='native-540p-encoded-unorm-v2';manifest['codecs']=[1,2,7,9];manifest['composite']=[960,540]
 manifest['color_contract']='encoded-unorm8-v1';manifest['native_color_migration']={'source_manifest_sha256':source_hash,'method':receipt['method'],'alpha_exact':True,'original_source_recovered':False,'second_lossy_DXT_encoding':True,'rgb_fit':receipt['rgb_fit'],'lossless_fallback_policy':receipt['lossless_fallback_policy']}
 manifest['statistics']['bytes']=sum(row['bytes']for row in manifest['files'])
 for archive in manifest['archives']:archive['bytes']=updates[archive['name']+'.mrg']['bytes']
 marker=json.dumps(manifest,ensure_ascii=False,indent=2).encode()+b'\n';(out/'vita-540p-assets.json').write_bytes(marker)
 receipt['manifest_sha256']=sha(marker);receipt['seconds']=time.monotonic()-all_start;receipt['converted_slots']=sum(a['converted_image_slots']for a in receipt['archives']);receipt['converted_distinct_range_images']=sum(a['converted_images_distinct_source_ranges']for a in receipt['archives']);assert receipt['converted_slots']==22176
 (out.parent/'native-color-migration.json').write_text(json.dumps(receipt,ensure_ascii=False,indent=2)+'\n');print(json.dumps({'complete':True,'manifest_sha256':receipt['manifest_sha256'],'seconds':receipt['seconds'],'converted_slots':receipt['converted_slots']}),flush=True)
 return receipt

def refine_entry(original,existing):
 raw=unpack(original);current=unpack(existing);reports=[]
 if raw[:4]==b'KIMG':new,report=convert_image(raw,existing=current);reports +=[report]if report else[]
 elif raw[:4]==b'KATL':
  v,count=struct.unpack_from('<II',raw,4);assert current[:12]==raw[:12];p=12;q=12;out=bytearray(raw[:12])
  for _ in range(count):
   nl,il=struct.unpack_from('<II',raw,p);p+=8;name=raw[p:p+nl];p+=nl
   n2,i2=struct.unpack_from('<II',current,q);q+=8;assert n2==nl and current[q:q+nl]==name;q+=nl
   new_image,report=convert_image(raw[p:p+il],existing=current[q:q+i2]);p+=il;q+=i2
   out+=struct.pack('<II',nl,len(new_image))+name+new_image
   if report:report['member']=name.decode();reports.append(report)
  assert p==len(raw)and q==len(current);new=bytes(out)
 else:return existing,[],len(current)
 if not reports or new==current:return existing,reports,len(current)
 compressed=zlib.compress(new,1);assert len(new)<=CAP
 return b'NXCX'+struct.pack('<III',len(new),len(compressed),0)+compressed,reports,len(new)

def refine_existing(root,out,jobs=2):
 global STRICT_UI
 load_refiner()
 assert jobs==2
 for base in ARCHIVES:
  STRICT_UI=base=='allui'
  done=out/(base+'.native-color.json');info=json.loads(done.read_text())
  if info.get('rgb_fit')=='nearest-and-inverse-endpoint-ls-v3-ui32-cg01pct':
   print(json.dumps({'refine_archive':base,'resume':True}),flush=True);continue
  before=time.monotonic();orows=list(struct.iter_unpack('<IHH',(root/(base+'.hed')).read_bytes()));nhed=(out/(base+'.hed')).read_bytes();rows=list(struct.iter_unpack('<IHH',nhed));end=next(i for i,r in enumerate(rows)if r[0]==0xffffffff);rows=rows[:end]
  seen={};dedup={};total=0;hed=bytearray();metrics=[];partial=out/(base+'.mrg.refine-part')
  with(root/(base+'.mrg')).open('rb')as of,(out/(base+'.mrg')).open('rb')as nf,mmap.mmap(of.fileno(),0,access=mmap.ACCESS_READ)as om,mmap.mmap(nf.fileno(),0,access=mmap.ACCESS_READ)as nm,partial.open('w+b')as dst,concurrent.futures.ThreadPoolExecutor(max_workers=jobs)as pool:
   for start in range(0,end,jobs*2):
    pending={}
    for i in range(start,min(start+jobs*2,end)):
     key=(orows[i],rows[i])
     if key not in seen and key not in pending:pending[key]=pool.submit(refine_entry,entry_payload(om,orows[i]),entry_payload(nm,rows[i]))if rows[i][1]else None
    for i in range(start,min(start+jobs*2,end)):
     key=(orows[i],rows[i])
     if key in seen:hed+=seen[key];continue
     if pending[key]is None:data,reports,n=b'',[],0
     else:data,reports,n=pending[key].result()
     sectors=(len(data)+2047)//2048;unpacked=(n+2047)//2048 if reports else rows[i][2];assert sectors<=65535 and unpacked<=65535 and n<=CAP
     identity=(sha(data),len(data),unpacked)
     if identity in dedup:offset=dedup[identity]
     else:offset=total//2048;dst.write(data);dst.write(bytes(sectors*2048-len(data)));total+=sectors*2048;dedup[identity]=offset
     record=struct.pack('<IHH',offset,sectors,unpacked);seen[key]=record;hed+=record
     for report in reports:report['entry_index']=i
     metrics.extend(reports)
    if start%400<jobs*2:print(json.dumps({'refine_archive':base,'entries':min(start+jobs*2,end),'total':end,'seconds':round(time.monotonic()-before,2)}),flush=True)
   dst.flush()
  partial.replace(out/(base+'.mrg'));hed+=nhed[end*8:];(out/(base+'.hed')).write_bytes(hed)
  info['rgb_fit']='nearest-and-inverse-endpoint-ls-v3-ui32-cg01pct';info['refine_seconds']=time.monotonic()-before;info['refined_rgb_blocks']=sum(r['refined_rgb_blocks']for r in metrics);info['lossless_fallback_images']=sum(r['output_format']=='PNG_UNORM'for r in metrics)
  pixels=sum(r['pixels']for r in metrics);info['quality']={'physical_pixels':pixels,'weighted_mae_rgba':[sum(r['pixels']*r['mae_rgba'][c]for r in metrics)/max(1,pixels)for c in range(4)],'max_rgba':[max((r['max_rgba'][c]for r in metrics),default=0)for c in range(4)],'premult_max_rgb':[max((r['premult_max_rgb'][c]for r in metrics),default=0)for c in range(3)],'alpha_exact':True}
  for suffix in('hed','mrg'):
   f=out/(base+'.'+suffix);info['outputs'][f.name]={'bytes':f.stat().st_size,'sha256':file_sha(f)}
  done.write_text(json.dumps(info,indent=2)+'\n');(out/(base+'.quality.json')).write_text(json.dumps(metrics,separators=(',',':'))+'\n');print(json.dumps(info),flush=True)
 return batch(root,out,jobs)

if __name__=='__main__':
 p=argparse.ArgumentParser();p.add_argument('--source',required=True,type=Path);p.add_argument('--output',type=Path);p.add_argument('--probe',action='store_true');p.add_argument('--refine-existing',action='store_true');p.add_argument('--jobs',type=int,default=2);args=p.parse_args()
 if args.probe:probe(args.source)
 elif args.refine_existing:refine_existing(args.source,args.output,args.jobs)
 else:batch(args.source,args.output,args.jobs)
