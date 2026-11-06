#!/usr/bin/env python3
"""Bounded material specialization generated from existing authored equations.

88..95 fix Fill/NoAlpha in the CURRENT half/FP32 simple entry, retaining its
precision and source-alpha semantics. 96..99 keep haze+premultiplication but
exclude soft/rule/group. 100..104 cover measured plain OP3/4/5/11/14 draws.
"""
import argparse, importlib.util
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
SOURCE=ROOT/'knife/vita/shaders/effects.cg'
spec=importlib.util.spec_from_file_location('ops',ROOT/'tools/generate-ps-operation-shaders.py')
ops=importlib.util.module_from_spec(spec);spec.loader.exec_module(ops)
VARIANTS={}
for index,(op,base,entry) in enumerate([(1,49,'fs_ps1_plain_half_fetch'),(6,50,'fs_ps6_plain_half_fetch'),(7,48,'fs_ps7_plain_fetch'),(13,52,'fs_ps13_plain_half_fetch')]):
    for shade in range(2):
        VARIANTS[88+2*index+shade]=dict(op=op,base=base,kind='fill' if shade==0 else 'noalpha',entry=entry,half=op!=7)
for index,op in enumerate([1,6,7,13]):
    VARIANTS[96+index]=dict(op=op,base=63+op,kind='haze',half=False)
for index,op in enumerate([3,4,5,11,14]):
    VARIANTS[100+index]=dict(op=op,base=63+op,kind='plain',half=False)
BEGIN='// BEGIN GENERATED MATERIAL VARIANTS\n'
END='// END GENERATED MATERIAL VARIANTS\n'
def generated(text):
    generic,_=ops.braced(text,text.index('float4 ps_apply('))
    bodies=ops.operation_bodies(generic)
    output=[BEGIN.rstrip(),'// Generated from existing source/operation bodies; do not hand edit.', '#ifdef KNIFE_MATERIAL_VARIANT']
    # Same source order as ps_source: sample/premultiply, force alpha, tint/fill,
    # opacity, alpha. Only absent features are removed, never reordered.
    output+=['PsSample ps_material_source(FragmentInput i) {',
      '#if KNIFE_MATERIAL_VARIANT >= 96 && KNIFE_MATERIAL_VARIANT <= 99',
      '    float4 t = tex2D(tex0, haze_uv(i));',
      '    if (i.hx.w > 0.5) { t.rgb = t.rgb * t.a; }',
      '#else','    float4 t = tex2D(tex0, i.uv);','#endif',
      '    if (tu.params2.z > 0.5) { t.a = 1.0; }',
      '    float3 s = t.rgb * i.color.rgb;',
      '    s = lerp(s, i.color.rgb, 1.0 - step(float3(tu.params.y, tu.params.y, tu.params.y), float3(0.5, 0.5, 0.5)));',
      '    PsSample result;', '    result.texel = t;', '    result.source = s;',
      '    result.opacity = i.color.a;', '    result.alpha = t.a * i.color.a;', '    return result;', '}']
    for shader,v in VARIANTS.items():
        output += [f'#if KNIFE_MATERIAL_VARIANT == {shader}',
          f'__nativecolor __regformat unsigned char4 fs_material_{shader}(FragmentInput i,',
          '    __regformat unsigned char4 destination : FRAGCOLOR) : COLOR {']
        if v['kind'] in ('fill','noalpha'):
            body,_=ops.braced(text,text.index('unsigned char4 '+v['entry']+'('))
            body=body.replace('tu.params.y','1.0' if v['kind']=='fill' else '0.0')
            body=body.replace('tu.params2.z','0.0' if v['kind']=='fill' else '1.0')
            output.append(body.strip('\n'))
        else:
            output.append('    PsSample source = ps_material_source(i);')
            output.append(ops.native_body(v['op'],bodies[v['op']]))
        output+=['}', '#endif']
    output+=['#endif',END.rstrip()]
    return '\n'.join(output)+'\n'
def main():
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--check',action='store_true');a=p.parse_args()
    text=SOURCE.read_text();block=generated(text)
    if BEGIN in text:
        start=text.index(BEGIN);end=text.index(END,start)+len(END);result=text[:start]+block+text[end:]
    else:
        anchor='// A bounded, material-selected chain,';start=text.index(anchor);result=text[:start]+block+'\n'+text[start:]
    if a.check:
        if result!=text:raise SystemExit('stale material variants; regenerate')
    else:SOURCE.write_text(result,newline='\n')
    print('material variants: 17 source entries verified; no GPU equivalence claim')
if __name__=='__main__':main()
