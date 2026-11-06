#ifndef KNIFE_MATERIAL_VARIANTS_H
#define KNIFE_MATERIAL_VARIANTS_H
#include <stdint.h>
enum { KV_MATERIAL_FIRST=88, KV_MATERIAL_COUNT=20, KV_MATERIAL_END=108 };
static inline int material_shader(uint32_t id) { return id>=88 && id<108; }
static inline int material_group(uint32_t id) { return id>=105 && id<108; }
static inline int material_haze(uint32_t id) { return id>=96 && id<=99; }
static inline int material_half(uint32_t id) { return id==88 || id==89 || id==90 || id==91 || id==94 || id==95; }
static inline uint32_t material_operation(uint32_t id) {
    static const uint8_t ops[20]={1,1,6,6,7,7,13,13,1,6,7,13,3,4,5,11,14,6,8,14};return material_shader(id)?ops[id-88]:0;
}
static inline uint32_t material_baseline(uint32_t id) {
    static const uint8_t base[20]={49,49,50,50,48,48,52,52,64,69,70,76,66,67,68,74,77,69,71,77};return material_shader(id)?base[id-88]:0;
}
static inline int material_enabled(uint32_t id,uint64_t options) {
    if (material_group(id)) return (options & (UINT64_C(1)<<42)) != 0;
    return (options & (UINT64_C(1)<<41)) && (!material_half(id) || (options & (UINT64_C(1)<<32)))
        && ((id!=92 && id!=93) || (options & (UINT64_C(1)<<39)));
}
#endif
