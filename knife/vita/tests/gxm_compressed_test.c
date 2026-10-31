/* CPU fault injection against the complete production C backend and real SDK
 * declarations. No Vita driver, decompressor, GPU, or emulator executes here. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#include KV_GXM_BACKEND_SOURCE
int sceGxmTextureSetData(SceGxmTexture *t, const void *data) { (void)t; (void)data; return 0; }

static volatile unsigned int host_completion;
volatile unsigned int *sceGxmGetNotificationRegion(void) { return &host_completion; }
int sceGxmNotificationWait(const SceGxmNotification *n) { *n->address = n->value; return 0; }

enum Fault { NONE, ALLOC, BASE, MAP, INIT, MIN, MAG, CLAMP_U, CLAMP_V, GAMMA };
enum { ALIGNMENT = 256 * 1024, MAX_DATA = 4 * 1024 * 1024 };
static _Alignas(256 * 1024) uint8_t backing[MAX_DATA], input[MAX_DATA];
static _Alignas(64) uint8_t movie_frame[128];
static unsigned checks, failures, cases, allocated, released, finishes, transfers;
static unsigned alloc_calls, init_calls, min_calls, mag_calls, u_calls, v_calls, gamma_calls;
static unsigned map_calls, unmap_calls, base_calls, free_calls, unexpected_calls;
static enum Fault fault;
static int unmap_failure, free_failure;
static SceUID current_thread = 71;
static KvGxm *owner;
static uint32_t wanted_width, wanted_height, wanted_format, wanted_srgb, wanted_linear;
static uint32_t wanted_allocation;
static char name[128];
static const int fault_code = -73, unmap_code = -81, free_code = -82;

static void expect(int ok, const char *expression, int line) {
    ++checks;
    if (!ok) { ++failures; fprintf(stderr, "FAIL %s line %d: %s\n", name, line, expression); }
}
#define EXPECT(e) expect(!!(e), #e, __LINE__)
static int status(enum Fault point) { return fault == point ? fault_code : 0; }
static int unexpected(void) { ++unexpected_calls; return -99; }
static void reset(KvGxm *g, const char *label) {
    ++cases; snprintf(name, sizeof(name), "%s", label);
    memset(g, 0, sizeof(*g)); owner = g;
    g->thread = current_thread; g->initialized = 1;
    g->context = (SceGxmContext *)(void *)movie_frame;
    g->stats.parameter_reserved_bytes = 16 * 1024 * 1024;
    fault = NONE; unmap_failure = free_failure = 0;
    allocated = released = finishes = transfers = 0;
    alloc_calls = init_calls = min_calls = mag_calls = u_calls = v_calls = gamma_calls = 0;
    map_calls = unmap_calls = base_calls = free_calls = unexpected_calls = 0;
}

/* These stubs constrain only the external SDK boundary. Allocation content is
 * deliberately dirty, so omitted zeroing is observable in the real owner. */
SceUID sceKernelGetThreadId(void) { return current_thread; }
int sceKernelGetFreeMemorySize(SceKernelFreeMemorySizeInfo *info) {
    EXPECT(info && info->size == (int)sizeof(*info));
    info->size_cdram = 64 * 1024 * 1024; return 0;
}
SceUID sceKernelAllocMemBlock(const char *label, SceKernelMemBlockType type,
        SceSize size, SceKernelAllocMemBlockOpt *options) {
    ++alloc_calls;
    EXPECT(label && type == SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW && options == NULL);
    EXPECT(size == wanted_allocation && size <= sizeof(backing) && !(size % ALIGNMENT));
    if (status(ALLOC)) return fault_code;
    if (size > sizeof(backing)) return -99;
    EXPECT(allocated == released);
    memset(backing, 0xa7, size); allocated = size;
    return 41;
}
int sceKernelGetMemBlockBase(SceUID uid, void **base) {
    ++base_calls; EXPECT(uid == 41 && base); if (status(BASE)) return fault_code;
    *base = backing; return 0;
}
int sceKernelFreeMemBlock(SceUID uid) {
    ++free_calls; EXPECT(uid == 41);
    if (free_failure) return free_code;
    EXPECT(allocated && !released); released = allocated; return 0;
}
int sceGxmMapMemory(void *base, unsigned size, SceGxmMemoryAttribFlags flags) {
    ++map_calls; EXPECT(base == backing && size == allocated);
    EXPECT(flags == (SCE_GXM_MEMORY_ATTRIB_READ | SCE_GXM_MEMORY_ATTRIB_WRITE));
    return status(MAP);
}
int sceGxmUnmapMemory(void *base) {
    ++unmap_calls; EXPECT(base == backing); return unmap_failure ? unmap_code : 0;
}
int sceGxmMapVertexUsseMemory(void *p, unsigned n, unsigned *o) { (void)p; (void)n; (void)o; return unexpected(); }
int sceGxmMapFragmentUsseMemory(void *p, unsigned n, unsigned *o) { (void)p; (void)n; (void)o; return unexpected(); }
int sceGxmUnmapVertexUsseMemory(void *p) { (void)p; return unexpected(); }
int sceGxmUnmapFragmentUsseMemory(void *p) { (void)p; return unexpected(); }
int sceGxmTextureInitSwizzledArbitrary(SceGxmTexture *t, const void *data,
        SceGxmTextureFormat format, unsigned w, unsigned h, unsigned mips) {
    ++init_calls; EXPECT(t && data == backing && (uint32_t)format == wanted_format);
    EXPECT(w == wanted_width && h == wanted_height && mips == 0);
    return status(INIT);
}
int sceGxmTextureSetMinFilter(SceGxmTexture *t, SceGxmTextureFilter filter) {
    ++min_calls; EXPECT(t && filter == (wanted_linear ? SCE_GXM_TEXTURE_FILTER_LINEAR : SCE_GXM_TEXTURE_FILTER_POINT));
    return status(MIN);
}
int sceGxmTextureSetMagFilter(SceGxmTexture *t, SceGxmTextureFilter filter) {
    ++mag_calls; EXPECT(t && filter == (wanted_linear ? SCE_GXM_TEXTURE_FILTER_LINEAR : SCE_GXM_TEXTURE_FILTER_POINT));
    return status(MAG);
}
int sceGxmTextureSetUAddrMode(SceGxmTexture *t, SceGxmTextureAddrMode mode) {
    ++u_calls; EXPECT(t && mode == SCE_GXM_TEXTURE_ADDR_CLAMP); return status(CLAMP_U);
}
int sceGxmTextureSetVAddrMode(SceGxmTexture *t, SceGxmTextureAddrMode mode) {
    ++v_calls; EXPECT(t && mode == SCE_GXM_TEXTURE_ADDR_CLAMP); return status(CLAMP_V);
}
int sceGxmTextureSetGammaMode(SceGxmTexture *t, SceGxmTextureGammaMode mode) {
    ++gamma_calls; EXPECT(t && wanted_srgb && mode == SCE_GXM_TEXTURE_GAMMA_BGR); return status(GAMMA);
}
void sceGxmFinish(SceGxmContext *context) { ++finishes; EXPECT(context == owner->context); }
int sceGxmTransferFinish(void) { ++transfers; return unexpected(); }
int sceGxmTextureInitLinear(SceGxmTexture *t, const void *p, SceGxmTextureFormat f,
        unsigned w, unsigned h, unsigned n) { (void)t; (void)p; (void)f; (void)w; (void)h; (void)n; return unexpected(); }
int sceGxmTextureValidate(const SceGxmTexture *t) { (void)t; return unexpected(); }
int sceGxmDestroyRenderTarget(SceGxmRenderTarget *t) { (void)t; return unexpected(); }
int sceGxmSyncObjectDestroy(SceGxmSyncObject *t) { (void)t; return unexpected(); }
/* Copy's render-target arm is linked but every immutable test must reject
 * before reaching any of these SDK operations. */
int sceGxmCreateRenderTarget(const SceGxmRenderTargetParams *p, SceGxmRenderTarget **o) { (void)p; (void)o; return unexpected(); }
int sceGxmSyncObjectCreate(SceGxmSyncObject **o) { (void)o; return unexpected(); }
int sceGxmColorSurfaceInit(SceGxmColorSurface *s, SceGxmColorFormat f, SceGxmColorSurfaceType t,
        SceGxmColorSurfaceScaleMode m, SceGxmOutputRegisterSize r, unsigned w, unsigned h,
        unsigned p, void *d) { (void)s; (void)f; (void)t; (void)m; (void)r; (void)w; (void)h; (void)p; (void)d; return unexpected(); }
int sceGxmColorSurfaceSetGammaMode(SceGxmColorSurface *s, SceGxmColorSurfaceGammaMode m) { (void)s; (void)m; return unexpected(); }
int sceGxmBeginScene(SceGxmContext *c, unsigned f, const SceGxmRenderTarget *t,
        const SceGxmValidRegion *r, SceGxmSyncObject *v, SceGxmSyncObject *s,
        const SceGxmColorSurface *p, const SceGxmDepthStencilSurface *d) {
    (void)c; (void)f; (void)t; (void)r; (void)v; (void)s; (void)p; (void)d; return unexpected();
}
int sceGxmEndScene(SceGxmContext *c, const SceGxmNotification *v, const SceGxmNotification *f) { (void)c; (void)v; (void)f; return unexpected(); }
void sceGxmSetViewport(SceGxmContext *c, float a, float b, float d, float e, float f, float g) { (void)c; (void)a; (void)b; (void)d; (void)e; (void)f; (void)g; (void)unexpected(); }
void sceGxmSetCullMode(SceGxmContext *c, SceGxmCullMode m) { (void)c; (void)m; (void)unexpected(); }
void sceGxmSetFrontDepthFunc(SceGxmContext *c, SceGxmDepthFunc m) { (void)c; (void)m; (void)unexpected(); }
void sceGxmSetBackDepthFunc(SceGxmContext *c, SceGxmDepthFunc m) { (void)c; (void)m; (void)unexpected(); }
void sceGxmSetFrontDepthWriteEnable(SceGxmContext *c, SceGxmDepthWriteMode m) { (void)c; (void)m; (void)unexpected(); }
void sceGxmSetBackDepthWriteEnable(SceGxmContext *c, SceGxmDepthWriteMode m) { (void)c; (void)m; (void)unexpected(); }
void sceGxmSetVertexProgram(SceGxmContext *c, const SceGxmVertexProgram *p) { (void)c; (void)p; (void)unexpected(); }
void sceGxmSetFragmentProgram(SceGxmContext *c, const SceGxmFragmentProgram *p) { (void)c; (void)p; (void)unexpected(); }
int sceGxmSetFragmentTexture(SceGxmContext *c, unsigned u, const SceGxmTexture *t) { (void)c; (void)u; (void)t; return unexpected(); }
int sceGxmSetVertexStream(SceGxmContext *c, unsigned s, const void *v) { (void)c; (void)s; (void)v; return unexpected(); }
int sceGxmDraw(SceGxmContext *c, SceGxmPrimitiveType p, SceGxmIndexFormat f, const void *i, unsigned n) { (void)c; (void)p; (void)f; (void)i; (void)n; return unexpected(); }
int sceGxmTransferCopy(uint32_t w, uint32_t h, uint32_t k, uint32_t m, SceGxmTransferColorKeyMode km,
        SceGxmTransferFormat sf, SceGxmTransferType st, const void *s, uint32_t sx, uint32_t sy, int32_t ss,
        SceGxmTransferFormat df, SceGxmTransferType dt, void *d, uint32_t dx, uint32_t dy, int32_t ds,
        SceGxmSyncObject *sync, uint32_t flags, const SceGxmNotification *n) {
    (void)w; (void)h; (void)k; (void)m; (void)km; (void)sf; (void)st; (void)s; (void)sx; (void)sy;
    (void)ss; (void)df; (void)dt; (void)d; (void)dx; (void)dy; (void)ds; (void)sync; (void)flags; (void)n;
    return unexpected();
}

static void setup_upload(uint32_t w, uint32_t h, uint32_t fmt, uint32_t length, uint32_t srgb, uint32_t linear) {
    wanted_width = w; wanted_height = h; wanted_format = fmt;
    wanted_srgb = srgb; wanted_linear = linear;
    wanted_allocation = (length + ALIGNMENT - 1) & ~(ALIGNMENT - 1U);
}
static int upload(KvGxm *g, uint32_t length, KvGxmTexture **out) {
    return kv_gxm_texture_create_compressed(g, input, length, wanted_width, wanted_height,
        wanted_format, wanted_srgb, wanted_linear, out);
}
static void cleanup_retained(KvGxm *g) {
    /* A test-owned partial owner remains reachable even if cleanup failed.
     * Retry the actual block release after removing only the SDK fault. */
    unmap_failure = free_failure = 0;
    while (g->textures) {
        KvGxmTexture *t = g->textures;
        EXPECT(block_free(g, &t->pixels) == 0);
        g->textures = t->next; free(t);
    }
}
static void invalid(uint32_t w, uint32_t h, uint32_t format, uint32_t length, uint32_t srgb, uint32_t linear) {
    KvGxm g; reset(&g, "invalid dimensions/format/length/flags");
    setup_upload(w,h,format,length,srgb,linear);
    KvGxmTexture *out = (KvGxmTexture *)(void *)movie_frame;
    EXPECT(upload(&g,length,&out) < 0);
    EXPECT(alloc_calls == 0 && init_calls == 0 && unexpected_calls == 0);
    EXPECT(g.textures == NULL && g.stats.cdram_mapped_bytes == 0 && !g.closing);
}
static void preconditions(void) {
    for (unsigned state=0; state<7; ++state) {
        KvGxm g; reset(&g, "owner/scene/pointer precondition");
        setup_upload(8,8,0x81000000U,32,0,0);
        KvGxmTexture *out = (KvGxmTexture *)(void *)movie_frame;
        if (state==1) g.thread = current_thread + 1;
        if (state==2) g.closing = 1;
        if (state==3) g.scene_open = 1;
        if (state==6) memset(&g,0,sizeof(g));
        EXPECT(kv_gxm_texture_create_compressed(state==0 ? NULL : &g, state==4 ? NULL : input,
            32,8,8,0x81000000U,0,0,state==5 ? NULL : &out) < 0);
        EXPECT(alloc_calls == 0 && init_calls == 0 && unexpected_calls == 0);
        EXPECT(g.textures == NULL && g.stats.cdram_mapped_bytes == 0);
    }
}
static void immutable(KvGxm *g, KvGxmTexture *t) {
    const SceGxmTexture descriptor = t->descriptor;
    uint8_t pixels[256]; memset(pixels,0x4d,sizeof(pixels));
    const unsigned previous_finishes = finishes, previous_allocations = alloc_calls;
    KvGxmTexture plain = {.width=t->width,.height=t->height,.pitch=t->width,
        .bpp=4,.srgb=t->srgb,.ready=1,.owner=g,.storage=KV_TEXTURE_LINEAR};
    t->next=&plain;
    EXPECT(kv_gxm_texture_update(g,t,0,0,1,1,pixels,4) < 0);
    EXPECT(kv_gxm_texture_read(g,t,pixels,sizeof(pixels)) < 0);
    EXPECT(kv_gxm_copy_texture(g,t,&plain) < 0);
    EXPECT(kv_gxm_copy_texture(g,&plain,t) < 0);
    EXPECT(kv_gxm_movie_texture_rebind(g,t,movie_frame,t->width,t->height) < 0);
    EXPECT(finishes == previous_finishes && alloc_calls == previous_allocations);
    EXPECT(transfers == 0 && unexpected_calls == 0 && !memcmp(&descriptor,&t->descriptor,sizeof(descriptor)));
    int intact=1; for (unsigned i=0;i<sizeof(pixels);++i) if(pixels[i]!=0x4d) intact=0;
    EXPECT(intact); t->next=NULL;
}
static void valid(uint32_t w, uint32_t h, uint32_t fmt, uint32_t length, uint32_t srgb, uint32_t linear) {
    KvGxm g; reset(&g, "valid upload copies payload and zeroes complete allocation");
    setup_upload(w,h,fmt,length,srgb,linear);
    for (uint32_t i=0;i<length;++i) input[i]=(uint8_t)(i*13U+19U);
    KvGxmTexture *out = (KvGxmTexture *)(void *)movie_frame;
    const int rc=upload(&g,length,&out);
    EXPECT(rc == 0 && "valid compressed upload must succeed");
    if (rc<0) { EXPECT(out==NULL); cleanup_retained(&g); return; }
    EXPECT(out && out != (KvGxmTexture *)(void *)movie_frame);
    if (!out || out==(KvGxmTexture *)(void *)movie_frame) return;
    EXPECT(out==g.textures && out->owner==&g && out->ready);
    EXPECT(out->width==w && out->height==h && out->srgb==srgb);
    EXPECT(out->storage==KV_TEXTURE_COMPRESSED && out->pitch==0 && out->bpp==0);
    EXPECT(out->target==NULL && out->sync==NULL && out->pixels.mapping==1 && out->pixels.cdram==1);
    EXPECT(out->pixels.size==wanted_allocation && g.stats.cdram_mapped_bytes==wanted_allocation);
    EXPECT(init_calls==1 && min_calls==1 && mag_calls==1 && u_calls==1 && v_calls==1 && gamma_calls==srgb);
    EXPECT(!memcmp(out->pixels.base,input,length));
    int zero=1; for(uint32_t i=length;i<out->pixels.size;++i) if(backing[i]) { zero=0; break; }
    EXPECT(zero && "all bytes outside encoded payload must be zero");
    input[0]^=0xff; EXPECT(backing[0]!=(input[0])); /* owner must not borrow encoded input */
    if(w==8 && h==8) immutable(&g,out);
    EXPECT(kv_gxm_texture_destroy(&g,out)==0);
    EXPECT(g.textures==NULL && g.stats.cdram_mapped_bytes==0 && released==allocated && !g.closing);
    EXPECT(unmap_calls==1 && free_calls==1 && unexpected_calls==0);
}
static void failed_upload(enum Fault point, int rollback) {
    KvGxm g; reset(&g,"SDK fault/rollback retains exactly one cleanup owner");
    setup_upload(8,8,0x87000000U,64,0,1); fault=point;
    unmap_failure=rollback==1; free_failure=rollback==2;
    KvGxmTexture *out=(KvGxmTexture *)(void *)movie_frame;
    const int rc=upload(&g,64,&out);
    EXPECT(rc==(rollback==1 ? unmap_code : rollback==2 ? free_code : fault_code));
    EXPECT(out==NULL && unexpected_calls==0);
    EXPECT(alloc_calls==1);
    EXPECT(base_calls==(point>ALLOC) && map_calls==(point>BASE));
    EXPECT(init_calls==(point>MAP) && min_calls==(point>INIT));
    EXPECT(mag_calls==(point>MIN) && u_calls==(point>MAG));
    EXPECT(v_calls==(point>CLAMP_U) && gamma_calls==0);
    if (rollback) {
        EXPECT(g.closing && g.textures && !g.textures->ready);
        if(g.textures) EXPECT(g.textures->owner==&g && g.textures->pixels.uid==41);
        EXPECT(g.stats.cdram_mapped_bytes==ALIGNMENT && released==0);
        const unsigned before=alloc_calls;
        EXPECT(upload(&g,64,&out)<0 && alloc_calls==before);
        cleanup_retained(&g);
        EXPECT(g.stats.cdram_mapped_bytes==0 && released==allocated);
    } else {
        EXPECT(!g.closing && !g.textures && g.stats.cdram_mapped_bytes==0);
        EXPECT(released==allocated);
    }
}
static void budget(void) {
    static const uint32_t used[] = {(48U*1024*1024-KV_JPEG_HW_CDRAM_RESERVE),(48U*1024*1024-KV_JPEG_HW_CDRAM_RESERVE)-ALIGNMENT+1U,UINT32_MAX};
    for(unsigned i=0;i<3;++i) {
        KvGxm g; reset(&g,"rounded CDRAM admission rejects before SDK allocation");
        setup_upload(1,1,0x81000000U,32,0,0); g.stats.cdram_mapped_bytes=used[i];
        KvGxmTexture *out=(KvGxmTexture *)(void *)movie_frame;
        EXPECT(upload(&g,32,&out)==KV_BUDGET && out==NULL);
        EXPECT(!alloc_calls && !g.textures && g.stats.cdram_mapped_bytes==used[i] && !g.closing);
    }
    KvGxm g; reset(&g,"reserved budget overflow rejects before SDK allocation");
    setup_upload(1,1,0x81000000U,32,0,0); g.stats.parameter_reserved_bytes=UINT32_MAX;
    KvGxmTexture *out=NULL;
    EXPECT(upload(&g,32,&out)==KV_BUDGET && !alloc_calls && !g.textures);
    reset(&g,"exact rounded budget boundary succeeds");
    setup_upload(1,1,0x81000000U,32,0,0); g.stats.cdram_mapped_bytes=(48U*1024*1024-KV_JPEG_HW_CDRAM_RESERVE)-ALIGNMENT;
    EXPECT(upload(&g,32,&out)==0);
    if(out) { EXPECT(g.stats.cdram_mapped_bytes==(48U*1024*1024-KV_JPEG_HW_CDRAM_RESERVE)); EXPECT(kv_gxm_texture_destroy(&g,out)==0); }
    EXPECT(g.stats.cdram_mapped_bytes==(48U*1024*1024-KV_JPEG_HW_CDRAM_RESERVE)-ALIGNMENT);
}
int main(void) {
    /* Public encoded formats are literal SDK ABI values, not test-owned IDs. */
    EXPECT((uint32_t)SCE_GXM_TEXTURE_FORMAT_PVRT4BPP_ABGR==0x81000000U);
    EXPECT((uint32_t)SCE_GXM_TEXTURE_FORMAT_UBC1_ABGR==0x85000000U);
    EXPECT((uint32_t)SCE_GXM_TEXTURE_FORMAT_UBC3_ABGR==0x87000000U);
    preconditions();
    static const uint32_t formats[]={0x81000000U,0x85000000U,0x87000000U};
    for(unsigned i=0;i<3;++i) {
        const uint32_t fmt=formats[i], tiny=i==0 ? 32U : i==1 ? 8U : 16U;
        invalid(0,1,fmt,tiny,0,0); invalid(1,0,fmt,tiny,0,0);
        invalid(2049,1,fmt,tiny,0,0); invalid(1,2049,fmt,tiny,0,0);
        invalid(UINT32_MAX,UINT32_MAX,fmt,UINT32_MAX,0,0);
        invalid(1,1,fmt,0,0,0); invalid(1,1,fmt,tiny-1,0,0); invalid(1,1,fmt,tiny+1,0,0);
        invalid(960,540,fmt,i==2 ? 960U*540U : 960U*540U/2U,0,0);
        invalid(8,8,fmt,i==2 ? 64U : 32U,2,0); invalid(8,8,fmt,i==2 ? 64U : 32U,0,2);
        valid(1,1,fmt,tiny,0,0);
        invalid(8,8,fmt,i==2 ? 64U : 32U,1,1);
        valid(8,8,fmt,i==2 ? 64U : 32U,0,1);
        valid(5,3,fmt,i==0 ? 32U : i==1 ? 16U : 32U,0,1);
        valid(960,540,fmt,i==2 ? 1048576U : 524288U,0,0);
        valid(960,461,fmt,i==2 ? 524288U : 262144U,0,1);
        valid(2048,2048,fmt,i==2 ? 4194304U : 2097152U,0,0);
    }
    invalid(8,8,0,32,0,0); invalid(8,8,0x81000001U,32,0,0);
    invalid(8,8,0x82000000U,32,0,0); invalid(8,8,0xffffffffU,32,0,0);
    invalid(8,8,0x85000000U,UINT32_MAX,0,0);
    valid(1,9,0x81000000U,64,0,0);
    valid(9,1,0x85000000U,32,0,1);
    for(enum Fault point=ALLOC;point<=CLAMP_V;++point) failed_upload(point,0);
    failed_upload(BASE,2); failed_upload(MAP,2);
    failed_upload(INIT,1); failed_upload(MIN,2); failed_upload(CLAMP_V,1); failed_upload(CLAMP_V,2);
    budget();
    printf("RESULT %s: %u cases, %u checks, %u failures; real C, actual SDK headers, host stubs\n",
        failures ? "FAIL" : "PASS",cases,checks,failures);
    return failures ? 1 : 0;
}

/* Deterministic clock for legacy timing sites; no device timing claim. */
SceUInt64 sceKernelGetProcessTimeWide(void) { static SceUInt64 t; return ++t; }

int sceGxmShaderPatcherCreateFragmentProgram(SceGxmShaderPatcher *patcher, SceGxmShaderPatcherId id,
    SceGxmOutputRegisterFormat format, SceGxmMultisampleMode ms, const SceGxmBlendInfo *blend,
    const SceGxmProgram *vs, SceGxmFragmentProgram **out) {
    (void)patcher; (void)id; (void)format; (void)ms; (void)blend; (void)vs; (void)out;
    return unexpected(); /* Rejected compressed operations must never reach this. */
}
