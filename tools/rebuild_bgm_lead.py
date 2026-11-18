"""Regenerate the bounded M10 opening stream from its exact new Ogg pages."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

from compact_media import ogg_crc
from generate_vita_asset_profile_540 import audio_bank_locations
from pack_audio_bank import pack


def make_lead(data):
    position=0;preskip=None;best=None;sequence=0;serial=None
    while position+27<=len(data) and position<256*1024:
        head=data[position:position+27]
        if head[:5]!=b'OggS\0':raise ValueError('Invalid Ogg page')
        count=head[26];lace=data[position+27:position+27+count]
        end=position+27+count+sum(lace)
        if len(lace)!=count or end>len(data):raise ValueError('Truncated Ogg page')
        page=bytearray(data[position:end]);expected=struct.unpack_from('<I',page,22)[0]
        page[22:26]=bytes(4)
        if ogg_crc(page)!=expected:raise ValueError('Ogg CRC mismatch')
        gp=struct.unpack_from('<Q',page,6)[0];sid,seq=struct.unpack_from('<II',page,14)
        if seq!=sequence or (serial is not None and sid!=serial):raise ValueError('Chained or disordered Ogg')
        serial=sid;sequence+=1;payload=page[27+count:]
        if preskip is None:
            if not payload.startswith(b'OpusHead') or len(payload)<19:raise ValueError('Missing Opus header')
            preskip=struct.unpack_from('<H',payload,10)[0]
        frames=gp-preskip if gp!=0xffffffffffffffff else -1
        if frames>5*48000:break
        if 3*48000<=frames<=5*48000 and lace and lace[-1]<255 and end<=256*1024:
            best=(position,end,frames)
        position=end
    if best is None:raise ValueError('No complete 3-5 second opening page within budget')
    start,end,frames=best;ogg=bytearray(data[:end]);ogg[start+5]|=4
    ogg[start+22:start+26]=bytes(4)
    struct.pack_into('<I',ogg,start+22,ogg_crc(ogg[start:end]))
    return struct.pack('<8sQII',b'KBGMST1\0',len(data),frames,len(ogg))+ogg


def refresh(assets):
    members=audio_bank_locations(assets)
    source,offset,size=members['stream/M10.ogg']
    with source.open('rb') as f:f.seek(offset);data=f.read(size)
    lead=make_lead(data);tmp=assets.parent/'new-M10.bgmstart.mrg';bank=assets/'bgm.mrg.new'
    if tmp.exists() or bank.exists():raise ValueError('Incomplete BGM rebuild retained; inspect before retry')
    tmp.write_bytes(lead)
    records={name.removeprefix('stream/'):value for name,value in members.items() if value[0].name=='bgm.mrg'}
    records['M10.bgmstart.mrg']=(tmp,0,len(lead))
    pack(records,bank);bank.replace(assets/'bgm.mrg');tmp.unlink()
    marker=assets/'vita-540p-assets.json';manifest=json.loads(marker.read_bytes())
    for row in manifest['files']:
        if row['path']=='stream/M10.bgmstart.mrg':
            row.update(bytes=len(lead),sha256=hashlib.sha256(lead).hexdigest())
    manifest['r4_m10_start']=dict(source_bytes=len(data),frames=struct.unpack_from('<I',lead,16)[0],
                                encoded_bytes=len(lead)-24,regenerated_from_current_ogg=True)
    marker.write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n','utf8')
    return manifest['r4_m10_start']


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--assets',required=True,type=Path)
    print(refresh(p.parse_args().assets))
