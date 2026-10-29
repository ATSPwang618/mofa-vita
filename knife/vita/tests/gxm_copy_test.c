/* Exercise the actual backend with SDK declarations and captured SDK calls.
 * This proves submission/lifetime/error contracts; the separate Vita3K pixel
 * oracle checks pixels and alpha. A CPU transfer cannot stand in for an RT. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#include KV_GXM_BACKEND_SOURCE

static unsigned checks, failures, cases, begins, ends, draws, transfers;
static unsigned creates, sync_creates, destroys, sync_destroys, surface_inits, gammas;
static int injected_stage, injected_status;
static SceGxmOutputRegisterSize surface_registers;
static KvGxm *owner;
static KvGxmTexture *source_texture, *destination_texture;
static Vertex vertex_storage[4];
static uint16_t index_storage[6];
static int context_token, target_token, sync_token, vertex_token;
typedef struct TestProgram {
    SceGxmOutputRegisterFormat format;
    int blended, released;
} TestProgram;
static TestProgram programs[512];
static unsigned programs_created, programs_released;
static void *mapped_memory;
static unsigned mapped_bytes, memory_frees;
static int creating_texture;
static int deny_offscreen_sync;
static unsigned call_order, last_finish, last_transfer_finish, last_destroy;

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
    EXPECT(context == owner->context); last_finish=++call_order;
}
int sceGxmTransferFinish(void) { last_transfer_finish=++call_order; return result(8); }
int sceGxmCreateRenderTarget(const SceGxmRenderTargetParams *p, SceGxmRenderTarget **out) {
    ++creates;
    if (creating_texture) destination_texture=owner->textures;
    EXPECT(p->width == destination_texture->width && p->height == destination_texture->height);
    EXPECT(p->scenesPerFrame == 1 && p->multisampleMode == SCE_GXM_MULTISAMPLE_NONE);
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
    EXPECT(p == destination_texture->target); ++destroys; last_destroy=++call_order; return result(9);
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
    EXPECT(type == SCE_GXM_COLOR_SURFACE_LINEAR && scale == SCE_GXM_COLOR_SURFACE_SCALE_NONE);
    surface_registers = registers;
    EXPECT(registers == (destination_texture->srgb ?
        SCE_GXM_OUTPUT_REGISTER_SIZE_64BIT : SCE_GXM_OUTPUT_REGISTER_SIZE_32BIT));
    EXPECT(width == destination_texture->width && height == destination_texture->height);
    EXPECT(stride == destination_texture->pitch && data == destination_texture->pixels.base);
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
        EXPECT(program->format == (destination_texture->srgb ?
            SCE_GXM_OUTPUT_REGISTER_FORMAT_HALF4 : SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4));
        EXPECT(!program->blended); /* Copy preserves all four channels. */
    }
}
int sceGxmShaderPatcherCreateFragmentProgram(SceGxmShaderPatcher *patcher,
    SceGxmShaderPatcherId id, SceGxmOutputRegisterFormat format,
    SceGxmMultisampleMode ms, const SceGxmBlendInfo *blend,
    const SceGxmProgram *vertex, SceGxmFragmentProgram **out) {
    (void)id;
    EXPECT(patcher == (owner->effects ? owner->effects->patcher : owner->patcher));
    EXPECT(ms == SCE_GXM_MULTISAMPLE_NONE && out);
    EXPECT(vertex == (const SceGxmProgram *)(const void *)(owner->effects ?
        knife_effects_vs_main_gxp : knife_quad_v_gxp));
    EXPECT(format == (destination_texture->srgb ?
        SCE_GXM_OUTPUT_REGISTER_FORMAT_HALF4 : SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4));
    if (result(10)) return result(10);
    EXPECT(programs_created < sizeof(programs) / sizeof(programs[0]));
    if (programs_created >= sizeof(programs) / sizeof(programs[0])) return -99;
    TestProgram *p = &programs[programs_created++];
    *p = (TestProgram){ .format=format, .blended=blend != NULL };
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
    (void)p; (void)id; return 0;
}
int sceGxmShaderPatcherDestroy(SceGxmShaderPatcher *p) { (void)p; return 0; }
SceUID sceKernelAllocMemBlock(const char *name, SceKernelMemBlockType type,
    SceSize size, SceKernelAllocMemBlockOpt *opt) {
    EXPECT(!mapped_memory && !strcmp(name,"knife-gxm"));
    EXPECT(type == SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW && opt == NULL);
    mapped_memory=malloc(size); mapped_bytes=size;
    EXPECT(mapped_memory != NULL);
    return mapped_memory ? 42 : -99;
}
int sceKernelGetMemBlockBase(SceUID id, void **base) {
    EXPECT(id == 42 && mapped_memory); *base=mapped_memory; return 0;
}
int sceGxmMapMemory(void *p, SceSize size, SceGxmMemoryAttribFlags attr) {
    EXPECT(p == mapped_memory && size == mapped_bytes);
    EXPECT(attr == (SCE_GXM_MEMORY_ATTRIB_READ | SCE_GXM_MEMORY_ATTRIB_WRITE)); return 0;
}
int sceGxmMapVertexUsseMemory(void *p, SceSize size, unsigned *offset) {
    (void)p; (void)size; (void)offset; EXPECT(0); return -99;
}
int sceGxmMapFragmentUsseMemory(void *p, SceSize size, unsigned *offset) {
    (void)p; (void)size; (void)offset; EXPECT(0); return -99;
}
int sceGxmUnmapMemory(void *p) { EXPECT(p == mapped_memory); return 0; }
int sceGxmUnmapVertexUsseMemory(void *p) { (void)p; EXPECT(0); return -99; }
int sceGxmUnmapFragmentUsseMemory(void *p) { (void)p; EXPECT(0); return -99; }
int sceKernelFreeMemBlock(SceUID id) {
    EXPECT(id == 42 && mapped_memory);
    free(mapped_memory); mapped_memory=NULL; ++memory_frees; return 0;
}
int sceGxmTextureInitLinear(SceGxmTexture *texture, const void *data,
    SceGxmTextureFormat format, unsigned width, unsigned height, unsigned mips) {
    EXPECT(texture == &owner->textures->descriptor && data == mapped_memory);
    EXPECT(format == SCE_GXM_TEXTURE_FORMAT_A8B8G8R8 && width == 480 && height == 270 && mips == 0);
    return 0;
}
int sceGxmTextureSetMinFilter(SceGxmTexture *t, SceGxmTextureFilter filter) {
    EXPECT(t == &owner->textures->descriptor && filter == SCE_GXM_TEXTURE_FILTER_LINEAR); return 0;
}
int sceGxmTextureSetMagFilter(SceGxmTexture *t, SceGxmTextureFilter filter) {
    EXPECT(t == &owner->textures->descriptor && filter == SCE_GXM_TEXTURE_FILTER_LINEAR); return 0;
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
    if (existing) destination.target=(SceGxmRenderTarget *)(void *)&target_token;
    owner=&g; source_texture=&source; destination_texture=&destination;
    begins=ends=draws=transfers=creates=sync_creates=destroys=sync_destroys=surface_inits=gammas=0;
    injected_stage=stage; injected_status=-73;
    programs_created=programs_released=0;
    ++cases; const unsigned before=failures;
    int rc=kv_gxm_copy_texture(&g,&source,&destination);
    EXPECT(rc == (stage ? -73 : 0));
    if (rt != 1 && rt != 3) {
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
            EXPECT(gammas == (existing || !srgb ? 0U : 1U));
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
                if (info) EXPECT(info->format == (target.srgb ?
                    SCE_GXM_OUTPUT_REGISTER_FORMAT_HALF4 : SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4));
            }
        }
    }
    EXPECT(programs_created == 4);
    /* Failure must propagate without caching an invalid program. */
    g.srgb_alpha_program=NULL; target.srgb=1; g.current_target=&target;
    injected_stage=10; SceGxmFragmentProgram *p=NULL;
    EXPECT(quad_program(&g,1,&p) == -73 && !g.srgb_alpha_program && !p);
    injected_stage=0;
    EXPECT(quad_program(&g,1,&p) == 0 && p);
    printf("%s quad output variants and destination switches\n",failures==before?"PASS":"FAIL");

    g.effects=calloc(1,sizeof(*g.effects));
    EXPECT(g.effects != NULL);
    if (!g.effects) return;
    programs_created=programs_released=0;
    ++cases;
    for (unsigned pass=0;pass<2;++pass) {
        for (unsigned mode=0;mode<2;++mode) {
            target.srgb=mode; g.current_target=mode ? &target : NULL;
            for (unsigned shader=0;shader<EFFECT_SHADERS;++shader) {
                for (unsigned blend=0;blend<EFFECT_BLENDS;++blend) {
                    EXPECT(effects_program(&g,shader,blend) == 0);
                    TestProgram *info=program_info(g.effects->fragment[shader][blend][mode]);
                    EXPECT(info && info->blended == (blend != 5));
                    if (info) EXPECT(info->format == (mode ?
                        SCE_GXM_OUTPUT_REGISTER_FORMAT_HALF4 : SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4));
                }
            }
        }
    }
    EXPECT(programs_created == 2 * EFFECT_SHADERS * EFFECT_BLENDS);
    EXPECT(effects_destroy(&g) == 0 && !g.effects);
    EXPECT(programs_released == programs_created);
    printf("%s all effects output variants, cache reuse and release\n",failures==before?"PASS":"FAIL");
}
static void startup_target(int srgb, int fail_gamma) {
    KvGxm g={.thread=7}; KvGxmTexture *target=NULL;
    owner=&g; creating_texture=1; injected_stage=fail_gamma ? 4 : 0;
    injected_status=(int)SCE_GXM_ERROR_INVALID_VALUE;
    creates=destroys=sync_creates=sync_destroys=surface_inits=gammas=memory_frees=0;
    ++cases; const unsigned before=failures;
    int rc=kv_gxm_texture_create_ex(&g,NULL,480,270,0,0,srgb,1,1,&target);
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
int main(void) {
    offscreen_sync_pressure();
    run(480,270,1,1,0,0);
    run(960,540,1,1,0,0); run(960,540,1,1,1,0);
    run(3,17,1,0,0,0); run(1,1,1,0,0,0); run(2048,2048,1,1,0,0);
    run(960,540,0,0,0,0); run(3,17,2,0,0,0);
    run(480,270,3,1,0,0); run(3,17,3,0,1,0);
    for (int stage=1;stage<=7;++stage) if (stage != 2) run(960,540,1,1,0,stage);
    output_programs();
    startup_target(1,0); startup_target(0,0); startup_target(1,1);
    seal_baked_texture(0); seal_baked_texture(8); seal_baked_texture(9); seal_invalid();
    printf("RESULT %s: %u cases, %u checks, %u failures\n",failures?"FAIL":"PASS",cases,checks,failures);
    return failures ? 1 : 0;
}
