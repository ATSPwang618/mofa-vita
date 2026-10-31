#!/usr/bin/env python3
"""Build filename-only stream metadata from the existing asset manifest."""
import argparse, hashlib, json
from pathlib import Path, PurePosixPath
p=argparse.ArgumentParser(description=__doc__);p.add_argument('manifest',type=Path);p.add_argument('output',type=Path);a=p.parse_args()
data=a.manifest.read_bytes();d=json.loads(data);names=[]
for row in d['files']:
 name=row['path'];parts=PurePosixPath(name).parts
 if not name.startswith('stream/'):continue
 if len(parts)!=2 or '..' in parts or '\\' in name or '\n' in name:raise ValueError('unsafe stream name')
 names.append(name)
if len(names)!=len(set(names)):raise ValueError('duplicate stream name')
text='# knife-stream-index-v1 '+hashlib.sha256(data).hexdigest()+'\n'+'\n'.join(sorted(names))+'\n'
a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(text)
print(f'{len(names)} filename entries; {len(text.encode())} bytes; manifest {hashlib.sha256(data).hexdigest()}')
