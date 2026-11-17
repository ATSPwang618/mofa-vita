"""Finish the original-only project: mixed-resolution boot art, font sheet and audit."""
import argparse
import collections
import csv
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import tempfile

from PIL import Image

spec=importlib.util.spec_from_file_location('exporter',Path(__file__).with_name('export-original-switch-png.py'))
e=importlib.util.module_from_spec(spec);spec.loader.exec_module(e)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--romfs',required=True,type=Path);p.add_argument('--out',required=True,type=Path)
    p.add_argument('--decoder-check',required=True,type=Path)
    p.add_argument('--kdu',required=True,type=Path,help='Production host kdu for exact BC3 interpolation')
    a=p.parse_args();root=a.romfs.resolve();out=a.out.resolve()
    manifest=json.loads((out/'manifest.json').read_text(encoding='utf8'))
    assert manifest['complete']
    manifest['complete']=False
    (out/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n',encoding='utf8')
    for record in manifest['images']:
        if record.get('format')=='0x1c06':
            with tempfile.TemporaryDirectory(prefix='mofa-bc3-') as directory:
                base=Path(directory)/'reference.png'
                proc=subprocess.run([str(a.kdu.resolve()),'--romfs',str(root),'extract',record['archive'],f'#{record["index"]}',str(base),'--indexed'],capture_output=True,encoding='utf8',errors='replace')
                assert proc.returncode==0,proc.stderr
                with Image.open(base.with_name(f'reference_{record["texture"]}.png')) as image:
                    result=e.resize(image.convert('RGBA').crop(tuple(record['crop'])),tuple(record['size']))
                path=out/record['path'];result.save(path,format='PNG',compress_level=3)
                record.update(bytes=path.stat().st_size,sha256=e.file_sha(path),pixel_sha256=e.sha(result.tobytes()),decoder='production-kdu-bc3')
        if record.get('format')!='0x1d01':continue
        archive=record['archive'];rows=list(struct.iter_unpack('<IHH',(root/(archive+'.hed')).read_bytes()));row=rows[record['index']]
        with (root/(archive+'.mrg')).open('rb') as f:
            f.seek(row[0]*2048);raw=e.unpack(f.read(row[1]*2048))
        image=list(e.textures(raw))[record['texture']][2]
        result=e.resize(image.crop(tuple(record['crop'])),tuple(record['size']))
        path=out/record['path'];result.save(path,format='PNG',compress_level=3)
        record.update(bytes=path.stat().st_size,sha256=e.file_sha(path),pixel_sha256=e.sha(result.tobytes()),decoder='bcdec_rs-compatible-bc4')
    # Thumbnail rows reuse story names. The archive marker, not the name of
    # each image, identifies this range; preserve its original pixel density.
    for archive in ('jallpac','allpacml'):
        names=(root/(archive+'.nam')).read_bytes()
        names=[names[i:i+32].split(b'\0')[0].split(b'\r')[0].decode('utf8') for i in range(0,len(names),32)]
        first=next(i for i,n in enumerate(names) if n.startswith('THUMSP_TOP.'))
        archive_rows=list(struct.iter_unpack('<IHH',(root/(archive+'.hed')).read_bytes()))
        groups=collections.defaultdict(list)
        for record in manifest['images']:
            if record.get('archive')==archive and record['index']>=first and record['scale']!=1:
                groups[record['index']].append(record)
        with (root/(archive+'.mrg')).open('rb') as f:
            for done,(index,records) in enumerate(groups.items()):
                row=archive_rows[index];f.seek(row[0]*2048);raw=e.unpack(f.read(row[1]*2048))
                images={k:image for k,label,image,fmt in e.textures(raw)}
                for record in records:
                    result=images[record['texture']].crop(tuple(record['crop']))
                    path=out/record['path'];result.save(path,format='PNG',compress_level=3)
                    record.update(size=list(result.size),scale=1,rule='native-thumbnail-index-range',bytes=path.stat().st_size,sha256=e.file_sha(path),pixel_sha256=e.sha(result.tobytes()))
                if done%1000==0:print(f'{archive}: preserved {done}/{len(groups)} native thumbnails',flush=True)
    rows=list(struct.iter_unpack('<IHH',(root/'parts.hed').read_bytes()))
    for record in manifest['images']:
        if record.get('archive') != 'parts':continue
        row=rows[record['index']]
        with (root/'parts.mrg').open('rb') as f:
            f.seek(row[0]*2048);raw=e.unpack(f.read(row[1]*2048))
        image=list(e.textures(raw))[record['texture']][2]
        assert image.size in [(1280,720),(1920,1080)]
        result=e.resize(image,(960,540));path=out/record['path'];result.save(path,format='PNG',compress_level=3)
        record.update(size=[960,540],scale=960/image.width,rule='boot-fullscreen-original-to-540p',bytes=path.stat().st_size,sha256=e.file_sha(path),pixel_sha256=e.sha(result.tobytes()))
    font=root/'exfont_nx01.bffnt';data=font.read_bytes()
    finf=struct.unpack_from('<H',data,6)[0]+8
    tglp=struct.unpack_from('<I',data,finf+12)[0]
    sheet_at=struct.unpack_from('<I',data,tglp+20)[0]
    for k,label,image,fmt in e.textures(data[sheet_at:]):
        # Native sheet is retained; glyph coordinates belong to its original metadata.
        rel=Path('standalone')/f'exfont_nx01_sheet_{k}.png';path=out/rel
        image.save(path,format='PNG',compress_level=3)
        manifest['images']=[r for r in manifest['images'] if r['path']!=rel.as_posix()]
        manifest['images'].append({'path':rel.as_posix(),'source_file':font.name,'source_sha256':e.file_sha(font),'texture':k,'texture_name':label,'format':fmt,'scale':1,'rule':'native-font-sheet-bottom-up','size':list(image.size),'bytes':path.stat().st_size,'sha256':e.file_sha(path),'pixel_sha256':e.sha(image.tobytes())})
    (out/'metadata'/font.name).write_bytes(data)
    seen=set();counts=collections.Counter();total=0
    for i,record in enumerate(manifest['images']):
        rel=record['path'];assert rel not in seen;seen.add(rel)
        path=out/rel;assert path.resolve().is_relative_to(out)
        assert e.file_sha(path)==record['sha256'],rel
        with Image.open(path) as image:
            assert image.format=='PNG' and image.mode=='RGBA' and list(image.size)==record['size'],rel
            assert e.sha(image.tobytes())==record['pixel_sha256'],rel
        counts[Path(rel).parts[0]]+=1;total+=path.stat().st_size
        if i%1000==0:print(f'Verified {i}/{len(manifest["images"])} PNG',flush=True)
    assert seen=={p.relative_to(out).as_posix() for p in out.rglob('*.png')}
    for record in manifest['non_images']:
        assert e.file_sha(out/record['path'])==record['sha256']
    original_checks={}
    for archive,entries in manifest['archives'].items():
        for name,identity in entries.items():
            assert e.file_sha(root/name)==identity['sha256'],name
        original_checks[archive]=True
    report={'status':'PASS','png_count':len(seen),'png_bytes':total,'counts':dict(counts),'every_png_rgba_roundtrip_verified':True,'every_source_archive_hash_verified':original_checks,'reference_decoder':json.loads(a.decoder_check.read_text(encoding='utf8')),'auxiliary_members_retained':len(manifest['non_images']),'runtime_assets_modified':False,'old_vita_pack_used':False,'old_editable_project_used':False,'runtime_compatibility_tested':False,'finalizer_sha256':e.file_sha(Path(__file__))}
    report['bc3_production_replacements']=sum(x.get('decoder')=='production-kdu-bc3' for x in manifest['images'])
    report['bc3_kdu_sha256']=e.file_sha(a.kdu)
    report['reference_decoder']['scope']='Comparisons made before finalization. BC3 Pillow differences are resolved by replacing all BC3 exports with production kdu pixels before resizing.'
    manifest.update(complete=True,png_count=len(seen),png_bytes=total,finalizer_sha256=report['finalizer_sha256'],finalizer_exporter_sha256=e.file_sha(Path(e.__file__)))
    (out/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n',encoding='utf8')
    (out/'verification.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf8')
    with (out/'图片索引.csv').open('w',encoding='utf-8-sig',newline='') as f:
        writer=csv.writer(f);writer.writerow(['文件','原归档','条目','原名称','纹理名称','原宽','原高','输出宽','输出高','比例','语言','规则'])
        for record in manifest['images']:
            original=record.get('source_texture_size',record['size'])
            writer.writerow([record['path'],record.get('archive',record.get('source_file','')),record.get('index',''),record.get('name',''),record.get('texture_name',''),*original,*record['size'],record['scale'],record.get('language',''),record['rule']])
    print(json.dumps({k:v for k,v in report.items() if k!='reference_decoder'},ensure_ascii=False,indent=2),flush=True)


if __name__=='__main__':main()
