"""Repair source-only PNG scaling; audit, never silently relax Vita resource guards."""
import argparse
import collections
from concurrent.futures import ThreadPoolExecutor
import csv
import importlib.util
import json
import math
from pathlib import Path
import struct
from PIL import Image

spec=importlib.util.spec_from_file_location('exporter',Path(__file__).with_name('export-original-switch-png.py'))
e=importlib.util.module_from_spec(spec);spec.loader.exec_module(e)
ROOT=Path(__file__).resolve().parents[1]

def save_json(path,data):
    tmp=path.with_suffix(path.suffix+'.partial')
    tmp.write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n',encoding='utf8',newline='\n')
    tmp.replace(path)

def key(r):return (r['archive'],r['index'],r['member'],r['texture'])

def align(x,n):return (x+n-1)//n*n

def rgba_charge(w,h):
    n=align(w,8)*h*4
    return align(n,64 if n<262144 else 262144)

def legacy_budget_size(w,rows,count):
    """Mirror the current pack's 1 MiB owner policy, for comparison only."""
    high=min(1.,960/w,540/rows,2048/w,2048/(rows*count))
    def shape(k):return max(1,math.floor(w*k+1e-9)),max(1,math.floor(rows*k+1e-9))*count
    lo=0.
    for _ in range(60):
        mid=(lo+high)/2
        if rgba_charge(*shape(mid))<=1048576:lo=mid
        else:high=mid
    return list(shape(lo))

def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--project',type=Path,required=True);ap.add_argument('--romfs',type=Path,required=True)
    ap.add_argument('--jobs',type=int,default=6);ap.add_argument('--apply',action='store_true')
    args=ap.parse_args();out=args.project.resolve();romfs=args.romfs.resolve()
    assert 1<=args.jobs<=8 and out!=romfs
    path=out/'manifest.json';original=path.read_bytes();m=json.loads(original)
    assert m['complete'], 'unfinished project requires recovery from the recorded pre-repair manifest'
    groups=collections.defaultdict(list)
    for r in m['images']:
        if 'archive' in r:groups[key(r)].append(r)
    corrections=[];new_records=[];changed_groups=0;changed_files=0
    plans={}
    for k,records in groups.items():
        r=records[0]
        plan=e.image_plan(r['archive'],r['index'],r['name'],r['source_texture_size'],r['format'],e.thumbnail_start(romfs,r['archive']),e.strip_names())
        plans[k]=plan
        actual={(r['language'],tuple(r['crop']),tuple(r['size'])) for r in records}
        wanted={(p['language'],tuple(p['crop']),tuple(p['size'])) for p in plan}
        if actual!=wanted:
            corrections.append({'archive':k[0],'index':k[1],'texture':k[3],'name':r['name'],
                'before':[dict(path=v['path'],crop=v['crop'],size=v['size'],language=v['language']) for v in records],
                'after':plan})
    if not args.apply:
        print(json.dumps({'required_corrections':len(corrections),'sample':corrections[:3]},ensure_ascii=False,indent=2));return
    backup=out/'metadata/manifest-before-scale-audit.json'
    if not backup.exists():backup.write_bytes(original)
    m['complete']=False;save_json(path,m)
    cached_archive=None;cached_index=None;cached_images=None
    for k,records in groups.items():
        plan=plans[k];sample=records[0]
        actual={(r['language'],tuple(r['crop']),tuple(r['size'])) for r in records}
        wanted={(p['language'],tuple(p['crop']),tuple(p['size'])) for p in plan}
        if actual==wanted:
            for r in records:
                p=next(p for p in plan if p['language']==r['language'])
                r.update(p,logical_full=r['source_texture_size'])
                new_records.append(r)
            continue
        archive,index,member,texture=k
        if (archive,index)!=(cached_archive,cached_index):
            row=list(struct.iter_unpack('<IHH',(romfs/(archive+'.hed')).read_bytes()))[index]
            with (romfs/(archive+'.mrg')).open('rb') as f:
                f.seek(row[0]*2048);raw=f.read(row[1]*2048)
            assert e.sha(raw)==sample['source_entry_sha256'], 'original entry changed'
            cached_images={(suffix,t):(label,im,fmt) for suffix,data in e.leaves(raw) for t,label,im,fmt in e.textures(data)}
            cached_archive,cached_index=archive,index
        _,source,_=cached_images[(member,texture)]
        assert list(source.size)==sample['source_texture_size']
        for pos,p in enumerate(plan):
            old=next((r for r in records if r['language']==p['language']),None)
            if old is None and len(records)==1 and pos==0:old=records[0]
            r=dict(old or sample)
            if old is None:
                base=Path(sample['path']);r['path']=base.with_name(base.stem+'_'+p['language']+'.png').as_posix()
                assert not (out/r['path']).exists(),'unexpected pre-existing output'
            target=(out/r['path']).resolve();assert target.is_relative_to(out)
            if old is not None:assert e.file_sha(target)==old['sha256'], 'edited output would be overwritten'
            image=e.resize(source.crop(tuple(p['crop'])),tuple(p['size']))
            temp=target.with_suffix('.png.partial');image.save(temp,format='PNG',compress_level=3);temp.replace(target)
            r.update(p,logical_full=list(source.size),bytes=target.stat().st_size,sha256=e.file_sha(target),
                pixel_sha256=e.sha(image.tobytes()),scale_repair='production-table-and-lookup-v1')
            new_records.append(r);changed_files+=1
        changed_groups+=1
    new_records.extend(r for r in m['images'] if 'archive' not in r)
    m['images']=sorted(new_records,key=lambda r:r['path'])
    print(f'Repaired {changed_groups} source textures; wrote {changed_files} PNG; total {len(new_records)}',flush=True)
    seen=set()
    def check(r):
        file=(out/r['path']).resolve();assert file.is_relative_to(out)
        assert e.file_sha(file)==r['sha256'],r['path']
        with Image.open(file) as im:
            assert im.mode=='RGBA' and list(im.size)==r['size'],r['path']
            assert e.sha(im.tobytes())==r['pixel_sha256'],r['path']
            opaque=im.getextrema()[3]==(255,255)
        r['opaque']=opaque
        if 'archive' in r:
            p=next(p for p in plans[key(r)] if p['language']==r['language'])
            assert all(r[x]==p[x] for x in ['crop','size','scale','language_count','language_slot']),r['path']
            cw,ch=r['crop'][2]-r['crop'][0],r['crop'][3]-r['crop'][1]
            r['effective_scale']=[r['size'][0]/cw,r['size'][1]/ch]
            assert all(abs(v-r['scale']*s)<=.500001 or v==1 for v,s in zip(r['size'],[cw,ch])),r['path']
        return r
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for i,r in enumerate(pool.map(check,m['images'])):
            assert r['path'] not in seen;seen.add(r['path'])
            if i%2000==0:print(f'PNG pixels + ratios verified {i}/{len(new_records)}',flush=True)
    assert seen=={p.relative_to(out).as_posix() for p in out.rglob('*.png')}
    for r in m['non_images']:assert e.file_sha(out/r['path'])==r['sha256']
    source_checks={}
    for archive,files in m['archives'].items():
        for name,info in files.items():assert e.file_sha(romfs/name)==info['sha256'],name
        source_checks[archive]=True
    groups=collections.defaultdict(list)
    for r in m['images']:
        if 'archive' in r:groups[key(r)].append(r)
    deployment=[];reason_counts=collections.Counter()
    for k,rs in groups.items():
        rs.sort(key=lambda r:r['language_slot']);r=rs[0];w,rows=r['size'];n=r['language_count'];h=rows*n
        assert len(rs)==n and {s['language_slot'] for s in rs}==set(range(n))
        assert all(s['size']==r['size'] and s['logical_full']==r['logical_full'] for s in rs)
        opaque=all(s['opaque'] for s in rs);fmt='DXT1' if opaque else 'DXT5'
        control=r['rule'] in ('native-control-or-tiny','native-soft-clip-lookup')
        if r['format'] in ('0x201','0x1d01'):fmt='R8-lossless-control'
        if r['rule']=='native-soft-clip-lookup':fmt='RGBA8-lossless-lookup'
        pw=max(4,1<<(w-1).bit_length());ph=max(4,1<<(h-1).bit_length())
        dxt_payload=pw*ph//(2 if opaque else 1)
        issues=[]
        if max(w,h)>2048:issues.append('texture-axis-over-2048')
        if w*h>960*1080:issues.append('whole-owner-over-1036800-pixels')
        if w*h*4>4*960*1080:issues.append('png-whole-owner-over-decoder-budget')
        story=k[0] in ('jallpac','jallpachd','allpacml','allpachdml')
        if story and not control and rgba_charge(w,h)>1048576:issues.append('existing-story-owner-policy-over-1MiB')
        for issue in issues:reason_counts[issue]+=1
        deployment.append(dict(archive=k[0],index=k[1],member=k[2],texture=k[3],name=r['name'],texture_name=r['texture_name'],
            logical=r['logical_full'],physical=[w,h],language_count=n,pngs=[v['path'] for v in rs],
            recommended_encoding=fmt,rgba_whole_owner_charge=rgba_charge(w,h),
            rgba_selected_strip_charge=rgba_charge(w,rows),hypothetical_dxt_payload_bytes=dxt_payload,
            hypothetical_dxt_allocation_bytes=align(dxt_payload,262144),
            current_policy_size_comparison=legacy_budget_size(w,rows,n) if story and not control else [w,h],
            png_native_decoder_dimension_pass=(w<=960 and h<=1080),
            dxt_direct_route_eligible_material=(n==1 and not control),
            language_crop_requires_cpu_decode=(n>1),
            en_zc_physical=[w,rows*2] if n==4 else [w,h],
            en_zc_axis_pass=(w<=2048 and rows*(2 if n==4 else 1)<=2048),
            en_zc_paths=[v['path'] for v in rs if v['language_slot'] in (1,2)] if n==4 else [v['path'] for v in rs],
            blockers=issues,dimension_checks_pass=not issues,packaging_required=True,
            preserves_logical_coordinates=True))
    counts=collections.Counter(Path(r['path']).parts[0] for r in m['images'])
    audit={'schema':'original-switch-scale-audit-v2','status':'PASS','scope':'PNG master scaling, file closure and source hashes; not device certification',
        'canvas':[960,540],'png_count':len(seen),'png_bytes':sum(r['bytes'] for r in m['images']),
        'counts':dict(counts),'corrected_source_textures':changed_groups,'rewritten_or_added_pngs':changed_files,
        'corrections':corrections,'production_language_stems':len(e.strip_names()),
        'language_source_textures':sum(v['language_count']==4 for v in deployment),
        'source_archive_checks':source_checks,'image_ratio_violations':0,'language_contract_violations':0,
        'runtime_texture_groups':len(deployment),'runtime_dimension_policy_blocked_groups':sum(bool(v['blockers']) for v in deployment),
        'blocker_counts':dict(reason_counts),'runtime_ready':False,'device_tested':False,
        'runtime_assets_modified':False,'old_pack_used_as_pixel_source':False,
        'tool_sha256':e.file_sha(Path(__file__)),'exporter_sha256':e.file_sha(Path(e.__file__)),
        'production_table_sha256':e.file_sha(ROOT/'knife/knife/data/exe_tables_witch.bin')}
    audit['png_native_decoder_dimension_blocked_groups']=sum(not r['png_native_decoder_dimension_pass'] for r in deployment)
    audit['en_zc_axis_blocked_groups']=sum(not r['en_zc_axis_pass'] for r in deployment)
    audit['current_runtime_language_count']=2
    audit['source_language_count']=4
    history_path=out/'metadata/scaling-repair-applied.json'
    if corrections:
        save_json(history_path,dict(corrected_source_textures=changed_groups,rewritten_or_added_pngs=changed_files,corrections=corrections))
    if history_path.exists():
        history=json.loads(history_path.read_text(encoding='utf8'))
        audit.update(this_run_corrected_source_textures=changed_groups,this_run_rewritten_or_added_pngs=changed_files,**history)
    save_json(out/'metadata/scaling-audit.json',audit)
    save_json(out/'metadata/vita-deployment-plan.json',{'canvas':[960,540],'ready_for_device':False,
        'note':'Dimensions only; DXT encoding, logical KIMG/KATL reconstruction, language profile, allocations, pack identity and device validation remain necessary. No proposed downscale has been applied to the PNG masters.',
        'textures':deployment})
    with (out/'图片索引.csv').open('w',encoding='utf-8-sig',newline='') as f:
        cw=csv.writer(f);cw.writerow(['文件','原归档','条目','原名称','纹理名称','原整图宽','原整图高','裁切左','裁切上','裁切宽','裁切高','输出宽','输出高','目标比例','实际横比','实际纵比','语言','条带数','规则'])
        for r in m['images']:
            src=r.get('source_texture_size',r['size']);crop=r.get('crop',[0,0,*src]);sc=r.get('effective_scale',[1,1])
            cw.writerow([r['path'],r.get('archive',r.get('source_file','')),r.get('index',''),r.get('name',''),r.get('texture_name',''),*src,
                crop[0],crop[1],crop[2]-crop[0],crop[3]-crop[1],*r['size'],r['scale'],*sc,r.get('language',''),r.get('language_count',1),r['rule']])
    with (out/'Vita部署检查.csv').open('w',encoding='utf-8-sig',newline='') as f:
        cw=csv.writer(f);cw.writerow(['归档','索引','名称','纹理','逻辑宽','逻辑高','原版四语言重组宽','原版四语言重组高','语言数','双语言重组宽','双语言重组高','建议编码','RGBA整图分配字节','RGBA单语言分配字节','DXT估算分配字节','PNG原生尺寸检查','现行限额候选宽','现行限额候选高','部署阻碍','PNG文件'])
        for r in deployment:cw.writerow([r['archive'],r['index'],r['name'],r['texture_name'],*r['logical'],*r['physical'],r['language_count'],*r['en_zc_physical'],r['recommended_encoding'],
            r['rgba_whole_owner_charge'],r['rgba_selected_strip_charge'],r['hypothetical_dxt_allocation_bytes'],r['png_native_decoder_dimension_pass'],*r['current_policy_size_comparison'],';'.join(r['blockers']) or 'dimensions-only-pass;needs-packaging',';'.join(r['pngs'])])
    m.update(complete=True,png_count=audit['png_count'],png_bytes=audit['png_bytes'],
        scale_revision='production-table-and-lookup-v1',runtime_ready=False,scaling_audit='metadata/scaling-audit.json')
    verification=json.loads((out/'verification.json').read_text(encoding='utf8'))
    verification.update(status='PASS',png_count=audit['png_count'],png_bytes=audit['png_bytes'],counts=dict(counts),
        scale_and_language_contract_checked=True,scaling_audit='metadata/scaling-audit.json',runtime_compatibility_tested=False)
    save_json(out/'verification.json',verification);save_json(path,m)
    print(json.dumps({k:v for k,v in audit.items() if k!='corrections'},ensure_ascii=False,indent=2),flush=True)

if __name__=='__main__':main()
