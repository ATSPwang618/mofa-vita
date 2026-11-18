"""Bounded source audit: MOD byte matches, bilingual text replay, update PFS index.

No NSP extraction or texture conversion. A mismatch means transformed/unresolved,
not necessarily external/custom content. Outputs contain metadata, not game data.
"""
import argparse
import json
from pathlib import Path
import struct

from asset_reproducibility import digest
from slim_languages import unpack_mzp, pack_mzp


def update_index(path):
    with path.open('rb') as stream:
        header=stream.read(16)
        if len(header)!=16: raise ValueError('Truncated PFS header')
        magic,count,size,_=struct.unpack('<4sIII',header)
        if magic!=b'PFS0' or not 0<count<=4096 or size>1024*1024:
            raise ValueError('Invalid PFS index')
        table=stream.read(count*24);names=stream.read(size);start=stream.tell()
        if len(table)!=count*24 or len(names)!=size: raise ValueError('Truncated index')
    entries=[]
    for offset,length,n,_ in struct.iter_unpack('<QQII',table):
        if offset+length>path.stat().st_size-start or n>=len(names) or b'\0' not in names[n:]:
            raise ValueError('PFS member outside input')
        entries.append(dict(name=names[n:names.index(0,n)].decode('utf8'),bytes=length))
    return entries


def audit(kit,mod,update):
    contract=json.loads((kit/'kit.json').read_bytes())
    result=dict(schema='knife-source-audit-v2',kit_sha256=digest(kit/'kit.json'),
                mod_files=[],file_origins={},source_reproducible_release=False)
    for p in sorted(mod.rglob('*')):
        if not p.is_file():continue
        rel=p.relative_to(mod).as_posix();sha=digest(p);reference=contract['files'].get(rel)
        equal=reference is not None and reference['sha256']==sha
        result['mod_files'].append(dict(path=rel,bytes=p.stat().st_size,sha256=sha,
                                       reference_present=reference is not None,reference_identical=equal))
        if equal:result['file_origins'][rel]=dict(status='verified-source-copy',source='MOD',sha256=sha)
    source=mod/'script_text.mrg';target=kit/'private/script_text.mrg'
    blocks=unpack_mzp(source.read_bytes());reference=unpack_mzp(target.read_bytes())
    if len(blocks)!=10:raise ValueError('Unexpected MOD language table')
    output=pack_mzp(blocks[2:6])
    equal=output==target.read_bytes()
    result['translation']=dict(source_blocks=len(blocks),reference_blocks=len(reference),
        selected_indices=[2,3,4,5],english_chengkong_repack_identical=equal,
        source_sha256=digest(source),target_sha256=digest(target),
        generator_sha256=digest(Path(__file__).with_name('slim_languages.py')))
    if equal:result['file_origins']['script_text.mrg']=dict(status='verified-transform',
        source='MOD/script_text.mrg',operation='unpack_mzp -> blocks[2:6] -> pack_mzp',
        source_sha256=digest(source),output_sha256=digest(target))
    for rel,row in contract['files'].items():
        if rel in result['file_origins']:continue
        if row['source']=='romfs':status='BASE-match-recorded-in-kit'
        elif rel.startswith('nintendo_'):status='firmware-input-required'
        else:status='transformed-or-unresolved'
        result['file_origins'][rel]=dict(status=status,sha256=row['sha256'])
    result['update']=dict(name=update.name,bytes=update.stat().st_size,sha256=digest(update),
                         members=update_index(update),merged_romfs_checked=False)
    result['limits']=['BASE matches use saved kit evidence, not a fresh full NSP extraction',
        'Update contents not merged; changed voices have not been attributed to update',
        'MOD UI requires conversion; archive hash inequality is not proof of missing source',
        'Proof images and composed previews retain recovery-only provenance',
        'Successful MOD text replay does not validate other production stages']
    return result


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ('kit','mod','update','report'):parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args()
    report=audit(args.kit,args.mod,args.update)
    args.report.parent.mkdir(parents=True,exist_ok=True)
    args.report.write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n','utf8')
    print('Text replay identical:',report['translation']['english_chengkong_repack_identical'])
    print('Direct MOD matches:',sum(r['reference_identical'] for r in report['mod_files']))
    print('Source-reproducible release: False; full extraction not performed')
