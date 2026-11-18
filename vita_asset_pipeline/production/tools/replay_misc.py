"""Non-story resources rebuilt from declared source inputs and fixed project art."""
import io,json,struct,re,shutil,subprocess,gzip,importlib.util,hashlib
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor
import replay_images as q
from recovery_kit import file_sha
from source_inputs import resolve
from compact_media import encode_portable
from optimize_compact import optimize_portable,textures
import slim_languages as slim

def atlas(rows):return struct.pack('<4sII',b'KATL',1,len(rows))+b''.join(struct.pack('<II',len(n.encode('utf8')),len(d))+n.encode('utf8')+d for n,d in rows)
def check(data,record,label):
 if q.b.texture.sha(data)!=record['sha256']:raise ValueError('Source output mismatch '+label)
 return data

def build_ui(config):
 q.init(config);b=q.b;R=q.R;out=R/'rebuilt-100';meta=q.load('misc-banks.json.gz');containers=q.load('ui-containers.json.gz');source=b.texture.Reader(R/'patched','allui',1);mod=Path(config['mod']);aliases=[];entries={};oldtitles={};title=None
 font=slim.ImageFont.truetype;slim.ImageFont.truetype=lambda path,*a,**k:font(str(mod/'zc/arshanghaisonggbpro_lt.otf'),*a,**k)
 with (mod/'allui.mrg').open('rb') as src:
  for i,row in enumerate(meta['allui']['rows']):
   name=source.name(i);sector,count,_=source.rows[i];src.seek(sector*2048);raw=b.original.unpack(src.read(count*2048))
   if re.search(r'_JA\.NXZ$',name):aliases.extend((name[:-7],label) for _,label,_,_ in b.original.textures(raw))
   if name=='NOFILE.EXISTS' or re.search(r'_(JA|ZT|KO)\.NXZ$',name):actual=b''
   elif raw[:4]==b'BNTX':
    rows=[];legacy_rows=[]
    for k,label,im,fmt in b.original.textures(raw,production_bc3=True):
     portable,_=encode_portable(im,im.size,srgb=fmt not in ('0x201','0x1d01','0x2001','0x1a01','0x1c01'),force_png=True);portable,_=optimize_portable(portable)
     if (i==49 and k in (170,247,256,260)) or (i==65 and k in range(128,134)):portable=slim.slim_kimg(portable)
     elif i in (1,2) and k==21:portable=slim.language_plate(portable)
     if name!='BORDER_PARTS.NXZ' and struct.unpack_from('<I',portable,24)[0] in (1,2):portable=q.legacy.migrate(b.Image.open(io.BytesIO(portable[32:])).convert('RGBA'),struct.unpack_from('<II',portable,8),ui=True)
     legacy_rows.append((label,portable))
     if i in (31,32) and k==0:
      if title is None:
       q.shim.mode(1);title=q.strip(b.texture.unpack(b.texture.encode(im.resize((960,540),b.Image.Resampling.LANCZOS),im.size)[0]));q.shim.mode(0)
      portable=title
     portable=q.correct(f'ui/{i}/{k}',portable);rows.append((label,portable))
    actual=atlas(rows)
    if i in (31,32):oldtitles[i]=atlas(legacy_rows)
   elif i==67:actual=slim.slim_sysmes(raw)
   elif i==68:
    count=struct.unpack_from('<H',raw)[0];actual=raw[:8]+raw[8+count*8:8+count*24]
   else:actual=raw
   c=containers[i]
   if b.texture.sha(actual)!=c['raw_sha256']:raise ValueError(f'UI raw mismatch {i}')
   entries[i]=q.wrap(actual,c)
   if b.texture.sha(entries[i])!=row[3]:raise ValueError(f'UI storage mismatch {i}')
  with (out/'allui.mrg').open('w+b') as f:
   f.truncate(meta['allui']['bytes'])
   for i,(sector,count,decoded,h) in enumerate(meta['allui']['rows']):f.seek(sector*2048);f.write(entries[i])
   for old in meta['old_titles']:
    data=oldtitles[old['index']];data+=bytes(-len(data)%2048)
    if b.texture.sha(data)!=old['sha256']:raise ValueError('Old title replay differs')
    f.seek(old['sector']*2048);f.write(data)
  (out/'allui.hed').write_bytes(b''.join(struct.pack('<IHH',*r[:3]) for r in meta['allui']['rows'])+struct.pack('<IHH',0xffffffff,0xffff,0xffff));(out/'allui.nam').write_bytes(bytes.fromhex(meta['allui']['names']))
  (out/'ui-aliases.tsv').write_text(''.join(f'{a}\t{n}\n' for a,n in aliases),encoding='utf8')
  if file_sha(out/'allui.mrg')!=meta['allui']['sha256'] or file_sha(out/'ui-aliases.tsv')!=meta['ui-aliases.tsv']:raise ValueError('UI bank mismatch')
 # Parts source has only the first two entries; the last two are repository artwork.
 rows=list(struct.iter_unpack('<IHH',(R/'patched/parts.hed').read_bytes()));data=(R/'patched/parts.mrg').read_bytes();parts={}
 for i in (0,1):
  s,n,_=rows[i];raw=b.original.unpack(data[s*2048:(s+n)*2048]);_,_,im,fmt=next(b.original.textures(raw,production_bc3=True));a,_=encode_portable(im,im.size,srgb=fmt not in ('0x201','0x1d01','0x2001','0x1a01','0x1c01'),force_png=False);a,_=optimize_portable(a)
  if struct.unpack_from('<I',a,24)[0] in (1,2):a=q.legacy.migrate(b.Image.open(io.BytesIO(a[32:])).convert('RGBA'),im.size)
  parts[i]=q.wrap(a,{'nx_level':meta['parts_levels'][i]})
 spec=importlib.util.spec_from_file_location('credit',Path(__file__).parent/'credit-intro/apply_credit_intro.py');credit=importlib.util.module_from_spec(spec);spec.loader.exec_module(credit)
 for i,n in enumerate(credit.IMAGES,2):parts[i]=credit.build_entry(credit.HERE/n)[0]
 with (out/'parts.mrg').open('w+b') as f:
  f.truncate(meta['parts']['bytes'])
  for i,(s,n,d,h) in enumerate(meta['parts']['rows']):
   if b.texture.sha(parts[i])!=h:raise ValueError(f'Parts mismatch {i}')
   f.seek(s*2048);f.write(parts[i])
 (out/'parts.hed').write_bytes(b''.join(struct.pack('<IHH',*r[:3]) for r in meta['parts']['rows'])+struct.pack('<IHH',0xffffffff,0xffff,0xffff))
 if file_sha(out/'parts.mrg')!=meta['parts']['sha256']:raise ValueError('Parts bank mismatch')
 source.volume.__exit__(None,None,None)
 for reader in b.originals.readers.values():reader.volume.__exit__(None,None,None)
 if q.legacy.proc is not None:
  q.legacy.proc.stdin.close();q.legacy.proc.wait(timeout=15);q.legacy.proc=None

def audio_job(task):
 source,dest,ffmpeg,c=task;d=Path(dest)
 if d.is_file() and file_sha(d)==c['sha256']:return str(d)
 if d.exists():d.unlink() # Only this task's owned incomplete stream, never the source.
 return q.b.audio_job((source,dest,ffmpeg,c))

def build_audio(config):
 R=Path(config['work']);out=R/'rebuilt-100';kit=q.load('source-contract.json.gz');contracts=q.load('audio-contract.json.gz')['streams'];expected=q.load('source-identities.json.gz')['banks'];stage=R/'audio-stage';stage.mkdir(exist_ok=True)
 from pack_audio_bank import pack
 for bank,names in kit['banks'].items():
  if (out/bank).is_file() and file_sha(out/bank)==expected[bank]:continue
  tasks=[(str(R/'patched/stream'/Path(n).with_suffix('.opus')),str(stage/n),config['ffmpeg'],contracts['stream/'+n]) for n in names if not n.endswith('.bgmstart.mrg')]
  with ProcessPoolExecutor(max_workers=config.get('workers',3)) as pool:
   for i,_ in enumerate(q.b.bounded(pool,audio_job,tasks,config.get('workers',3))):
    if i%500==0:print('audio',bank,i,flush=True)
  if 'M10.bgmstart.mrg' in names:
   from rebuild_bgm_lead import make_lead
   (stage/'M10.bgmstart.mrg').write_bytes(make_lead((stage/'M10.ogg').read_bytes()))
  temp=out/(bank+'.building')
  if temp.exists():temp.unlink()
  pack({n:(stage/n,0,(stage/n).stat().st_size) for n in names},temp)
  if file_sha(temp)!=expected[bank]:raise ValueError('Audio bank mismatch '+bank)
  temp.replace(out/bank)
  for n in names:(stage/n).unlink()

def build_movies(config):
 R=Path(config['work']);out=R/'rebuilt-100';contracts=q.load('source-identities.json.gz');corrections=q.load('historical-corrections.json.gz')['records'];(out/'movie').mkdir(exist_ok=True)
 for name in ['opening_en.mp4','opening_zc.mp4']:
  p=out/'movie'/name;key='movie/'+name
  if p.is_file() and file_sha(p)==contracts['movies'][key]:continue
  source=R/'patched/movie'/name
  if file_sha(source)!=contracts['movie_sources'][key]:raise ValueError('Movie source changed '+name)
  temp=R/('encode-deterministic-'+name);ready=temp.with_suffix('.ready.json')
  if not ready.exists():
   temp.unlink(missing_ok=True) # Owned incomplete output only.
   subprocess.run([config['ffmpeg'],'-nostdin','-v','error','-i',str(source),'-map','0:v:0','-map','0:a?','-map_metadata','-1','-filter_threads','1','-vf','scale=960:540:flags=lanczos','-c:v','libx264','-preset','medium','-crf','22','-pix_fmt','yuv420p','-threads','1','-c:a','copy','-movflags','+faststart',str(temp)],check=True)
   ready.write_text(json.dumps(dict(sha256=file_sha(temp))),encoding='utf8')
  if file_sha(temp)!=json.loads(ready.read_bytes())['sha256']:raise ValueError('Movie checkpoint changed '+name)
  raw=temp.read_bytes();rec=corrections.get(key)
  if rec:raw=q.apply(raw,bytes.fromhex(rec['patch']),rec)
  if q.b.texture.sha(raw)!=contracts['movies'][key]:raise ValueError('Movie encoder differs '+name)
  p.write_bytes(raw)

def copy_direct(config):
 R=Path(config['work']);out=R/'rebuilt-100';out.mkdir(exist_ok=True);kit=q.load('source-contract.json.gz')
 for rel,(data,origin) in resolve(R/'patched',Path(config['mod']),Path(config['logo']),kit).items():
  p=out/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_bytes(data)
 (out/'language-profile.txt').write_bytes((q.DATA/'language-profile.txt').read_bytes())
 (out/'vita-540p-assets.json').write_bytes(gzip.decompress((q.DATA/'manifest-100.json.gz').read_bytes()))
 for p in (R/'firmware').rglob('*.bfttf'):
  if p.name in kit['files']:check(p.read_bytes(),kit['files'][p.name],p.name);shutil.copyfile(p,out/p.name)
 missing=[n for n in kit['files'] if n.endswith('.bfttf') and not (out/n).is_file()]
 if missing:raise ValueError('Missing declared firmware fonts: '+str(missing))
