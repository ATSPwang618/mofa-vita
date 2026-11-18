"""Prepare a private reconstruction kit from the known 100% DXT reference.

This is a LOCAL recovery input, not a redistributable source-only recipe. It
explicitly retains non-NSP/custom content and proof-bound images. Never commit
the generated kit. Game image variants and audio bank payloads are not copied.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import struct

from recover_runtime_recipe import image_contract
from resize_story_variants import Reader, unpack, ROOT, sha
from generate_vita_asset_profile_540 import audio_bank_locations
from compact_media import nopus_packets, opus_samples


def file_sha(path):
    with path.open('rb') as f:
        return hashlib.file_digest(f,'sha256').hexdigest()


def opus_contract(data):
    """Read Ogg pages structurally; marker bytes may also occur in the payload."""
    position=0;head=None;last=None
    while position<len(data):
        if data[position:position+5]!=b'OggS\0':raise ValueError('Invalid Ogg page')
        count=data[position+26];lace=data[position+27:position+27+count]
        end=position+27+count+sum(lace)
        if len(lace)!=count or end>len(data):raise ValueError('Truncated Ogg')
        payload=data[position+27+count:end]
        if head is None:
            if not payload.startswith(b'OpusHead'):raise ValueError('Missing OpusHead')
            head=(payload[9],struct.unpack_from('<H',payload,10)[0])
        last=struct.unpack_from('<Q',data,position+6)[0];position=end
    return head[0],last-head[1]


def prepare(reference, romfs, recipe_file, luts_file, output):
    recipe=json.loads(recipe_file.read_bytes())
    marker=(reference/'vita-540p-assets.json').read_bytes()
    if recipe['source_manifest_sha256']!=sha(marker):
        raise ValueError('Reference identity differs from recovered recipe')
    manifest=json.loads(marker)
    output.mkdir(parents=True,exist_ok=False)
    (output/'private').mkdir()
    contract=dict(schema='knife-private-recovery-kit-v1', contains_game_data=True,
                  source_manifest_sha256=sha(marker), files={}, proofs={}, banks={}, previews=[])
    contract['provenance']=dict(method='recovered-output-contract',
        source_reproducible_release=False,
        classification_note='private-supplement means retained reference bytes; it does not prove '
                            'the source is absent from an update, MOD or original transformation',
        unresolved=['update/mod source merge','historical image transformation chain'])
    contract['grade_source_sha256']=file_sha(ROOT/'knife/knife/src/grade.rs')
    # Preserve contracts that apply to the copied UI and regenerated memory-bounded
    # images, not historical statistics or claims about earlier image conversions.
    contract['manifest_contracts']={k:manifest[k] for k in
        ('offline_story_memory_budget','offline_ui_ranges','r1_offline_script_tones','r4_m10_start')
        if k in manifest}
    for p in reference.rglob('*'):
        if not p.is_file():continue
        rel=p.relative_to(reference).as_posix()
        if (rel=='vita-540p-assets.json' or rel in ('voice.mrg','bgm.mrg')
                or any(rel.startswith(b+'.') for b in [*recipe['archives'],'savethumb'])):
            continue
        source=romfs/rel
        identical=source.is_file() and source.stat().st_size==p.stat().st_size and file_sha(source)==file_sha(p)
        record=dict(bytes=p.stat().st_size,sha256=file_sha(p),source='romfs' if identical else 'private-supplement')
        record['origin_status']='verified-BASE-copy' if identical else 'transformed-or-unresolved'
        contract['files'][rel]=record
        if not identical:
            dst=output/'private'/rel;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(p,dst)
    protected=set()
    for name in ['vita_live_alpha_data.rs','vita_sparse_coverage.rs']:
        protected.update(re.findall(r'entry: "(KQ[^\"]+)"',(ROOT/'knife/knife-render/src'/name).read_text('utf8')))
    for base,archive in recipe['archives'].items():
        reader=Reader(reference,base,manifest.get('story_archive_volumes',{}).get(base,1))
        try:
            for v in archive['variants']:
                if base!='jallpac' or v['name'] not in protected:continue
                start=v['index'];count=v.get('tile_count',0)+2 if v['tiled'] else 1
                entries=[]
                for i in range(start,start+count):
                    data=reader.read(i);digest=sha(data);path=output/'private/proofs'/digest
                    path.parent.mkdir(exist_ok=True)
                    if not path.exists():path.write_bytes(data)
                    entries.append(dict(sha256=digest,decoded=reader.rows[i][2],name=reader.name(i)))
                contract['proofs'][base+':'+v['name']]=entries
        finally:reader.volume.__exit__(None,None,None)
    for name,(path,offset,size) in audio_bank_locations(reference).items():
        contract['banks'].setdefault(path.name,[]).append(name.removeprefix('stream/'))
        with path.open('rb') as f:f.seek(offset);payload=f.read(size)
        audio_difference=False
        if name.endswith('.ogg'):
            source=romfs/Path(name).with_suffix('.opus')
            if source.is_file():
                channels,preskip,packets=nopus_packets(source.read_bytes())
                original_contract=(channels,sum(map(opus_samples,packets))-preskip)
                audio_difference=opus_contract(payload)!=original_contract
            else:audio_difference=True
        if name.endswith('.bgmstart.mrg') or audio_difference:
            dst=output/'private'/name;dst.parent.mkdir(parents=True,exist_ok=True)
            dst.write_bytes(payload)
            contract['files'][name]=dict(bytes=size,sha256=file_sha(dst),source='private-supplement')
            if audio_difference:contract['files'][name]['reason']='BASE audio sample/channel contract differs or source missing'
    thumbs=Reader(reference,'savethumb',1)
    try:
        for i,row in enumerate(thumbs.rows):
            if row[0]==0xffffffff:break
            data=thumbs.read(i);name=thumbs.name(i)
            record=dict(name=name,**image_contract(unpack(data)))
            marker_preview=name in ('A00FACE_TOP.GXT','A00THUMSP_TOP.GXT')
            if 'FINISHED_' in name or marker_preview:
                # Composed captures and two generated empty directory-marker
                # previews are not individual Switch image payloads.
                digest=sha(data);path=output/'private/proofs'/digest
                path.parent.mkdir(exist_ok=True)
                if not path.exists():path.write_bytes(data)
                record.update(private_sha256=digest,decoded=row[2])
                if marker_preview:record['private_reason']='generated-directory-marker-preview'
            contract['previews'].append(record)
    finally:thumbs.volume.__exit__(None,None,None)
    shutil.copyfile(recipe_file,output/'recipe.json')
    shutil.copyfile(luts_file,output/'grade-luts.bin')
    contract['recipe_sha256']=file_sha(output/'recipe.json')
    contract['luts_sha256']=file_sha(output/'grade-luts.bin')
    contract['private_bytes']=sum(p.stat().st_size for p in (output/'private').rglob('*') if p.is_file())
    (output/'kit.json').write_text(json.dumps(contract,indent=2)+'\n','utf8')
    print('Private recovery inputs:',contract['private_bytes'],'bytes; NOT an NSP-only source kit')


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    for name in ['reference','romfs','recipe','luts','output']:p.add_argument('--'+name,type=Path,required=True)
    a=p.parse_args();prepare(a.reference,a.romfs,a.recipe,a.luts,a.output)
