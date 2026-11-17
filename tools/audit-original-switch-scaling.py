"""Audit fresh Switch PNG dimensions against production image table metadata."""
import collections
import importlib.util
import json
import math
from pathlib import Path
import struct

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / 'dist/原始switch版本-540p分辨率项目'
ROMFS = ROOT / '.work/original-switch-png-20261114/romfs'

def strip_names():
    spec=importlib.util.spec_from_file_location('exporter',Path(__file__).with_name('export-original-switch-png.py'))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    return module.strip_names()

def main():
    m = json.loads((PROJECT/'manifest.json').read_text(encoding='utf8'))
    strips = strip_names()
    stats = {}
    problems = []
    for archive in ['jallpac','jallpachd','allpacml','allpachdml','allui','parts']:
        rows = [r for r in m['images'] if r.get('archive')==archive]
        wrong = []
        for r in rows:
            expected_strip = Path(r['name']).stem in strips
            if expected_strip != bool(r['language']):
                wrong.append(r)
                problems.append({'path':r['path'],'name':r['name'],'source':r['source_texture_size'],
                    'output':r['size'],'expected_strips':4 if expected_strip else 1,
                    'actual_language':r['language'],'kind':'language-contract'})
        stats[archive] = dict(count=len(rows), rules=dict(collections.Counter(r['rule'] for r in rows)),
            language_contract_mismatches=len(wrong),
            over_2048=sum(max(r['size'])>2048 for r in rows),
            over_1036800_pixels=sum(math.prod(r['size'])>960*1080 for r in rows),
            over_1MiB_rgba=sum(((r['size'][0]+7)//8*8)*r['size'][1]*4>1048576 for r in rows))
    report={'schema':'original-switch-scale-audit-v1','strip_names':sorted(strips),'archives':stats,'issues':problems}
    out=PROJECT/'metadata/scaling-audit-current-shapes.json'
    out.write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf8')
    print(json.dumps({'archives':stats,'issues_sample':problems[:12]},ensure_ascii=False,indent=2))

if __name__=='__main__': main()
