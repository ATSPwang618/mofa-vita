#!/usr/bin/env python3
'''Host differential of generated material bodies against previous authored paths.
Half storage is rounded through _Float16. UV sampling values are controlled;
this does not establish Cg execution/GPU pixel equivalence or timing.
'''
import importlib.util, json, subprocess, hashlib
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
def load(name,file):
 s=importlib.util.spec_from_file_location(name,ROOT/'tools'/file);m=importlib.util.module_from_spec(s);s.loader.exec_module(m);return m
allps=load('allps','test-vita-all-ps-math.py')
variants=load('variants','generate-material-variants.py')
text=(ROOT/'knife/vita/shaders/effects.cg').read_text()
base=allps.source_cpp(text).removesuffix(allps.MAIN)
base+=r'''
struct half3 {float3 v;half3(float3 x):v(float(_Float16(x.r)),float(_Float16(x.g)),float(_Float16(x.b))){} operator float3()const{return v;}};
'''
body=allps.full.plain.body
trans=allps.transform
for old in (48,49,50,52):
 name={48:'fs_ps7_plain_fetch',49:'fs_ps1_plain_half_fetch',50:'fs_ps6_plain_half_fetch',52:'fs_ps13_plain_half_fetch'}[old]
 base+=f'U4 old{old}(FragmentInput i,U4 destination){{\n'+trans(body(text,'unsigned char4 '+name+'('))+'\n}\n'
for shader,v in variants.VARIANTS.items():
 if shader>=96:
  base+=f'#define KNIFE_MATERIAL_VARIANT {shader}\nPsSample source{shader}(FragmentInput i){{\n'+trans(body(text,'PsSample ps_material_source('))+f'\n}}\n#undef KNIFE_MATERIAL_VARIANT\n'
 code=body(text,f'unsigned char4 fs_material_{shader}(').replace('ps_material_source(i)',f'source{shader}(i)')
 base+=f'U4 candidate{shader}(FragmentInput i,U4 destination){{\n'+trans(code)+'\n}\n'
main=r'''
static uint32_t rng=0x14785432;
static uint32_t random32(){rng^=rng<<13;rng^=rng>>17;rng^=rng<<5;return rng;}
static float value(){const float edge[]={0,1,1.f/255,0.499999f,0.5f,0.500001f,1.f/65536};
 return random32()%3? float(random32()%65536)/65535.f : edge[random32()%7];}
int main(){
 unsigned cases=0;
 for(unsigned id=88;id<108;++id) for(unsigned n=0;n<20000;++n){
  unsigned ops[]={1,1,6,6,7,7,13,13,1,6,7,13,3,4,5,11,14,6,8,14};unsigned op=ops[id-88];
  sampled={value(),value(),value(),value()};soft_value=1;secondary={0,0,0,0};
  FragmentInput i{{0,0},{{0,0}},{},{},{}};i.uv=hazed_uv;i.color={value(),value(),value(),value()};
  tu.params={float(op),id<96?float(id%2==0):float(random32()%2),960,540};
  tu.params2={-1,0,id<96?float(id%2!=0):float(random32()%2),0};
  if(id>=96&&id<=99){i.hx={.2f,-.7f,.9f,float(random32()%2)};}
  if(id>=105){tu.params.y=0;tu.params2.z=0;tu.params2.w=1;secondary={0,0,0,(n%3==0)?254.f/255.f:value()};}
  U4 d{(uint8_t)random32(),(uint8_t)random32(),(uint8_t)random32(),(uint8_t)random32()};
  U4 expected{0,0,0,0},actual{0,0,0,0};
  switch(id){REF_CASES default:std::abort();}
  switch(id){CAND_CASES default:std::abort();}
  if(!(actual==expected)){std::fprintf(stderr,"DIFF shader=%u case=%u\n",id,n);return 1;}
  ++cases;
 }
 std::printf("PASS %u material differential cases; half baseline preserved; controlled samples\n",cases);
}
'''
refs=[]
for shader,v in variants.VARIANTS.items():
 f='old'+str(v['base']) if shader<96 else 'full'+str(v['op'])
 refs.append(f'case {shader}:expected={f}(i,d);break;')
main=main.replace('REF_CASES',''.join(refs)).replace('CAND_CASES',''.join(f'case {i}:actual=candidate{i}(i,d);break;' for i in variants.VARIANTS))
out=ROOT/'.work/vita/material-variants';out.mkdir(parents=True,exist_ok=True)
results={}
for name,code in [('green',base+main),('red-fill-alpha',(base+main).replace('U4 candidate88(FragmentInput i,U4 destination){','U4 candidate88(FragmentInput i,U4 destination){ sampled.a=1.0f;')),
 ('red-group-mask',(base+main).replace('U4 candidate105(FragmentInput i,U4 destination){','U4 candidate105(FragmentInput i,U4 destination){ secondary.a=1.0f;')),
 ('red-haze-premultiply',(base+main).replace('if (i.hx.w > 0.5f) { t.rgb = t.rgb * t.a; }','if (false) { t.rgb = t.rgb * t.a; }'))]:
 src=out/(name+'.cpp');src.write_text(code)
 subprocess.run(['c++','-std=c++17','-O2','-Wall','-Wextra','-Werror','-Wno-unused-function','-fno-fast-math','-ffp-contract=off',str(src),'-o',str(out/name)],check=True)
 p=subprocess.run([str(out/name)],text=True,capture_output=True);results[name]={'exit_code':p.returncode,'output':p.stdout+p.stderr}
 print(name,p.stdout+p.stderr)
 if (name=='green')!=(p.returncode==0):raise SystemExit('Unexpected differential outcome '+name)
native=out/'native'
subprocess.run(['cc','-std=c11','-Wall','-Wextra','-Werror','-O2','-ffunction-sections','-fdata-sections','-fsanitize=address,undefined','-Wl,--gc-sections','-idirafter','/opt/vitasdk/arm-vita-eabi/include',str(ROOT/'knife/vita/tests/material_variants_test.c'),'-lm','-o',str(native)],check=True)
p=subprocess.run([str(native)],text=True,capture_output=True);print(p.stdout+p.stderr);p.check_returncode()
results['native']={'exit_code':p.returncode,'output':p.stdout+p.stderr}
(out/'receipt.json').write_text(json.dumps({'source_sha256':hashlib.sha256(text.encode()).hexdigest(),'results':results,'gpu_verified':False},indent=2)+'\n')
