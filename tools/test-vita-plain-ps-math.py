#!/usr/bin/env python3
"""Execute authored plain PS preparation/equations through a host F32 shim.

Uses actual Cg bodies for sample_img, ps_source, mix/overlay, native pack/unpack,
and entries 21/30/31; the reference selects the authored generic op branches.
Known integer-byte anchors and broken-source controls prevent a tautological
comparison. This does not compile Cg/GXP or execute texture sampling/the GPU.
"""
from pathlib import Path
from datetime import datetime, timezone
import hashlib
import json
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "knife/vita/shaders/effects.cg"


def body(text, marker):
    start = text.index("{", text.index(marker))
    depth, end = 1, start + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start + 1:end - 1]


SHIM = r'''
#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
struct float2 {float x,y;};
float2 operator/(float2 a,float2 b){return {a.x/b.x,a.y/b.y};}
struct float3 {
    float r,g,b;
    float3():r(0),g(0),b(0){}
    float3(float x,float y,float z):r(x),g(y),b(z){}
};
float3 operator+(float3 a,float3 b){return {a.r+b.r,a.g+b.g,a.b+b.b};}
float3 operator-(float3 a,float3 b){return {a.r-b.r,a.g-b.g,a.b-b.b};}
float3 operator-(float a,float3 b){return {a-b.r,a-b.g,a-b.b};}
float3 operator*(float3 a,float3 b){return {a.r*b.r,a.g*b.g,a.b*b.b};}
float3 operator*(float a,float3 b){return {a*b.r,a*b.g,a*b.b};}
float3 operator*(float3 a,float b){return b*a;}
float3 operator/(float3 a,float b){return {a.r/b,a.g/b,a.b/b};}
float3 operator/(float3 a,float3 b){return {a.r/b.r,a.g/b.g,a.b/b.b};}
float3 min(float3 a,float3 b){return {std::fmin(a.r,b.r),std::fmin(a.g,b.g),std::fmin(a.b,b.b)};}
float3 max(float3 a,float3 b){return {std::fmax(a.r,b.r),std::fmax(a.g,b.g),std::fmax(a.b,b.b)};}
float min(float a,float b){return std::fmin(a,b);}
float3 step(float3 edge,float3 x){return {x.r<edge.r?0.f:1.f,x.g<edge.g?0.f:1.f,x.b<edge.b?0.f:1.f};}
float3 lerp(float3 x,float3 y,float3 a){return x+a*(y-x);}
struct U4;
struct float4 {
    float3 rgb; float a;
    float x,y,z,w; float2 zw;
    float4():float4(0,0,0,0){}
    float4(float3 v,float alpha):rgb(v),a(alpha),x(v.r),y(v.g),z(v.b),w(alpha),zw{v.b,alpha}{}
    float4(float r,float g,float b,float alpha):float4(float3(r,g,b),alpha){}
    explicit float4(const U4 &bytes);
};
struct U4 {
    std::array<uint8_t,4> bytes;
    U4(uint8_t r,uint8_t g,uint8_t b,uint8_t a):bytes{r,g,b,a}{}
    explicit U4(float4 v):U4((uint8_t)v.rgb.r,(uint8_t)v.rgb.g,(uint8_t)v.rgb.b,(uint8_t)v.a){}
    bool operator==(U4 b)const{return bytes==b.bytes;}
    bool operator!=(U4 b)const{return !(*this==b);}
};
float4::float4(const U4 &v):float4(v.bytes[0],v.bytes[1],v.bytes[2],v.bytes[3]){}
float4 operator*(float4 a,float b){return {a.rgb*b,a.a*b};}
float4 operator/(float4 a,float b){return {a.rgb/b,a.a/b};}
float4 operator+(float4 a,float b){return {a.rgb+float3(b,b,b),a.a+b};}
float4 floor(float4 v){return {std::floor(v.rgb.r),std::floor(v.rgb.g),std::floor(v.rgb.b),std::floor(v.a)};}
float4 clamp(float4 a,float4 lo,float4 hi){return {
    std::clamp(a.rgb.r,lo.rgb.r,hi.rgb.r),std::clamp(a.rgb.g,lo.rgb.g,hi.rgb.g),
    std::clamp(a.rgb.b,lo.rgb.b,hi.rgb.b),std::clamp(a.a,lo.a,hi.a)};}
struct Position {float2 xy;};
struct FragmentInput {float2 uv;Position pos;float4 color;float4 hx;float4 texel;};
struct PsSample {float4 texel;float3 source;float opacity,alpha;};
struct Uniforms {float4 params,params2;} tu{{1,0,960,540},{-1,0,0,0}};
const int tex0=0,tex2=2;
static float4 sampled(0,0,0,0);
float4 tex2D(int sampler,float2){if(sampler!=tex0)std::abort();return sampled;}
float2 haze_uv(FragmentInput i){return i.uv;}
float soft_mask(FragmentInput){return 1.f;}
float3 group_rgb(float3){std::abort();}
float group_alpha(float,float){std::abort();}
float rule_at(float2,float){std::abort();}
float rule_mask(float,float,float){std::abort();}
'''


MAIN = r'''
static uint64_t cases=0, gold_checks=0;
static bool check(unsigned op,float4 texel,float4 tint,U4 destination,bool fill,bool noalpha){
    sampled=texel;tu.params={float(op),fill?1.f:0.f,960,540};tu.params2={-1,0,noalpha?1.f:0.f,0};
    FragmentInput input{{.25f,.75f},{{100,200}},tint,{0,0,0,0},texel};
    const U4 expected=reference(input,destination);
    const U4 actual=op==1?plain1(input,destination):op==6?plain6(input,destination):op==7?plain7(input,destination):plain13(input,destination);
    if(actual!=expected){std::fprintf(stderr,"mismatch case=%llu op=%u fill=%u noalpha=%u\n",
        (unsigned long long)cases,op,fill,noalpha);return false;}
    // This relation is independent of the selected blending equation.
    float alpha=(noalpha?1.f:texel.a)*tint.a;
    if(alpha<=0 && actual!=destination){std::fprintf(stderr,"nonpositive alpha changed destination RGBA\n");return false;}
    if(alpha>0 && actual.bytes[3]!=255){std::fprintf(stderr,"positive PS alpha must store opaque\n");return false;}
    ++cases;return true;
}
static bool gold(unsigned op,float4 texel,float4 tint,U4 d,bool fill,bool noalpha,U4 wanted){
    if(!check(op,texel,tint,d,fill,noalpha))return false;
    sampled=texel;tu.params={float(op),fill?1.f:0.f,960,540};tu.params2={-1,0,noalpha?1.f:0.f,0};
    FragmentInput input{{0,0},{{0,0}},tint,{0,0,0,0},texel};
    const U4 actual=op==1?plain1(input,d):op==6?plain6(input,d):op==7?plain7(input,d):plain13(input,d);
    ++gold_checks;
    if(actual!=wanted){std::fprintf(stderr,"gold anchor=%llu op=%u actual=%u,%u,%u,%u wanted=%u,%u,%u,%u\n",
        (unsigned long long)gold_checks,op,actual.bytes[0],actual.bytes[1],actual.bytes[2],actual.bytes[3],
        wanted.bytes[0],wanted.bytes[1],wanted.bytes[2],wanted.bytes[3]);return false;}
    return true;
}
int main(){
    if(!gold(7,{1,1,1,1},{1,1,1,1},{0,64,128,37},false,false,{0,255,255,255}))return 1;
    if(!gold(7,{1,1,1,.5f},{1,1,1,1},{32,64,128,37},false,false,{64,128,255,255}))return 1;
    if(!gold(7,{1,1,1,0},{1,1,1,1},{17,99,201,7},false,false,{17,99,201,7}))return 1;
    // Hand-derived anchors use native byte destinations and exact dyadic inputs.
    if(!gold(1,{.25f,.5f,.75f,.5f},{.5f,.25f,1,.5f},{64,128,192,37},false,false,{72,136,208,255}))return 1;
    // Fill retains sampled alpha but ignores sampled RGB.
    if(!gold(1,{.125f,.875f,.25f,.5f},{.5f,.25f,1,.5f},{0,0,0,37},true,false,{32,16,64,255}))return 1;
    // NoAlpha ignores sampled alpha, then applies vertex opacity once.
    if(!gold(1,{.25f,.5f,.75f,0},{.5f,.25f,1,.5f},{0,0,0,37},false,true,{16,16,96,255}))return 1;
    // Dodge k=0 returns white even at destination zero; blend opacity is applied after it.
    if(!gold(6,{1,1,1,.5f},{1,1,1,.5f},{0,127,255,1},false,false,{64,159,255,255}))return 1;
    // Overlay chooses on destination, with different RGB branches in the same pixel.
    if(!gold(13,{.75f,.25f,.25f,1},{1,1,1,1},{64,128,255,19},false,false,{96,65,255,255}))return 1;
    if(!gold(13,{1,1,1,0},{1,1,1,1},{17,99,201,7},true,false,{17,99,201,7}))return 1;
    const float values[]={-2,-.01f,-0.f,0,1.f/65536,1.f/255,.25f,
        std::nextafter(.5f,0.f),.5f,std::nextafter(.5f,1.f),.75f,
        std::nextafter(1.f,0.f),1,std::nextafter(1.f,2.f),2,4};
    const uint8_t bytes[]={0,1,2,63,64,127,128,129,254,255};
    for(unsigned op:{1u,6u,7u,13u})for(bool fill:{false,true})for(bool noalpha:{false,true})
        for(float s:values)for(float ta:values)for(float opacity:values)for(uint8_t d:bytes){
            if(!check(op,{s,s*.5f,1.f-s,ta},{.5f,1.f,.25f,opacity},{d,(uint8_t)(255-d),(uint8_t)(d/2),37},fill,noalpha))return 1;
        }
    uint32_t seed=0x113619;
    auto random=[&](){seed^=seed<<13;seed^=seed>>17;seed^=seed<<5;return seed;};
    auto value=[&](){return (int(random()%80001)-30000)/10000.f;};
    for(unsigned n=0;n<200000;++n){
        float4 t(value(),value(),value(),value()),c(value(),value(),value(),value());
        U4 d((uint8_t)random(),(uint8_t)random(),(uint8_t)random(),(uint8_t)random());
        if(!check(std::array<unsigned,4>{1,6,7,13}[random()%4],t,c,d,random()&1,random()&1))return 1;
    }
    std::printf("PASS: %llu authored-source F32 cases; %llu independent RGBA8 anchors\n",
        (unsigned long long)cases,(unsigned long long)gold_checks);
}
'''


def source_cpp(text):
    replacements = {"unsigned char4": "U4"}
    def transform(value):
        for old, new in replacements.items():
            value = value.replace(old, new)
        return value
    generated = SHIM
    for signature, marker in [
        ("float4 sample_img(FragmentInput i)", "float4 sample_img("),
        ("PsSample ps_source(FragmentInput i)", "PsSample ps_source("),
        ("float3 ps_mix(float3 d,float3 r,float a)", "float3 ps_mix("),
        ("float3 ps_overlay(float3 d,float3 s,float a)", "float3 ps_overlay("),
        ("U4 native_pack(float4 value)", "unsigned char4 native_pack("),
        ("float4 native_unpack(U4 value)", "float4 native_unpack("),
        ("U4 plain1(FragmentInput i,U4 destination)", "unsigned char4 fs_ps1_plain_fetch("),
        ("U4 plain6(FragmentInput i,U4 destination)", "unsigned char4 fs_ps6_plain_fetch("),
        ("U4 plain7(FragmentInput i,U4 destination)", "unsigned char4 fs_ps7_plain_fetch("),
        ("U4 plain13(FragmentInput i,U4 destination)", "unsigned char4 fs_ps13_fetch("),
    ]:
        generated += signature + "{\n" + transform(body(text, marker)) + "\n}\n"
    prelude = body(text, "float4 ps_apply(").split("// Return each operation directly", 1)[0]
    prelude = prelude.replace("return d4;", "return destination;")
    generated += "U4 reference(FragmentInput i,U4 destination){float4 d4=native_unpack(destination);\n"
    generated += transform(prelude) + "(void)t;(void)ca;(void)zero;\nfloat4 result=[&]()->float4{\n"
    for op in (1, 6, 7, 13):
        marker = f"if (op == {op}.0)"
        generated += f"if(op=={op}.0){{\n{body(text, marker)}\n}}\n"
    generated += "std::abort();}();return native_pack(result);}\n" + MAIN
    # C++17 has no C compound-array literal.
    return generated.replace("(unsigned[]){1,6,13}[random()%3]", "std::array<unsigned,3>{1,6,13}[random()%3]")


def main():
    source = SOURCE.read_text()
    output = ROOT / ".work/vita/plain-ps-math"
    output.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=output))
    compiler = os.environ.get("CXX", "c++")
    flags = ["-std=c++17", "-O2", "-Wall", "-Wextra", "-Werror", "-fno-fast-math", "-ffp-contract=off"]
    cpp = source_cpp(source)
    mutants = {
        "green": cpp,
        "red-fill-discards-sampled-alpha": cpp.replace(
            "float a = t.a * i.color.a;", "float a = (tu.params.y > .5f ? 1.f : t.a) * i.color.a;"),
        "red-noalpha-forgotten": cpp.replace(
            "if (tu.params2.z > 0.5) { t.a = 1.0; }", "if (false) { t.a = 1.0; }", 3),
        "red-opacity-multiplied-twice": cpp.replace(
            "float a = t.a * i.color.a;", "float a = t.a * i.color.a * i.color.a;"),
        "red-dodge-zero-boundary": cpp.replace("k.r > 0.0 ? raw.r : 1.0", "k.r > 0.0 ? raw.r : 0.0"),
        "red-overlay-source-branch": cpp.replace("d.r < 0.5 ? low.r", "s.r < 0.5 ? low.r"),
        "red-op7-zero-destination": cpp.replace("d.r > 0.0 ? raw.r : 0.0", "d.r > 0.0 ? raw.r : 1.0", 1),
    }
    results = {}
    for name, code in mutants.items():
        path = run / f"{name}.cpp"
        path.write_text(code)
        command = [compiler, *flags, str(path), "-o", str(run / name)]
        subprocess.run(command, check=True)
        result = subprocess.run([str(run / name)], text=True, capture_output=True)
        (run / f"{name}.log").write_text(result.stdout + result.stderr)
        results[name] = {"exit_code": result.returncode, "output": result.stdout + result.stderr,
                         "command": command, "source_sha256": hashlib.sha256(code.encode()).hexdigest()}
        if (name == "green") != (result.returncode == 0):
            raise SystemExit(f"Unexpected {name} result: {result.stdout}{result.stderr}")
        print(name, (result.stdout + result.stderr).strip())
    receipt = {"schema": "knife-vita-plain-ps-host-math-v1", "time_utc": datetime.now(timezone.utc).isoformat(),
        "shader_source_sha256": hashlib.sha256(SOURCE.read_bytes()).hexdigest(),
        "compiler": subprocess.check_output([compiler, "--version"], text=True).splitlines()[0],
        "results": results, "run_directory": str(run),
        "limitations": ["Host F32 vector shim; no Cg/GXP compiler or GPU execution.",
            "Texture sample values are supplied; filtering, interpolation and UV/WPOS are not exercised.",
            "Haze/soft/rule/group are excluded by the production plain-source gate and abort if invoked in this shim.",
            "CPU results do not admit Half candidates or prove GPU pixel equivalence."]}
    (run / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print("RECEIPT", run / "receipt.json")


if __name__ == "__main__":
    main()
