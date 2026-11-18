"""Resume the fixed 70% archive stage without repeating verified archives."""
import gzip,json,re,shutil,struct,os
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor
import resize_story_variants as t
from replay_images import load,DATA
from recovery_kit import file_sha
from build_recovered_runtime import bounded

def build(config):
 os.environ['KNIFE_EXACT_REPLAY']='1'
 root=Path(config['work']);source=root/'rebuilt-100';out=root/'rebuilt-70';out.mkdir(exist_ok=True)
 source_meta=load('manifest-100.json.gz');target=load('manifest-70.json.gz');expected={r['path']:r for r in target['files']}
 protected=set()
 for filename in ['vita_live_alpha_data.rs','vita_sparse_coverage.rs']:
  protected.update(re.findall(r'entry: "(KQ[^"]+)"',(t.ROOT/'knife/knife-render/src'/filename).read_text(encoding='utf8')))
 for base,count in source_meta['story_gxt_variants']['original_entries'].items():
  names=[n for n in expected if n.startswith(base+'.')]
  if all((out/n).is_file() and file_sha(out/n)==expected[n]['sha256'] for n in names):
   print('verified 70% archive',base,flush=True);continue
  volumes=source_meta.get('story_archive_volumes',{}).get(base,1);r=t.Reader(source,base,volumes);w=t.Writer(out,base,overwrite_incomplete=True)
  for i in range(count):w.write(r.name(i),r.read(i),r.rows[i][2])
  w.names[:]=r.names[:count*32];tasks=[];i=count
  while i<len(r.rows)-2:
   tasks.append((i,.7,base=='jallpac' and r.name(i) in protected));raw=t.unpack(r.read(i))
   i+=struct.unpack_from('<I',raw,24)[0]*struct.unpack_from('<I',raw,28)[0]+2 if raw[:4]==b'KQTL' else 1
  report=dict(variants=[]);workers=config.get('workers',3)
  with ProcessPoolExecutor(max_workers=workers,initializer=t.init_worker,initargs=(str(source),base,volumes,config['python_deps'])) as pool:
   for result in bounded(pool,t.convert,tasks,workers):t.consume(result,r,w,report,base)
  w.finish();r.volume.__exit__(None,None,None)
  for name in names:
   if file_sha(out/name)!=expected[name]['sha256']:raise ValueError('70% archive differs '+name)
  print('70% ARCHIVE EXACT',base,flush=True)
 for p in source.rglob('*'):
  if not p.is_file():continue
  rel=p.relative_to(source)
  if rel.name=='vita-540p-assets.json' or any(rel.name.startswith(base+'.') for base in source_meta['story_gxt_variants']['original_entries']):continue
  dest=out/rel;dest.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(p,dest)
 (out/'vita-540p-assets.json').write_bytes(gzip.decompress((DATA/'manifest-70.json.gz').read_bytes()))
