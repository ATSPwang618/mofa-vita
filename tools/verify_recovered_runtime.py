"""Audit a fresh full-resolution reconstruction against recovered contracts.

Checks every original image and appended key, not just representative samples.
This is a structural check, not a claim of pixel equality or device validation.
"""
import argparse
import json
from pathlib import Path
import struct

from recover_runtime_recipe import image_contract
from recovery_kit import file_sha
from resize_story_variants import Reader, unpack, sha


def verify(assets, kit):
    recipe=json.loads((kit/'recipe.json').read_bytes())
    manifest=json.loads((assets/'vita-540p-assets.json').read_bytes())
    report=dict(originals=0,variants=0,tiles=0,previews=0,proofs=0,
                recipe_sha256=file_sha(kit/'recipe.json'),hardware_tested=False)
    private=json.loads((kit/'kit.json').read_bytes())
    for base,archive in recipe['archives'].items():
        reader=Reader(assets,base,manifest.get('story_archive_volumes',{}).get(base,1))
        try:
            for i,row in enumerate(archive['originals']):
                assert reader.name(i)==row['name'], (base,i,'name')
                data=reader.read(i);actual=image_contract(unpack(data))
                if 'magic' in row:
                    assert sha(data)==row['stored_sha256'], (base,i,'non-image')
                else:
                    for key in ('logical','physical','codec','png_color_type','jpeg_tables','jpeg_subsampling'):
                        if key in row:assert actual.get(key)==row[key], (base,i,key)
                report['originals']+=1
            i=len(archive['originals'])
            for row in archive['variants']:
                assert reader.name(i)==row['name'], (base,i,'variant name')
                data=reader.read(i);raw=unpack(data)
                proof=private['proofs'].get(base+':'+row['name'])
                if proof:
                    # Grid descriptor ordinals may change, but image payloads may not.
                    for slot,p in enumerate(proof):
                        if unpack(reader.read(i+slot))[:4]!=b'KQTL':
                            assert sha(reader.read(i+slot))==p['sha256'], (base,i,slot,'proof')
                            report['proofs']+=1
                if row['tiled']:
                    magic,version,lw,lh,w,h,cols,rows=struct.unpack_from('<4s7I',raw)
                    assert magic==b'KQTL' and version==1
                    assert [lw,lh]==row['logical'] and [w,h]==row['physical']
                    assert cols*rows==row['tile_count']
                    for slot in range(cols*rows+1):
                        index,tw,th,fmt,size=struct.unpack_from('<5I',raw,32+slot*20)
                        assert index==i+1+slot
                        traw=unpack(reader.read(index));actual=image_contract(traw)
                        assert actual['physical']==[tw,th] and actual['format']==fmt
                        assert struct.unpack_from('<I',traw,48)[0]==size
                        if slot==0:
                            assert actual['logical']==row['fallback']['logical']
                            assert actual['physical']==row['fallback']['physical']
                        else:report['tiles']+=1
                    i+=2+cols*rows
                else:
                    actual=image_contract(raw)
                    assert actual['logical']==row['logical'] and actual['physical']==row['physical']
                    assert actual['codec']==9
                    i+=1
                report['variants']+=1
            assert i==len(reader.rows)-2 and reader.name(i)=='KQ_VARIANTS.NXZ'
        finally:reader.volume.__exit__(None,None,None)
    reader=Reader(assets,'savethumb',1)
    try:
        assert len(reader.rows)-1==len(private['previews'])
        for i,row in enumerate(private['previews']):
            assert reader.name(i)==row['name']
            data=reader.read(i)
            if 'private_sha256' in row:assert sha(data)==row['private_sha256']
            else:
                actual=image_contract(unpack(data))
                assert actual['physical']==row['physical'] and actual['logical']==row['logical']
            report['previews']+=1
    finally:reader.volume.__exit__(None,None,None)
    return report


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--assets',type=Path,required=True)
    p.add_argument('--kit',type=Path,required=True)
    a=p.parse_args();result=verify(a.assets,a.kit)
    (a.assets.parent/'recovered-verification.json').write_text(json.dumps(result,indent=2)+'\n','utf8')
    print(json.dumps(result,indent=2))
