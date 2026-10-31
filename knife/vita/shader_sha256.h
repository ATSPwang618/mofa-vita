/* Small streaming SHA-256 for compiler identity, independent of device libraries.
 * No allocations and no file writes. Used only during shader initialization. */
#ifndef KNIFE_SHADER_SHA256_H
#define KNIFE_SHADER_SHA256_H
#include <stdint.h>
#include <stddef.h>
#include <string.h>
typedef struct { uint32_t h[8]; uint64_t bytes; unsigned used; unsigned char block[64]; } KvShaderSha256;
static uint32_t shader_ror(uint32_t value, unsigned count) { return value >> count | value << (32 - count); }
static void shader_sha256_block(KvShaderSha256 *state, const unsigned char *block) {
    static const uint32_t k[64] = {
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2
    };
    uint32_t words[64];
    for (unsigned i = 0; i < 16; ++i) words[i] = (uint32_t)block[i*4] << 24
        | (uint32_t)block[i*4+1] << 16 | (uint32_t)block[i*4+2] << 8 | block[i*4+3];
    for (unsigned i = 16; i < 64; ++i) {
        uint32_t a = words[i-15], b = words[i-2];
        words[i] = words[i-16] + (shader_ror(a,7) ^ shader_ror(a,18) ^ (a>>3))
            + words[i-7] + (shader_ror(b,17) ^ shader_ror(b,19) ^ (b>>10));
    }
    uint32_t a=state->h[0], b=state->h[1], c=state->h[2], d=state->h[3],
        e=state->h[4], f=state->h[5], g=state->h[6], h=state->h[7];
    for (unsigned i=0; i<64; ++i) {
        uint32_t t = h + (shader_ror(e,6)^shader_ror(e,11)^shader_ror(e,25))
            + ((e&f)^((~e)&g)) + k[i] + words[i];
        uint32_t u = (shader_ror(a,2)^shader_ror(a,13)^shader_ror(a,22)) + ((a&b)^(a&c)^(b&c));
        h=g; g=f; f=e; e=d+t; d=c; c=b; b=a; a=t+u;
    }
    state->h[0]+=a; state->h[1]+=b; state->h[2]+=c; state->h[3]+=d;
    state->h[4]+=e; state->h[5]+=f; state->h[6]+=g; state->h[7]+=h;
}
static void shader_sha256_init(KvShaderSha256 *state) {
    *state = (KvShaderSha256){ .h={0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
        0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19} };
}
static void shader_sha256_update(KvShaderSha256 *state, const void *data, size_t bytes) {
    const unsigned char *input=data;
    state->bytes += bytes;
    while (bytes) {
        size_t take=64-state->used; if (take>bytes) take=bytes;
        memcpy(state->block+state->used,input,take); state->used+=(unsigned)take; input+=take; bytes-=take;
        if (state->used==64) { shader_sha256_block(state,state->block); state->used=0; }
    }
}
static void shader_sha256_finish(KvShaderSha256 *state, char hex[65]) {
    uint64_t bits=state->bytes*8;
    state->block[state->used++]=0x80;
    if (state->used>56) {
        memset(state->block+state->used,0,64-state->used); shader_sha256_block(state,state->block); state->used=0;
    }
    memset(state->block+state->used,0,56-state->used);
    for (unsigned i=0; i<8; ++i) state->block[63-i]=(unsigned char)(bits>>(i*8));
    shader_sha256_block(state,state->block);
    static const char digits[]="0123456789abcdef";
    for (unsigned i=0; i<32; ++i) {
        unsigned byte=state->h[i/4]>>(24-(i%4)*8)&255;
        hex[i*2]=digits[byte>>4]; hex[i*2+1]=digits[byte&15];
    }
    hex[64]=0;
}
#endif
