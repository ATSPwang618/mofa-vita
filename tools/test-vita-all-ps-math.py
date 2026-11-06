#!/usr/bin/env python3
"""Exercise every compiled-choice complete-source PS operation with host F32.

Both generic and compile-time entries are extracted from authored Cg. Separate
RGBA8 anchors cover all 24 operations, alpha exceptions, and operation-specific
boundaries. Controlled texture/haze/soft inputs are not GPU execution evidence.
"""
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "knife/vita/shaders/effects.cg"
spec = importlib.util.spec_from_file_location("full_ps_math", ROOT / "tools/test-vita-full-ps-math.py")
full = importlib.util.module_from_spec(spec)
spec.loader.exec_module(full)

SHIM = full.SHIM + r'''
float3 abs(float3 v){return {std::fabs(v.r),std::fabs(v.g),std::fabs(v.b)};}
float3 pow(float3 a,float3 b){return {std::pow(a.r,b.r),std::pow(a.g,b.g),std::pow(a.b,b.b)};}
float3 select_lt(float3 x,float3 edge,float3 low,float3 high){return {
    x.r<edge.r?low.r:high.r,x.g<edge.g?low.g:high.g,x.b<edge.b?low.b:high.b};}
'''

MAIN = r'''
struct Material {
    bool fill=false,noalpha=false,group=false,rule=false,premultiply=false;
    float soft=1.f,mask=.5f,rule_sample=.5f,progress=.5f,flags=2040.f;
};
static uint64_t cases=0,gold_checks=0;
static void prepare(unsigned op,float4 texel,const Material &m){
    sampled=texel;secondary={m.rule_sample,0,0,m.mask};soft_value=m.soft;
    tu.params={float(op),m.fill?1.f:0.f,960,540};
    tu.params2={m.rule?m.progress:-1.f,m.flags,m.noalpha?1.f:0.f,m.group?1.f:0.f};
}
using Entry=U4(*)(FragmentInput,U4);
static const Entry entries[]={FULL_ENTRIES};
static bool check(unsigned op,float4 texel,float4 tint,U4 d,const Material &m){
    prepare(op,texel,m);
    FragmentInput input{{.25f,.75f},{{100,200}},tint,{0,0,0,m.premultiply?1.f:0.f},texel};
    reset_trace();const U4 expected=native_pack(generic(input,native_unpack(d)));
    if(!trace_matches(m.group,m.rule)){std::fprintf(stderr,"generic source trace lost\n");return false;}
    reset_trace();const U4 actual=entries[op-1](input,d);
    if(!trace_matches(m.group,m.rule)){std::fprintf(stderr,"compiled-choice source trace lost op=%u\n",op);return false;}
    if(actual!=expected){
        std::fprintf(stderr,"mismatch case=%llu op=%u fill=%u noalpha=%u group=%u rule=%u premultiply=%u\n",
            (unsigned long long)cases,op,m.fill,m.noalpha,m.group,m.rule,m.premultiply);return false;
    }
    ++cases;return true;
}
static bool gold(unsigned op,float4 texel,float4 tint,U4 d,const Material &m,U4 wanted){
    if(!check(op,texel,tint,d,m))return false;
    prepare(op,texel,m);
    FragmentInput input{{.25f,.75f},{{100,200}},tint,{0,0,0,m.premultiply?1.f:0.f},texel};
    const U4 actual=entries[op-1](input,d);++gold_checks;
    if(actual!=wanted){
        std::fprintf(stderr,"gold anchor=%llu op=%u actual=%u,%u,%u,%u wanted=%u,%u,%u,%u\n",
            (unsigned long long)gold_checks,op,actual.bytes[0],actual.bytes[1],actual.bytes[2],actual.bytes[3],
            wanted.bytes[0],wanted.bytes[1],wanted.bytes[2],wanted.bytes[3]);return false;
    }
    return true;
}
int main(){
    // Hand-calculated: s=(1/4,1/2,3/4), ca=1/2, sampled alpha=1/2, a=1/4.
    const U4 black[]={
        {16,32,48,255},{0,0,0,255},{16,32,48,255},{0,0,0,255},
        {0,0,0,255},{0,0,0,255},{0,0,0,255},{16,32,48,255},
        {16,32,48,255},{16,32,48,255},{0,0,32,255},{0,64,64,255},
        {0,0,0,255},{0,0,0,255},{0,0,0,255},{32,64,96,255},
        {0,0,0,255},{64,128,191,255},{16,32,48,255},{0,0,0,255},
        {32,64,96,255},{16,32,48,255},{0,0,0,255},{64,128,191,255}};
    const U4 white[]={
        {255,255,255,255},{207,223,239,255},{255,255,255,255},{207,223,239,255},
        {255,255,255,255},{255,255,255,255},{255,255,255,255},{239,223,207,255},
        {239,223,207,255},{239,223,207,255},{223,255,255,255},{255,255,255,255},
        {255,255,255,255},{255,255,255,255},{207,223,239,255},{255,255,255,255},
        {159,191,223,255},{64,128,191,255},{44,60,76,255},{159,191,223,255},
        {255,255,255,255},{255,255,255,255},{255,255,255,255},{64,128,191,255}};
    for(unsigned op=1;op<=24;++op){
        if(!gold(op,{.25f,.5f,.75f,.5f},{1,1,1,.5f},{0,0,0,37},{},black[op-1]))return 1;
        if(!gold(op,{.25f,.5f,.75f,.5f},{1,1,1,.5f},{255,255,255,37},{},white[op-1]))return 1;
    }
    // The alpha guard belongs to this exact subset, even when ca stays positive.
    for(unsigned op:{1u,2u,3u,4u,5u,6u,7u,8u,9u,10u,11u,12u,13u,14u,15u,19u,20u,23u,24u})
        for(float a:{-1.f,-0.f,0.f})
            if(!gold(op,{.25f,.5f,.75f,a},{1,1,1,.5f},{64,128,192,37},{},{64,128,192,37}))return 1;
    // 16/17/18/21/22 intentionally still render when sampled alpha is zero.
    if(!gold(16,{.25f,.5f,.75f,0},{1,1,1,.5f},{64,128,192,37},{},{96,192,255,255}))return 1;
    if(!gold(17,{.25f,.5f,.75f,0},{1,1,1,.5f},{64,128,192,37},{},{0,64,160,255}))return 1;
    if(!gold(18,{.25f,.5f,.75f,0},{1,1,1,.5f},{64,128,192,37},{},{64,128,191,255}))return 1;
    if(!gold(21,{.25f,.5f,.75f,0},{1,1,1,.5f},{0,0,0,37},{},{32,64,96,255}))return 1;
    if(!gold(22,{.25f,.5f,.75f,0},{1,1,1,.5f},{64,128,192,37},{},{76,144,204,255}))return 1;
    for(float ca:{-1.f,-0.f,0.f})
        if(!gold(21,{.25f,.5f,.75f,1},{1,1,1,ca},{64,128,192,37},{},{64,128,192,37}))return 1;
    // Op9 subtracts the weighted source before abs; it differs from op8's interpolation.
    if(!gold(9,{.5f,.5f,.5f,1},{1,1,1,.5f},{64,128,192,37},{},{0,64,128,255}))return 1;
    // Dodge zero denominator, Burn zero source, and HardLight's source-alpha normalization.
    if(!gold(6,{1,1,1,.5f},{1,1,1,.5f},{0,127,255,1},{},{64,159,255,255}))return 1;
    if(!gold(5,{0,0,0,1},{1,1,1,1},{64,128,192,37},{},{0,0,0,255}))return 1;
    if(!gold(12,{.25f,.5f,.75f,.5f},{1,1,1,.5f},{0,0,0,37},{},{0,64,64,255}))return 1;
    if(!gold(14,{.25f,.5f,.75f,1},{1,1,1,1},{64,128,192,37},{},{32,128,211,255}))return 1;
    Material m;m.group=true;
    if(!gold(1,{1,1,1,1.f/256},{1,1,1,1},{0,0,0,37},m,{1,1,1,255}))return 1;
    m.noalpha=true;m.soft=.25f;
    if(!gold(1,{.25f,.5f,.75f,0},{1,1,1,1},{0,0,0,37},m,{32,64,96,255}))return 1;
    m={};m.rule=true;m.soft=.5f;
    if(!gold(1,{.25f,.5f,.75f,1},{1,1,1,.5f},{0,0,0,37},m,{8,16,24,255}))return 1;
    const float values[]={-2,-.01f,-0.f,0,1.f/65536,1.f/255,.25f,
        std::nextafter(.5f,0.f),.5f,std::nextafter(.5f,1.f),.75f,
        std::nextafter(1.f,0.f),1,std::nextafter(1.f,2.f),2,4};
    const uint8_t bytes[]={0,1,63,64,127,128,129,254,255};
    for(unsigned op=1;op<=24;++op)for(unsigned bits=0;bits<32;++bits)
        for(float s:values)for(float ta:values)for(uint8_t d:bytes){
            m={};m.fill=bits&1;m.noalpha=bits&2;m.group=bits&4;m.rule=bits&8;m.premultiply=bits&16;
            m.soft=.375f;m.mask=.501f;m.rule_sample=.625f;
            if(!check(op,{s,s*.5f,1.f-s,ta},{.5f,1.f,.25f,.625f},
                      {d,(uint8_t)(255-d),(uint8_t)(d/2),37},m))return 1;
        }
    uint32_t seed=0x248764;
    auto random=[&](){seed^=seed<<13;seed^=seed>>17;seed^=seed<<5;return seed;};
    auto value=[&](){return (int(random()%80001)-30000)/10000.f;};
    for(unsigned n=0;n<200000;++n){
        float4 t(value(),value(),value(),value()),c(value(),value(),value(),value());
        U4 d((uint8_t)random(),(uint8_t)random(),(uint8_t)random(),(uint8_t)random());
        unsigned bits=random();m={};m.fill=bits&1;m.noalpha=bits&2;m.group=bits&4;m.rule=bits&8;m.premultiply=bits&16;
        m.soft=value();m.mask=value();m.rule_sample=value();m.progress=(random()%10001)/10000.f;
        m.flags=float(8*(1+random()%255)+(random()%8));
        if(!check(1+random()%24,t,c,d,m))return 1;
    }
    std::printf("PASS: %llu authored-source F32 cases; %llu independent RGBA8 anchors; all 24 compile-time operations\n",
        (unsigned long long)cases,(unsigned long long)gold_checks);
}
'''.replace("FULL_ENTRIES", ",".join(f"full{i}" for i in range(1, 25)))


def transform(value):
    value = value.replace("unsigned char4", "U4").replace("tex2D(tex2, q).r", "tex2D(tex2, q).rgb.r")
    value = re.sub(r"\((\w+) < (float3\(0\.5, 0\.5, 0\.5\))\) \? low : high", r"select_lt(\1, \2, low, high)", value)
    return re.sub(r"\b([0-9]+\.[0-9]+)(?![\w.])", r"\1f", value)


def source_cpp(text):
    marker = "unsigned char4 fs_ps_operation_fetch("
    if marker not in text:
        raise AssertionError("missing compile-time complete-source entry fs_ps_operation_fetch")
    entry = full.plain.body(text, marker)
    assert "tu.params.x" not in entry, "compiled-choice entry must not dispatch from a runtime op uniform"
    assert not re.search(r"\bhalf[234]?\b", entry), "compiled-choice entry must retain F32"
    generated = SHIM
    for signature, query in [
        ("float unorm8_round(float value)", "float unorm8_round("),
        ("float3 group_rgb(float3 pixel)", "float3 group_rgb("),
        ("float group_alpha(float pixel,float mask)", "float group_alpha("),
        ("float rule_at(float2 uv,float flags)", "float rule_at("),
        ("float rule_mask(float r,float p,float flags)", "float rule_mask("),
        ("float4 sample_img(FragmentInput i)", "float4 sample_img("),
        ("PsSample ps_source(FragmentInput i)", "PsSample ps_source("),
        ("float3 ps_mix(float3 d,float3 r,float a)", "float3 ps_mix("),
        ("float3 ps_hard_light(float3 d,float3 s,float a)", "float3 ps_hard_light("),
        ("float3 ps_overlay(float3 d,float3 s,float a)", "float3 ps_overlay("),
        ("U4 native_pack(float4 value)", "unsigned char4 native_pack("),
        ("float4 native_unpack(U4 value)", "float4 native_unpack("),
    ]:
        code = full.plain.body(text, query)
        if "ps_source(" in query:
            code = "++source_calls;\n" + code
        generated += signature + "{\n" + transform(code) + "\n}\n"
    generated += "float4 generic(FragmentInput i,float4 d4){\n" + transform(full.plain.body(text, "float4 ps_apply(")) + "\n}\n"
    for op in range(1, 25):
        generated += f"#define KNIFE_PS_FULL_OP {op}\nU4 full{op}(FragmentInput i,U4 destination){{(void)destination;\n"
        generated += transform(entry) + "\n}\n#undef KNIFE_PS_FULL_OP\n"
    return generated + MAIN


def main():
    source = SOURCE.read_text()
    cpp = source_cpp(source)
    output = ROOT / ".work/vita/all-ps-math"
    output.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=output))
    compiler = os.environ.get("CXX", "c++")
    flags = ["-std=c++17", "-O2", "-Wall", "-Wextra", "-Werror", "-fno-fast-math", "-ffp-contract=off"]
    mutants = {
        "green": source,
        "red-alpha-guard-on-op16": source.replace("#elif KNIFE_PS_FULL_OP == 16\n", "#elif KNIFE_PS_FULL_OP == 16\n    if (min(source.alpha, 0.0) == source.alpha) { return destination; }\n"),
        "red-op9-interpolates-abs": source.replace("abs(d - a * s)", "ps_mix(d, abs(d - s), a)"),
        "red-op12-loses-normalization": source.replace("h = min(s / t.a, one);", "h = s; (void)t; (void)one;"),
        "red-op14-branches-on-destination": source.replace("float3 r = (s < float3", "float3 r = (d < float3"),
        "red-op19-loses-destination-alpha": source.replace("ps_mix(d * d4.a, s, a)", "ps_mix(d, s, a)"),
        "red-op20-uses-sampled-alpha": source.replace("ps_mix(d, min(d, s), ca)", "ps_mix(d, min(d, s), a)"),
        "red-op21-loses-opacity-earlyout": source.replace("if (ca <= 0.0) { return d4; }", "if (false) { return d4; }").replace("if (ca <= 0.0) { return destination; }", "if (false) { return destination; }"),
        "red-op22-loses-inner-opacity": source.replace("one - (one - d) * (one - s * ca)", "one - (one - d) * (one - s)"),
        "red-op23-uses-opacity": source.replace("min(d * s + (1.0 - a) * d, one)", "min(d * s + (1.0 - source.opacity) * d, one)"),
        "red-noalpha-forgotten": source.replace("if (tu.params2.z > 0.5) { t.a = 1.0; }", "if (false) { t.a = 1.0; }"),
        "red-group-quantization-removed": source.replace("return unorm8_round(unorm8_round(pixel) * unorm8_round(mask));", "return pixel * mask;"),
    }
    results = {}
    for name, shader in mutants.items():
        # Operation mutations affect the candidate only. Shared-source mutants
        # still use independent byte anchors to catch common mistakes.
        if name.startswith("red-op") or name=="red-alpha-guard-on-op16":
            begin=source.index("// BEGIN GENERATED PS OPERATIONS")
            end=source.index("// END GENERATED PS OPERATIONS",begin)
            changed_begin=shader.index("// BEGIN GENERATED PS OPERATIONS")
            changed_end=shader.index("// END GENERATED PS OPERATIONS",changed_begin)
            shader=source[:begin]+shader[changed_begin:changed_end]+source[end:]
        code = source_cpp(shader)
        if name != "green" and code == cpp:
            raise AssertionError(f"mutation did not alter extracted shader: {name}")
        path = run / f"{name}.cpp"
        path.write_text(code)
        command = [compiler, *flags, *(["-Wno-unused-variable"] if name!="green" else []), str(path), "-o", str(run / name)]
        subprocess.run(command, check=True)
        result = subprocess.run([str(run / name)], text=True, capture_output=True)
        log = result.stdout + result.stderr
        (run / f"{name}.log").write_text(log)
        results[name] = {"exit_code": result.returncode, "output": log, "command": command,
                         "source_sha256": hashlib.sha256(code.encode()).hexdigest()}
        if (name == "green") != (result.returncode == 0):
            raise SystemExit(f"Unexpected {name} result: {log}")
        print(name, log.strip())
    receipt = {"schema": "knife-vita-all-ps-host-math-v1", "time_utc": datetime.now(timezone.utc).isoformat(),
        "shader_source_sha256": hashlib.sha256(SOURCE.read_bytes()).hexdigest(),
        "compiler": subprocess.check_output([compiler, "--version"], text=True).splitlines()[0],
        "results": results, "run_directory": str(run),
        "limitations": ["Host F32 vector shim; no Cg/GXP compiler or GPU execution.",
            "Controlled texture values and haze/soft helper outputs; filtering, interpolation and haze/soft helper math are not exercised.",
            "The same authored ps_source executes once for every generic and compiled-choice entry.",
            "CPU results do not prove GPU pixel equivalence or speed."]}
    (run / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print("RECEIPT", run / "receipt.json")


if __name__ == "__main__":
    main()
