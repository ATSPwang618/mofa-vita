"""Audit every entry of a PVRTC candidate against its original native DXT pack."""
import argparse
import json
from pathlib import Path
import re
import struct

from resize_story_variants import ROOT, Reader, sha, unpack
from generate_vita_asset_profile_540 import audio_bank_locations
from verify_resized_story_pack import digest_range
from vita_image_metadata import check_story_rgba_budget, image_info


def verify(source, candidate):
    report=json.loads((candidate.parent/'pvrtc-report.json').read_bytes())
    marker=(source/'vita-540p-assets.json').read_bytes()
    assert sha(marker)==report['source_manifest_sha256']
    original=json.loads(marker)
    manifest=json.loads((candidate/'vita-540p-assets.json').read_bytes())
    banks=audio_bank_locations(candidate)
    for row in manifest['files']:
        path=candidate/row['path']
        location=(path,0,path.stat().st_size) if path.is_file() else banks[row['path']]
        assert location[2]==row['bytes'] and digest_range(*location)==row['sha256'],row['path']
    print('Manifest hashes',len(manifest['files']),flush=True)
    originals=original['story_gxt_variants']['original_entries']
    assert originals==manifest['story_gxt_variants']['original_entries']
    unchanged=0
    for path in source.rglob('*'):
        if not path.is_file():continue
        rel=path.relative_to(source)
        if rel.name=='vita-540p-assets.json' or any(str(rel).startswith(b+'.') for b in originals):continue
        other=candidate/rel
        assert path.stat().st_size==other.stat().st_size
        assert digest_range(path,0,path.stat().st_size)==digest_range(other,0,other.stat().st_size),rel
        unchanged+=1
    accepted={(r['archive'],r['index']):r for r in report['accepted']}
    proofs=set()
    for path in (ROOT/'knife/knife-render/src').glob('vita*.rs'):
        proofs.update(re.findall(r'b"([a-f0-9]{64})"',path.read_text('utf8')))
    seen=set();base_count=fallbacks=variants=images=saved_gpu=0
    for base,count in originals.items():
        a=Reader(source,base,original.get('story_archive_volumes',{}).get(base,1))
        b=Reader(candidate,base,manifest.get('story_archive_volumes',{}).get(base,1))
        assert a.names==b.names and len(a.rows)==len(b.rows)
        for i in range(count):
            assert a.rows[i][1:]==b.rows[i][1:] and a.read(i)==b.read(i),(base,i)
            base_count+=1
        for i in range(count,len(a.rows)-2):
            old,new=unpack(a.read(i)),unpack(b.read(i))
            if old[:4]==b'KQTL':
                assert old[:32]==new[:32],(base,i,'grid changed')
                cols,rows=struct.unpack_from('<II',old,24)
                for slot in range(cols*rows+1):
                    ad=struct.unpack_from('<5I',old,32+slot*20)
                    bd=struct.unpack_from('<5I',new,32+slot*20)
                    assert ad[:3]==bd[:3]
                    if slot==0:
                        assert ad==bd and a.read(ad[0])==b.read(bd[0])
                        fallbacks+=1
                variants+=1
                continue
            assert old[:28]==new[:28],(base,i,'image geometry or encoding changed')
            oi,ni=image_info(old),image_info(new)
            images+=1
            if (base,i) in accepted:
                assert sha(old[:oi['envelope_bytes']]) not in proofs
                assert sha(old[32:oi['envelope_bytes']]) not in proofs
                record=accepted[base,i]
                assert record['alpha_rmse']<=4 and record['composite_rmse']<=5 and record['composite_p99']<=24
                assert oi['format']=='dxt5' and ni['format']=='pvrtc2_4'
                assert ni['gpu_payload_bytes']*2==oi['gpu_payload_bytes']
                assert len(b.read(i))<len(a.read(i))
                seen.add((base,i));saved_gpu+=oi['gpu_payload_bytes']-ni['gpu_payload_bytes']
            else:
                assert a.read(i)==b.read(i),(base,i,'unexpected reencode')
            if a.name(i).startswith('KQ') and not a.name(i).startswith('KQT'):variants+=1
        a.volume.__exit__(None,None,None);b.volume.__exit__(None,None,None)
        print('Verified dimensions / original bytes / native formats / fallbacks:',base,flush=True)
    assert seen==set(accepted)
    budget=check_story_rgba_budget(lambda n:candidate/n,manifest)
    return dict(manifest_files=len(manifest['files']),unchanged_files=unchanged,
        original_entries_identical=base_count,fallbacks_identical=fallbacks,
        variants=variants,appended_images=images,converted_images=len(seen),
        summed_entry_gpu_payload_reduction=saved_gpu,
        note='Summed entry payloads include duplicates; this is not resident memory savings.',
        production_metadata_and_budget=budget,hardware_tested=False)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source',type=Path,required=True)
    parser.add_argument('--candidate',type=Path,required=True)
    args=parser.parse_args()
    result=verify(args.source,args.candidate)
    (args.candidate.parent/'verification.json').write_text(json.dumps(result,indent=2)+'\n','utf8')
    print(json.dumps(result,indent=2),flush=True)
