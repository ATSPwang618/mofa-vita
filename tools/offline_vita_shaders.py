#!/usr/bin/env python3
"""Compile authored runtime Cg on the PC and embed validated GXP, never compiler modules."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'knife/vita/shaders/effects.cg'
HEADER = ROOT / 'knife/vita/shaders/generated/offline_programs.h'
ENTRIES = {11:'fs_alter',15:'fs_group_composite',17:'fs_ps_fetch',18:'fs_ps19_fetch',
    19:'fs_ps19_group_fetch',20:'fs_ps11_fetch',21:'fs_ps13_fetch',22:'fs_sprite_plain',
    23:'fs_ps19_plain_fetch',25:'fs_ps2_fetch',26:'fs_forcealpha_plain',28:'fs_rim_text_batched',
    29:'fs_ps7_fetch',30:'fs_ps1_plain_fetch',31:'fs_ps6_plain_fetch',48:'fs_ps7_plain_fetch',
    49:'fs_ps1_plain_half_fetch',50:'fs_ps6_plain_half_fetch',51:'fs_ps7_plain_half_fetch',
    52:'fs_ps13_plain_half_fetch',53:'fs_blur_weights',54:'fs_ps1_full_fetch',
    55:'fs_ps6_full_fetch',56:'fs_ps13_full_fetch',57:'fs_ps_chain2_fetch',
    **{i:'fs_ps_chain2_fetch' for i in range(32,48)},
    **{i:'fs_ps_operation_fetch' for i in range(64,88)},
    **{i:f'fs_material_{i}' for i in range(88,105)}}

def digest(data): return hashlib.sha256(data).hexdigest()
def macros(shader):
    fetch=17<=shader<=21 or shader in (23,25,29,30,31,57) or 32<=shader<=52 or 54<=shader<=56 or 64<=shader<=104
    out=['KNIFE_RUNTIME_NATIVE_FETCH=1','KNIFE_RUNTIME_PS_SPECIALIZED=1'][:(2 if shader>=19 else 1) if fetch else 0]
    if 88<=shader<=104:
        out+=['KNIFE_MATERIAL_VARIANT='+str(shader)]
        if shader in (88,89,90,91,94,95):out+=['KNIFE_RUNTIME_HALF_COLOR=1']
    elif 64<=shader<=87:out+=['KNIFE_PS_FULL_OP='+str(shader-63)]
    elif 49<=shader<=52:out+=['KNIFE_RUNTIME_HALF_COLOR=1']
    elif shader==53:out=['KNIFE_RUNTIME_BLUR_WEIGHTS=1']
    elif shader==57:out+=['KNIFE_CHAIN_OP_A=7','KNIFE_CHAIN_OP_B=7']
    elif 32<=shader<48:
        ops=[2,11,13,19];out+=['KNIFE_CHAIN_OP_A='+str(ops[(shader-32)//4]),'KNIFE_CHAIN_OP_B='+str(ops[(shader-32)%4])]
    return out

def reflect(data,shader):
    def u(at):return struct.unpack_from('<I',data,at)[0]
    def relative(at,length):
        offset=at+struct.unpack_from('<i',data,at)[0]
        if offset<0 or length<0 or offset+length>size:raise ValueError('GXP relative range')
        return offset
    if len(data)<156 or len(data)>1048576 or data[:4]!=b'GXP\0':raise ValueError('GXP magic/size')
    size=u(8)
    if size<156 or size>len(data) or len(data)-size>3 or any(data[size:]):raise ValueError('GXP payload/padding')
    flags=u(20)
    if not flags&1:raise ValueError('not a fragment program')
    native=bool(macros(shader) and macros(shader)[0]=='KNIFE_RUNTIME_NATIVE_FETCH=1')
    varying=relative(44,32)
    if data[varying+10:varying+12]!=bytes([7 if native else 1,4]):raise ValueError('fragment output ABI')
    if native and shader!=81 and flags&0x80==0:raise ValueError('missing native output')
    count=u(36)
    if count>128 or not u(60):raise ValueError('parameter/instruction count')
    table=relative(40,count*16);relative(64,u(60)*8)
    params=[];samplers=0
    for i in range(count):
        at=table+i*16;nameat=relative(at,1);end=data.find(b'\0',nameat,size)
        if end<0 or end-nameat>96:raise ValueError('parameter name')
        name=data[nameat:end].decode('ascii');category=data[at+4]&15;typ=data[at+4]>>4
        components=data[at+5]&15;container=data[at+5]>>4;arr=u(at+8);index=u(at+12)
        if category>2:raise ValueError('parameter category')
        if category!=0 and typ!=0:raise ValueError('non-F32 uniform/sampler')
        if category==2:
            if index>=4 or name!=['tex0','tex1','tex2','soft_lut'][index] or samplers&(1<<index):raise ValueError('sampler mapping')
            samplers|=1<<index
        if category==1:
            expected={'uTex0Size':2,'uTex1Size':2,'uSoftLutSize':2,'uQuadHx':4,'uQuadHy':4,'tu.params':4,'tu.params2':4}
            weight=shader==53 and name=='uBlurWeights'
            if container!=14 or components!=(1 if weight else expected.get(name)) or arr!=(33 if weight else 1) or index+components*arr>u(100):raise ValueError('uniform layout')
        elif arr!=1:raise ValueError('unexpected array')
        params.append(dict(name=name,category=category,type=typ,components=components,array=arr,index=index))
    if not samplers&1 or native and samplers&2:raise ValueError('sampler contract')
    return dict(flags=flags,output_type=data[varying+10],sampler_bits=samplers,primary_instructions=u(60),parameters=params)

def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--check',action='store_true',help='Check embedded byte identities without requiring a compiler')
    p.add_argument('--compiler',default=os.environ.get('KNIFE_VITA_CGC','D:/unity/PSVITA/sdk/host_tools/bin/psp2cgc.exe'))
    p.add_argument('--backend',choices=['psp2cgc','vita3k-shacc'],default='psp2cgc')
    p.add_argument('--config',help='Vita3K-Shacc setup configuration, when using that backend')
    p.add_argument('--ids',help='comma separated program IDs, for a bounded probe; no header is emitted')
    args=p.parse_args()
    if args.check:
        text=HEADER.read_text(encoding='utf-8')
        if digest(SOURCE.read_bytes()) not in text:raise SystemExit('Stale offline shader source identity; regenerate GXP')
        ids=set()
        for match in re.finditer(r'knife_offline_(\d+)\[\]=\{(.*?)\};',text,re.S):
            shader=int(match[1]);data=bytes(int(b,16) for b in re.findall(r'0x([0-9a-f]{2})',match[2]))
            reflect(data,shader)
            if f'/* shader {shader} SHA256 {digest(data)} */' not in text:raise SystemExit('Offline shader bytes changed')
            ids.add(shader)
        if ids!=set(ENTRIES):raise SystemExit('Incomplete offline shader inventory')
        print('PASS embedded offline GXP inventory:',len(ids),'programs; authored source identity matches')
        return
    compiler=Path(args.compiler).resolve(strict=True)
    if args.backend=='vita3k-shacc' and not args.config:p.error('--config required for Vita3K-Shacc')
    out=ROOT/'.work/vita/offline-vita-shaders';out.mkdir(parents=True,exist_ok=True)
    source=SOURCE.read_bytes();rows=[]
    ids=sorted(map(int,args.ids.split(','))) if args.ids else sorted(ENTRIES)
    flags=['-O3','-bestprecision','-nofastmath','-nofastint','-W3','-Werror','-nocolor','-lang','en'] if args.backend=='psp2cgc' else ['-O3','-W3']
    for shader in ids:
        entry=ENTRIES[shader];defines=macros(shader)
        # Vita3K-Shacc accepts self-contained source, not include files or CLI -D.
        flattened=out/f'effects-{shader}.cg'
        flattened.write_bytes((''.join('#define '+v.replace('=',' ',1)+'\n' for v in defines)).encode()+source)
        gxp=out/f'effects-{shader}.gxp'
        command=[str(compiler)]
        if args.backend=='vita3k-shacc':command+=['--config',str(Path(args.config).resolve())]
        command+=['-profile','sce_fp_psp2','-entry',entry,*flags,'-o',str(gxp),str(flattened)]
        result=subprocess.run(command,capture_output=True,timeout=60)
        (out/f'effects-{shader}.log').write_bytes(result.stdout+result.stderr)
        if result.returncode:raise SystemExit(f'Compiler failed shader {shader}: '+(result.stdout+result.stderr).decode(errors='replace'))
        data=gxp.read_bytes();metadata=reflect(data,shader)
        row=dict(shader=shader,entry=entry,macros=defines,bytes=len(data),sha256=digest(data),metadata=metadata,command=command)
        rows.append(row);print(shader,len(data),metadata['primary_instructions'],metadata['sampler_bits'],hex(metadata['flags']),flush=True)
    receipt=dict(schema='knife-vita-pc-offline-gxp-v1',backend=args.backend,compiler_sha256=digest(compiler.read_bytes()),
        compiler_path=str(compiler),source_sha256=digest(source),programs=rows,gpu_pixel_equivalence=False,hardware_performance_verified=False)
    (out/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n',encoding='utf-8')
    if args.ids:return
    h=['/* Generated authored GXP only; no compiler modules. */\n#ifndef KNIFE_OFFLINE_PROGRAMS_H\n#define KNIFE_OFFLINE_PROGRAMS_H\n',
        '#define KNIFE_OFFLINE_SOURCE_SHA256 "'+digest(source)+'"\n',
        '#define KNIFE_OFFLINE_COMPILER_SHA256 "'+receipt['compiler_sha256']+'"\n']
    for row in rows:
        shader=row['shader'];data=(out/f'effects-{shader}.gxp').read_bytes()
        h.append(f'/* shader {shader} SHA256 {digest(data)} */\n_Alignas(4) static const unsigned char knife_offline_{shader}[]={{\n')
        h.extend('    '+','.join(f'0x{b:02x}' for b in data[i:i+16])+',\n' for i in range(0,len(data),16))
        h.append('};\n')
    h.append('static const unsigned char *knife_offline_program(unsigned id,unsigned *bytes){switch(id){\n')
    h.extend(f'case {r["shader"]}:*bytes=sizeof(knife_offline_{r["shader"]});return knife_offline_{r["shader"]};\n' for r in rows)
    h.append('default:*bytes=0;return NULL;}}\n#endif\n')
    HEADER.write_text(''.join(h),encoding='utf-8',newline='\n')
    print('EMBEDDED',len(rows),'programs',sum(r['bytes'] for r in rows),'bytes',HEADER)

if __name__=='__main__':main()
