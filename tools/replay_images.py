"""Locked source-image replay. Never reads a finished runtime pack."""
import ctypes,gzip,io,json,math,os,struct,zlib,sys
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor
import build_recovered_runtime as b
import replay_legacy_images as legacy
from historical_encoding import apply
from png_container_contract import restore
DATA=Path(__file__).with_name('source_replay')
def load(name):return json.loads(gzip.decompress((DATA/name).read_bytes()))
def wrap(raw,c):
 level=c.get('nx_level')
 if level is not None:
  packed=zlib.compress(raw,level);raw=struct.pack('<4sIII',b'NXCX',len(raw),len(packed),0)+packed
 return raw+bytes(-len(raw)%2048)
def strip(raw):return raw[:32+int.from_bytes(raw[28:32],'little')] if raw[:4]==b'KIMG' else raw
def correct(identity,raw):
 rec=corrections.get(identity)
 return apply(raw,bytes.fromhex(rec['patch']),rec) if rec else raw

def init(config):
 global cfg,R,recipe,contracts,banks,corrections,shim,previews
 cfg=config;R=Path(cfg['work']);sys.path.insert(0,cfg['python_deps'])
 b.initialize(str(R/'patched'),str(DATA),None,str(DATA/'image-history.json.gz'))
 os.environ['LEGACY_EXE']=str(DATA/'legacy-compact-replay.exe');legacy.init()
 import ispc_texcomp._ispc_texcomp as mod
 if b.texture.sha(Path(mod.__file__).read_bytes())!='1e69c985ec6a499f541f1d403b1697d8255b6ea07d4aa60a6957e06593b9bf98':raise ValueError('Unsupported ISPC binary')
 shim=ctypes.CDLL(str(DATA/'reciprocal_compat.dll'));shim.start.argtypes=[ctypes.c_void_p]
 if shim.start(ctypes.WinDLL(mod.__file__)._handle)!=282:raise ValueError('ISPC reciprocal adapter failed')
 shim.mode(0);recipe=load('recipe.json.gz');contracts=load('original-containers.json.gz');banks=load('bank-contracts.json.gz');corrections=load('historical-corrections.json.gz')['records'];previews=load('source-contract.json.gz')['previews']
 for name in ['original-cache','variant-cache','preview-cache','metadata-cache']:(R/name).mkdir(exist_ok=True)
def cached(kind,digest):
 p=R/kind/digest
 if p.is_file():
  data=p.read_bytes()
  if b.texture.sha(data)!=digest:raise ValueError('Corrupted cache '+str(p))
  return data
 return None
def publish(kind,data,expected):
 if b.texture.sha(data)!=expected:raise ValueError('Output identity mismatch '+expected)
 p=R/kind/expected
 if p.exists():
  if b.texture.sha(p.read_bytes())!=expected:raise ValueError('Cache collision')
  return
 temp=p.with_suffix('.'+str(os.getpid())+'.tmp');temp.write_bytes(data)
 from convert_story_pvrtc import publish_cache
 publish_cache(temp,p)
def original_job(task):
 base,i=task;c=recipe['archives'][base]['originals'][i];h=c['stored_sha256']
 if cached('original-cache',h) is not None:return base,i,True
 shim.mode(0)
 if 'magic' in c:data=b.originals.readers[base].read(i)
 elif (base,i) in b.history:data=b.original_job((base,i,c))[1]
 else:
  raw=legacy.build(base,i,c);container=contracts[base][i];identity=f'original/{base}/{i}'
  if identity in corrections:
   if b.texture.sha(b.originals.readers[base].read(i))!=corrections[identity]['source_sha256']:raise ValueError('Correction source mismatch')
   raw=correct(identity,raw)
  elif 'png' in container:
   png=restore(raw[32:],container['png']);raw=raw[:28]+struct.pack('<I',len(png))+png
  if b.texture.sha(raw)!=container['raw_sha256']:raise ValueError(f'Original intermediate mismatch {base}:{i}')
  data=wrap(raw,container)
 publish('original-cache',data,h);return base,i,False
prepared_cache={}
def variant_job(task):
 base,k=task;i=k['index'];ids=list(range(i+1,i+2+k['tile_count'])) if k['tiled'] else [i];rows=banks[base]['rows'];hashes=[rows[j][3] for j in ids]
 if all(cached('variant-cache',h) is not None or cached('original-cache',h) is not None for h in hashes):return base,i,True
 shim.mode(1)
 old=b.original.textures;b.original.textures=lambda raw:old(raw,production_bc3=True)
 try:
  key=(base,k['source_index'],k['language'],tuple(k['physical']))
  if key not in prepared_cache:
   im,_=b.originals.image(base,k,False);im=b.original.resize(im,tuple(k['physical']))
   if sum(x.width*x.height*4 for x in prepared_cache.values())+im.width*im.height*4>96*1024**2:prepared_cache.clear()
   if im.width*im.height*4<=96*1024**2:prepared_cache[key]=im
  else:im=prepared_cache[key]
  im=b.apply_lut(im,k,b.luts)
 finally:b.original.textures=old
 if k['tiled']:
  h=hashes[0];fallback=cached('original-cache',h)
  blocks=[fallback if fallback is not None else b.texture.encode(im.resize(tuple(k['fallback']['physical']),b.Image.Resampling.LANCZOS),k['fallback']['logical'],force_bc3=k['fallback']['format']==0x87000000)[0]]
  for y in range(0,im.height,504):
   for x in range(0,im.width,504):
    tile=im.crop((max(0,x-4),max(0,y-4),min(im.width,x+508),min(im.height,y+508)));blocks.append(b.texture.encode(tile,tile.size)[0])
 else:blocks=[b.texture.encode(im,k['logical'],k['raw_storage'],force_bc3=k['format']==0x87000000)[0]]
 for data,h,j in zip(blocks,hashes,ids):
  if b.texture.sha(data)!=h:data=wrap(strip(b.texture.unpack(data)),banks[base]['storage'].get(str(j),{}))
  publish('variant-cache',data,h)
 return base,i,False
def preview_job(i):
 h=banks['savethumb']['rows'][i][3]
 if cached('preview-cache',h) is not None:return 'savethumb',i,True
 shim.mode(1);rec=previews[i];stem=rec['name'][:-4];source=stem[3:];grade=int(stem[1:3],16)
 if i==11037:base,idx='jallpac',2030
 elif 'private_sha256' in rec or rec['logical']==[4,4]:base=idx=None
 elif source in b.thumbs:base,idx='jallpac',b.thumbs[source]
 else:
  base=next(x for x in ('allpacml','jallpac','allpachdml','jallpachd') if source in b.originals.names[x]);idx=max(j for j,c in enumerate(recipe['archives'][base]['originals']) if c['name'].rsplit('.',1)[0]==source)
 if base is None:im=b.Image.new('RGBA',tuple(rec['physical']),(0,0,0,255 if stem[0]=='O' else 0))
 else:
  c=recipe['archives'][base]['originals'][idx];raw=b.texture.unpack(cached('original-cache',c['stored_sha256']));im=b.texture.decode(raw) if c['codec']==9 else b.Image.open(io.BytesIO(raw[32:])).convert('RGBA');im=b.apply_lut(im,dict(grade=grade,bright_bits='3F800000',contrast_bits='3F800000'),b.luts);im=im.resize(tuple(rec['physical']),b.Image.Resampling.LANCZOS)
  if stem[0]=='O':im.putalpha(255)
 raw=strip(b.texture.unpack(b.texture.encode(im,rec['logical'],force_bc3=rec['format']==0x87000000)[0]));raw=correct(f'preview/{i}',raw);publish('preview-cache',wrap(raw,banks['savethumb']['storage'].get(str(i),{})),h);return 'savethumb',i,False
def pack_banks(config):
 # Unreachable garbage in the historical 100% intermediate is discarded by
 # the subsequent 70% writer. All indexed data, including duplicate names, is exact.
 R=Path(config['work']);out=R/'rebuilt-100';out.mkdir(exist_ok=True);book=load('bank-contracts.json.gz');rec=load('recipe.json.gz');t=b.texture
 for base,c in book.items():
  fs=[]
  for volume in c['volumes']:
   f=(out/volume['name']).open('w+b');f.truncate(volume['bytes']);fs.append(f)
  meta={}
  if base!='savethumb':
   for k in rec['archives'][base]['variants']:
    if not k['tiled']:continue
    i=k['index'];w,h=k['physical'];descs=[]
    for j in range(i+1,i+2+k['tile_count']):
     digest=c['rows'][j][3];p=next(R/d/digest for d in ['original-cache','variant-cache'] if (R/d/digest).is_file());raw=t.unpack(p.read_bytes());descs.append((j,*struct.unpack_from('<II',raw,16),struct.unpack_from('<I',raw,84)[0],struct.unpack_from('<I',raw,48)[0]))
    raw=struct.pack('<4s7I',b'KQTL',1,*k['logical'],w,h,(w+503)//504,(h+503)//504)+b''.join(struct.pack('<5I',*d) for d in descs);meta[i]=wrap(raw,c['storage'].get(str(i),{}))
  seen=set()
  for i,(sector,count,decoded,digest) in enumerate(c['rows']):
   if not count or sector in seen:continue
   seen.add(sector)
   if i in meta:data=meta[i]
   else:data=next(R/d/digest for d in ['original-cache','variant-cache','preview-cache'] if (R/d/digest).is_file()).read_bytes()
   if t.sha(data)!=digest or len(data)!=count*2048:raise ValueError(f'Pack identity mismatch {base}:{i}')
   pos=sector*2048;offset=0
   while offset<len(data):
    n=min(len(data)-offset,t.VOLUME_BYTES-pos%t.VOLUME_BYTES);f=fs[pos//t.VOLUME_BYTES];f.seek(pos%t.VOLUME_BYTES);f.write(data[offset:offset+n]);pos+=n;offset+=n
  for f in fs:f.close()
  (out/(base+'.hed')).write_bytes(b''.join(struct.pack('<IHH',*r[:3]) for r in c['rows'])+struct.pack('<IHH',0xffffffff,0xffff,0xffff));(out/(base+'.nam')).write_bytes(bytes.fromhex(c['names']));print('packed',base,flush=True)
def build(config):
 rec=load('recipe.json.gz');seen=set();tasks=[]
 for base,a in rec['archives'].items():
  for i,c in enumerate(a['originals']):
   if c['stored_sha256'] not in seen:tasks.append((base,i));seen.add(c['stored_sha256'])
 phases=[('originals',original_job,tasks),('variants',variant_job,[(base,k) for base,a in rec['archives'].items() for k in a['variants']]),('previews',preview_job,list(range(len(load('source-contract.json.gz')['previews']))))]
 workers=config.get('workers',3)
 for name,fn,tasks in phases:
  with ProcessPoolExecutor(max_workers=workers,initializer=init,initargs=(config,)) as pool:
   for n,row in enumerate(b.bounded(pool,fn,tasks,workers)):
    if n%250==0:print(name,n,'/',len(tasks),flush=True)
 pack_banks(config)
