"""Selected real-source checks against current main's production contracts."""
from hashlib import sha256
import json
from pathlib import Path
import struct
from .production_driver import activate,signature
from .switch import write_json,sha_file


def verify(config,source_stamp,output):
    activate()
    import replay_images as q
    output.mkdir(parents=True,exist_ok=True);q.init(config);results=[]
    for base,index in [('jallpac',0),('jallpac',649),('jallpac',802),('jallpac',1115),('jallpachd',2029),('allpacml',5)]:
        q.original_job((base,index));record=q.recipe['archives'][base]['originals'][index]
        data=q.cached('original-cache',record['stored_sha256']);assert data is not None
        (output/f'{base}-{index}.bin').write_bytes(data)
        results.append(dict(kind='original',archive=base,index=index,stored_sha256=record['stored_sha256'],exact=True))
        print('Sample original',base,index,'exact',flush=True)
    import resize_story_variants as resize
    import convert_story_pvrtc as pvrtc
    selected=q.load('pvrtc-selection.json.gz');pvrtc_checked=False
    for base,index in [('jallpac',39186),('jallpac',98155),('jallpachd',12540)]:
        key=next(k for k in q.recipe['archives'][base]['variants'] if k['index']==index)
        q.variant_job((base,key))
        ids=list(range(index+1,index+2+key['tile_count'])) if key['tiled'] else [index];blocks={}
        for i in ids:
            digest=q.banks[base]['rows'][i][3]
            blocks[i]=q.cached('variant-cache',digest) or q.cached('original-cache',digest);assert blocks[i] is not None
        if key['tiled']:
            desc=[]
            for i in ids:
                raw=q.b.texture.unpack(blocks[i])
                desc.append((i,*struct.unpack_from('<II',raw,16),struct.unpack_from('<I',raw,84)[0],struct.unpack_from('<I',raw,48)[0]))
            w,h=key['physical']
            blocks[index]=q.wrap(struct.pack('<4s7I',b'KQTL',1,*key['logical'],w,h,(w+503)//504,(h+503)//504)+
                                 b''.join(struct.pack('<5I',*d) for d in desc),q.banks[base]['storage'].get(str(index),{}))
        class SelectedReader:
            rows=q.banks[base]['rows']
            def read(self,i):return blocks[i]
        resize.reader=SelectedReader();q.shim.mode(0);result=resize.convert((index,.7,False));before=tuple(key['physical'])
        if config.get('image_backend')=='cuda':
            from . import resize_acceleration as acceleration
            from PIL import Image
            acceleration.install()
            try:
                candidate=resize.convert((index,.7,False))
                assert candidate==result,'CUDA 70% output differs from original tools'
                result=candidate
            finally:
                Image.Image.resize=acceleration.ORIGINAL_RESIZE
                acceleration.ORIGINAL_RESIZE=None
        assert result['before']==before
        assert result['after']==(before if result['keep'] else tuple(max(1,round(n*.7)) for n in before))
        if key['tiled']:assert result['blocks'][0][0]==blocks[ids[0]]
        results.append(dict(kind='variant+70pct',archive=base,index=index,tiled=key['tiled'],before=before,
                            after=result['after'],fallback_preserved=True,exact=True))
        print('Sample variant and 70% resize',base,index,'exact',flush=True)
        if not pvrtc_checked:
            by_input={row['input_sha256']:(int(i),row) for i,row in selected.get(base,{}).items()}
            for block in result['blocks']:
                match=by_input.get(sha256(block[0]).hexdigest())
                if match is None:continue
                ordinal,record=match
                class PvrtcReader:
                    rows={ordinal:(0,0,block[1])}
                    def read(self,i):
                        assert i==ordinal
                        return block[0]
                pvrtc.reader=PvrtcReader();pvrtc.work=output/'pvrtc-cache';pvrtc.work.mkdir(exist_ok=True)
                pvrtc.pvrt=config['encoder'];pvrtc.gxttool=config['packer'];pvrtc.proofs=set()
                pvrtc.codec=resize.gxt;pvrtc.Image=resize.Image;pvrtc.np=resize.np
                converted,quality=pvrtc.encode(ordinal)
                assert quality['reason']=='accepted'
                assert sha256(converted[0]).hexdigest()==record['stored_sha256']
                results.append(dict(kind='pvrtc2-4bpp',archive=base,index=ordinal,
                                    stored_sha256=record['stored_sha256'],exact=True,quality=quality))
                pvrtc_checked=True;print('Sample PVRTC2',base,ordinal,'exact',flush=True);break
    assert pvrtc_checked,'Selected cases did not exercise the locked PVRTC encoder'
    for index in (0,418,624,10959,11037):
        rec=q.previews[index];stem=rec['name'][:-4];source=stem[3:]
        if index==11037:base,idx='jallpac',2030
        elif 'private_sha256' in rec or rec['logical']==[4,4]:base=idx=None
        elif source in q.b.thumbs:base,idx='jallpac',q.b.thumbs[source]
        else:
            base=next(b for b in ('allpacml','jallpac','allpachdml','jallpachd') if source in q.b.originals.names[b])
            idx=max(i for i,c in enumerate(q.recipe['archives'][base]['originals']) if c['name'].rsplit('.',1)[0]==source)
        if base is not None:q.original_job((base,idx))
        q.preview_job(index);digest=q.banks['savethumb']['rows'][index][3]
        assert q.cached('preview-cache',digest) is not None
        results.append(dict(kind='savethumb',index=index,stored_sha256=digest,exact=True))
        print('Sample preview',index,'exact',flush=True)
    import replay_misc
    from rebuild_bgm_lead import make_lead
    contracts=q.load('audio-contract.json.gz')['streams'];banks=q.load('source-contract.json.gz')['banks']
    for bank,names in banks.items():
        name='M10.ogg' if bank=='bgm.mrg' else next(n for n in names if n.endswith('.ogg'));target=output/name
        replay_misc.audio_job((str(Path(config['work'])/'patched/stream'/Path(name).with_suffix('.opus')),
                               str(target),config['ffmpeg'],contracts['stream/'+name]))
        results.append(dict(kind='audio',bank=bank,name=name,sha256=sha_file(target),exact=True))
        if name=='M10.ogg':
            lead=make_lead(target.read_bytes());expected=q.load('source-contract.json.gz')['files']['stream/M10.bgmstart.mrg']
            assert sha256(lead).hexdigest()==expected['sha256']
            results.append(dict(kind='bgm-start',name='M10.bgmstart.mrg',exact=True))
    for reader in q.b.originals.readers.values():reader.volume.__exit__(None,None,None)
    if q.legacy.proc is not None:q.legacy.proc.stdin.close();q.legacy.proc.wait(timeout=20);q.legacy.proc=None
    report=dict(schema='mofa-current-main-samples-v2',passed=True,cases=results,conversion_signature=signature(),
        romfs_source_sha256=sha_file(source_stamp),
        upstream_revision=json.loads((Path(__file__).with_name('production')/'upstream.json').read_bytes())['revision'],
        historical_encoder_corrections=True,hardware_tested=False)
    write_json(output/'verification.json',report);print('Current main samples passed:',len(results),flush=True)
    return report
