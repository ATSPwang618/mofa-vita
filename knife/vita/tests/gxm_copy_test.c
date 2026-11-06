/* Exercise the actual backend with SDK declarations and captured SDK calls.
 * This proves submission/lifetime/error contracts; the separate Vita3K pixel
 * oracle checks pixels and alpha. A CPU transfer cannot stand in for an RT. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
/* Use the production runtime-query boundary with host-owned program fixtures,
 * so the actual optional startup loop can observe ID29. */
#define __vita__ 1
#include KV_GXM_BACKEND_SOURCE
#undef __vita__
static const SceGxmProgram *host_runtime_programs[128];
int kv_runtime_shader_initialize(void) { return 0; }
const SceGxmProgram *kv_runtime_shader_program(uint32_t shader) {
    return shader < 128 ? host_runtime_programs[shader] : NULL;
}
int kv_runtime_shader_available(uint32_t shader) { return kv_runtime_shader_program(shader) != NULL; }
void kv_runtime_shader_shutdown(void) { memset(host_runtime_programs, 0, sizeof(host_runtime_programs)); }
int sceGxmProgramCheck(const SceGxmProgram *program) { (void)program; return 0; }
int sceGxmShaderPatcherCreate(const SceGxmShaderPatcherParams *params, SceGxmShaderPatcher **out) {
    (void)params; (void)out; abort(); /* Existing initialized fixtures must be reused. */
}
int sceGxmTextureValidate(const SceGxmTexture *t) { (void)t; return 0; }
int sceGxmTextureSetData(SceGxmTexture *t, const void *data) { (void)t; (void)data; return 0; }
/* Direct draw rejection must stop before any uniform reservation or upload. */
unsigned int sceGxmProgramGetDefaultUniformBufferSize(const SceGxmProgram *p) { (void)p; abort(); }
int sceGxmReserveFragmentDefaultUniformBuffer(SceGxmContext *c, void **out) { (void)c; (void)out; abort(); }
int sceGxmSetUniformDataF(void *b, const SceGxmProgramParameter *p, unsigned int offset,
    unsigned int count, const float *data) {
    (void)b; (void)p; (void)offset; (void)count; (void)data; abort();
}

static volatile unsigned int host_completion[BATCH_SLOTS];
volatile unsigned int *sceGxmGetNotificationRegion(void) { return host_completion; }
int sceGxmNotificationWait(const SceGxmNotification *n) { *n->address = n->value; return 0; }

static unsigned audited_finishes;
static unsigned checks, failures, cases, begins, ends, draws, transfers;
static unsigned creates, sync_creates, destroys, sync_destroys, surface_inits, gammas;
static int injected_stage, injected_status;
static SceGxmShaderPatcherId patch_failure_id;
static SceGxmShaderPatcherId unregister_failure_id;
static const SceGxmProgram *invalid_sampler_program;
static SceGxmOutputRegisterSize surface_registers;
static KvGxm *owner;
static KvGxmTexture *source_texture, *destination_texture;
static Vertex vertex_storage[4];
static uint16_t index_storage[6];
static int context_token, target_token, sync_token, vertex_token;
typedef struct TestProgram {
    SceGxmOutputRegisterFormat format;
    int blended, released;
    SceGxmBlendInfo blend_info;
} TestProgram;
static TestProgram programs[512];
static unsigned programs_created, programs_released;
static void *mapped_memory;
static unsigned mapped_bytes, memory_frees;
static int creating_texture;
static int deny_offscreen_sync;
static unsigned call_order, last_finish, last_transfer_finish, last_destroy;
static unsigned target_queries, expected_scenes = 1, reject_candidate_capacity;
static unsigned fail_target_query, target_query_bytes = 4096;
static unsigned tiled_texture_inits;
static int tiled_init_status;
static unsigned reject_candidate_surface;
static int tiled_readback_fixture;
static int tiled_probe_fixture, tiled_probe_corrupt;
static void *probe_memory[4];
static unsigned probe_memory_bytes[4];
static int probe_memory_index(const void *p) {
    for(unsigned i=0;i<4;++i) if(probe_memory[i]==p) return (int)i;
    return -1;
}

static TestProgram *program_info(const SceGxmFragmentProgram *p) {
    for (unsigned i = 0; i < programs_created; ++i)
        if (p == (const SceGxmFragmentProgram *)(const void *)&programs[i]) return &programs[i];
    return NULL;
}

static void expect(int condition, const char *expression, int line) {
    ++checks;
    if (condition) return;
    ++failures;
    fprintf(stderr, "FAIL case %u line %d: %s\n", cases, line, expression);
}
#define EXPECT(value) expect(!!(value), #value, __LINE__)
static int result(int stage) { return injected_stage == stage ? injected_status : 0; }
SceUID sceKernelGetThreadId(void) { return 7; }
void sceGxmFinish(SceGxmContext *context) {
    ++audited_finishes;
    EXPECT(context == owner->context); last_finish=++call_order;
}
int sceGxmTransferFinish(void) { last_transfer_finish=++call_order; return result(8); }
int sceGxmGetRenderTargetMemSize(const SceGxmRenderTargetParams *p, unsigned int *bytes) {
    ++target_queries;
    EXPECT(p->scenesPerFrame == 1 || p->scenesPerFrame == 2 || p->scenesPerFrame == 4);
    *bytes = target_query_bytes * p->scenesPerFrame;
    return fail_target_query && p->scenesPerFrame > 1 ? -75 : 0;
}
int sceGxmCreateRenderTarget(const SceGxmRenderTargetParams *p, SceGxmRenderTarget **out) {
    ++creates;
    if (creating_texture) destination_texture=owner->textures;
    EXPECT(p->width == destination_texture->width && p->height == destination_texture->height);
    EXPECT((p->scenesPerFrame == expected_scenes || p->scenesPerFrame == 1)
        && p->multisampleMode == SCE_GXM_MULTISAMPLE_NONE);
    if (reject_candidate_capacity && p->scenesPerFrame > 1) return -76;
    if (!result(1)) *out = (SceGxmRenderTarget *)(void *)&target_token;
    return result(1);
}
int sceGxmSyncObjectCreate(SceGxmSyncObject **out) {
    ++sync_creates;
    if (deny_offscreen_sync) return (int)SCE_GXM_ERROR_DRIVER;
    if (!result(2)) *out = (SceGxmSyncObject *)(void *)&sync_token;
    return result(2);
}
int sceGxmDestroyRenderTarget(SceGxmRenderTarget *p) {
    if(tiled_probe_fixture) EXPECT(p==(SceGxmRenderTarget *)(void *)&target_token);
    else EXPECT(p == destination_texture->target || p == owner->target);
    ++destroys; last_destroy=++call_order; return result(9);
}
int sceGxmSyncObjectDestroy(SceGxmSyncObject *p) {
    EXPECT(p == destination_texture->sync); ++sync_destroys; return 0;
}
int sceGxmColorSurfaceInit(SceGxmColorSurface *surface, SceGxmColorFormat format,
    SceGxmColorSurfaceType type, SceGxmColorSurfaceScaleMode scale,
    SceGxmOutputRegisterSize registers, unsigned width, unsigned height,
    unsigned stride, void *data) {
    ++surface_inits;
    EXPECT(surface == &destination_texture->surface && format == SCE_GXM_COLOR_FORMAT_A8B8G8R8);
    EXPECT(type == (destination_texture->storage == KV_TEXTURE_TILED
        ? SCE_GXM_COLOR_SURFACE_TILED : SCE_GXM_COLOR_SURFACE_LINEAR)
        && scale == SCE_GXM_COLOR_SURFACE_SCALE_NONE);
    surface_registers = registers;
    EXPECT(registers == SCE_GXM_OUTPUT_REGISTER_SIZE_32BIT);
    EXPECT(width == destination_texture->width && height == destination_texture->height);
    EXPECT(stride == destination_texture->pitch && data == destination_texture->pixels.base);
    if(reject_candidate_surface && destination_texture->target_scenes>1) return -78;
    return result(3);
}
int sceGxmColorSurfaceSetGammaMode(SceGxmColorSurface *surface, SceGxmColorSurfaceGammaMode gamma) {
    ++gammas;
    EXPECT(surface == &destination_texture->surface && gamma == SCE_GXM_COLOR_SURFACE_GAMMA_BGR);
    /* SceGxm's native format table permits RGBA8 gamma only with 64-bit
       output registers. Vita3K does not enforce this firmware constraint. */
    if (surface_registers != SCE_GXM_OUTPUT_REGISTER_SIZE_64BIT)
        return (int)SCE_GXM_ERROR_INVALID_VALUE;
    return result(4);
}
int sceGxmBeginScene(SceGxmContext *context, unsigned flags,
    const SceGxmRenderTarget *target, const SceGxmValidRegion *region,
    SceGxmSyncObject *vertex_sync, SceGxmSyncObject *fragment_sync,
    const SceGxmColorSurface *surface, const SceGxmDepthStencilSurface *depth) {
    ++begins;
    if(tiled_probe_fixture) for(KvGxmTexture *t=owner->textures;t;t=t->next)
        if(surface==&t->surface) { destination_texture=t; break; }
    EXPECT(context == owner->context && flags == 0 && region == NULL && vertex_sync == NULL);
    EXPECT(target == destination_texture->target && fragment_sync == destination_texture->sync);
    EXPECT(surface == &destination_texture->surface && depth == &owner->depth);
    return result(5);
}
int sceGxmEndScene(SceGxmContext *context, const SceGxmNotification *vs, const SceGxmNotification *fs) {
    ++ends; EXPECT(context == owner->context && vs == NULL && fs == NULL); return result(7);
}
void sceGxmSetViewport(SceGxmContext *context, float x, float sx, float y, float sy, float z, float sz) {
    EXPECT(context == owner->context);
    EXPECT(x - sx == 0 && x + sx == destination_texture->width);
    EXPECT(y + sy == 0 && y - sy == destination_texture->height);
    EXPECT(z - sz == 0 && z + sz == 1);
}
void sceGxmSetCullMode(SceGxmContext *context, SceGxmCullMode mode) {
    EXPECT(context == owner->context && mode == SCE_GXM_CULL_NONE);
}
void sceGxmSetFrontDepthFunc(SceGxmContext *context, SceGxmDepthFunc f) {
    EXPECT(context == owner->context && f == SCE_GXM_DEPTH_FUNC_ALWAYS);
}
void sceGxmSetBackDepthFunc(SceGxmContext *context, SceGxmDepthFunc f) {
    EXPECT(context == owner->context && f == SCE_GXM_DEPTH_FUNC_ALWAYS);
}
void sceGxmSetFrontDepthWriteEnable(SceGxmContext *context, SceGxmDepthWriteMode mode) {
    EXPECT(context == owner->context && mode == SCE_GXM_DEPTH_WRITE_DISABLED);
}
void sceGxmSetBackDepthWriteEnable(SceGxmContext *context, SceGxmDepthWriteMode mode) {
    EXPECT(context == owner->context && mode == SCE_GXM_DEPTH_WRITE_DISABLED);
}
void sceGxmSetVertexProgram(SceGxmContext *context, const SceGxmVertexProgram *p) {
    EXPECT(context == owner->context && p == owner->vertex_program);
}
void sceGxmSetFragmentProgram(SceGxmContext *context, const SceGxmFragmentProgram *p) {
    TestProgram *program = program_info(p);
    EXPECT(context == owner->context && program && !program->released);
    if (program) {
        EXPECT(program->format == SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4);
        EXPECT(!program->blended); /* Copy preserves all four channels. */
    }
}
int sceGxmShaderPatcherCreateFragmentProgram(SceGxmShaderPatcher *patcher,
    SceGxmShaderPatcherId id, SceGxmOutputRegisterFormat format,
    SceGxmMultisampleMode ms, const SceGxmBlendInfo *blend,
    const SceGxmProgram *vertex, SceGxmFragmentProgram **out) {
    EXPECT(patcher == (owner->effects ? owner->effects->patcher : owner->patcher));
    EXPECT(ms == SCE_GXM_MULTISAMPLE_NONE && out);
    EXPECT(vertex == (const SceGxmProgram *)(const void *)(owner->effects ?
        knife_effects_vs_main_gxp : knife_quad_v_gxp));
    EXPECT(format == SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4);
    if (result(10) && (!patch_failure_id || id == patch_failure_id)) return result(10);
    EXPECT(programs_created < sizeof(programs) / sizeof(programs[0]));
    if (programs_created >= sizeof(programs) / sizeof(programs[0])) return -99;
    TestProgram *p = &programs[programs_created++];
    *p = (TestProgram){ .format=format, .blended=blend != NULL };
    if (blend) p->blend_info = *blend;
    *out = (SceGxmFragmentProgram *)(void *)p;
    return 0;
}
int sceGxmShaderPatcherReleaseFragmentProgram(SceGxmShaderPatcher *patcher,
    SceGxmFragmentProgram *p) {
    (void)patcher;
    TestProgram *program = program_info(p);
    EXPECT(program && !program->released);
    if (program) program->released = 1;
    ++programs_released;
    return 0;
}
int sceGxmShaderPatcherReleaseVertexProgram(SceGxmShaderPatcher *p, SceGxmVertexProgram *v) {
    (void)p; (void)v; return 0;
}
int sceGxmShaderPatcherUnregisterProgram(SceGxmShaderPatcher *p, SceGxmShaderPatcherId id) {
    (void)p; return id == unregister_failure_id ? -74 : 0;
}
int sceGxmShaderPatcherRegisterProgram(SceGxmShaderPatcher *p, const SceGxmProgram *program,
    SceGxmShaderPatcherId *id) {
    EXPECT(p == owner->effects->patcher && program && id);
    *id = (SceGxmShaderPatcherId)(void *)program;
    return 0;
}
int sceGxmShaderPatcherDestroy(SceGxmShaderPatcher *p) { (void)p; return 0; }
SceUID sceKernelAllocMemBlock(const char *name, SceKernelMemBlockType type,
    SceSize size, SceKernelAllocMemBlockOpt *opt) {
    EXPECT((tiled_probe_fixture || !mapped_memory) && !strcmp(name,"knife-gxm"));
    EXPECT((type == SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW
        || type == SCE_KERNEL_MEMBLOCK_TYPE_USER_RW_UNCACHE) && opt == NULL);
    mapped_memory=malloc(size); mapped_bytes=size;
    EXPECT(mapped_memory != NULL);
    if(tiled_probe_fixture) {
        for(unsigned i=0;i<4;++i) if(!probe_memory[i]) {
            probe_memory[i]=mapped_memory; probe_memory_bytes[i]=size;
            return mapped_memory?(SceUID)(42+i):-99;
        }
        EXPECT(0); free(mapped_memory); return -99;
    }
    return mapped_memory ? 42 : -99;
}
int sceKernelGetMemBlockBase(SceUID id, void **base) {
    if(tiled_probe_fixture) {
        EXPECT(id>=42 && id<46 && probe_memory[id-42]);
        *base=probe_memory[id-42]; return 0;
    }
    EXPECT(id == 42 && mapped_memory); *base=mapped_memory; return 0;
}
int sceGxmMapMemory(void *p, SceSize size, SceGxmMemoryAttribFlags attr) {
    if(tiled_probe_fixture) {
        const int i=probe_memory_index(p);
        EXPECT(i>=0 && size==probe_memory_bytes[i]);
    } else EXPECT(p == mapped_memory && size == mapped_bytes);
    EXPECT(attr == (SCE_GXM_MEMORY_ATTRIB_READ | SCE_GXM_MEMORY_ATTRIB_WRITE)); return 0;
}
int sceGxmMapVertexUsseMemory(void *p, SceSize size, unsigned *offset) {
    (void)p; (void)size; (void)offset; EXPECT(0); return -99;
}
int sceGxmMapFragmentUsseMemory(void *p, SceSize size, unsigned *offset) {
    (void)p; (void)size; (void)offset; EXPECT(0); return -99;
}
int sceGxmUnmapMemory(void *p) {
    EXPECT(tiled_probe_fixture?probe_memory_index(p)>=0:p==mapped_memory); return 0;
}
int sceGxmUnmapVertexUsseMemory(void *p) { (void)p; EXPECT(0); return -99; }
int sceGxmUnmapFragmentUsseMemory(void *p) { (void)p; EXPECT(0); return -99; }
int sceKernelFreeMemBlock(SceUID id) {
    if(tiled_probe_fixture) {
        EXPECT(id>=42 && id<46 && probe_memory[id-42]);
        free(probe_memory[id-42]); probe_memory[id-42]=NULL; ++memory_frees;
        mapped_memory=NULL; mapped_bytes=0;
        for(unsigned i=0;i<4;++i) if(probe_memory[i]) {
            mapped_memory=probe_memory[i]; mapped_bytes=probe_memory_bytes[i];
        }
        return 0;
    }
    EXPECT(id == 42 && mapped_memory);
    free(mapped_memory); mapped_memory=NULL; ++memory_frees; return 0;
}
int sceGxmTextureInitLinear(SceGxmTexture *texture, const void *data,
    SceGxmTextureFormat format, unsigned width, unsigned height, unsigned mips) {
    EXPECT(texture == &owner->textures->descriptor && data == mapped_memory);
    EXPECT(format == SCE_GXM_TEXTURE_FORMAT_A8B8G8R8
        && width == owner->textures->width && height == owner->textures->height && mips == 0);
    return 0;
}
int sceGxmTextureInitTiled(SceGxmTexture *texture,const void *data,
    SceGxmTextureFormat format,unsigned width,unsigned height,unsigned mips) {
    ++tiled_texture_inits;
    EXPECT(texture == &owner->textures->descriptor && data == mapped_memory);
    EXPECT(format == SCE_GXM_TEXTURE_FORMAT_A8B8G8R8 && width == 960 && height == 540 && mips == 0);
    EXPECT(mapped_bytes >= 960 * 544 * 4);
    return tiled_init_status;
}
int sceGxmTextureSetMinFilter(SceGxmTexture *t, SceGxmTextureFilter filter) {
    EXPECT(t == &owner->textures->descriptor && (filter == SCE_GXM_TEXTURE_FILTER_LINEAR
        || filter == SCE_GXM_TEXTURE_FILTER_POINT)); return 0;
}
int sceGxmTextureSetMagFilter(SceGxmTexture *t, SceGxmTextureFilter filter) {
    EXPECT(t == &owner->textures->descriptor && (filter == SCE_GXM_TEXTURE_FILTER_LINEAR
        || filter == SCE_GXM_TEXTURE_FILTER_POINT)); return 0;
}
int sceGxmTextureSetUAddrMode(SceGxmTexture *t, SceGxmTextureAddrMode mode) {
    EXPECT(t == &owner->textures->descriptor && mode == SCE_GXM_TEXTURE_ADDR_CLAMP); return 0;
}
int sceGxmTextureSetVAddrMode(SceGxmTexture *t, SceGxmTextureAddrMode mode) {
    EXPECT(t == &owner->textures->descriptor && mode == SCE_GXM_TEXTURE_ADDR_CLAMP); return 0;
}
int sceGxmTextureSetGammaMode(SceGxmTexture *t, SceGxmTextureGammaMode mode) {
    EXPECT(t == &owner->textures->descriptor && mode == SCE_GXM_TEXTURE_GAMMA_BGR); return 0;
}
int sceGxmSetFragmentTexture(SceGxmContext *context, unsigned unit, const SceGxmTexture *p) {
    if(tiled_probe_fixture) for(KvGxmTexture *t=owner->textures;t;t=t->next)
        if(p==&t->descriptor) { source_texture=t; break; }
    EXPECT(context == owner->context && unit == 0 && p == &source_texture->descriptor); return 0;
}
int sceGxmSetVertexStream(SceGxmContext *context, unsigned stream, const void *data) {
    EXPECT(context == owner->context && stream == 0 && data == vertex_storage); return 0;
}
int sceGxmDraw(SceGxmContext *context, SceGxmPrimitiveType primitive,
    SceGxmIndexFormat format, const void *indices, unsigned count) {
    ++draws;
    EXPECT(context == owner->context && primitive == SCE_GXM_PRIMITIVE_TRIANGLES);
    EXPECT(format == SCE_GXM_INDEX_FORMAT_U16 && indices == index_storage && count == 6);
    for (unsigned i = 0; i < 4; ++i) {
        EXPECT(vertex_storage[i].position[0] == (i & 1 ? 1.0f : -1.0f));
        EXPECT(vertex_storage[i].position[1] == (i & 2 ? -1.0f : 1.0f));
        EXPECT(vertex_storage[i].uv[0] == (i & 1 ? 1.0f : 0.0f));
        EXPECT(vertex_storage[i].uv[1] == (i & 2 ? 1.0f : 0.0f));
        for (unsigned c = 0; c < 4; ++c) EXPECT(vertex_storage[i].color[c] == 1.0f);
    }
    if (tiled_readback_fixture && !result(6)) {
        /* GPU output fixture deliberately differs from the source backing.
           The real backend must sample into linear staging before CPU read. */
        EXPECT(source_texture->storage == KV_TEXTURE_TILED);
        EXPECT(destination_texture->storage == KV_TEXTURE_LINEAR);
        for(unsigned p=0;p<destination_texture->width*destination_texture->height*4u;++p)
            ((uint8_t *)destination_texture->pixels.base)[p]=(uint8_t[]){23,87,197,33}[p%4];
    }
    if(tiled_probe_fixture && !result(6)) {
        EXPECT(source_texture->pixels.base!=destination_texture->pixels.base);
        memcpy(destination_texture->pixels.base,source_texture->pixels.base,960*540*4u);
        if(tiled_probe_corrupt && source_texture->storage==KV_TEXTURE_TILED) {
            const unsigned offset=tiled_probe_corrupt==1?(32*960+32)*4u:(539*960+959)*4u+3u;
            ((uint8_t *)destination_texture->pixels.base)[offset]^=1;
        }
    }
    return result(6);
}
int sceGxmTransferCopy(uint32_t width, uint32_t height, uint32_t key, uint32_t mask,
    SceGxmTransferColorKeyMode key_mode, SceGxmTransferFormat sf, SceGxmTransferType st,
    const void *src, uint32_t sx, uint32_t sy, int32_t ss, SceGxmTransferFormat df,
    SceGxmTransferType dt, void *dst, uint32_t dx, uint32_t dy, int32_t ds,
    SceGxmSyncObject *sync, uint32_t flags, const SceGxmNotification *notification) {
    ++transfers;
    EXPECT(width == source_texture->width && height == source_texture->height);
    EXPECT(key == 0 && mask == 0 && key_mode == SCE_GXM_TRANSFER_COLORKEY_NONE);
    const SceGxmTransferFormat expected = source_texture->bpp == 1 ?
        SCE_GXM_TRANSFER_FORMAT_U8_R : SCE_GXM_TRANSFER_FORMAT_U8U8U8U8_ABGR;
    EXPECT(sf == expected && df == expected && st == SCE_GXM_TRANSFER_LINEAR && dt == st);
    EXPECT(src == source_texture->pixels.base && dst == destination_texture->pixels.base);
    EXPECT(ss == (int32_t)(source_texture->pitch * source_texture->bpp));
    EXPECT(ds == (int32_t)(destination_texture->pitch * destination_texture->bpp));
    EXPECT(sx == 0 && sy == 0 && dx == 0 && dy == 0 && sync == NULL && flags == 0 && notification == NULL);
    return 0;
}
static void run(unsigned width, unsigned height, int rt, int srgb, int existing, int stage) {
    KvGxmTexture destination = {.width=width,.height=height,.pitch=(width+7)&~7U,
        .bpp=rt == 2 ? 1U : 4U,.srgb=srgb,.ready=1};
    KvGxmTexture source = destination;
    KvGxm g = {.thread=7,.textures=&source,.quads=MAX_QUADS};
    g.context = (SceGxmContext *)(void *)&context_token;
    g.vertex_program = (SceGxmVertexProgram *)(void *)&vertex_token;
    g.vertices.base = vertex_storage; g.indices.base = index_storage;
    source.next = &destination;
    source.pixels.base = &source; destination.pixels.base = &destination;
    if (rt == 1) source.target = (SceGxmRenderTarget *)(void *)&source;
    if (rt == 3) source.rendered = 1; /* Baked pixels after attachment release. */
    if (rt == 4) source.storage = KV_TEXTURE_TILED;
    if (rt == 5) destination.storage = KV_TEXTURE_TILED;
    if (existing) destination.target=(SceGxmRenderTarget *)(void *)&target_token;
    owner=&g; source_texture=&source; destination_texture=&destination;
    begins=ends=draws=transfers=creates=sync_creates=destroys=sync_destroys=surface_inits=gammas=0;
    injected_stage=stage; injected_status=-73;
    programs_created=programs_released=0;
    ++cases; const unsigned before=failures;
    int rc=kv_gxm_copy_texture(&g,&source,&destination);
    EXPECT(rc == (stage ? -73 : 0));
    if (rt != 1 && rt != 3 && rt != 4 && rt != 5) {
        EXPECT(transfers == 1 && begins == 0 && ends == 0 && draws == 0 && creates == 0);
    } else {
        EXPECT(transfers == 0);
        if (!stage || stage >= 6) {
            EXPECT(begins == 1 && ends == 1 && draws == 1);
            /* EndScene failure leaves ownership uncertain. Retain the scene
               and target for destroy instead of claiming it was closed. */
            if (stage == 7) EXPECT(g.scene_open && g.current_target == &destination);
            else EXPECT(!g.scene_open && g.current_target == NULL);
            EXPECT(destination.rendered == (stage != 7));
            EXPECT(g.quads == (stage == 6 ? 0U : 1U));
        } else {
            EXPECT(begins == (stage == 5 ? 1U : 0U) && ends == 0 && draws == 0);
            EXPECT(!g.scene_open && g.current_target == NULL);
        }
        if (!stage) {
            EXPECT(creates == (existing ? 0U : 1U));
            EXPECT(sync_creates == 0);
            EXPECT(surface_inits == (existing ? 0U : 1U));
            EXPECT(gammas == 0);
            EXPECT(g.stats.draws == 1);
        }
        if (stage >= 1 && stage <= 4) {
            EXPECT(destination.target == NULL && destination.sync == NULL);
            EXPECT(destroys == (stage > 1 ? 1U : 0U));
            EXPECT(sync_destroys == 0);
        }
        if (stage != 7) EXPECT(!g.closing);
        else EXPECT(g.closing);
    }
    printf("%s %ux%u kind=%d srgb=%d existing=%d failure_stage=%d\n",
        failures == before ? "PASS" : "FAIL",width,height,rt,srgb,existing,stage);
}
static void output_programs(void) {
    KvGxmTexture target = {.srgb=1};
    KvGxm g = {.current_target=&target};
    owner=&g; destination_texture=&target;
    programs_created=programs_released=0; injected_stage=0;
    ++cases; const unsigned before=failures;
    /* A CPU texture with gamma does not set the render destination ABI.
       Alternate offscreen, display and linear targets; each variant is reused. */
    for (unsigned pass=0;pass<3;++pass) {
        for (unsigned mode=0;mode<3;++mode) {
            target.srgb=mode == 0;
            g.current_target=mode == 1 ? NULL : &target;
            for (unsigned alpha=0;alpha<2;++alpha) {
                SceGxmFragmentProgram *p=NULL;
                EXPECT(quad_program(&g,alpha,&p) == 0);
                TestProgram *info=program_info(p);
                EXPECT(info && info->blended == (int)alpha);
                if (info) EXPECT(info->format == SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4);
            }
        }
    }
    EXPECT(programs_created == 2);
    /* Failure must propagate without caching an invalid program. */
    g.alpha_program=NULL; target.srgb=1; g.current_target=&target;
    injected_stage=10; SceGxmFragmentProgram *p=NULL;
    EXPECT(quad_program(&g,1,&p) == -73 && !g.alpha_program && !p);
    injected_stage=0;
    EXPECT(quad_program(&g,1,&p) == 0 && p);
    printf("%s quad output variants and destination switches\n",failures==before?"PASS":"FAIL");

    g.effects=calloc(1,sizeof(*g.effects));
    EXPECT(g.effects != NULL);
    if (!g.effects) return;
    for (unsigned shader=0;shader<EFFECT_SHADERS;++shader)
        if (legacy_effect_source(shader)) g.effects->ids[shader]=(SceGxmShaderPatcherId)(void *)&target;
    programs_created=programs_released=0;
    ++cases;
    for (unsigned pass=0;pass<2;++pass) {
        for (unsigned mode=0;mode<2;++mode) {
            target.srgb=mode; g.current_target=mode ? &target : NULL;
            for (unsigned shader=0;shader<EFFECT_SHADERS;++shader) {
                if (!legacy_effect_source(shader)) continue;
                for (unsigned blend=0;blend<EFFECT_BLENDS;++blend) {
                    EXPECT(effects_program(&g,shader,blend) == 0);
                    TestProgram *info=program_info(g.effects->fragment[shader][blend][0]);
                    EXPECT(info && info->blended == (blend != 5));
                    if (info) EXPECT(info->format == SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4);
                    if (info && (blend == 0 || blend == 6 || blend == 9)) {
                        EXPECT(info->blend_info.colorSrc == SCE_GXM_BLEND_FACTOR_ONE);
                        EXPECT(info->blend_info.colorDst == SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA);
                        EXPECT(info->blend_info.alphaSrc == (blend == 0 ?
                            SCE_GXM_BLEND_FACTOR_ZERO : SCE_GXM_BLEND_FACTOR_ONE));
                        EXPECT(info->blend_info.alphaDst == (blend == 9 ?
                            SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA : SCE_GXM_BLEND_FACTOR_ONE));
                    }
                }
            }
        }
    }
    EXPECT(programs_created == 17 * EFFECT_BLENDS);
    ++cases;
    EXPECT(effects_text_cache_initialize(&g) == KV_STATE && !(g.shader_capabilities & (UINT64_C(1) << 24)));
    /* Model startup with an available, registered runtime 11 and no optional
       bit 11. Merely seeing a runtime pointer is insufficient if registration
       selected a fallback, and the old bundled Alter must remain ineligible. */
    static const unsigned int runtime_alter_token[16] = {0};
    const SceGxmProgram *runtime_alter = (const SceGxmProgram *)(const void *)runtime_alter_token;
    EXPECT(!(g.shader_capabilities & (UINT64_C(1) << 11)));
    EXPECT(effects_text_cache_initialize_program(&g, legacy_effect_source(11)) == KV_STATE);
    EXPECT(effects_text_cache_initialize_program(&g, runtime_alter) == KV_STATE);
    EXPECT(!(g.shader_capabilities & (UINT64_C(1) << 24)));
    EXPECT(sceGxmShaderPatcherReleaseFragmentProgram(g.effects->patcher,
        g.effects->fragment[11][0][0]) == 0);
    g.effects->fragment[11][0][0] = NULL;
    g.effects->source[11] = runtime_alter;
    EXPECT(effects_text_cache_initialize_program(&g, NULL) == KV_STATE);
    EXPECT(effects_text_cache_initialize_program(&g, runtime_alter) == 0
        && (g.shader_capabilities & (UINT64_C(1) << 24)) && !(g.shader_capabilities & (UINT64_C(1) << 11)));
    /* Simulate cache-only patch failure: ordinary text and the encoded source
       capability survive; a later successful initialization can enable it. */
    EXPECT(sceGxmShaderPatcherReleaseFragmentProgram(g.effects->patcher,
        g.effects->fragment[4][9][0]) == 0);
    g.effects->fragment[4][9][0] = NULL;
    injected_stage = 10; injected_status = -73;
    EXPECT(effects_text_cache_initialize_program(&g, runtime_alter) == -73);
    EXPECT(!(g.shader_capabilities & (UINT64_C(1) << 24)) && !(g.shader_capabilities & (UINT64_C(1) << 11)));
    EXPECT(g.effects->fragment[4][0][0] != NULL && !g.closing);
    injected_stage = 0;
    EXPECT(effects_text_cache_initialize_program(&g, runtime_alter) == 0
        && (g.shader_capabilities & (UINT64_C(1) << 24)) && !(g.shader_capabilities & (UINT64_C(1) << 11)));
    ++cases;
    /* New Darken entry has no legacy blob. Its prepatch failure disables only
       ID25; generic17 remains available for renderer-selected fallback. */
    EXPECT(effect_fetch_shader(25) && effect_ps_shader(25));
    EXPECT(!legacy_effect_source(25) && !legacy_effect_source(24));
    g.effects->source[25] = runtime_alter;
    g.effects->ids[25] = (SceGxmShaderPatcherId)(void *)&target;
    g.shader_capabilities |= (UINT64_C(1) << 25) | (UINT64_C(1) << 17);
    g.thread = 7;
    EXPECT(kv_gxm_shader_available(&g, 25) == 1 && kv_gxm_shader_available(&g, 24) == 1);
    injected_stage = 10; injected_status = -73;
    EXPECT(effects_program_mode(&g, 25, 5, 0) == -73 && !g.effects->fragment[25][5][0]);
    EXPECT(disable_optional_shader(&g, 25) == 0);
    EXPECT(!(g.shader_capabilities & (UINT64_C(1) << 25)) && (g.shader_capabilities & (UINT64_C(1) << 17)));
    EXPECT((g.shader_capabilities & (UINT64_C(1) << 24)) && !g.closing);
    injected_stage = 0;
    g.effects->source[25] = runtime_alter;
    g.effects->ids[25] = (SceGxmShaderPatcherId)(void *)&target;
    EXPECT(effects_program_mode(&g, 25, 5, 0) == 0);
    TestProgram *darken = program_info(g.effects->fragment[25][5][0]);
    EXPECT(darken && !darken->blended && darken->format == SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4);
    KvEffectVertex darken_vertices[6] = {0};
    float darken_params[8] = {2, 0, 960, 540, -1, 0, 0, 0};
    EXPECT(effect_inputs(darken_vertices, 6, 25, 5, darken_params) == 0);
    EXPECT(effect_inputs(darken_vertices, 6, 24, 5, darken_params) == KV_BAD_ARGUMENT);
    EXPECT(effect_inputs(darken_vertices, 6, 25, 0, darken_params) == KV_BAD_ARGUMENT);
    for (unsigned p = 0; p < 8; ++p) {
        if (p == 2 || p == 3 || p == 5) continue;
        const float old = darken_params[p];
        darken_params[p] = p == 0 ? 19 : p == 4 ? 0 : 1;
        EXPECT(effect_inputs(darken_vertices, 6, 25, 5, darken_params) == KV_BAD_ARGUMENT);
        darken_params[p] = old;
    }
    for (unsigned row = 0; row < 5; ++row) for (unsigned column = 0; column < 4; ++column) {
        float *value = row < 2 ? &darken_vertices[0].haze[row][column]
            : &darken_vertices[0].soft[row - 2][column];
        *value = 0.1f;
        EXPECT(effect_inputs(darken_vertices, 6, 25, 5, darken_params) == KV_BAD_ARGUMENT);
        *value = 0;
    }
    ++cases;
    /* Run the actual optional initializer: omitting 29 from its startup list
       must fail this test even though a manually invoked disable would work. */
    static const unsigned int runtime_ps7_token[16] = {0};
    const SceGxmProgram *runtime_ps7 = (const SceGxmProgram *)(const void *)runtime_ps7_token;
    EXPECT(effect_fetch_shader(29) && effect_ps_shader(29) && !legacy_effect_source(29));
    host_runtime_programs[11] = runtime_alter;
    host_runtime_programs[17] = runtime_alter;
    host_runtime_programs[25] = runtime_alter;
    host_runtime_programs[28] = runtime_alter;
    host_runtime_programs[29] = runtime_ps7;
    for (unsigned i = 0; i < 4; ++i) {
        const unsigned shader[] = {17, 25, 28, 29};
        const unsigned s = shader[i];
        g.effects->source[s] = host_runtime_programs[s];
        g.effects->ids[s] = (SceGxmShaderPatcherId)(const void *)host_runtime_programs[s];
        g.shader_capabilities |= UINT64_C(1) << s;
    }
    EXPECT(effects_program_mode(&g, 17, 5, 0) == 0);
    EXPECT(effects_program_mode(&g, 28, 5, 0) == 0);
    SceGxmFragmentProgram *saved17 = g.effects->fragment[17][5][0];
    SceGxmFragmentProgram *saved25 = g.effects->fragment[25][5][0];
    SceGxmFragmentProgram *saved28 = g.effects->fragment[28][5][0];
    EXPECT(kv_gxm_shader_available(&g, 29) == 1);
    patch_failure_id = g.effects->ids[29];
    injected_stage = 10; injected_status = -73;
    EXPECT(effects_optional_initialize(&g) == 0);
    EXPECT(kv_gxm_shader_available(&g, 29) == 0 && !g.effects->fragment[29][5][0]);
    EXPECT(kv_gxm_shader_available(&g, 17) == 1 && kv_gxm_shader_available(&g, 25) == 1);
    EXPECT(kv_gxm_shader_available(&g, 28) == 1 && kv_gxm_shader_available(&g, 24) == 1);
    EXPECT(g.effects->fragment[17][5][0] == saved17 && g.effects->fragment[25][5][0] == saved25);
    EXPECT(g.effects->fragment[28][5][0] == saved28 && !g.closing);
    injected_stage = 0; patch_failure_id = NULL;
    g.effects->source[29] = runtime_ps7;
    g.effects->ids[29] = (SceGxmShaderPatcherId)(const void *)runtime_ps7;
    g.shader_capabilities |= UINT64_C(1) << 29;
    EXPECT(effects_optional_initialize(&g) == 0);
    TestProgram *ps7 = program_info(g.effects->fragment[29][5][0]);
    EXPECT(kv_gxm_shader_available(&g, 29) == 1 && ps7 && !ps7->blended);
    if (ps7) EXPECT(ps7->format == SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4);
    for (unsigned b = 0; b < EFFECT_BLENDS; ++b)
        if (b != 5) EXPECT(!g.effects->fragment[29][b][0]);
    ++cases;
    invalid_sampler_program = runtime_ps7;
    EXPECT(effects_sampler_metadata(&g, 29, runtime_ps7) == 0);
    EXPECT(kv_gxm_shader_available(&g, 29) == 0 && !g.effects->ids[29]);
    EXPECT(!g.effects->source[29] && !g.effects->fragment[29][5][0]);
    EXPECT(g.effects->optional_status[29] == KV_BAD_ARGUMENT && !g.closing);
    EXPECT(g.effects->fragment[17][5][0] == saved17 && g.effects->fragment[25][5][0] == saved25);
    EXPECT(g.effects->fragment[28][5][0] == saved28 && kv_gxm_shader_available(&g, 24) == 1);
    /* Existing entries keep the original fatal metadata result. */
    EXPECT(effects_sampler_metadata(&g, 17, runtime_ps7) == KV_BAD_ARGUMENT);
    g.effects->source[29] = runtime_ps7;
    g.effects->ids[29] = (SceGxmShaderPatcherId)(const void *)runtime_ps7;
    g.shader_capabilities |= UINT64_C(1) << 29;
    unregister_failure_id = g.effects->ids[29];
    EXPECT(effects_sampler_metadata(&g, 29, runtime_ps7) == -74);
    EXPECT(g.effects->ids[29] == unregister_failure_id && kv_gxm_shader_available(&g, 29) == 1);
    unregister_failure_id = NULL;
    EXPECT(effects_sampler_metadata(&g, 29, runtime_ps7) == 0);
    EXPECT(!g.effects->ids[29] && kv_gxm_shader_available(&g, 29) == 0);
    invalid_sampler_program = NULL;
    /* New complete-source entries must prepatch at startup, one capability
       per program. Failing OP6 cannot close OP1/OP13 or any fallback path. */
    ++cases;
    static const unsigned int runtime_full_tokens[3][16]={{0},{0},{0}};
    for(unsigned s=54;s<=56;++s) {
        const SceGxmProgram *full=(const SceGxmProgram *)(const void *)runtime_full_tokens[s-54];
        host_runtime_programs[s]=full;
        g.effects->source[s]=full;
        g.effects->ids[s]=(SceGxmShaderPatcherId)(const void *)full;
        g.shader_capabilities |= UINT64_C(1)<<s;
        EXPECT(effect_fetch_shader(s) && effect_ps_shader(s) && !legacy_effect_source(s));
    }
    kv_render_set_options(UINT64_C(1)<<30);
    const unsigned before_disabled=programs_created;
    EXPECT(effects_optional_initialize(&g)==0);
    for(unsigned s=54;s<=56;++s) EXPECT(!g.effects->fragment[s][5][0]);
    EXPECT(programs_created==before_disabled);
    kv_render_set_options(UINT64_C(1)<<37);
    patch_failure_id=g.effects->ids[55]; injected_stage=10; injected_status=-73;
    EXPECT(effects_optional_initialize(&g)==0);
    EXPECT(kv_gxm_shader_available(&g,54)==1 && kv_gxm_shader_available(&g,55)==0
        && kv_gxm_shader_available(&g,56)==1);
    EXPECT(!g.effects->ids[55] && !g.effects->fragment[55][5][0] && !g.closing);
    EXPECT(g.effects->fragment[17][5][0]==saved17 && g.effects->fragment[25][5][0]==saved25);
    EXPECT(g.effects->fragment[28][5][0]==saved28 && kv_gxm_shader_available(&g,24)==1);
    injected_stage=0; patch_failure_id=NULL;
    g.effects->source[55]=host_runtime_programs[55];
    g.effects->ids[55]=(SceGxmShaderPatcherId)(const void *)host_runtime_programs[55];
    g.shader_capabilities |= UINT64_C(1)<<55;
    EXPECT(effects_optional_initialize(&g)==0);
    /* Direct C callers cannot bypass the experiment or startup-admission
       gate. The rejection precedes texture validation and vertex reservation. */
    const SceUID saved_thread=g.thread;
    const int saved_scene_open=g.scene_open;
    g.thread=7; g.scene_open=1;
    const unsigned before_gated_draws=draws, before_gated_begins=begins;
    const uint32_t before_gated_used=g.effects->used;
    const KvGxmTexture *gated_inputs[4]={NULL,NULL,NULL,NULL};
    const uint32_t full_ops[3]={1,6,13};
    KvEffectVertex gated_vertices[3]={0};
    for(unsigned s=54;s<=56;++s) {
        const float gated_params[8]={(float)full_ops[s-54],0,960,540,-1,0,0,0};
        kv_render_set_options(UINT64_C(1)<<30);
        EXPECT(draw_effect_inner(&g,gated_vertices,3,s,5,gated_inputs,gated_params)==KV_STATE);
        kv_render_set_options(UINT64_C(1)<<s); /* capability namespace is not options */
        EXPECT(draw_effect_inner(&g,gated_vertices,3,s,5,gated_inputs,gated_params)==KV_STATE);
        kv_render_set_options(UINT64_C(1)<<37);
        g.shader_capabilities &= ~(UINT64_C(1)<<s);
        EXPECT(draw_effect_inner(&g,gated_vertices,3,s,5,gated_inputs,gated_params)==KV_STATE);
        g.shader_capabilities |= UINT64_C(1)<<s;
    }
    EXPECT(draws==before_gated_draws && begins==before_gated_begins && g.effects->used==before_gated_used);
    g.thread=saved_thread; g.scene_open=saved_scene_open;
    for(unsigned s=54;s<=56;++s) {
        TestProgram *full=program_info(g.effects->fragment[s][5][0]);
        EXPECT(kv_gxm_shader_available(&g,s)==1 && full && !full->blended);
        if(full) EXPECT(full->format==SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4);
        for(unsigned b=0;b<EFFECT_BLENDS;++b) if(b!=5) EXPECT(!g.effects->fragment[s][b][0]);
        invalid_sampler_program=host_runtime_programs[s];
        EXPECT(effects_sampler_metadata(&g,s,host_runtime_programs[s])==0);
        EXPECT(kv_gxm_shader_available(&g,s)==0 && !g.effects->ids[s]);
        EXPECT(!g.effects->source[s] && !g.effects->fragment[s][5][0] && !g.closing);
        EXPECT(g.effects->optional_status[s]==KV_BAD_ARGUMENT);
    }
    invalid_sampler_program=NULL;
    ++cases;
    static const unsigned int half_tokens[4][16]={{0},{0},{0},{0}};
    for(unsigned s=49;s<=52;++s) {
        host_runtime_programs[s]=(const SceGxmProgram *)(const void *)half_tokens[s-49];
        g.effects->source[s]=host_runtime_programs[s];
        g.effects->ids[s]=(SceGxmShaderPatcherId)(const void *)host_runtime_programs[s];
        g.shader_capabilities|=UINT64_C(1)<<s;
    }
    kv_render_set_options(KV_OPT_HALF_COLOR);
    patch_failure_id=g.effects->ids[50]; injected_stage=10; injected_status=-73;
    EXPECT(effects_optional_initialize(&g)==0);
    EXPECT(kv_gxm_shader_available(&g,49)==1 && kv_gxm_shader_available(&g,50)==0);
    EXPECT(kv_gxm_shader_available(&g,51)==1 && kv_gxm_shader_available(&g,52)==1);
    EXPECT(g.effects->fragment[49][5][0] && !g.effects->fragment[50][5][0]);
    EXPECT(!g.effects->ids[50] && !g.closing);
    injected_stage=0; patch_failure_id=NULL;
    g.thread=7; g.scene_open=1;
    kv_render_set_options(0);
    const unsigned half_draws=draws;
    const uint32_t half_used=g.effects->used;
    for(unsigned s=49;s<=52;++s) {
        const float params[8]={(float)(unsigned[]){1,6,7,13}[s-49],0,960,540,-1,0,0,0};
        EXPECT(draw_effect_inner(&g,gated_vertices,3,s,5,gated_inputs,params)==KV_STATE);
    }
    EXPECT(draws==half_draws && g.effects->used==half_used);
    g.thread=saved_thread; g.scene_open=saved_scene_open;
    ++cases;
    static unsigned int op7_tokens[2][16];
    op7_tokens[0][15]=36;op7_tokens[1][15]=51;
    for(unsigned i=0;i<2;++i) {
        unsigned s=i?57:48;
        host_runtime_programs[s]=(const SceGxmProgram *)(const void *)op7_tokens[i];
        g.effects->source[s]=host_runtime_programs[s];
        g.effects->ids[s]=(SceGxmShaderPatcherId)(const void *)host_runtime_programs[s];
    }
    g.shader_capabilities|=UINT64_C(1)<<48;
    kv_render_set_options(KV_OPT_PS_PLAIN_OP7|KV_OPT_PS_CHAIN_OP7);
    EXPECT(effects_optional_initialize(&g)==0);
    EXPECT(g.effects->fragment[48][5][0] && g.effects->fragment[57][5][0]);
    EXPECT((g.effects->chain_admitted&(UINT32_C(1)<<16))!=0);
    g.thread=7; EXPECT(kv_gxm_chain_pair_available(&g,7,7)==1);
    EXPECT(kv_gxm_chain_pair_available(&g,7,13)==0);
    kv_render_set_options(KV_OPT_PS_PLAIN_OP7);
    EXPECT(kv_gxm_chain_pair_available(&g,7,7)==0);
    g.scene_open=1;const uint32_t op7_used=g.effects->used;
    const float chain_params[8]={7,7,960,540,-1,0,0,0};
    EXPECT(draw_effect_inner(&g,gated_vertices,3,27,5,gated_inputs,chain_params)==KV_STATE);
    kv_render_set_options(0);const float single_params[8]={7,0,960,540,-1,0,0,0};
    EXPECT(draw_effect_inner(&g,gated_vertices,3,48,5,gated_inputs,single_params)==KV_STATE);
    EXPECT(g.effects->used==op7_used);g.scene_open=saved_scene_open;
    kv_render_set_options(KV_OPT_PS_PLAIN_OP7|KV_OPT_PS_CHAIN_OP7);
    op7_tokens[1][15]=73; /* Worse than the two singles: refuse and retain singles. */
    EXPECT(effects_chain_initialize(&g)==0 && kv_gxm_chain_pair_available(&g,7,7)==0);
    EXPECT(!g.effects->ids[57] && g.effects->fragment[48][5][0] && !g.closing);
    op7_tokens[1][15]=51;g.effects->source[57]=host_runtime_programs[57];
    g.effects->ids[57]=(SceGxmShaderPatcherId)(const void *)host_runtime_programs[57];
    patch_failure_id=g.effects->ids[57];injected_stage=10;injected_status=-73;
    EXPECT(effects_chain_initialize(&g)==0 && kv_gxm_chain_pair_available(&g,7,7)==0);
    EXPECT(g.effects->fragment[48][5][0] && !g.closing);
    patch_failure_id=NULL;injected_stage=0;
    invalid_sampler_program=host_runtime_programs[48];
    EXPECT(effects_sampler_metadata(&g,48,host_runtime_programs[48])==0);
    EXPECT(!g.effects->ids[48] && !g.closing);invalid_sampler_program=NULL;g.thread=saved_thread;
    kv_render_set_options(0);
    memset(host_runtime_programs, 0, sizeof(host_runtime_programs));
    EXPECT(effects_destroy(&g) == 0 && !g.effects);
    EXPECT(programs_released == programs_created);
    printf("%s all effects output variants, cache reuse and release\n",failures==before?"PASS":"FAIL");
}
/* Enabled experiments still report not-ready when no program compiled or
   when registration rejected every candidate. This exercises the real logger. */
static void full_material_missing_logs(void) {
    KvGxm g={.thread=7}; owner=&g;
    memset(host_runtime_programs,0,sizeof(host_runtime_programs));
    kv_render_set_options(UINT64_C(1)<<37);
    ++cases;
    EXPECT(effects_optional_initialize(&g)==0);
    EXPECT(!g.effects && !g.shader_capabilities);
    g.effects=calloc(1,sizeof(*g.effects)); EXPECT(g.effects!=NULL);
    if(g.effects) {
        static const unsigned int rejected_tokens[3][16]={{0},{0},{0}};
        for(unsigned s=54;s<=56;++s) {
            host_runtime_programs[s]=(const SceGxmProgram *)(const void *)rejected_tokens[s-54];
            g.effects->optional_status[s]=-71;
        }
        ++cases;
        EXPECT(effects_optional_initialize(&g)==0);
        EXPECT(!g.shader_capabilities);
        EXPECT(effects_destroy(&g)==0 && !g.effects);
    }
    memset(host_runtime_programs,0,sizeof(host_runtime_programs));
    kv_render_set_options(0);
}
static void all_operations_high_bank(void) {
    ++cases; KvGxm g={.thread=7}; owner=&g;
    g.effects=calloc(1,sizeof(*g.effects)); EXPECT(g.effects!=NULL);
    if(!g.effects)return;
    static const unsigned int tokens[24][16]={{0}};
    for(unsigned id=64;id<=87;++id){
        const SceGxmProgram *p=(const SceGxmProgram*)(const void*)tokens[id-64];
        host_runtime_programs[id]=p;g.effects->source[id]=p;
        g.effects->ids[id]=(SceGxmShaderPatcherId)(const void*)p;
        shader_capability_set(&g,id,1);
        EXPECT(shader_capability_ready(&g,id) && g.shader_capabilities==0);
    }
    EXPECT(!shader_capability_ready(&g,128) && !shader_capability_ready(&g,UINT32_MAX));
    kv_render_set_options(0);unsigned created=programs_created;
    EXPECT(effects_optional_initialize(&g)==0 && programs_created==created);
    kv_render_set_options(UINT64_C(1)<<38);
    patch_failure_id=g.effects->ids[75];injected_stage=10;injected_status=-73;
    EXPECT(effects_optional_initialize(&g)==0);
    EXPECT(!shader_capability_ready(&g,75) && !g.effects->ids[75] && !g.closing);
    injected_stage=0;patch_failure_id=NULL;
    g.scene_open=1;
    KvEffectVertex vertices[3]={0};const KvGxmTexture *inputs[4]={NULL,NULL,NULL,NULL};
    for(unsigned id=64;id<=87;++id){
        if(id==75)continue;
        EXPECT(kv_gxm_shader_available(&g,id)==1 && g.effects->fragment[id][5][0]);
        for(unsigned blend=0;blend<EFFECT_BLENDS;++blend)if(blend!=5)EXPECT(!g.effects->fragment[id][blend][0]);
        float params[8]={(float)(id-63),0,960,540,-1,0,0,0};
        const unsigned used=g.effects->used;
        kv_render_set_options(0);EXPECT(draw_effect_inner(&g,vertices,3,id,5,inputs,params)==KV_STATE);
        kv_render_set_options(UINT64_C(1)<<38);shader_capability_set(&g,id,0);
        EXPECT(draw_effect_inner(&g,vertices,3,id,5,inputs,params)==KV_STATE && g.effects->used==used);
        shader_capability_set(&g,id,1);
    }
    g.scene_open=0;EXPECT(effects_destroy(&g)==0);
    memset(host_runtime_programs,0,sizeof(host_runtime_programs));kv_render_set_options(0);
}
static void material_variant_startup_failures(void) {
    ++cases; KvGxm g={.thread=7}; owner=&g;
    g.effects=calloc(1,sizeof(*g.effects)); EXPECT(g.effects!=NULL); if(!g.effects)return;
    static uint32_t tokens[105][16];memset(tokens,0,sizeof(tokens));
    for(unsigned id=88;id<105;++id){
        unsigned ids[2]={id,material_baseline(id)};
        for(unsigned k=0;k<2;++k){unsigned s=ids[k];tokens[s][15]=k?100:20;
            const SceGxmProgram *p=(const SceGxmProgram*)(const void*)tokens[s];
            host_runtime_programs[s]=p;g.effects->source[s]=p;
            g.effects->ids[s]=(SceGxmShaderPatcherId)(const void*)p;shader_capability_set(&g,s,1);}
    }
    tokens[96][15]=101; /* Cost rejection retains the complete base. */
    kv_render_set_options(KV_OPT_PS_MATERIAL_VARIANTS|KV_OPT_HALF_COLOR|KV_OPT_PS_PLAIN_OP7);
    patch_failure_id=g.effects->ids[90];injected_stage=10;injected_status=-73;
    EXPECT(effects_optional_initialize(&g)==0 && !g.closing);
    injected_stage=0;patch_failure_id=NULL;g.scene_open=1;
    KvEffectVertex vertices[3]={0};const KvGxmTexture *inputs[4]={NULL,NULL,NULL,NULL};
    for(unsigned id=88;id<105;++id){
        EXPECT(kv_gxm_shader_available(&g,id)==(id!=90 && id!=96));
        if(id==90 || id==96){EXPECT(!g.effects->ids[id]);continue;}
        EXPECT(g.effects->fragment[id][5][0]!=NULL);
        float params[8]={(float)material_operation(id),id<96 && id%2==0,960,540,-1,0,id<96 && id%2!=0,0};
        unsigned used=g.effects->used;
        kv_render_set_options(0);EXPECT(draw_effect_inner(&g,vertices,3,id,5,inputs,params)==KV_STATE);
        kv_render_set_options(KV_OPT_PS_MATERIAL_VARIANTS|KV_OPT_HALF_COLOR|KV_OPT_PS_PLAIN_OP7);
        shader_capability_set(&g,id,0);EXPECT(draw_effect_inner(&g,vertices,3,id,5,inputs,params)==KV_STATE);
        EXPECT(g.effects->used==used);shader_capability_set(&g,id,1);
    }
    g.scene_open=0;EXPECT(effects_destroy(&g)==0);
    memset(host_runtime_programs,0,sizeof(host_runtime_programs));kv_render_set_options(0);
}
static void startup_target(int srgb, int fail_gamma) {
    KvGxm g={.thread=7}; KvGxmTexture *target=NULL;
    owner=&g; creating_texture=1; injected_stage=fail_gamma ? 4 : 0;
    injected_status=(int)SCE_GXM_ERROR_INVALID_VALUE;
    creates=destroys=sync_creates=sync_destroys=surface_inits=gammas=memory_frees=0;
    ++cases; const unsigned before=failures;
    int rc=kv_gxm_texture_create_ex(&g,NULL,480,270,0,0,srgb,1,1,&target);
    if (srgb) {
        EXPECT(rc == KV_BAD_ARGUMENT && target == NULL && creates == 0 && gammas == 0);
        creating_texture=0; printf("PASS native target rejects gamma request\n"); return;
    }
    EXPECT(rc == (fail_gamma ? (int)SCE_GXM_ERROR_INVALID_VALUE : 0));
    EXPECT(creates == 1 && sync_creates == 0 && surface_inits == 1);
    EXPECT(gammas == (srgb ? 1U : 0U));
    /* 480*270*RGBA8 aligned to 256 KiB: register width must not double storage. */
    EXPECT(mapped_bytes == 512 * 1024);
    if (fail_gamma) {
        EXPECT(!target && !g.textures && !mapped_memory);
        EXPECT(strstr(g.error,"effects surface gamma") && strstr(g.error,"805b0003"));
    } else {
        EXPECT(target && target == g.textures && target->ready && target->bpp == 4);
        EXPECT(g.stats.cdram_mapped_bytes == 512 * 1024);
        EXPECT(texture_target_destroy(&g,target) == 0);
        EXPECT(block_free(&g,&target->pixels) == 0);
        free(target); g.textures=NULL;
    }
    EXPECT(!g.closing && g.stats.cdram_mapped_bytes == 0 && memory_frees == 1);
    EXPECT(destroys == 1 && sync_destroys == 0);
    printf("%s startup 480x270 srgb=%d gamma_failure=%d storage=512KiB\n",
        failures==before?"PASS":"FAIL",srgb,fail_gamma);
    creating_texture=0;
}
static void offscreen_sync_pressure(void) {
    /* Display owns the admitted sync objects. A finite driver budget must
       not stop a UI bake that already waits for the GPU between scenes. */
    KvGxm g={.thread=7};
    owner=&g; injected_stage=0; deny_offscreen_sync=1;
    creates=destroys=sync_creates=sync_destroys=0;
    ++cases; const unsigned before=failures;
    for (unsigned i=0;i<96;++i) {
        KvGxmTexture image={.width=80,.height=24,.pitch=80,.bpp=4,.srgb=i%2,.ready=1};
        destination_texture=&image;
        EXPECT(texture_target_ensure(&g,&image) == 0);
        EXPECT(image.target != NULL && image.sync == NULL);
        EXPECT(texture_target_destroy(&g,&image) == 0);
    }
    EXPECT(creates == 96 && destroys == 96);
    EXPECT(sync_creates == 0 && sync_destroys == 0 && !g.closing);
    printf("%s 96 UI targets with no spare driver sync objects\n",failures==before?"PASS":"FAIL");
    deny_offscreen_sync=0;
}
static void seal_baked_texture(int stage) {
    uint8_t pixels[32]; memset(pixels,0x53,sizeof(pixels));
    KvGxmTexture image={.width=2,.height=2,.pitch=8,.bpp=4,.srgb=1,.ready=1,.rendered=1};
    image.target=(SceGxmRenderTarget *)(void *)&target_token;
    image.pixels.base=pixels; image.pixels.size=sizeof(pixels);
    memset(&image.descriptor,0x17,sizeof(image.descriptor));
    const SceGxmTexture descriptor=image.descriptor;
    KvGxm g={.thread=7,.textures=&image};
    g.context=(SceGxmContext *)(void *)&context_token;
    g.stats.cdram_mapped_bytes=sizeof(pixels);
    owner=&g; destination_texture=&image; injected_stage=stage; injected_status=-73;
    call_order=last_finish=last_transfer_finish=last_destroy=0;
    destroys=sync_destroys=memory_frees=0;
    ++cases; const unsigned before=failures;
    EXPECT(kv_gxm_texture_seal(&g,&image) == (stage ? -73 : 0));
    EXPECT(last_finish && last_finish < last_transfer_finish);
    EXPECT(destroys == (stage == 8 ? 0U : 1U));
    if (destroys) EXPECT(last_transfer_finish < last_destroy);
    EXPECT(g.closing == (stage != 0));
    EXPECT(image.ready && image.rendered && image.pixels.base == pixels);
    EXPECT(!memcmp(&descriptor,&image.descriptor,sizeof(descriptor)));
    EXPECT(g.stats.cdram_mapped_bytes == sizeof(pixels));
    EXPECT(memory_frees == 0 && sync_destroys == 0);
    for (unsigned i=0;i<sizeof(pixels);++i) EXPECT(pixels[i] == 0x53);
    if (stage) {
        EXPECT(image.target != NULL && g.textures == &image);
        EXPECT(strstr(g.error,"sealing baked texture retained owner") != NULL);
    } else {
        EXPECT(image.target == NULL && image.sync == NULL);
        EXPECT(kv_gxm_texture_seal(&g,&image) == 0 && destroys == 1);
    }
    printf("%s baked texture attachment release failure_stage=%d\n",failures==before?"PASS":"FAIL",stage);
}
static void seal_invalid(void) {
    KvGxmTexture image={.ready=1,.bpp=4};
    KvGxmTexture foreign={.ready=1,.bpp=4};
    KvGxm g={.thread=7,.textures=&image};
    owner=&g; destination_texture=&image; injected_stage=0;
    call_order=0; destroys=memory_frees=0;
    ++cases; const unsigned before=failures;
    EXPECT(kv_gxm_texture_seal(&g,NULL) == KV_STATE);
    EXPECT(kv_gxm_texture_seal(&g,&foreign) == KV_STATE);
    g.scene_open=1; EXPECT(kv_gxm_texture_seal(&g,&image) == KV_STATE); g.scene_open=0;
    g.white=&image; EXPECT(kv_gxm_texture_seal(&g,&image) == KV_STATE); g.white=NULL;
    image.storage=KV_TEXTURE_COMPRESSED; EXPECT(kv_gxm_texture_seal(&g,&image) == KV_STATE);
    image.storage=KV_TEXTURE_MOVIE; EXPECT(kv_gxm_texture_seal(&g,&image) == KV_STATE);
    image.storage=KV_TEXTURE_LINEAR; image.bpp=1; EXPECT(kv_gxm_texture_seal(&g,&image) == KV_STATE);
    EXPECT(!g.closing && !call_order && !destroys && !memory_frees);
    printf("%s attachment release rejects active, foreign and unsupported textures\n",failures==before?"PASS":"FAIL");
}
static void target_capacity_experiments(void) {
    for (unsigned requested = 1; requested <= 4; requested *= 2) {
        for (unsigned fault = 0; fault < 5; ++fault) {
            KvGxm g = {.thread=7};
            KvGxmTexture image = {.width=960,.height=540,.pitch=960,.bpp=4};
            owner=&g; destination_texture=&image; expected_scenes=requested;
            target_queries=creates=destroys=0; injected_stage=0;
            fail_target_query = fault == 1; reject_candidate_capacity = fault == 2;
            reject_candidate_surface = fault == 3;
            if(fault==4 && requested>1) g.stats.main_mapped_bytes=MAIN_MAP_LIMIT-target_query_bytes;
            kv_render_set_options(requested == 2 ? KV_OPT_RT_SCENES_2
                : requested == 4 ? KV_OPT_RT_SCENES_4 : 0);
            ++cases;
            EXPECT(texture_target_ensure(&g,&image) == 0);
            EXPECT(image.target_scenes == (fault && requested > 1 ? 1 : requested));
            EXPECT(image.target_driver_bytes == target_query_bytes * image.target_scenes);
            EXPECT(target_queries == (fault && requested > 1 ? 2U : 1U));
            if(fault==3 && requested>1) EXPECT(destroys==1 && creates==2);
            EXPECT(texture_target_destroy(&g,&image) == 0);
            EXPECT(g.target_driver_bytes == 0 && !image.target && !g.closing);
        }
    }
    kv_render_set_options(KV_OPT_RT_SCENES_2 | KV_OPT_RT_SCENES_4);
    EXPECT(target_scene_capacity() == 1);
    kv_render_set_options(0); expected_scenes=1;
    fail_target_query=reject_candidate_capacity=reject_candidate_surface=0;
}
static void target_driver_reservation(void) {
    KvGxm g={.thread=7,.target_driver_bytes=8192};
    owner=&g; injected_stage=0;
    g.stats.main_mapped_bytes=MAIN_MAP_LIMIT-g.target_driver_bytes;
    Block block={0};
    ++cases;
    EXPECT(block_alloc(&g,&block,4096,0,1)==KV_BUDGET);
    if(block.uid>0) EXPECT(block_free(&g,&block)==0);
    EXPECT(!mapped_memory && g.target_driver_bytes==8192);
    g.stats.main_mapped_bytes-=4096;
    EXPECT(block_alloc(&g,&block,4096,0,1)==0);
    EXPECT(g.stats.main_mapped_bytes+g.target_driver_bytes==MAIN_MAP_LIMIT);
    EXPECT(block_free(&g,&block)==0);
    g.target_driver_bytes=MAIN_MAP_LIMIT+1U;
    EXPECT(block_alloc(&g,&block,4096,0,1)==KV_BUDGET);
    if(block.uid>0) EXPECT(block_free(&g,&block)==0);
    EXPECT(!mapped_memory);
    /* The display target shares the exact queried accounting boundary. */
    KvGxm display={.thread=7};
    KvGxmTexture extent={.width=WIDTH,.height=HEIGHT};
    owner=&display; destination_texture=&extent;
    EXPECT(display_target_create(&display)==0);
    EXPECT(display.display_target_scenes==1);
    EXPECT(display.display_target_driver_bytes==target_query_bytes);
    EXPECT(display.target_driver_bytes==target_query_bytes);
    EXPECT(display_target_destroy(&display)==0);
    EXPECT(!display.target && display.target_driver_bytes==0);
    EXPECT(!display.display_target_driver_bytes && !display.display_target_scenes);
}
static void tiled_target_diagnostic(void) {
    for(unsigned rejected=0;rejected<2;++rejected) {
        KvGxm g={.thread=7}; KvGxmTexture *image=NULL;
        owner=&g; creating_texture=1; injected_stage=0; expected_scenes=1;
        tiled_texture_inits=creates=destroys=surface_inits=memory_frees=0;
        tiled_init_status=rejected?-77:0;
        kv_render_set_options(KV_OPT_TILED_TARGETS);
        ++cases;
        EXPECT(kv_gxm_texture_create_ex(&g,NULL,960,540,0,0,0,1,1,&image)==0);
        EXPECT(image && image->storage==KV_TEXTURE_LINEAR && image->target);
        EXPECT(g.tiled_probe_attempted && g.tiled_probe_native_ready==!rejected);
        EXPECT(tiled_texture_inits==1 && creates==(rejected?1U:2U));
        EXPECT(mapped_bytes==2*1024*1024 && g.stats.cdram_mapped_bytes==mapped_bytes);
        EXPECT(texture_target_destroy(&g,image)==0);
        EXPECT(block_free(&g,&image->pixels)==0); free(image); g.textures=NULL;
        EXPECT(g.target_driver_bytes==0 && !mapped_memory && !g.closing);
    }
    uint32_t padded=0,padding=0;
    EXPECT(tiled_target_layout(960,540,&padded,&padding)==0);
    EXPECT(padded==960*544*4 && padding==960*4*4);
    EXPECT(tiled_target_layout(UINT32_MAX,540,&padded,&padding)==KV_BAD_ARGUMENT);
    KvGxm g={.thread=7}; uint8_t pixels[4]={0};
    KvGxmTexture tiled={.width=1,.height=1,.pitch=32,.bpp=1,.ready=1,.storage=KV_TEXTURE_TILED};
    KvGxmTexture linear={.width=1,.height=1,.pitch=8,.bpp=4,.ready=1};
    g.textures=&tiled; tiled.next=&linear; owner=&g;
    EXPECT(kv_gxm_texture_read(&g,&tiled,pixels,sizeof(pixels))==KV_BAD_ARGUMENT);
    EXPECT(kv_gxm_texture_update(&g,&tiled,0,0,1,1,pixels,4)==KV_BAD_ARGUMENT);
    EXPECT(kv_gxm_copy_texture(&g,&tiled,&linear)==KV_BAD_ARGUMENT);
    EXPECT(kv_gxm_copy_texture(&g,&linear,&tiled)==KV_BAD_ARGUMENT);
    kv_render_set_options(0); creating_texture=0; tiled_init_status=0;
}
static void tiled_admitted_targets_and_fallback(void) {
    for(unsigned reject=0;reject<2;++reject) {
        KvGxm g={.thread=7,.tiled_probe_attempted=1,.tiled_probe_native_ready=1,.tiled_targets_ready=1};
        KvGxmTexture *image=NULL;
        owner=&g; creating_texture=1; injected_stage=0; expected_scenes=1;
        tiled_texture_inits=creates=destroys=surface_inits=memory_frees=0;
        tiled_init_status=reject?-77:0;
        kv_render_set_options(KV_OPT_TILED_TARGETS);
        ++cases;
        EXPECT(kv_gxm_texture_create_ex(&g,NULL,960,540,0,0,0,1,1,&image)==0);
        EXPECT(image && image->storage==(reject?KV_TEXTURE_LINEAR:KV_TEXTURE_TILED));
        EXPECT(image->width==960 && image->height==540 && image->pitch==960);
        EXPECT(image->pixels.size>=960*544*4u && mapped_bytes==2*1024*1024);
        EXPECT(tiled_texture_inits==1 && !g.closing);
        EXPECT(g.tiled_targets_ready==!reject);
        image->rendered=1;
        EXPECT(kv_gxm_texture_seal(&g,image)==0 && !image->target && image->rendered);
        EXPECT(image->storage==(reject?KV_TEXTURE_LINEAR:KV_TEXTURE_TILED));
        EXPECT(kv_gxm_texture_destroy(&g,image)==0 && !mapped_memory && !g.target_driver_bytes);
    }
    for(unsigned kind=0;kind<3;++kind) {
        KvGxm g={.thread=7,.tiled_probe_attempted=1,.tiled_targets_ready=1};
        KvGxmTexture *image=NULL;
        owner=&g; creating_texture=1; injected_stage=0; expected_scenes=1;
        tiled_texture_inits=0; tiled_init_status=0;
        kv_render_set_options(kind==0?0:KV_OPT_TILED_TARGETS);
        ++cases;
        /* Disabled option, nonmatching dimensions, and private readback
           staging each remain linear despite earlier successful admission. */
        EXPECT(texture_create_ex(&g,NULL,kind==1?480:960,kind==1?270:540,
            0,0,0,0,1,kind!=2,&image)==0);
        EXPECT(image && image->storage==KV_TEXTURE_LINEAR && !tiled_texture_inits);
        EXPECT(kv_gxm_texture_destroy(&g,image)==0 && !mapped_memory);
    }
    kv_render_set_options(0); creating_texture=0;
}
static void tiled_pattern_probe_admits_only_exact_staged_pixels(unsigned corrupt) {
    KvGxmTexture white={.width=1,.height=1,.pitch=8,.bpp=4,.ready=1};
    KvGxm g={.thread=7,.textures=&white,.white=&white};
    g.context=(SceGxmContext *)(void *)&context_token;
    g.vertex_program=(SceGxmVertexProgram *)(void *)&vertex_token;
    g.vertices.base=vertex_storage; g.indices.base=index_storage;
    owner=&g; creating_texture=tiled_probe_fixture=1;
    tiled_probe_corrupt=(int)corrupt; tiled_init_status=0; expected_scenes=1;
    injected_stage=0; programs_created=programs_released=draws=begins=ends=transfers=0;
    tiled_texture_inits=memory_frees=0;
    kv_reset_effect_state();
    EXPECT(quad_program(&g,0,&g.opaque_program)==0);
    KvGxmTexture *image=NULL;
    kv_render_set_options(KV_OPT_TILED_TARGETS);
    ++cases;
    EXPECT(kv_gxm_texture_create_ex(&g,NULL,960,540,0,0,0,1,1,&image)==0);
    EXPECT(g.tiled_probe_attempted && g.tiled_probe_native_ready && g.tiled_targets_ready==!corrupt);
    EXPECT(image && image->storage==(corrupt?KV_TEXTURE_LINEAR:KV_TEXTURE_TILED));
    EXPECT(image->next==&white && !g.closing && !g.scene_open);
    EXPECT(draws==2 && !transfers && memory_frees==2);
    EXPECT(image->width==960 && image->height==540 && image->pixels.size>=960*544*4u);
    /* Probe writes never become the initial production target's content. */
    unsigned nonzero=0;
    for(unsigned p=0;p<960*544*4u;++p) if(((uint8_t *)image->pixels.base)[p]) { nonzero=1; break; }
    EXPECT(!nonzero);
    EXPECT(kv_gxm_texture_destroy(&g,image)==0 && !mapped_memory && g.textures==&white);
    EXPECT(!g.target_driver_bytes);
    for(unsigned i=0;i<4;++i) EXPECT(!probe_memory[i]);
    EXPECT(sceGxmShaderPatcherReleaseFragmentProgram(g.patcher,g.opaque_program)==0);
    kv_render_set_options(0); creating_texture=tiled_probe_fixture=tiled_probe_corrupt=0;
}
static void tiled_readback_stages_and_cleans_up(unsigned fault) {
    KvGxmTexture source={.width=960,.height=540,.pitch=960,.bpp=4,.ready=1,.storage=KV_TEXTURE_TILED};
    KvGxm g={.thread=7,.textures=&source,.tiled_probe_attempted=1,.tiled_targets_ready=1};
    g.context=(SceGxmContext *)(void *)&context_token;
    g.vertex_program=(SceGxmVertexProgram *)(void *)&vertex_token;
    g.vertices.base=vertex_storage; g.indices.base=index_storage;
    /* A CPU dereference of this GPU-only tiled backing would be invalid. */
    source.pixels.base=(void *)(uintptr_t)1;
    owner=&g; source_texture=&source; destination_texture=NULL;
    creating_texture=tiled_readback_fixture=1; tiled_init_status=0; expected_scenes=1;
    injected_stage=(int)fault; injected_status=-73;
    programs_created=programs_released=draws=begins=ends=transfers=tiled_texture_inits=0;
    uint8_t *data=calloc(1,960*540*4u); EXPECT(data!=NULL);
    if(!data) return;
    kv_render_set_options(KV_OPT_TILED_TARGETS);
    ++cases;
    EXPECT(kv_gxm_texture_read(&g,&source,data,960*540*4u)==(fault?-73:0));
    EXPECT(!tiled_texture_inits && !transfers);
    if(!fault) {
        unsigned mismatch=0;
        for(unsigned p=0;p<960*540*4u;++p) if(data[p]!=(uint8_t[]){23,87,197,33}[p%4]) { mismatch=1; break; }
        EXPECT(!mismatch && draws==1 && begins==1 && ends==1);
    }
    if(fault==7) {
        EXPECT(g.closing && g.scene_open && g.textures!=&source && mapped_memory);
        /* The mock has no GPU. Explicitly retire the retained fixture only
           after asserting production preserved its uncertain scene owner. */
        g.closing=g.scene_open=0;
        EXPECT(kv_gxm_texture_destroy(&g,g.textures)==0);
    } else EXPECT(g.textures==&source && !mapped_memory && !g.closing);
    EXPECT(source.ready && source.storage==KV_TEXTURE_TILED && !g.target_driver_bytes);
    free(data);
    kv_render_set_options(0); creating_texture=tiled_readback_fixture=0; injected_stage=0;
}
int main(void) {
    kv_perf_set_enabled(1);
    target_capacity_experiments();
    target_driver_reservation();
    tiled_target_diagnostic();
    tiled_admitted_targets_and_fallback();
    tiled_pattern_probe_admits_only_exact_staged_pixels(0);
    tiled_pattern_probe_admits_only_exact_staged_pixels(1);
    tiled_pattern_probe_admits_only_exact_staged_pixels(2);
    tiled_readback_stages_and_cleans_up(0);
    tiled_readback_stages_and_cleans_up(1);
    tiled_readback_stages_and_cleans_up(6);
    tiled_readback_stages_and_cleans_up(7);
    offscreen_sync_pressure();
    run(480,270,1,1,0,0);
    run(960,540,1,1,0,0); run(960,540,1,1,1,0);
    run(3,17,1,0,0,0); run(1,1,1,0,0,0); run(2048,2048,1,1,0,0);
    run(960,540,0,0,0,0); run(3,17,2,0,0,0);
    run(480,270,3,1,0,0); run(3,17,3,0,1,0);
    /* An unrendered tiled source/destination must never reach the linear
       transfer engine; the descriptor/surface layout determines GPU copying. */
    run(960,540,4,0,0,0); run(960,540,5,0,0,0);
    for (int stage=1;stage<=7;++stage) if (stage != 2 && stage != 4) run(960,540,1,1,0,stage);
    output_programs();
    full_material_missing_logs();
    all_operations_high_bank();
    material_variant_startup_failures();
    startup_target(1,0); startup_target(0,0); startup_target(1,1);
    seal_baked_texture(0); seal_baked_texture(8); seal_baked_texture(9); seal_invalid();
    KvPerfSample audit[KV_PERF_COUNT];
    EXPECT(kv_perf_snapshot(audit, KV_PERF_COUNT) == 0);
    uint64_t finish_calls = 0;
    for (unsigned i = KV_PERF_FINISH_TARGET; i <= KV_PERF_FINISH_MOVIE; ++i) finish_calls += audit[i].calls;
    EXPECT(finish_calls == audited_finishes);
    EXPECT(audit[KV_PERF_COPY_QUAD].calls > 0 && audit[KV_PERF_COPY_TRANSFER].calls > 0);
    EXPECT(audit[KV_PERF_COPY_QUAD].bytes > 0 && audit[KV_PERF_COPY_TRANSFER].bytes > 0);
    printf("RESULT %s: %u cases, %u checks, %u failures\n",failures?"FAIL":"PASS",cases,checks,failures);
    return failures ? 1 : 0;
}

/* Deterministic clock for legacy timing sites; no device timing claim. */
SceUInt64 sceKernelGetProcessTimeWide(void) { static SceUInt64 t; return ++t; }

/* This SDK double reports no live default uniforms. Real shader ABI is ARM/GPU tested separately. */
const SceGxmProgramParameter *sceGxmProgramFindParameterByName(const SceGxmProgram *p, const char *name) {
    if (p == invalid_sampler_program && !strcmp(name, "tex2"))
        return (const SceGxmProgramParameter *)(const void *)p;
    return NULL;
}
SceGxmParameterCategory sceGxmProgramParameterGetCategory(const SceGxmProgramParameter *p) {
    (void)p; return SCE_GXM_PARAMETER_CATEGORY_UNIFORM;
}
unsigned int sceGxmProgramParameterGetComponentCount(const SceGxmProgramParameter *p) { (void)p; return 0; }
