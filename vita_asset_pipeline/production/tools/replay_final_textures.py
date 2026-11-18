"""Replay the fixed, previously quality-audited PVRTC selection, not a new trial."""
import argparse,json,struct,shutil,gzip,sys,os
from pathlib import Path
from concurrent.futures import ProcessPoolExecutor
import convert_story_pvrtc as p
from replay_images import load,DATA
from build_recovered_runtime import bounded
from recovery_kit import file_sha

def init(config,base):
 global selection,original_encode
 source=Path(config['work'])/'rebuilt-70';m=load('manifest-70.json.gz');selection=load('pvrtc-selection.json.gz').get(base,{})
 cache=Path(config['work'])/'exact-pvrtc-cache';cache.mkdir(exist_ok=True)
 p.init_worker(source,base,m.get('story_archive_volumes',{}).get(base,1),cache,config['encoder'],config['packer'],[])
 original_encode=p.encode;p.encode=encode

def encode(index,protected=False):
 data=p.reader.read(index);raw=p.unpack(data);w,h=struct.unpack_from('<II',raw,16);g=raw[32:32+int.from_bytes(raw[28:32],'little')];fmt,payload=struct.unpack_from('<I',g,52)[0],struct.unpack_from('<I',g,16)[0];rec=selection.get(str(index))
 if rec is None:return (data,p.reader.rows[index][2],(w,h,fmt,payload)),dict(reason='locked-DXT')
 if p.sha(data)!=rec['input_sha256']:raise ValueError(f'PVRTC input differs at {index}')
 block,result=original_encode(index,False)
 if result['reason']!='accepted' or p.sha(block[0])!=rec['stored_sha256']:raise ValueError(f'PVRTC output differs at {index}: {result}')
 return block,result

def build(config,only=None):
 work=Path(config['work']);source=work/'rebuilt-70';out=work/'final-assets';out.mkdir(exist_ok=True);m=load('manifest-70.json.gz');fm=load('manifest-final.json.gz');expected={x['path']:x for x in fm['files']};summary={}
 for base,count in m['story_gxt_variants']['original_entries'].items():
  if only and base!=only:continue
  names=[n for n in expected if n.startswith(base+'.')]
  if all((out/n).is_file() and file_sha(out/n)==expected[n]['sha256'] for n in names):print('verified cache',base,flush=True);continue
  r=p.Reader(source,base,m.get('story_archive_volumes',{}).get(base,1));w=p.Writer(out,base,overwrite_incomplete=True)
  for i in range(count):w.write(r.name(i),r.read(i),r.rows[i][2])
  w.names[:]=r.names[:count*32];tasks=[];i=count
  while i<len(r.rows)-2:
   tasks.append((i,False));raw=p.unpack(r.read(i));i+=struct.unpack_from('<I',raw,24)[0]*struct.unpack_from('<I',raw,28)[0]+2 if raw[:4]==b'KQTL' else 1
  with ProcessPoolExecutor(max_workers=config.get('workers',3),initializer=init,initargs=(config,base)) as pool:
   for n,(old,blocks,records) in enumerate(bounded(pool,p.convert,tasks,config.get('workers',3))):
    if len(w.rows)!=old:raise ValueError('Ordinal drift')
    raw=p.unpack(r.read(old))
    if raw[:4]==b'KQTL':
     head=raw[:32]+b''.join(struct.pack('<5I',old+1+j,*block[2]) for j,block in enumerate(blocks));w.write(r.name(old),*p.stored(head,True))
     for j,block in enumerate(blocks):w.write(r.name(old+1+j),block[0],block[1])
    else:w.write(r.name(old),blocks[0][0],blocks[0][1])
    if n%250==0:print('PVRTC',base,n,'/',len(tasks),flush=True)
  w.finish();(out/(base+'.nam')).write_bytes(r.names);r.volume.__exit__(None,None,None)
  for name in names:
   if file_sha(out/name)!=expected[name]['sha256']:raise ValueError('Final archive mismatch '+name)
  summary[base]=dict(exact=True,entries=len(w.rows)-1);print('FINAL ARCHIVE EXACT',base,flush=True)
 if only:return summary
 for src in source.rglob('*'):
  if not src.is_file():continue
  rel=src.relative_to(source)
  if rel.name=='vita-540p-assets.json' or any(rel.name.startswith(base+'.') for base in m['story_gxt_variants']['original_entries']):continue
  dst=out/rel;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(src,dst)
 (out/'vita-540p-assets.json').write_bytes(gzip.decompress((DATA/'manifest-final.json.gz').read_bytes()))
 return summary
if __name__=='__main__':
 ap=argparse.ArgumentParser(description=__doc__);ap.add_argument('--config',type=Path,required=True);ap.add_argument('--archive');args=ap.parse_args();build(json.loads(args.config.read_text(encoding='utf-8-sig')),args.archive)
