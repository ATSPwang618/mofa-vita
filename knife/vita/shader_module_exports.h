/* Bounds-checked SCE ELF export table reader. The 0x20-byte export layout and
 * relative module-info fields follow vita-toolchain/src/sce-elf-defs.h.
 * Memory is explicit so the exact production parser can run in host tests. */
#ifndef KNIFE_SHADER_MODULE_EXPORTS_H
#define KNIFE_SHADER_MODULE_EXPORTS_H
#include <stdint.h>
#include <stddef.h>
#include <string.h>
typedef struct { uintptr_t address; size_t bytes; const unsigned char *memory; } KvShaderSegment;
typedef struct { KvShaderSegment segments[4]; } KvShaderModuleMemory;
static const unsigned char *shader_module_view(const KvShaderModuleMemory *module, uintptr_t address, size_t bytes) {
    for (unsigned i=0; i<4; ++i) {
        const KvShaderSegment *segment=&module->segments[i];
        if (segment->memory && address>=segment->address && address-segment->address<=segment->bytes
            && bytes<=segment->bytes-(address-segment->address)) return segment->memory+(address-segment->address);
    }
    return NULL;
}
static uint32_t shader_module_word(const unsigned char *data) {
    return (uint32_t)data[0]|(uint32_t)data[1]<<8|(uint32_t)data[2]<<16|(uint32_t)data[3]<<24;
}
static unsigned shader_module_half(const unsigned char *data) { return data[0]|(unsigned)data[1]<<8; }
static int shader_module_resolve(const KvShaderModuleMemory *module, uint32_t info_offset,
    const uint32_t *required_nids, uintptr_t *results, unsigned required_count) {
    const uintptr_t base=module->segments[0].address;
    if (info_offset>module->segments[0].bytes) return 0;
    const unsigned char *info=shader_module_view(module,base+info_offset,0x5c);
    if (!info || memcmp(info+4,"SceShaccCg\0",11)) return 0;
    uint32_t start=shader_module_word(info+36), end=shader_module_word(info+40);
    if (end<start || end-start>4096 || (end-start)%32) return 0;
    const unsigned char *exports=shader_module_view(module,base+start,end-start);
    if (!exports) return 0;
    memset(results,0,required_count*sizeof(*results));
    for (unsigned at=0; at<end-start; at+=32) {
        const unsigned char *record=exports+at;
        if (shader_module_half(record)!=32) return 0;
        if (shader_module_word(record+16)!=UINT32_C(0xa05bbebb)) continue;
        const unsigned funcs=shader_module_half(record+6);
        const uint32_t variables=shader_module_word(record+8), tls=shader_module_word(record+12);
        if (!funcs || funcs>256 || variables>256 || tls>256 || funcs+variables+tls>256) return 0;
        const unsigned char *nids=shader_module_view(module,shader_module_word(record+24),4*(funcs+variables+tls));
        const unsigned char *entries=shader_module_view(module,shader_module_word(record+28),4*(funcs+variables+tls));
        if (!nids || !entries) return 0;
        for (unsigned n=0; n<funcs; ++n) for (unsigned r=0; r<required_count; ++r) {
            if (shader_module_word(nids+4*n)!=required_nids[r]) continue;
            uintptr_t entry=shader_module_word(entries+4*n);
            if (results[r] || !shader_module_view(module,entry&~(uintptr_t)1,2)) return 0;
            results[r]=entry;
        }
    }
    for (unsigned r=0; r<required_count; ++r) if (!results[r]) return 0;
    return 1;
}
#endif
