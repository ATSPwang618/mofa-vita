"""Extract MOFA's three unchanged BFTTF shared fonts from local firmware."""
from concurrent.futures import ThreadPoolExecutor
import json
import re
import shutil

from .switch import hactool, sha_file, write_json

TITLES = {
    '0100000000000811': ('nintendo_udsg-r_std_003.bfttf',),
    '0100000000000814': ('nintendo_udsg-r_org_zh-cn_003.bfttf', 'nintendo_udsg-r_ext_zh-cn_003.bfttf'),
}


def prepare_fonts(firmware, work, executable, keys):
    output = work / 'fonts'
    output.mkdir(parents=True, exist_ok=True)
    marker = output / 'provenance.json'
    if marker.is_file():
        records = json.loads(marker.read_bytes())
        if all((output/name).is_file() and sha_file(output/name)==r['sha256'] for name,r in records.items()) and len(records)==3:
            return output
        raise ValueError('invalid font cache; select another work directory')
    candidates = [p/'00' if p.is_dir() else p for p in sorted(firmware.iterdir())]
    candidates = [p for p in candidates if p.is_file()]
    def inspect(path):
        label=path.parent.name if path.name=='00' else path.name
        info = hactool(executable, keys, work, 'firmware-'+label[:32]+'.log', '-i', path)
        match = re.search(r'(?:Title ID|Program I[dD])\s*:\s*(010000000000081[14])',info,re.I)
        return (path, match.group(1).lower()) if match and 'RomFS' in info else None
    records = {}
    with ThreadPoolExecutor(4) as pool:
        for first in range(0,len(candidates),16):
            for result in pool.map(inspect,candidates[first:first+16]):
                if result:
                    source,title = result
                    hactool(executable,keys,work,'font-'+title+'.log','--romfsdir='+str(output),source)
                    for name in TITLES[title]:
                        if not (output/name).is_file():
                            raise ValueError(f'font title does not contain {name}')
                        records[name]=dict(title_id=title,source=str(source),source_sha256=sha_file(source),
                                           sha256=sha_file(output/name))
            if len(records)==3:
                break
    if len(records)!=3:
        raise ValueError('firmware is missing MOFA shared font titles 0811 / 0814')
    write_json(marker,records)
    return output
