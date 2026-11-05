#!/usr/bin/env python3
"""Execute the authored Cg op7 arithmetic through a host vector shim.

This checks specialized-vs-generic post-source arithmetic and UNORM8 results.
Texture sampling, ps_source, Cg/GXP execution and hardware timing are outside
this CPU check. Native routing tests cover preservation of source parameters.
"""
from pathlib import Path
import hashlib
import json
import os
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "knife/vita/shaders/effects.cg"


def body(text, marker):
    start = text.index("{", text.index(marker))
    depth = 1
    end = start + 1
    while depth:
        depth += (text[end] == "{") - (text[end] == "}")
        end += 1
    return text[start + 1:end - 1]


def main():
    source = SOURCE.read_text()
    specialized = body(source, "unsigned char4 fs_ps7_fetch(")
    generic = body(source, "else if (op == 7.0)")
    # Full source preparation must remain shared, rather than replacing it with
    # a plain tex2D read which would drop masks, haze, Fill or NoAlpha.
    assert "ps_source(i)" in specialized
    assert "return destination;" in specialized
    assert "tex2D(" not in specialized
    cpp = r'''
#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
struct float3 { float r,g,b; float3(float x,float y,float z):r(x),g(y),b(z){} };
float3 operator-(float3 a,float3 b){return {a.r-b.r,a.g-b.g,a.b-b.b};}
float3 operator*(float a,float3 b){return {a*b.r,a*b.g,a*b.b};}
float3 operator/(float3 a,float3 b){return {a.r/b.r,a.g/b.g,a.b/b.b};}
float3 min(float3 a,float3 b){return {std::fmin(a.r,b.r),std::fmin(a.g,b.g),std::fmin(a.b,b.b)};}
float3 max(float3 a,float3 b){return {std::fmax(a.r,b.r),std::fmax(a.g,b.g),std::fmax(a.b,b.b)};}
float min(float a,float b){return std::fmin(a,b);}
struct float4 { float3 rgb; float a; float4(float3 v,float w):rgb(v),a(w){} };
using U4=std::array<uint8_t,4>;
struct PsSample {float3 source;float alpha;};
struct FragmentInput {PsSample sample;};
PsSample ps_source(FragmentInput i){return i.sample;}
float4 native_unpack(U4 v){return {{v[0]/255.f,v[1]/255.f,v[2]/255.f},v[3]/255.f};}
U4 native_pack(float4 v){
    float f[4]={v.rgb.r,v.rgb.g,v.rgb.b,v.a};U4 out;
    for(int c=0;c<4;c++)out[c]=(uint8_t)std::floor(std::clamp(f[c],0.f,1.f)*255.f+.5f);
    return out;
}
U4 specialized(FragmentInput i,U4 destination){
''' + specialized + r'''
}
U4 reference(FragmentInput i,U4 destination){
    PsSample source=ps_source(i);float3 s=source.source;float a=source.alpha;
    if(min(a,0.0)==a)return destination;
    float3 d=native_unpack(destination).rgb;
    const float3 one(1,1,1),zero(0,0,0);
    float4 result=[&]()->float4{
''' + generic + r'''
    }();return native_pack(result);
}
int main(){
    uint64_t cases=0;uint32_t seed=0x1246;
    auto check=[&](float3 s,float a,U4 d){
        FragmentInput i{{s,a}};U4 actual=specialized(i,d),expected=reference(i,d);
        if(actual!=expected){std::fprintf(stderr,"op7 mismatch case=%llu\n",(unsigned long long)cases);return false;}
        if(a<=0 && actual!=d){std::fprintf(stderr,"zero alpha changed RGBA\n");return false;}
        ++cases;return true;
    };
    const float sources[]={-2,-.01f,0,1.f/255,.5f,std::nextafter(1.f,0.f),1,std::nextafter(1.f,2.f),1.5f,2,4};
    const float alphas[]={-1,-0.f,0,1.f/65536,1.f/255,.5f,std::nextafter(1.f,0.f),1,std::nextafter(1.f,2.f),2};
    const uint8_t destinations[]={0,1,2,127,128,254,255};
    for(float s:sources)for(float a:alphas)for(uint8_t d:destinations)for(uint8_t da:destinations)
        if(!check({s,s*.5f,s*2},a,{d,(uint8_t)(255-d),(uint8_t)(d/2),da}))return 1;
    // Explicit values on both sides of k=max(1-a*s,0), with zero d lanes.
    for(float a:alphas)if(a>0)for(uint8_t da:destinations){
        float edge=1.f/a;
        if(!check({std::nextafter(edge,0.f),edge,std::nextafter(edge,INFINITY)},a,{0,127,255,da}))return 1;
    }
    auto random=[&](){seed^=seed<<13;seed^=seed>>17;seed^=seed<<5;return seed;};
    for(unsigned n=0;n<200000;n++){
        auto value=[&](){return (int(random()%60001)-20000)/10000.f;};
        float3 s(value(),value(),value());float a=value();
        U4 d={(uint8_t)random(),(uint8_t)random(),(uint8_t)random(),(uint8_t)random()};
        if(!check(s,a,d))return 1;
    }
    std::printf("PASS: %llu finite post-source op7 cases, exact UNORM8 RGBA match\n",(unsigned long long)cases);
}
'''
    output = ROOT / ".work/vita/ps7-math"
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="run-", dir=output) as run:
        path = Path(run)
        (path / "test.cpp").write_text(cpp)
        command = [os.environ.get("CXX", "c++"), "-std=c++17", "-O2", "-Wall", "-Wextra",
                   "-Werror", "-fno-fast-math", "-ffp-contract=off",
                   str(path / "test.cpp"), "-o", str(path / "test")]
        subprocess.run(command, check=True)
        result = subprocess.run([str(path / "test")], check=True, text=True, capture_output=True)
        print(result.stdout.strip())
        receipt = {"shader_source_sha256": hashlib.sha256(SOURCE.read_bytes()).hexdigest(),
                   "command": command[0:8], "result": result.stdout.strip(),
                   "authority": "host vector arithmetic only; not Cg/GXP, sampling, pixels or hardware timing"}
        (output / "result.json").write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
