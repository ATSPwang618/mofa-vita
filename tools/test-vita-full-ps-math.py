#!/usr/bin/env python3
"""Run extracted complete-source PS1/6/13 Cg bodies through a host F32 shim.

The candidate and generic reference execute the authored sample_img, ps_source,
group quantization, rule, blend and native RGBA8 pack bodies. Texture values and
haze/soft helper results are controlled inputs; forwarding and call counts are
checked. Independent byte anchors and broken-source controls protect against
shared-code mistakes. Host results are not Cg/GXP or GPU rendering evidence.
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
spec = importlib.util.spec_from_file_location("plain_ps_math", ROOT / "tools/test-vita-plain-ps-math.py")
plain = importlib.util.module_from_spec(spec)
spec.loader.exec_module(plain)

SHIM = plain.SHIM.split("const int tex0=0,tex2=2;", 1)[0].replace(
    "struct float2 {float x,y;};",
    "struct float2 {float x,y;float2(float a,float b):x(a),y(b){}};",
)
SHIM += r'''
float3 operator+(float3 a,float b){return {a.r+b,a.g+b,a.b+b};}
float floor(float v){return std::floor(v);}
float3 floor(float3 v){return {floor(v.r),floor(v.g),floor(v.b)};}
float clamp(float a,float lo,float hi){return std::clamp(a,lo,hi);}
float3 clamp(float3 a,float3 lo,float3 hi){return {
    clamp(a.r,lo.r,hi.r),clamp(a.g,lo.g,hi.g),clamp(a.b,lo.b,hi.b)};}
const int tex0=0,tex2=2;
static float4 sampled(0,0,0,0), secondary(0,0,0,0);
static float soft_value=1.f;
static unsigned source_calls=0,haze_calls=0,soft_calls=0,image_samples=0,secondary_samples=0;
static float2 hazed_uv(.375f,.625f);
float4 tex2D(int sampler,float2 uv){
    if(sampler==tex0){
        if(uv.x!=hazed_uv.x||uv.y!=hazed_uv.y)std::abort();
        ++image_samples;return sampled;
    }
    if(sampler==tex2){++secondary_samples;return secondary;}
    std::abort();
}
float2 haze_uv(FragmentInput){++haze_calls;return hazed_uv;}
float soft_mask(FragmentInput){++soft_calls;return soft_value;}
static void reset_trace(){source_calls=haze_calls=soft_calls=image_samples=secondary_samples=0;}
static bool trace_matches(bool group,bool rule){
    return source_calls==1&&haze_calls==1&&soft_calls==1&&image_samples==1&&
           secondary_samples==unsigned(group)+unsigned(rule);
}
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
static U4 candidate(unsigned op,FragmentInput i,U4 d){
    return op==1?full1(i,d):op==6?full6(i,d):full13(i,d);
}
static bool check(unsigned op,float4 texel,float4 tint,U4 d,const Material &m){
    prepare(op,texel,m);
    FragmentInput input{{.25f,.75f},{{100,200}},tint,{0,0,0,m.premultiply?1.f:0.f},texel};
    reset_trace();const U4 expected=reference(input,d);
    if(!trace_matches(m.group,m.rule)){std::fprintf(stderr,"generic source trace lost\n");return false;}
    reset_trace();const U4 actual=candidate(op,input,d);
    if(!trace_matches(m.group,m.rule)){std::fprintf(stderr,"full source trace lost op=%u\n",op);return false;}
    if(actual!=expected){
        std::fprintf(stderr,"mismatch case=%llu op=%u fill=%u noalpha=%u group=%u rule=%u premultiply=%u\n",
            (unsigned long long)cases,op,m.fill,m.noalpha,m.group,m.rule,m.premultiply);return false;
    }
    // For the plain fixture alpha is independent of the shared source helper.
    if(!m.group&&!m.rule){
        const float alpha=(m.noalpha?1.f:texel.a*m.soft)*tint.a;
        if(alpha<=0&&actual!=d){std::fprintf(stderr,"nonpositive alpha changed destination RGBA\n");return false;}
        if(alpha>0&&actual.bytes[3]!=255){std::fprintf(stderr,"positive PS alpha must store opaque\n");return false;}
    }
    ++cases;return true;
}
static bool gold(unsigned op,float4 texel,float4 tint,U4 d,const Material &m,U4 wanted){
    if(!check(op,texel,tint,d,m))return false;
    prepare(op,texel,m);
    FragmentInput input{{.25f,.75f},{{100,200}},tint,{0,0,0,m.premultiply?1.f:0.f},texel};
    const U4 actual=candidate(op,input,d);++gold_checks;
    if(actual!=wanted){
        std::fprintf(stderr,"gold anchor=%llu op=%u actual=%u,%u,%u,%u wanted=%u,%u,%u,%u\n",
            (unsigned long long)gold_checks,op,actual.bytes[0],actual.bytes[1],actual.bytes[2],actual.bytes[3],
            wanted.bytes[0],wanted.bytes[1],wanted.bytes[2],wanted.bytes[3]);return false;
    }
    return true;
}
int main(){
    // Integer-byte destinations and dyadic values make these independently calculable.
    if(!gold(1,{.25f,.5f,.75f,.5f},{.5f,.25f,1,.5f},{64,128,192,37},{},{72,136,208,255}))return 1;
    Material m;m.fill=true;
    if(!gold(1,{.125f,.875f,.25f,.5f},{.5f,.25f,1,.5f},{0,0,0,37},m,{32,16,64,255}))return 1;
    m={};m.noalpha=true;
    if(!gold(1,{.25f,.5f,.75f,0},{.5f,.25f,1,.5f},{0,0,0,37},m,{16,16,96,255}))return 1;
    if(!gold(6,{1,1,1,.5f},{1,1,1,.5f},{0,127,255,1},{},{64,159,255,255}))return 1;
    // Native byte destinations straddle Overlay's 0.5 threshold; exact 0.5 is exercised below.
    if(!gold(13,{.75f,.25f,.25f,1},{1,1,1,1},{64,128,255,19},{},{96,65,255,255}))return 1;
    m={};m.soft=.5f;
    if(!gold(1,{.25f,.5f,.75f,1},{1,1,1,1},{0,0,0,37},m,{32,64,96,255}))return 1;
    m.premultiply=true;
    if(!gold(1,{.25f,.5f,.75f,1},{1,1,1,1},{0,0,0,37},m,{16,32,48,255}))return 1;
    m={};m.group=true;
    if(!gold(1,{.25f,.5f,.75f,.5f},{1,1,1,1},{0,0,0,37},m,{16,32,48,255}))return 1;
    // Quantize each factor before multiplying and quantize that product again:
    // round(1/256*255)=1, round(.5*255)=128, round(1*128/255)=1.
    if(!gold(1,{1,1,1,1.f/256},{1,1,1,1},{0,0,0,37},m,{1,1,1,255}))return 1;
    m.noalpha=true;m.soft=.25f;
    // NoAlpha runs after soft sampling and before the group's RGBA8 mask-alpha product.
    if(!gold(1,{.25f,.5f,.75f,0},{1,1,1,1},{0,0,0,37},m,{32,64,96,255}))return 1;
    m={};m.group=true;m.premultiply=true;
    if(!gold(1,{.25f,.5f,.75f,.5f},{1,1,1,1},{0,0,0,37},m,{8,16,24,255}))return 1;
    m={};m.rule=true;m.soft=.5f;
    if(!gold(1,{.25f,.5f,.75f,1},{1,1,1,.5f},{0,0,0,37},m,{8,16,24,255}))return 1;
    // Every dedicated operation must preserve destination alpha on alpha <= 0.
    for(unsigned op:{1u,6u,13u})for(float a:{-1.f,-0.f,0.f}){
        if(!gold(op,{.75f,.5f,.25f,a},{1,1,1,1},{17,99,201,7},{},{17,99,201,7}))return 1;
    }
    // Call the extracted overlay helper at the otherwise unrepresentable native 0.5 boundary.
    float3 overlay=ps_overlay({.5f,std::nextafter(.5f,0.f),std::nextafter(.5f,1.f)},{.25f,.75f,.25f},1.f);
    if(overlay.r!=.25f||overlay.g>=.75f||overlay.b<=.25f){std::fprintf(stderr,"overlay threshold anchor\n");return 1;}
    const float values[]={-2,-.01f,-0.f,0,1.f/65536,1.f/255,.25f,
        std::nextafter(.5f,0.f),.5f,std::nextafter(.5f,1.f),.75f,
        std::nextafter(1.f,0.f),1,std::nextafter(1.f,2.f),2,4};
    const uint8_t bytes[]={0,1,63,64,127,128,129,254,255};
    for(unsigned op:{1u,6u,13u})for(unsigned bits=0;bits<32;++bits)
        for(float s:values)for(float ta:values)for(uint8_t d:bytes){
            m={};m.fill=bits&1;m.noalpha=bits&2;m.group=bits&4;m.rule=bits&8;m.premultiply=bits&16;
            m.soft=.375f;m.mask=.501f;m.rule_sample=.625f;
            if(!check(op,{s,s*.5f,1.f-s,ta},{.5f,1.f,.25f,.625f},
                      {d,(uint8_t)(255-d),(uint8_t)(d/2),37},m))return 1;
        }
    uint32_t seed=0x13654;
    auto random=[&](){seed^=seed<<13;seed^=seed>>17;seed^=seed<<5;return seed;};
    auto value=[&](){return (int(random()%80001)-30000)/10000.f;};
    for(unsigned n=0;n<200000;++n){
        float4 t(value(),value(),value(),value()),c(value(),value(),value(),value());
        U4 d((uint8_t)random(),(uint8_t)random(),(uint8_t)random(),(uint8_t)random());
        unsigned bits=random();m={};m.fill=bits&1;m.noalpha=bits&2;m.group=bits&4;m.rule=bits&8;m.premultiply=bits&16;
        m.soft=value();m.mask=value();m.rule_sample=value();m.progress=(random()%10001)/10000.f;
        m.flags=float(8*(1+random()%255)+(random()%8));
        if(!check(std::array<unsigned,3>{1,6,13}[random()%3],t,c,d,m))return 1;
    }
    std::printf("PASS: %llu authored-source F32 cases; %llu independent RGBA8 anchors; source forwarding checked\n",
        (unsigned long long)cases,(unsigned long long)gold_checks);
}
'''


def source_cpp(text):
    def transform(value):
        value = value.replace("unsigned char4", "U4").replace("tex2D(tex2, q).r", "tex2D(tex2, q).rgb.r")
        # Cg unsuffixed floating constants are F32; C++ defaults to double.
        return re.sub(r"\b([0-9]+\.[0-9]+)(?![\w.])", r"\1f", value)

    generated = SHIM
    for signature, marker in [
        ("float unorm8_round(float value)", "float unorm8_round("),
        ("float3 group_rgb(float3 pixel)", "float3 group_rgb("),
        ("float group_alpha(float pixel,float mask)", "float group_alpha("),
        ("float rule_at(float2 uv,float flags)", "float rule_at("),
        ("float rule_mask(float r,float p,float flags)", "float rule_mask("),
        ("float4 sample_img(FragmentInput i)", "float4 sample_img("),
        ("PsSample ps_source(FragmentInput i)", "PsSample ps_source("),
        ("float3 ps_mix(float3 d,float3 r,float a)", "float3 ps_mix("),
        ("float3 ps_overlay(float3 d,float3 s,float a)", "float3 ps_overlay("),
        ("U4 native_pack(float4 value)", "unsigned char4 native_pack("),
        ("float4 native_unpack(U4 value)", "float4 native_unpack("),
    ]:
        code = plain.body(text, marker)
        if "ps_source(" in marker:
            code = "++source_calls;\n" + code
        generated += signature + "{\n" + transform(code) + "\n}\n"

    # A whole-source entry has to use the same preparation helper once, with the
    # selected equation in its own body; reaching ps_apply retains the dispatcher.
    for op in (1, 6, 13):
        marker = f"unsigned char4 fs_ps{op}_full_fetch("
        if marker not in text:
            raise AssertionError(f"missing complete-source entry fs_ps{op}_full_fetch")
        code = plain.body(text, marker)
        assert len(re.findall(r"\bps_source\(i\)", code)) == 1, f"op{op} must share ps_source exactly once"
        assert "ps_apply(" not in code, f"op{op} must remove the generic operation dispatcher"
        assert not re.search(r"\bhalf[234]?\b", code), f"op{op} must retain F32 arithmetic"
        generated += f"U4 full{op}(FragmentInput i,U4 destination){{\n{transform(code)}\n}}\n"

    prelude = plain.body(text, "float4 ps_apply(").split("// Return each operation directly", 1)[0]
    prelude = prelude.replace("return d4;", "return destination;")
    generated += "U4 reference(FragmentInput i,U4 destination){float4 d4=native_unpack(destination);\n"
    generated += transform(prelude) + "(void)t;(void)ca;(void)zero;\nfloat4 result=[&]()->float4{\n"
    for op in (1, 6, 13):
        generated += f"if(op=={op}.0f){{\n{transform(plain.body(text, f'if (op == {op}.0)'))}\n}}\n"
    return generated + "std::abort();}();return native_pack(result);}\n" + MAIN


def main():
    source = SOURCE.read_text()
    cpp = source_cpp(source)
    output = ROOT / ".work/vita/full-ps-math"
    output.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=output))
    compiler = os.environ.get("CXX", "c++")
    flags = ["-std=c++17", "-O2", "-Wall", "-Wextra", "-Werror", "-fno-fast-math", "-ffp-contract=off"]
    mutants = {
        "green": source,
        "red-soft-alpha-forgotten": source.replace("float a = t.a * soft_mask(i);", "float a = t.a; (void)soft_mask(i);"),
        "red-fill-discards-sampled-alpha": source.replace("result.alpha = t.a * ca;", "result.alpha = (tu.params.y > 0.5 ? 1.0 : t.a) * ca;"),
        "red-noalpha-forgotten": source.replace("if (tu.params2.z > 0.5) { t.a = 1.0; }", "if (false) { t.a = 1.0; }"),
        "red-noalpha-after-group": source.replace(
            "if (tu.params2.z > 0.5) { t.a = 1.0; }\n    if (tu.params2.w > 0.5) {\n"
            "        t.rgb = group_rgb(t.rgb);\n        t.a = group_alpha(t.a, tex2D(tex2, i.uv).a);\n    }",
            "if (tu.params2.w > 0.5) {\n        t.rgb = group_rgb(t.rgb);\n"
            "        t.a = group_alpha(t.a, tex2D(tex2, i.uv).a);\n    }\n"
            "    if (tu.params2.z > 0.5) { t.a = 1.0; }"),
        "red-group-mask-forgotten": source.replace("group_alpha(t.a, tex2D(tex2, i.uv).a)", "group_alpha(t.a, 1.0 + 0.0 * tex2D(tex2, i.uv).a)"),
        "red-group-quantization-removed": source.replace("return unorm8_round(unorm8_round(pixel) * unorm8_round(mask));", "return pixel * mask;"),
        "red-rule-opacity-forgotten": source.replace("ca *= rule_mask(", "ca *= 1.0 + 0.0 * rule_mask("),
        "red-opacity-multiplied-twice": source.replace("float a = source.alpha;", "float a = source.alpha * source.opacity;"),
        "red-alpha-earlyout-removed": source.replace("if (min(source.alpha, 0.0) == source.alpha) { return destination; }", "if (false) { return destination; }"),
        "red-dodge-zero-boundary": source.replace("k.r > 0.0 ? raw.r : 1.0", "k.r > 0.0 ? raw.r : 0.0"),
        "red-overlay-source-branch": source.replace("d.r < 0.5 ? low.r", "s.r < 0.5 ? low.r"),
    }
    results = {}
    for name, shader in mutants.items():
        code = source_cpp(shader)
        if name != "green" and code == cpp:
            raise AssertionError(f"mutation did not alter extracted shader: {name}")
        path = run / f"{name}.cpp"
        path.write_text(code)
        command = [compiler, *flags, str(path), "-o", str(run / name)]
        subprocess.run(command, check=True)
        result = subprocess.run([str(run / name)], text=True, capture_output=True)
        log = result.stdout + result.stderr
        (run / f"{name}.log").write_text(log)
        results[name] = {"exit_code": result.returncode, "output": log, "command": command,
                         "source_sha256": hashlib.sha256(code.encode()).hexdigest()}
        if (name == "green") != (result.returncode == 0):
            raise SystemExit(f"Unexpected {name} result: {log}")
        print(name, log.strip())
    receipt = {"schema": "knife-vita-full-ps-host-math-v1", "time_utc": datetime.now(timezone.utc).isoformat(),
        "shader_source_sha256": hashlib.sha256(SOURCE.read_bytes()).hexdigest(),
        "compiler": subprocess.check_output([compiler, "--version"], text=True).splitlines()[0],
        "results": results, "run_directory": str(run),
        "limitations": ["Host F32 vector shim; no Cg/GXP compiler or GPU execution.",
            "Controlled texture values and haze/soft helper outputs; filtering, interpolation and haze/soft helper math are not exercised.",
            "The actual shared sample_img, ps_source, group RGBA8 quantization and rule functions execute with forwarding checks.",
            "CPU results do not prove GPU pixel equivalence or speed."]}
    (run / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print("RECEIPT", run / "receipt.json")


if __name__ == "__main__":
    main()
