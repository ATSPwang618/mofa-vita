/* Private implementation included by gxm_backend.c. No host GL/VitaGL path. */
#include "shaders/generated/effects.h"

enum { EFFECT_SHADERS = 17, EFFECT_BLENDS = 9, EFFECT_VERTICES = 32760 };
struct KvEffects {
    Block buffer, vertex_usse, fragment_usse, vertices, indices;
    SceGxmShaderPatcher *patcher;
    SceGxmShaderPatcherId vertex_id, ids[EFFECT_SHADERS];
    SceGxmVertexProgram *vertex;
    SceGxmFragmentProgram *fragment[EFFECT_SHADERS][EFFECT_BLENDS][OUTPUT_MODES];
    /* Uniform handles are resolved once per patched program: a per-draw string
       lookup cost more than the draw itself on hardware. */
    const SceGxmProgramParameter *uniforms[EFFECT_SHADERS][EFFECT_BLENDS][OUTPUT_MODES][7];
    uint32_t used;
    /* Index cursor for the open scene: the vertex stream is bound once per scene and
       each draw only advances this, as the reference Vita renderer does. */
    uint32_t index_used;
    uint32_t index_base;
};
static const SceGxmProgram *effect_source(uint32_t shader) {
    const void *const programs[EFFECT_SHADERS] = {
        knife_effects_fs_sprite_gxp, knife_effects_fs_noalpha_gxp,
        knife_effects_fs_forcealpha_gxp, knife_effects_fs_fill_gxp,
        knife_effects_fs_text_gxp, knife_effects_fs_rim_text_gxp,
        knife_effects_fs_mono_gxp, knife_effects_fs_nega_gxp,
        knife_effects_fs_transition_gxp, knife_effects_fs_rule_gxp,
        knife_effects_fs_blur_gxp, knife_effects_fs_alter_gxp,
        knife_effects_fs_present_gxp, knife_effects_fs_present_srgb_gxp,
        knife_effects_fs_ps_gxp, knife_effects_fs_group_composite_gxp,
        knife_effects_fs_sprite_mask_gxp };
    return shader < EFFECT_SHADERS ? programs[shader] : NULL;
}
static const char *const effect_uniform_names[7] = {
    "uTex0Size", "uTex1Size", "uSoftLutSize", "uQuadHx", "uQuadHy", "tu.params", "tu.params2" };
static const uint32_t effect_uniform_counts[7] = { 2, 2, 2, 4, 4, 4, 4 };
static int effects_initialize(KvGxm *g) {
    if (g->effects) return 0;
    KvEffects *e = calloc(1, sizeof(*e));
    if (!e) return fail(g, "effects owner allocation", KV_ALLOC);
    g->effects = e; /* Every partial allocation remains reachable on failure. */
    CHECK(g, block_alloc(g, &e->buffer, 256 * 1024, 0, 1));
    CHECK(g, block_alloc(g, &e->vertex_usse, 64 * 1024, 0, 2));
    CHECK(g, block_alloc(g, &e->fragment_usse, 1024 * 1024, 0, 3));
    CHECK(g, block_alloc(g, &e->vertices, EFFECT_VERTICES * sizeof(KvEffectVertex), 0, 1));
    CHECK(g, block_alloc(g, &e->indices, EFFECT_VERTICES * sizeof(uint16_t), 0, 1));
    for (uint32_t i = 0; i < EFFECT_VERTICES; ++i) ((uint16_t *)e->indices.base)[i] = (uint16_t)i;
    const SceGxmShaderPatcherParams patcher = {
        .hostAllocCallback = patcher_alloc, .hostFreeCallback = patcher_free,
        .bufferMem = e->buffer.base, .bufferMemSize = e->buffer.size,
        .vertexUsseMem = e->vertex_usse.base, .vertexUsseMemSize = e->vertex_usse.size,
        .vertexUsseOffset = e->vertex_usse.offset,
        .fragmentUsseMem = e->fragment_usse.base, .fragmentUsseMemSize = e->fragment_usse.size,
        .fragmentUsseOffset = e->fragment_usse.offset };
    CHECK(g, sceGxmShaderPatcherCreate(&patcher, &e->patcher));
    const SceGxmProgram *vs = (const SceGxmProgram *)knife_effects_vs_main_gxp;
    CHECK(g, sceGxmProgramCheck(vs)); ++g->stats.program_checks;
    CHECK(g, sceGxmShaderPatcherRegisterProgram(e->patcher, vs, &e->vertex_id));
    const char *names[8] = { "v.pos", "v.uv", "v.color", "v.hx", "v.hy", "v.s0", "v.s1", "v.s2" };
    const uint16_t offsets[8] = { 0, 8, 16, 32, 48, 64, 80, 96 };
    const uint8_t counts[8] = { 2, 2, 4, 4, 4, 4, 4, 4 };
    SceGxmVertexAttribute attributes[8] = {0};
    for (uint32_t i = 0; i < 8; ++i) {
        const SceGxmProgramParameter *p = sceGxmProgramFindParameterByName(vs, names[i]);
        if (!p || sceGxmProgramParameterGetCategory(p) != SCE_GXM_PARAMETER_CATEGORY_ATTRIBUTE
            || sceGxmProgramParameterGetComponentCount(p) < counts[i]
            || sceGxmProgramParameterGetComponentCount(p) > 4)
            return fail(g, names[i], KV_BAD_ARGUMENT);
        attributes[i] = (SceGxmVertexAttribute){ .streamIndex = 0, .offset = offsets[i],
            .format = SCE_GXM_ATTRIBUTE_FORMAT_F32, .componentCount = counts[i],
            .regIndex = sceGxmProgramParameterGetResourceIndex(p) };
    }
    const SceGxmVertexStream stream = { .stride = sizeof(KvEffectVertex),
        .indexSource = SCE_GXM_INDEX_SOURCE_INDEX_16BIT };
    CHECK(g, sceGxmShaderPatcherCreateVertexProgram(e->patcher, e->vertex_id,
        attributes, 8, &stream, 1, &e->vertex));
    for (uint32_t i = 0; i < EFFECT_SHADERS; ++i) {
        const SceGxmProgram *fs = effect_source(i);
        CHECK(g, sceGxmProgramCheck(fs)); ++g->stats.program_checks;
        CHECK(g, sceGxmShaderPatcherRegisterProgram(e->patcher, fs, &e->ids[i]));
        const char *samplers[] = { "tex0", "tex1", "tex2", "soft_lut" };
        for (uint32_t unit = 0; unit < 4; ++unit) {
            const SceGxmProgramParameter *p = sceGxmProgramFindParameterByName(fs, samplers[unit]);
            if (p && (sceGxmProgramParameterGetCategory(p) != SCE_GXM_PARAMETER_CATEGORY_SAMPLER
                || sceGxmProgramParameterGetResourceIndex(p) != unit))
                return fail(g, "effects sampler metadata", KV_BAD_ARGUMENT);
        }
    }
    return 0;
}
static int effects_ensure(KvGxm *g) {
    int result = effects_initialize(g);
    if (result < 0) g->closing = 1;
    return result;
}
static int effects_program(KvGxm *g, uint32_t shader, uint32_t blend) {
    KvEffects *e = g->effects;
    const uint32_t mode = output_mode(g);
    if (e->fragment[shader][blend][mode]) return 0;
    SceGxmBlendInfo b = { .colorMask = SCE_GXM_COLOR_MASK_ALL,
        .colorFunc = SCE_GXM_BLEND_FUNC_ADD, .alphaFunc = SCE_GXM_BLEND_FUNC_ADD,
        .colorSrc = SCE_GXM_BLEND_FACTOR_ONE, .colorDst = SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
        .alphaSrc = SCE_GXM_BLEND_FACTOR_ZERO, .alphaDst = SCE_GXM_BLEND_FACTOR_ONE };
    if (blend == 1) b.colorDst = SCE_GXM_BLEND_FACTOR_ONE;
    if (blend == 2) { b.colorDst = SCE_GXM_BLEND_FACTOR_ONE; b.colorFunc = SCE_GXM_BLEND_FUNC_REVERSE_SUBTRACT; }
    if (blend == 3) b.colorSrc = SCE_GXM_BLEND_FACTOR_DST_COLOR;
    if (blend == 4) b.colorDst = SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_COLOR;
    if (blend == 6 || blend == 7) b.alphaSrc = SCE_GXM_BLEND_FACTOR_ONE;
    if (blend == 7 || blend == 8) { b.colorSrc = SCE_GXM_BLEND_FACTOR_ZERO; b.colorDst = SCE_GXM_BLEND_FACTOR_ONE; }
    if (blend == 8) b.alphaDst = SCE_GXM_BLEND_FACTOR_SRC_ALPHA;
    CHECK(g, sceGxmShaderPatcherCreateFragmentProgram(e->patcher, e->ids[shader],
        mode ? SCE_GXM_OUTPUT_REGISTER_FORMAT_HALF4 : SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4,
        SCE_GXM_MULTISAMPLE_NONE,
        blend == 5 ? NULL : &b, (const SceGxmProgram *)knife_effects_vs_main_gxp,
        &e->fragment[shader][blend][mode]));
    /* Resolve every uniform this shader uses once; absent ones are dead in this
       specialised entry and stay NULL. */
    const SceGxmProgram *source = effect_source(shader);
    for (uint32_t u = 0; u < 7; ++u) {
        const SceGxmProgramParameter *p =
            sceGxmProgramFindParameterByName(source, effect_uniform_names[u]);
        if (p && (sceGxmProgramParameterGetCategory(p) != SCE_GXM_PARAMETER_CATEGORY_UNIFORM
            || sceGxmProgramParameterGetComponentCount(p) != effect_uniform_counts[u]))
            return fail(g, effect_uniform_names[u], KV_BAD_ARGUMENT);
        e->uniforms[shader][blend][mode][u] = p;
    }
    return 0;
}
static int effects_destroy(KvGxm *g) {
    KvEffects *e = g->effects;
    if (!e) return 0;
    for (uint32_t s = 0; s < EFFECT_SHADERS; ++s) {
        for (uint32_t b = 0; b < EFFECT_BLENDS; ++b) {
            for (uint32_t mode = 0; mode < OUTPUT_MODES; ++mode) {
                if (e->fragment[s][b][mode]) {
                    CHECK(g, sceGxmShaderPatcherReleaseFragmentProgram(e->patcher, e->fragment[s][b][mode]));
                    e->fragment[s][b][mode] = NULL;
                }
            }
        }
        if (e->ids[s]) { CHECK(g, sceGxmShaderPatcherUnregisterProgram(e->patcher, e->ids[s])); e->ids[s] = NULL; }
    }
    if (e->vertex) { CHECK(g, sceGxmShaderPatcherReleaseVertexProgram(e->patcher, e->vertex)); e->vertex = NULL; }
    if (e->vertex_id) { CHECK(g, sceGxmShaderPatcherUnregisterProgram(e->patcher, e->vertex_id)); e->vertex_id = NULL; }
    if (e->patcher) { CHECK(g, sceGxmShaderPatcherDestroy(e->patcher)); e->patcher = NULL; }
    Block *blocks[] = { &e->indices, &e->vertices, &e->fragment_usse, &e->vertex_usse, &e->buffer };
    for (uint32_t i = 0; i < 5; ++i) CHECK(g, block_free(g, blocks[i]));
    free(e); g->effects = NULL;
    return 0;
}
static int texture_target_destroy(KvGxm *g, KvGxmTexture *t) {
    if (t->target) { CHECK(g, sceGxmDestroyRenderTarget(t->target)); t->target = NULL; }
    if (t->sync) { CHECK(g, sceGxmSyncObjectDestroy(t->sync)); t->sync = NULL; }
    return 0;
}
static int texture_target_ensure(KvGxm *g, KvGxmTexture *t) {
    if (t->target) return 0;
    const SceGxmRenderTargetParams target = { .width = t->width, .height = t->height,
        .scenesPerFrame = 1, .multisampleMode = SCE_GXM_MULTISAMPLE_NONE, .driverMemBlock = -1 };
    const char *stage = "effects render target";
    const uint64_t target_start = kv_probe_now();
    int result = sceGxmCreateRenderTarget(&target, &t->target);
    kv_probe_add(&kv_gxm_timing_counters.target_create_us, &kv_gxm_timing_counters.target_create_calls, target_start);
#define TARGET_STEP(label, expression) do { if (result >= 0) { stage = label; result = (expression); } } while (0)
    /* Offscreen passes are serialized by Finish before sampling/reuse. A
       display-queue sync per atlas wastes native driver objects; NULL is the
       same offscreen submission contract used by vitaGL. Display buffers
       retain their own sync objects. */
    TARGET_STEP("effects color surface", sceGxmColorSurfaceInit(&t->surface,
        SCE_GXM_COLOR_FORMAT_A8B8G8R8, SCE_GXM_COLOR_SURFACE_LINEAR,
        SCE_GXM_COLOR_SURFACE_SCALE_NONE,
        t->srgb ? SCE_GXM_OUTPUT_REGISTER_SIZE_64BIT : SCE_GXM_OUTPUT_REGISTER_SIZE_32BIT,
        t->width, t->height, t->pitch, t->pixels.base));
    if (t->srgb) TARGET_STEP("effects surface gamma", sceGxmColorSurfaceSetGammaMode(&t->surface, SCE_GXM_COLOR_SURFACE_GAMMA_BGR));
#undef TARGET_STEP
    if (result < 0) {
        const int cleanup = texture_target_destroy(g, t);
        if (cleanup < 0) { g->closing = 1; return fail(g, "render target rollback retained owner", cleanup); }
        return fail(g, stage, result);
    }
    return 0;
}
int32_t kv_gxm_texture_seal(KvGxm *g, KvGxmTexture *t) {
    if (!active(g) || g->scene_open || !texture_owned(g, t) || t == g->white
        || t->storage != KV_TEXTURE_LINEAR || t->bpp != 4) return KV_STATE;
#ifdef KV_GXM_DEV_DIAGNOSTICS
    if (g->diagnostic_scanout == t) return fail(g, "DEV target is still scanned out", KV_STATE);
#endif
    /* Baking creates a sampled image, not a persistent framebuffer. Complete
       both engines before releasing attachments, retaining pixels/descriptor.
       On uncertain teardown retain the owner for backend destruction. */
    sceGxmFinish(g->context);
    int result = sceGxmTransferFinish();
    if (result >= 0) result = texture_target_destroy(g, t);
    if (result < 0) { g->closing = 1; return fail(g, "sealing baked texture retained owner", result); }
    return 0;
}
int32_t kv_gxm_texture_create_ex(KvGxm *g, const uint8_t *data, uint32_t width,
    uint32_t height, uint32_t stride, uint32_t format, uint32_t srgb,
    uint32_t linear, uint32_t render_target, KvGxmTexture **out) {
    if (!active(g) || !out || g->scene_open || !width || !height || width > 2048 || height > 2048
        || format > 1 || srgb > 1 || linear > 1 || render_target > 1
        || (format == 1 && (srgb || render_target))) return KV_BAD_ARGUMENT;
    *out = NULL;
    const uint32_t bpp = format ? 1 : 4;
    if (data && (stride < width * bpp || stride > 2048 * 4)) return KV_BAD_ARGUMENT;
    KvGxmTexture *t = calloc(1, sizeof(*t));
    if (!t) return fail(g, "effects texture owner", KV_ALLOC);
    t->width = width; t->height = height; t->pitch = (width + 7) & ~7U;
    t->bpp = bpp; t->srgb = srgb; t->owner = g;
    t->next = g->textures; g->textures = t;
    const char *stage = "effects texture CDRAM allocation";
    int result = block_alloc(g, &t->pixels, t->pitch * height * bpp, 1, 1);
#define EFFECT_TEXTURE_STEP(label, expression) do { if (result >= 0) { stage = label; result = (expression); } } while (0)
    if (result >= 0) {
        memset(t->pixels.base, 0, t->pixels.size);
        if (data) for (uint32_t y = 0; y < height; ++y)
            memcpy((uint8_t *)t->pixels.base + y * t->pitch * bpp, data + y * stride, width * bpp);
    }
    EFFECT_TEXTURE_STEP("effects texture init", sceGxmTextureInitLinear(&t->descriptor, t->pixels.base,
        format ? SCE_GXM_TEXTURE_FORMAT_U8_RRRR : SCE_GXM_TEXTURE_FORMAT_A8B8G8R8, width, height, 0));
    const SceGxmTextureFilter filter = linear ? SCE_GXM_TEXTURE_FILTER_LINEAR : SCE_GXM_TEXTURE_FILTER_POINT;
    EFFECT_TEXTURE_STEP("effects min filter", sceGxmTextureSetMinFilter(&t->descriptor, filter));
    EFFECT_TEXTURE_STEP("effects mag filter", sceGxmTextureSetMagFilter(&t->descriptor, filter));
    EFFECT_TEXTURE_STEP("effects clamp u", sceGxmTextureSetUAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
    EFFECT_TEXTURE_STEP("effects clamp v", sceGxmTextureSetVAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
    if (srgb) EFFECT_TEXTURE_STEP("effects texture gamma", sceGxmTextureSetGammaMode(&t->descriptor, SCE_GXM_TEXTURE_GAMMA_BGR));
    int target_error = 0;
    if (result >= 0 && render_target) {
        result = texture_target_ensure(g, t);
        target_error = result < 0;
    }
#undef EFFECT_TEXTURE_STEP
    if (result < 0) {
        int cleanup = texture_target_destroy(g, t);
        if (cleanup >= 0) cleanup = block_free(g, &t->pixels);
        if (cleanup < 0) { g->closing = 1; return fail(g, "effects texture rollback retained owner", cleanup); }
        g->textures = t->next; free(t);
        return target_error ? result : fail(g, stage, result);
    }
    t->ready = 1;
    TRACE_TEXTURE(g, t);
    *out = t;
    return 0;
}
int32_t kv_gxm_texture_create_compressed(KvGxm *g, const uint8_t *data,
    uint32_t length, uint32_t width, uint32_t height, uint32_t format,
    uint32_t srgb, uint32_t linear, KvGxmTexture **out) {
    if (!active(g) || !out || g->scene_open || !data || !width || !height
        || width > 2048 || height > 2048 || srgb > 1 || linear > 1
        || (format != SCE_GXM_TEXTURE_FORMAT_PVRT4BPP_ABGR
            && format != SCE_GXM_TEXTURE_FORMAT_UBC1_ABGR
            && format != SCE_GXM_TEXTURE_FORMAT_UBC3_ABGR)) return KV_BAD_ARGUMENT;
    *out = NULL;
    /* InitSwizzledArbitrary takes logical dimensions; the allocation still
       contains every padded compression block in the enclosing PoT image. */
    const uint32_t minimum = format == SCE_GXM_TEXTURE_FORMAT_PVRT4BPP_ABGR ? 8 : 4;
    uint32_t backing_width = minimum, backing_height = minimum;
    while (backing_width < width) backing_width *= 2;
    while (backing_height < height) backing_height *= 2;
    const uint32_t pixels = backing_width * backing_height;
    const uint32_t expected = format == SCE_GXM_TEXTURE_FORMAT_UBC3_ABGR ? pixels : pixels / 2;
    if (length != expected) return KV_BAD_ARGUMENT;
    KvGxmTexture *t = calloc(1, sizeof(*t));
    if (!t) return fail(g, "compressed texture owner", KV_ALLOC);
    t->width = width; t->height = height; t->srgb = srgb; t->owner = g;
    t->storage = KV_TEXTURE_COMPRESSED;
    t->next = g->textures; g->textures = t;
    const char *stage = "compressed texture CDRAM allocation";
    int result = block_alloc(g, &t->pixels, length, 1, 1);
#define COMPRESSED_STEP(label, expression) do { if (result >= 0) { stage = label; result = (expression); } } while (0)
    if (result >= 0) {
        memset(t->pixels.base, 0, t->pixels.size);
        memcpy(t->pixels.base, data, length);
    }
    COMPRESSED_STEP("compressed texture init", sceGxmTextureInitSwizzledArbitrary(
        &t->descriptor, t->pixels.base, (SceGxmTextureFormat)format, width, height, 0));
    const SceGxmTextureFilter filter = linear ? SCE_GXM_TEXTURE_FILTER_LINEAR : SCE_GXM_TEXTURE_FILTER_POINT;
    COMPRESSED_STEP("compressed min filter", sceGxmTextureSetMinFilter(&t->descriptor, filter));
    COMPRESSED_STEP("compressed mag filter", sceGxmTextureSetMagFilter(&t->descriptor, filter));
    COMPRESSED_STEP("compressed clamp u", sceGxmTextureSetUAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
    COMPRESSED_STEP("compressed clamp v", sceGxmTextureSetVAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
    if (srgb) COMPRESSED_STEP("compressed texture gamma", sceGxmTextureSetGammaMode(&t->descriptor, SCE_GXM_TEXTURE_GAMMA_BGR));
#undef COMPRESSED_STEP
    if (result < 0) {
        const int cleanup = block_free(g, &t->pixels);
        if (cleanup < 0) { g->closing = 1; return fail(g, "compressed texture rollback retained owner", cleanup); }
        g->textures = t->next; free(t);
        return fail(g, stage, result);
    }
    t->ready = 1;
    TRACE_TEXTURE(g, t);
    *out = t;
    return 0;
}
int32_t kv_gxm_texture_update(KvGxm *g, KvGxmTexture *t, uint32_t x, uint32_t y,
    uint32_t width, uint32_t height, const uint8_t *data, uint32_t stride) {
    if (!active(g) || g->scene_open || !texture_owned(g, t) || !data
        || t->storage != KV_TEXTURE_LINEAR || (t->bpp != 1 && t->bpp != 4)
        || !width || !height || x > t->width || y > t->height
        || width > t->width - x || height > t->height - y
        || stride < width * t->bpp || stride > 2048 * 4) return KV_BAD_ARGUMENT;
    sceGxmFinish(g->context);
    CHECK(g, sceGxmTransferFinish());
    for (uint32_t row = 0; row < height; ++row)
        memcpy((uint8_t *)t->pixels.base + ((y + row) * t->pitch + x) * t->bpp,
            data + row * stride, width * t->bpp);
    return 0;
}
/* Viewport/depth state cached by target size; reset by the present path, which sets a
   different viewport for the display surface. */
static uint32_t kv_bound_width, kv_bound_height;
void kv_reset_effect_state(void) { kv_bound_width = kv_bound_height = 0; }
/* Called by the present path: a new frame starts with an empty vertex ring. */
void kv_reset_effect_ring(KvGxm *g) { if (g->effects) g->effects->used = 0; }

static int effect_scene(KvGxm *g, KvGxmTexture *t) {
    CHECK(g, sceGxmBeginScene(g->context, 0, t->target, NULL, NULL, t->sync, &t->surface, &g->depth));
    g->scene_open = 1; g->current_target = t;
    if (g->effects) g->effects->used = 0;
    /* The viewport and the depth/cull state depend only on the target size, and a scene
       switches between two sizes at most, so re-issuing seven driver calls per pass was
       pure overhead (~2 ms of CPU submission on each of ~40 passes per heavy frame). */
    if (kv_bound_width != t->width || kv_bound_height != t->height) {
        sceGxmSetViewport(g->context, t->width / 2.0f, t->width / 2.0f,
            t->height / 2.0f, -(t->height / 2.0f), 0.5f, 0.5f);
        sceGxmSetCullMode(g->context, SCE_GXM_CULL_NONE);
        sceGxmSetFrontDepthFunc(g->context, SCE_GXM_DEPTH_FUNC_ALWAYS);
        sceGxmSetBackDepthFunc(g->context, SCE_GXM_DEPTH_FUNC_ALWAYS);
        sceGxmSetFrontDepthWriteEnable(g->context, SCE_GXM_DEPTH_WRITE_DISABLED);
        sceGxmSetBackDepthWriteEnable(g->context, SCE_GXM_DEPTH_WRITE_DISABLED);
        kv_bound_width = t->width;
        kv_bound_height = t->height;
    }
    return 0;
}
int32_t kv_gxm_begin_target(KvGxm *g, KvGxmTexture *t, const float *clear, int32_t finish_before) {
    if (!active(g) || g->scene_open || !texture_owned(g, t) || !t->target) return KV_STATE;
    const uint64_t begin_start = kv_probe_now();
    if (clear) for (uint32_t i = 0; i < 4; ++i) if (!isfinite(clear[i])) return KV_BAD_ARGUMENT;
    CHECK(g, effects_ensure(g));
    /* The caller knows the frame's pass dependencies and asks for a drain only when this
       pass samples a target an earlier pass wrote (measured: ~20% of passes). Removing all
       of them black-screened part of the game (r51); keeping all of them wasted ~66 ms of
       driver round trips per 39-pass frame. */
    if (finish_before) {
        const uint64_t t0 = kv_probe_now();
        sceGxmFinish(g->context);
        kv_probe_add(&kv_gxm_timing_counters.finish_us, &kv_gxm_timing_counters.finish_calls, t0);
    }
    CHECK(g, effect_scene(g, t));
    if (clear) {
        /* A clear may have nonzero RGB with zero alpha: avoid shader premultiplication. */
        g->quads = 0;
        const uint64_t clear_start = kv_probe_now();
        CHECK(g, kv_gxm_draw_quad(g, NULL, (float[]){0,0,WIDTH,CONTENT_H},
            (float[]){0,0,1,1}, clear, 0));
        kv_gxm_timing_counters.clear_us += kv_probe_now() - clear_start;
    }
    kv_probe_add(&kv_gxm_timing_counters.begin_target_us,
        &kv_gxm_timing_counters.begin_target_calls, begin_start);
    return 0;
}
int32_t kv_gxm_end_target(KvGxm *g) {
    if (!active(g) || !g->scene_open || !g->current_target) return KV_STATE;
    const uint64_t end_start = kv_probe_now();
    int result = sceGxmEndScene(g->context, NULL, NULL);
    kv_probe_add(&kv_gxm_timing_counters.end_scene_us, &kv_gxm_timing_counters.end_scene_calls, end_start);
    if (result < 0) { g->closing = 1; return fail(g, "end offscreen scene", result); }
    g->current_target->rendered = 1;
    g->scene_open = 0; g->current_target = NULL;
    /* No Finish here. Every consumer of this target finishes first: a later pass goes
       through kv_gxm_begin_target and present goes through kv_gxm_begin, both of which
       call sceGxmFinish before sampling. With ~50 passes per heavy frame the second
       Finish per pass cost ~40 ms of pure synchronisation. */
    return 0;
}
static int packed_integer(float value, float low, float high) {
    return value >= low && value <= high && truncf(value) == value;
}
static int effect_inputs(const KvEffectVertex *v, uint32_t count, uint32_t shader, uint32_t blend,
    const float params[8]) {
    if (!v || !params || !count || count % 3 || count > EFFECT_VERTICES
        || shader >= EFFECT_SHADERS || blend >= EFFECT_BLENDS) return KV_BAD_ARGUMENT;
    for (uint32_t i = 0; i < 8; ++i) if (!isfinite(params[i])) return KV_BAD_ARGUMENT;
    for (uint32_t i = 0; i < count; ++i) {
        /* The struct has only floats and its ABI is asserted, but pointer
           arithmetic may not legally walk across distinct struct members. */
        float floats[28];
        memcpy(floats, &v[i], sizeof(floats));
        for (uint32_t j = 0; j < 28; ++j) if (!isfinite(floats[j])) return KV_BAD_ARGUMENT;
        for (uint32_t row = 0; row < 2; ++row)
            for (uint32_t j = 0; j < 4; ++j)
                if (v[i].haze[row][j] != v[0].haze[row][j]) return KV_BAD_ARGUMENT;
        if (v[i].soft[2][0] < 0) return KV_BAD_ARGUMENT;
        if (v[i].soft[2][0] > 0 && (!packed_integer(v[i].soft[2][1],0,17)
            || !packed_integer(v[i].soft[2][2],1,256))) return KV_BAD_ARGUMENT;
    }
    if (shader == 5 && (!packed_integer(v[0].haze[1][3],0,16777215)
        || v[0].haze[1][2] < 0 || v[0].haze[1][2] > 1)) return KV_BAD_ARGUMENT;
    if ((shader == 9 || (shader == 8 && params[2] > 0.5f))
        && !packed_integer(params[1],8,2047)) return KV_BAD_ARGUMENT;
    if (shader == 14 && (!packed_integer(params[0],1,24)
        || (params[4] >= 0 && !packed_integer(params[5],8,2047)))) return KV_BAD_ARGUMENT;
    /* tex2 is either a rule or a same-layout group mask, never both. */
    if (shader == 14 && (!packed_integer(params[7],0,1)
        || (params[7] == 1 && (params[4] >= 0 || params[6] != 0)))) return KV_BAD_ARGUMENT;
    if ((shader == 9 || shader == 14) && (params[2] <= 0 || params[3] <= 0)) return KV_BAD_ARGUMENT;
    if (shader == 10 && (!packed_integer(params[3],0,2147483520.0f)
        || !packed_integer(params[4],0,2))) return KV_BAD_ARGUMENT;
    return 0;
}
int32_t kv_gxm_draw_effect(KvGxm *g, const KvEffectVertex *vertices, uint32_t count,
    uint32_t shader, uint32_t blend, const KvGxmTexture *const input[4], const float params[8]) {
    if (!active(g) || !g->scene_open || !input) return KV_STATE;
    if (effect_inputs(vertices,count,shader,blend,params) < 0) return KV_BAD_ARGUMENT;
    const KvGxmTexture *textures[4];
    for (uint32_t i = 0; i < 4; ++i) {
        textures[i] = input[i] ? input[i] : g->white;
        if (!texture_owned(g, textures[i]) || textures[i] == g->current_target) return KV_BAD_ARGUMENT;
    }
    CHECK(g, effects_ensure(g));
    KvEffects *e = g->effects;
    if (count > EFFECT_VERTICES - e->used) return fail(g, "effects scene vertex budget", KV_BUDGET);
    CHECK(g, effects_program(g, shader, blend));
    const SceGxmProgram *fs = effect_source(shader);
    sceGxmSetVertexProgram(g->context, e->vertex);
    sceGxmSetFragmentProgram(g->context, e->fragment[shader][blend][output_mode(g)]);
    for (uint32_t i = 0; i < 4; ++i)
        CHECK(g, sceGxmSetFragmentTexture(g->context, i, &textures[i]->descriptor));
    void *uniforms = NULL;
    if (sceGxmProgramGetDefaultUniformBufferSize(fs))
        CHECK(g, sceGxmReserveFragmentDefaultUniformBuffer(g->context, &uniforms));
    const float sizes[3][2] = {
        {textures[0]->width, textures[0]->height}, {textures[1]->width, textures[1]->height},
        {textures[3]->width, textures[3]->height} };
    // Handles were resolved once when the program was created; only the values
    // are written per draw, so this cannot change what the shader reads.
    const SceGxmProgramParameter *const *slot = e->uniforms[shader][blend][output_mode(g)];
    const float *const values[7] = { sizes[0], sizes[1], sizes[2], vertices[0].haze[0],
        vertices[0].haze[1], params, params + 4 };
    for (uint32_t u = 0; u < 7; ++u)
        if (slot[u]) CHECK(g, sceGxmSetUniformDataF(uniforms, slot[u], 0,
            effect_uniform_counts[u], values[u]));
    if (e->used + count > EFFECT_VERTICES) {
        /* Drain and start the ring over: safe only here, because nothing is in flight for
           a reused slot after a Finish and every recorded draw has been submitted. */
        sceGxmFinish(g->context);
        e->used = 0;
    }
    KvEffectVertex *destination = (KvEffectVertex *)e->vertices.base + e->used;
    memcpy(destination, vertices, count * sizeof(*vertices));
    /* Split the CPU cost of one draw from the submit call itself. */
    const uint64_t cpu_start = kv_probe_now();
    CHECK(g, sceGxmSetVertexStream(g->context, 0, destination));
    kv_gxm_timing_counters.draw_cpu_us += kv_probe_now() - cpu_start;
    const uint64_t gxm_start = kv_probe_now();
    CHECK(g, sceGxmDraw(g->context, SCE_GXM_PRIMITIVE_TRIANGLES, SCE_GXM_INDEX_FORMAT_U16, e->indices.base, count));
    kv_gxm_timing_counters.draw_gxm_us += kv_probe_now() - gxm_start;
    ++kv_gxm_timing_counters.draw_calls;
    e->used += count; ++g->stats.draws;
    return 0;
}
int32_t kv_gxm_copy_texture(KvGxm *g, const KvGxmTexture *source, KvGxmTexture *destination) {
    if (!active(g) || g->scene_open || source == destination
        || !texture_owned(g,source) || !texture_owned(g,destination)
        || source->storage != KV_TEXTURE_LINEAR || destination->storage != KV_TEXTURE_LINEAR
        || source->width != destination->width || source->height != destination->height
        || (source->bpp != 1 && source->bpp != 4)
        || source->bpp != destination->bpp || source->srgb != destination->srgb) return KV_BAD_ARGUMENT;
    sceGxmFinish(g->context);
    CHECK(g, sceGxmTransferFinish());
    if (source->target || source->rendered) {
        /* Keep rendered pixels on the GPU. Some runtimes leave the CPU backing
           stale after Finish; a transfer from that backing loses the backdrop.
           quad_f samples raw RGBA, including alpha, with no premultiplication;
           matching texture/surface gamma modes preserve the stored encoding.
           Sealing a baked image drops its target but retains this GPU origin. */
        const int target_result = texture_target_ensure(g, destination);
        if (target_result < 0) return target_result;
        CHECK(g, effect_scene(g, destination));
        g->quads = 0;
        const int draw = kv_gxm_draw_quad(g, source, (float[]){0,0,WIDTH,CONTENT_H},
            (float[]){0,0,1,1}, (float[]){1,1,1,1}, 0);
        /* End a begun scene even if submission failed; failed EndScene keeps a
           closing owner so its GPU resources cannot be reused prematurely. */
        const int end = kv_gxm_end_target(g);
        return end < 0 ? end : draw;
    }
    const SceGxmTransferFormat format = source->bpp == 4 ?
        SCE_GXM_TRANSFER_FORMAT_U8U8U8U8_ABGR : SCE_GXM_TRANSFER_FORMAT_U8_R;
    CHECK(g, sceGxmTransferCopy(source->width, source->height, 0, 0, SCE_GXM_TRANSFER_COLORKEY_NONE,
        format, SCE_GXM_TRANSFER_LINEAR, source->pixels.base, 0, 0, source->pitch * source->bpp,
        format, SCE_GXM_TRANSFER_LINEAR, destination->pixels.base, 0, 0, destination->pitch * destination->bpp,
        NULL, 0, NULL));
    CHECK(g, sceGxmTransferFinish());
    return 0;
}
int32_t kv_gxm_present_canvas(KvGxm *g, const KvGxmTexture *canvas) {
    if (!active(g) || g->scene_open || !texture_owned(g,canvas) || canvas->bpp != 4) return KV_STATE;
    CHECK(g, effects_ensure(g));
    CHECK(g, kv_gxm_begin(g, (float[]){0,0,0,1}));
    g->effects->used = 0;
    KvEffectVertex v[6] = {0};
    const float positions[6][2] = {{-1,1},{1,1},{-1,-1},{-1,-1},{1,1},{1,-1}};
    const float uv[6][2] = {{0,0},{1,0},{0,1},{0,1},{1,0},{1,1}};
    for (uint32_t i = 0; i < 6; ++i) {
        memcpy(v[i].position, positions[i], sizeof(positions[i]));
        memcpy(v[i].uv, uv[i], sizeof(uv[i]));
        for (uint32_t j = 0; j < 4; ++j) v[i].color[j] = 1;
    }
    const KvGxmTexture *textures[4] = {canvas,NULL,NULL,NULL};
    CHECK(g, kv_gxm_draw_effect(g,v,6,12,5,textures,(float[8]){0}));
    return kv_gxm_present(g);
}
int32_t kv_gxm_texture_read(KvGxm *g, const KvGxmTexture *texture, uint8_t *data, uint32_t capacity) {
    if (!active(g) || g->scene_open || !texture_owned(g,texture) || !data
        || texture->storage != KV_TEXTURE_LINEAR || (texture->bpp != 1 && texture->bpp != 4)
        || capacity < texture->width * texture->height * texture->bpp) return KV_BAD_ARGUMENT;
    sceGxmFinish(g->context);
    CHECK(g, sceGxmTransferFinish());
    for (uint32_t y = 0; y < texture->height; ++y)
        memcpy(data + y * texture->width * texture->bpp,
            (uint8_t *)texture->pixels.base + y * texture->pitch * texture->bpp,
            texture->width * texture->bpp);
    return 0;
}
