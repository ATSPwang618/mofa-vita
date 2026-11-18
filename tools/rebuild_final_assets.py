"""BASE NSP + 1.02 update + Chengkong MOD + logo + firmware -> exact final assets.

The fixed historical recipe (including disclosed encoder corrections) ships with
this source revision. No prepared game pack or private proof payload is required.
"""
import argparse,gzip,hashlib,json,os,shutil,subprocess,sys,time,uuid
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
DATA=ROOT/'tools/source_replay'

def sha(p):
 with Path(p).open('rb') as f:return hashlib.file_digest(f,'sha256').hexdigest()
def source_sha(p):
 p=Path(p)
 if p.suffix.lower() in {'.py','.rs','.c','.h','.cpp','.md','.json','.toml','.lock'} or p.name=='COPYING.LGPL-2.1':
  return hashlib.sha256(p.read_bytes().replace(b'\r\n',b'\n')).hexdigest()
 return sha(p)
def save(p,x):
 t=p.with_suffix(p.suffix+'.tmp');t.write_text(json.dumps(x,ensure_ascii=False,indent=2)+'\n',encoding='utf8');t.replace(p)
def tree(root):return {p.relative_to(root).as_posix():dict(bytes=p.stat().st_size,sha256=sha(p)) for p in sorted(root.rglob('*')) if p.is_file()}
def verify_tree(root,contract):
 if tree(root)!=contract:raise ValueError('Checkpoint changed: '+str(root))
def load(name):return json.loads(gzip.decompress((DATA/name).read_bytes()))
def run(args,cfg):subprocess.run([sys.executable,'-X','utf8',str(ROOT/'tools'/args[0]),*map(str,args[1:])],cwd=ROOT,check=True)
def input_file(c,key):
 p=Path(c[key]).resolve()
 if not p.is_file():raise ValueError('Missing input '+key+': '+str(p))
 return p

def extract(c,R,state):
 from rebuild_from_nsp import member,PROGRAM,PROGRAM_SHA,UPDATE_PROGRAM,UPDATE_SHA
 # Exact program members are checked, rather than NSP filename or title alone.
 for key,name,expected,tempname in [('nsp',PROGRAM,PROGRAM_SHA,'program.nca'),('update',UPDATE_PROGRAM,UPDATE_SHA,'update.nca')]:
  p=input_file(c,key);off,size=member(p,name);temp=R/tempname
  if not state.get('romfs'):
   if not temp.exists():
    with p.open('rb') as source,temp.open('xb') as dst:
     source.seek(off);left=size
     while left:
      data=source.read(min(4*1024**2,left))
      if not data:raise ValueError('Truncated NSP')
      dst.write(data);left-=len(data)
   if sha(temp)!=expected:raise ValueError('Unsupported/corrupted '+key)
  else:
   digest=hashlib.sha256()
   with p.open('rb') as f:
    f.seek(off);left=size
    while left:
     data=f.read(min(4*1024**2,left))
     if not data:raise ValueError('Truncated NSP')
     digest.update(data);left-=len(data)
   if digest.hexdigest()!=expected:raise ValueError('Source member changed '+key)
 if state.get('romfs'):verify_tree(R/'patched',state['romfs']);return
 out=R/'patched'
 if out.exists():raise ValueError('Partial extraction exists; inspect extract.log before removing the owned partial stage')
 with (R/'extract.log').open('wb') as log:subprocess.run([c['nstool'],'-k',c['keys'],'--basenca',str(R/'program.nca'),'-x','/1',str(out),str(R/'update.nca')],stdout=log,stderr=subprocess.STDOUT,check=True)
 from source_inputs import resolve
 resolve(out,Path(c['mod']),Path(c['logo']),load('source-contract.json.gz'))
 state['romfs']=tree(out);save(R/'state.json',state)
 for name in ('program.nca','update.nca'):(R/name).unlink()

def fonts(c,R):
 expected={n:r['sha256'] for n,r in load('source-contract.json.gz')['files'].items() if n.endswith('.bfttf')};dst=R/'firmware';dst.mkdir(exist_ok=True)
 def collect(root):
  for p in Path(root).rglob('*.bfttf'):
   if p.name in expected and sha(p)==expected[p.name]:
    target=dst/p.name
    if target.resolve()!=p.resolve():shutil.copyfile(p,target)
 collect(dst)
 if all((dst/n).is_file() and sha(dst/n)==h for n,h in expected.items()):return
 source=Path(c['firmware']);collect(source)
 if all((dst/n).is_file() and sha(dst/n)==h for n,h in expected.items()):return
 # Firmware may be supplied as the user's extracted Ryujinx registered directory.
 import re
 for p in source.glob('*.nca'):
  p=p/'00' if p.is_dir() else p
  if not p.is_file() or p.stat().st_size<128*1024:continue
  probe=subprocess.run([c['nstool'],'-k',c['keys'],str(p)],capture_output=True,check=True).stdout.decode(errors='replace')
  if not re.search(r'010000000000081[14]',probe,re.I):continue
  stage=dst/('nca-'+p.parent.stem if p.name=='00' else 'nca-'+p.stem);stage.mkdir(exist_ok=True)
  subprocess.run([c['nstool'],'-k',c['keys'],'-x',str(stage),str(p)],capture_output=True,check=True);collect(stage)
  save(stage/'source.json',dict(input=str(p),sha256=sha(p)))
  if all((dst/n).is_file() and sha(dst/n)==h for n,h in expected.items()):return
 raise ValueError('Required firmware fonts unavailable; fonts are not present in the retail NSP')

def preflight(c):
 for k in ['nsp','update','logo','keys','nstool','encoder','packer']:c[k]=str(input_file(c,k))
 c['ffmpeg']=str(Path(shutil.which(c.get('ffmpeg','ffmpeg')) or c.get('ffmpeg','ffmpeg')).resolve())
 for key in ['mod','firmware','python_deps']:
  c[key]=str(Path(c[key]).resolve())
  if not Path(c[key]).is_dir():raise ValueError('Missing directory '+key)
 for key in ['work','output']:c[key]=str(Path(c[key]).resolve())
 R=Path(c['work']);out=Path(c['output'])
 if R==ROOT or not R.is_relative_to(ROOT/'.work') or out==ROOT or R.is_relative_to(out) or out.is_relative_to(R):raise ValueError('Work must be a dedicated repository .work child; output must be separate')
 if not 1<=c.get('workers',3)<=8:raise ValueError('workers must be 1..8')
 sys.path.insert(0,c['python_deps']);import PIL,numpy,ispc_texcomp
 lock=json.loads((DATA/'toolchain-lock.json').read_bytes())
 for k in ['nstool','encoder','packer','ffmpeg']:
  if sha(c[k])!=lock['tools'][k]:raise ValueError('Tool differs from locked version: '+k)
 for name,value in [('pillow',PIL.__version__),('numpy',numpy.__version__),('ispc_texcomp',ispc_texcomp.__version__)]:
  if value!=lock['versions'][name]:raise ValueError('Python dependency differs: '+name)
 for rel,h in lock['generators'].items():
  if source_sha(ROOT/rel)!=h:raise ValueError('Generator changed: '+rel)
 for rel,h in lock['recipe_files'].items():
  if source_sha(DATA/rel)!=h:raise ValueError('Recipe file changed: '+rel)
 if sys.version.split()[0]!=lock['versions']['python']:raise ValueError('Use the locked Python version')
 kit=load('source-contract.json.gz')
 if sha(c['logo'])!=kit['files']['movie/logo.mp4']['sha256']:raise ValueError('Custom logo differs from target')
 inputs={k:dict(path=c[k],sha256=sha(c[k])) for k in ['logo']}
 inputs['firmware']=dict(path=c['firmware'],expected_fonts={n:r['sha256'] for n,r in kit['files'].items() if n.endswith('.bfttf')})
 for key in ['nsp','update']:
  inputs[key]=dict(path=c[key],bytes=Path(c[key]).stat().st_size,identity='SHA-256 of the selected Program NCA; checked during extract')
 for rel,h in lock['mod_files'].items():
  p=Path(c['mod'])/rel
  if not p.is_file() or sha(p)!=h:raise ValueError('Translation input differs: '+rel)
  inputs['mod/'+rel]=dict(path=str(p),sha256=h)
 for rel,h in lock['project_art'].items():
  if sha(ROOT/rel)!=h:raise ValueError('Project artwork differs: '+rel)
 return dict(recipe_sha256=hashlib.sha256(json.dumps(lock,sort_keys=True,separators=(',',':'),ensure_ascii=True).encode()).hexdigest(),inputs=inputs,source_members=lock['source_members'])

def main():
 os.environ['KNIFE_EXACT_REPLAY']='1'
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--config',type=Path,required=True);p.add_argument('--check',action='store_true');p.add_argument('--keep-work',action='store_true');a=p.parse_args();c=json.loads(a.config.read_text(encoding='utf-8-sig'));identity=preflight(c);R=Path(c['work']);out=Path(c['output']);print('Inputs and locked tools verified',flush=True)
 if a.check:return
 from verify_final_delivery import verify
 lock=json.loads((ROOT/'doc/evidence/59-final-assets-lock.json').read_bytes())
 if out.exists():
  if not (out/'source-rebuild-receipt.json').exists():raise ValueError('Output exists; select a new output directory')
  previous=json.loads((out/'source-rebuild-receipt.json').read_bytes())
  if previous.get('recipe_sha256')!=identity['recipe_sha256'] or previous.get('sources')!=identity:raise ValueError('Existing output belongs to different source recipe')
  result=verify(out/'assets',lock);print(json.dumps(result));return
 R.mkdir(parents=True,exist_ok=True);owner=R/'.exact-replay-owner.json'
 if owner.exists():
  if json.loads(owner.read_bytes())!=identity:raise ValueError('Sources or recipe changed; use a new work directory')
 else:
  if any(R.iterdir()):raise ValueError('Fresh work directory must be empty')
  if shutil.disk_usage(R).free<50*1024**3:raise ValueError('Fresh replay needs at least 50 GiB free scratch space')
  save(owner,identity)
 token=R/'exact-replay.lock'
 with token.open('x') as f:f.write(str(os.getpid()))
 state=json.loads((R/'state.json').read_bytes()) if (R/'state.json').exists() else {}
 try:
  extract(c,R,state);fonts(c,R)
  if not state.get('images'):
   import replay_images
   replay_images.build(c);state['images']=True;save(R/'state.json',state)
  if not state.get('full'):
   import replay_misc
   replay_misc.copy_direct(c);replay_misc.build_ui(c);replay_misc.build_audio(c);replay_misc.build_movies(c);state['full']=tree(R/'rebuilt-100');save(R/'state.json',state)
  else:verify_tree(R/'rebuilt-100',state['full'])
  if not state.get('small'):
   import replay_resize
   replay_resize.build(c)
   state['small']=tree(R/'rebuilt-70');save(R/'state.json',state)
  else:verify_tree(R/'rebuilt-70',state['small'])
  import replay_final_textures
  replay_final_textures.build(c);result=verify(R/'final-assets',lock)
  if preflight(c)!=identity:raise ValueError('Recipe changed during build')
  result.update(schema='source-exact-replay-v1',source_recipe_verified=True,recipe_sha256=identity['recipe_sha256'],sources=identity,hardware_tested=False,scope='assets only; excludes EBOOT/VPK',historical_corrections=True,pure_nsp_only=False)
  out.mkdir();(R/'final-assets').rename(out/'assets');save(out/'source-rebuild-receipt.json',result);save(R/'completion.json',result)
  if not a.keep_work:
   allowed=['patched','firmware','original-cache','variant-cache','preview-cache','metadata-cache','rebuilt-100','rebuilt-70','exact-pvrtc-cache','audio-stage']
   for name in allowed:
    target=(R/name).resolve()
    if target.parent!=R.resolve():raise ValueError('Unsafe cleanup path')
    if target.exists():
     if target.is_symlink() or (hasattr(target,'is_junction') and target.is_junction()):raise ValueError('Refusing linked cleanup')
     for child in target.rglob('*'):
      if child.is_symlink() or (hasattr(child,'is_junction') and child.is_junction()):raise ValueError('Refusing nested linked cleanup')
     shutil.rmtree(target)
   for name in ['encode-deterministic-opening_en.mp4','encode-deterministic-opening_zc.mp4']:
    temp=R/name;temp.unlink(missing_ok=True);temp.with_suffix('.ready.json').unlink(missing_ok=True)
  print('EXACT FINAL DELIVERY VERIFIED',out,flush=True)
 finally:token.unlink(missing_ok=True)
if __name__=='__main__':main()
