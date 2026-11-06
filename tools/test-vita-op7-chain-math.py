#!/usr/bin/env python3
"""Host arithmetic oracle for actual ordered OP7/7 Cg bodies; no GPU claim."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("plain_math", Path(__file__).with_name("test-vita-plain-ps-math.py"))
plain = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plain)
source = plain.SOURCE.read_text()
cpp = plain.source_cpp(source)
assert cpp.endswith(plain.MAIN)
cpp = cpp[:-len(plain.MAIN)]
cpp += "\n#define KNIFE_CHAIN_OP_A 7\n#define KNIFE_CHAIN_OP_B 7\n"
for name in ("chain_plain_a", "chain_plain_b"):
    cpp += "U4 " + name + "(float4 t,float4 tint,U4 destination){\n"
    extracted = plain.body(source, "unsigned char4 " + name + "(").replace("unsigned char4", "U4")
    for channel in "rgb": extracted = extracted.replace("d."+channel+" >", "d.rgb."+channel+" >")
    cpp += extracted + "\n}\n"
cpp += r'''
int main(){
 uint32_t seed=0x7a071234;uint64_t cases=0,intermediate_changes=0;
 auto random=[&](){seed^=seed<<13;seed^=seed>>17;seed^=seed<<5;return seed;};
 auto value=[&](){return (int(random()%40001)-10000)/10000.f;};
 const float edges[]={-1.f,0.f,1.e-8f,1.f/255.f,.5f,.99999994f,1.f,1.00001f,2.f};
 for(unsigned i=0;i<400000;++i){
  float4 a(value(),value(),value(),i<729?edges[i%9]:value());
  float4 b(value(),value(),value(),i<729?edges[(i/9)%9]:value());
  float4 ca(value(),value(),value(),i<729?edges[(i/81)%9]:value());
  float4 cb(value(),value(),value(),value());
  U4 d((uint8_t)random(),(uint8_t)random(),(uint8_t)random(),(uint8_t)random());
  tu.params={7,0,960,540};tu.params2={-1,0,0,0};
  FragmentInput input{{.25f,.75f},{{100,200}},ca,{0,0,0,0},a};sampled=a;
  U4 middle=reference(input,d);
  input.color=cb;sampled=b;U4 expected=reference(input,middle);
  U4 actual=chain_plain_b(b,cb,chain_plain_a(a,ca,d));
  if(actual!=expected){std::fprintf(stderr,"OP7 pair mismatch case=%u\n",i);return 1;}
  intermediate_changes+=middle!=d;++cases;
 }
 std::printf("PASS: %llu ordered OP7 pair cases, %llu intermediate changes; actual source bodies\n",
  (unsigned long long)cases,(unsigned long long)intermediate_changes);
}
'''
run = Path(tempfile.mkdtemp(prefix="r9-op7-chain-",dir=ROOT/".work/vita"))
results = {}
for name, code in {"green":cpp,"red-drop-first":cpp.replace("chain_plain_b(b,cb,chain_plain_a(a,ca,d))","chain_plain_b(b,cb,d)"),
                   "red-reverse-order":cpp.replace("chain_plain_b(b,cb,chain_plain_a(a,ca,d))","chain_plain_a(a,ca,chain_plain_b(b,cb,d))")}.items():
    path=run/(name+".cpp");path.write_text(code)
    command=[os.environ.get("CXX","c++"),"-std=c++17","-O2","-Wall","-Wextra","-Werror","-fno-fast-math","-ffp-contract=off",str(path),"-o",str(run/name)]
    subprocess.run(command,check=True)
    result=subprocess.run([str(run/name)],text=True,capture_output=True)
    results[name]={"exit_code":result.returncode,"output":result.stdout+result.stderr}
    (run/(name+".log")).write_text(result.stdout+result.stderr)
    print(name,(result.stdout+result.stderr).strip())
    if (result.returncode==0)!=(name=="green"):raise SystemExit("Unexpected "+name)
receipt={"schema":"knife-op7-chain-host-math-v1","source_sha256":hashlib.sha256(plain.SOURCE.read_bytes()).hexdigest(),
 "results":results,"limitations":["Host F32 operations only; no GPU interpolation, sampler, Cg precision or hardware speed claim.",
 "Original generic source is evaluated twice with its RGBA8 store/load boundary preserved."]}
(run/"receipt.json").write_text(json.dumps(receipt,indent=2)+"\n")
print("RECEIPT",run/"receipt.json")
