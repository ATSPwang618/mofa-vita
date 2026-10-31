/* Private implementation included by gxm_backend.c. No host GL/VitaGL path. */
#include "shaders/generated/effects.h"

enum { EFFECT_SHADERS = 26, EFFECT_BLENDS = 10, EFFECT_VERTICES = 32760 };
struct KvEffects {
    Block buffer, vertex_usse, fragment_usse, vertices, indices;
    SceGxmShaderPatcher *patcher;
    SceGxmShaderPatcherId vertex_id, ids[EFFECT_SHADERS];
    const SceGxmProgram *source[EFFECT_SHADERS];
    int optional_status[EFFECT_SHADERS];
    SceGxmVertexProgram *vertex, *compact_vertex, *constant_vertex;
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
static int effect_fetch_shader(uint32_t shader) {
    return (shader >= 17 && shader <= 21) || shader == 23 || shader == 25;
}
static int effect_ps_shader(uint32_t shader) { return shader == 14 || effect_fetch_shader(shader); }
static const SceGxmProgram *legacy_effect_source(uint32_t shader) {
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
static const SceGxmProgram *effect_source(uint32_t shader) {
    const SceGxmProgram *runtime = kv_runtime_shader_program(shader);
    return runtime ? runtime : legacy_effect_source(shader);
}
static const SceGxmProgram *bound_effect_source(const KvEffects *e, uint32_t shader) {
    return e->source[shader] ? e->source[shader] : legacy_effect_source(shader);
}
static const char *const effect_uniform_names[7] = {
    "uTex0Size", "uTex1Size", "uSoftLutSize", "uQuadHx", "uQuadHy", "tu.params", "tu.params2" };
static const uint32_t effect_uniform_counts[7] = { 2, 2, 2, 4, 4, 4, 4 };
enum { EFFECT_STREAM_FULL, EFFECT_STREAM_ZERO, EFFECT_STREAM_CONSTANT,
    EFFECT_CONSTANT_BYTES = 80 };
_Static_assert(offsetof(KvEffectVertex, haze) == sizeof(Vertex), "compact attributes ABI");
_Static_assert(sizeof(KvEffectVertex) - sizeof(Vertex) == EFFECT_CONSTANT_BYTES, "constant stream ABI");
static void effect_compact_attributes(SceGxmVertexAttribute attributes[8], int constants) {
    for (uint32_t i = 3; i < 8; ++i) {
        attributes[i].streamIndex = 1;
        attributes[i].offset = constants ? (i - 3) * 16 : 0;
    }
}
/* effect_inputs has already rejected nonfinite data. Compare representations so
 * signed zeros are preserved, and varying soft coordinates retain the full VS
 * inputs. A constant stream still uses the identical compiled vertex program. */
static uint32_t effect_stream_layout(const KvEffectVertex *vertices, uint32_t count) {
    if (!count) return EFFECT_STREAM_FULL;
    uint32_t first[EFFECT_CONSTANT_BYTES / sizeof(uint32_t)];
    memcpy(first, (const uint8_t *)vertices + sizeof(Vertex), sizeof(first));
    for (uint32_t i = 1; i < count; ++i)
        if (memcmp(first, (const uint8_t *)&vertices[i] + sizeof(Vertex), sizeof(first)))
            return EFFECT_STREAM_FULL;
    for (uint32_t i = 0; i < sizeof(first) / sizeof(first[0]); ++i)
        if (first[i]) return EFFECT_STREAM_CONSTANT;
    return EFFECT_STREAM_ZERO;
}
static uint32_t effect_stream_bytes(uint32_t count, uint32_t layout) {
    return count * (layout ? sizeof(Vertex) : sizeof(KvEffectVertex))
        + (layout == EFFECT_STREAM_CONSTANT ? EFFECT_CONSTANT_BYTES : 0);
}
static void effect_stream_write(void *destination, const KvEffectVertex *vertices,
    uint32_t count, uint32_t layout) {
    if (!layout) { memcpy(destination, vertices, count * sizeof(*vertices)); return; }
    for (uint32_t i = 0; i < count; ++i)
        memcpy((uint8_t *)destination + i * sizeof(Vertex), &vertices[i], sizeof(Vertex));
    if (layout == EFFECT_STREAM_CONSTANT)
        memcpy((uint8_t *)destination + count * sizeof(Vertex),
            (const uint8_t *)vertices + sizeof(Vertex), EFFECT_CONSTANT_BYTES);
}
static int effects_initialize(KvGxm *g) {
    if (g->effects) return 0;
    KvEffects *e = calloc(1, sizeof(*e));
    if (!e) return fail(g, "effects owner allocation", KV_ALLOC);
    g->effects = e; /* Every partial allocation remains reachable on failure. */
    CHECK(g, block_alloc(g, &e->buffer, 256 * 1024, 0, 1));
    CHECK(g, block_alloc(g, &e->vertex_usse, 64 * 1024, 0, 2));
    CHECK(g, block_alloc(g, &e->fragment_usse, 1024 * 1024, 0, 3));
    if (!(kv_render_options() & KV_OPT_BATCH_ARENA))
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
    if (kv_render_options() & KV_OPT_COMPACT_STREAM) {
        /* Same compiled VS and varying ABI; five zero attributes use an immutable
           stride-zero stream, a pattern also used by VitaGL custom_shaders.c. */
        effect_compact_attributes(attributes, 0);
        const SceGxmVertexStream compact[2] = {
            { .stride = sizeof(Vertex), .indexSource = SCE_GXM_INDEX_SOURCE_INDEX_16BIT },
            { .stride = 0, .indexSource = SCE_GXM_INDEX_SOURCE_INDEX_16BIT } };
        const int compact_status = sceGxmShaderPatcherCreateVertexProgram(e->patcher, e->vertex_id,
            attributes, 8, compact, 2, &e->compact_vertex);
        if (compact_status < 0 && e->compact_vertex) {
            CHECK(g, sceGxmShaderPatcherReleaseVertexProgram(e->patcher, e->compact_vertex));
            e->compact_vertex = NULL;
        }
        /* Distinct offsets preserve each float4 when haze/soft are constant
           across this draw. The 80-byte stream lives in the batch arena. */
        effect_compact_attributes(attributes, 1);
        const int constant_status = sceGxmShaderPatcherCreateVertexProgram(e->patcher, e->vertex_id,
            attributes, 8, compact, 2, &e->constant_vertex);
        if (constant_status < 0 && e->constant_vertex) {
            CHECK(g, sceGxmShaderPatcherReleaseVertexProgram(e->patcher, e->constant_vertex));
            e->constant_vertex = NULL;
        }
        char line[192];
        const int length = snprintf(line, sizeof(line),
            "{\"kind\":\"compact_stream\",\"enabled\":%u,\"patch_status\":%d,"
            "\"constant_enabled\":%u,\"constant_patch_status\":%d}\n",
            e->compact_vertex != NULL, compact_status, e->constant_vertex != NULL, constant_status);
        if (length > 0 && (size_t)length < sizeof(line)) (void)kv_perf_console(line, (uint32_t)length);
    }
    for (uint32_t i = 0; i < EFFECT_SHADERS; ++i) {
        const SceGxmProgram *fs = effect_source(i);
        if (!fs) continue; /* Optional PsFetch has no unsafe legacy equivalent. */
        int status = sceGxmProgramCheck(fs); ++g->stats.program_checks;
        if (status >= 0) status = sceGxmShaderPatcherRegisterProgram(e->patcher, fs, &e->ids[i]);
        const int optional = fs != legacy_effect_source(i);
        if (status < 0 && optional) {
            e->optional_status[i] = status;
            if (e->ids[i]) {
                CHECK(g, sceGxmShaderPatcherUnregisterProgram(e->patcher, e->ids[i]));
                e->ids[i] = NULL;
            }
            /* Driver rejection cannot turn a compiled experiment into a runtime
               rendering failure. PsFetch has no legacy direct-draw equivalent. */
            fs = legacy_effect_source(i);
            if (!fs) continue;
            status = sceGxmProgramCheck(fs); ++g->stats.program_checks;
            if (status >= 0) status = sceGxmShaderPatcherRegisterProgram(e->patcher, fs, &e->ids[i]);
        } else if (status >= 0 && optional) g->shader_capabilities |= 1U << i;
        if (status < 0) return fail(g, "effects program registration", status);
        e->source[i] = fs;
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
static int effects_program_mode(KvGxm *g, uint32_t shader, uint32_t blend, uint32_t mode) {
    KvEffects *e = g->effects;
    if (mode != 0) return fail(g, "Vita effects require encoded UCHAR4 target", KV_BAD_ARGUMENT);
    if (!e->ids[shader] || !bound_effect_source(e, shader)) return fail(g, "optional shader unavailable", KV_STATE);
    if (e->fragment[shader][blend][mode]) return 0;
    const uint64_t patch_started = kv_perf_begin();
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
    /* Cached text must retain coverage in its transparent target. This is
       standard premultiplied over for both RGB and alpha; game Over (6)
       intentionally adds alpha and must keep its original equation. */
    if (blend == 9) {
        b.alphaSrc = SCE_GXM_BLEND_FACTOR_ONE;
        b.alphaDst = SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA;
    }
    CHECK(g, sceGxmShaderPatcherCreateFragmentProgram(e->patcher, e->ids[shader],
        SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4,
        SCE_GXM_MULTISAMPLE_NONE,
        blend == 5 ? NULL : &b, (const SceGxmProgram *)knife_effects_vs_main_gxp,
        &e->fragment[shader][blend][mode]));
    /* Resolve every uniform this shader uses once; absent ones are dead in this
       specialised entry and stay NULL. */
    const SceGxmProgram *source = bound_effect_source(e, shader);
    for (uint32_t u = 0; u < 7; ++u) {
        const SceGxmProgramParameter *p =
            sceGxmProgramFindParameterByName(source, effect_uniform_names[u]);
        if (p && (sceGxmProgramParameterGetCategory(p) != SCE_GXM_PARAMETER_CATEGORY_UNIFORM
            || sceGxmProgramParameterGetComponentCount(p) != effect_uniform_counts[u]))
            return fail(g, effect_uniform_names[u], KV_BAD_ARGUMENT);
        e->uniforms[shader][blend][mode][u] = p;
    }
    kv_perf_end(KV_PERF_PROGRAM_PATCH, patch_started, 0, 0);
    return 0;
}
static int effects_program(KvGxm *g, uint32_t shader, uint32_t blend) {
    return effects_program_mode(g, shader, blend, output_mode(g));
}
static int disable_optional_shader(KvGxm *g, uint32_t shader) {
    if (!g || !g->effects || shader >= EFFECT_SHADERS) return KV_BAD_ARGUMENT;
    KvEffects *e = g->effects;
    for (uint32_t b = 0; b < EFFECT_BLENDS; ++b) for (uint32_t mode = 0; mode < OUTPUT_MODES; ++mode) {
        if (e->fragment[shader][b][mode]) {
            CHECK(g, sceGxmShaderPatcherReleaseFragmentProgram(e->patcher, e->fragment[shader][b][mode]));
            e->fragment[shader][b][mode] = NULL;
        }
        memset(e->uniforms[shader][b][mode], 0, sizeof(e->uniforms[shader][b][mode]));
    }
    if (e->ids[shader]) {
        CHECK(g, sceGxmShaderPatcherUnregisterProgram(e->patcher, e->ids[shader]));
        e->ids[shader] = NULL;
    }
    g->shader_capabilities &= ~(1U << shader);
    e->source[shader] = legacy_effect_source(shader);
    if (e->source[shader]) CHECK(g, sceGxmShaderPatcherRegisterProgram(e->patcher, e->source[shader], &e->ids[shader]));
    return 0;
}
/* Capability 24 is a checked text-cache pipeline, not an effect-shader ID.
   Prepatch before frames are recorded so an optional allocation/link failure
   can select ordinary text rather than fail midway through GPU execution. */
static int effects_text_cache_initialize_program(KvGxm *g, const SceGxmProgram *runtime) {
    g->shader_capabilities &= ~(1U << 24);
    /* Shader capability bits describe optional fast paths. A mandatory entry
       need not have bit 11 set; require the actual registered runtime program
       instead. The bundled legacy Alter has a different colour contract. */
    if (!g->effects || !runtime || runtime == legacy_effect_source(11)
        || !g->effects->ids[11] || bound_effect_source(g->effects, 11) != runtime)
        return KV_STATE;
    const uint32_t shader[] = {4, 5, 11}, blend[] = {9, 9, 0};
    for (uint32_t i = 0; i < 3; ++i) {
        const int status = effects_program_mode(g, shader[i], blend[i], 0);
        if (status < 0) return status;
    }
    g->shader_capabilities |= 1U << 24;
    return 0;
}
static int effects_text_cache_initialize(KvGxm *g) {
    return effects_text_cache_initialize_program(g, kv_runtime_shader_program(11));
}
static int effects_optional_initialize(KvGxm *g) {
    int status_by_shader[9] = {0};
    const uint32_t optional[9] = {15, 17, 18, 19, 20, 21, 22, 23, 25};
    int available = kv_runtime_shader_available(11);
    for (uint32_t i = 0; i < 9; ++i) available |= kv_runtime_shader_available(optional[i]);
    if (available) {
        CHECK(g, effects_ensure(g));
        for (uint32_t i = 0; i < 9; ++i) {
            const uint32_t shader = optional[i];
            status_by_shader[i] = g->effects->optional_status[shader];
            if (!(g->shader_capabilities & (1U << shader))) continue;
            int status = 0;
            for (uint32_t blend = 0; status >= 0 && blend < EFFECT_BLENDS; ++blend) {
                if (effect_fetch_shader(shader) && blend != 5) continue;
                status = effects_program_mode(g, shader, blend, 0);
            }
            status_by_shader[i] = status;
            if (status < 0) CHECK(g, disable_optional_shader(g, shader));
        }
    }
    const int text_cache_status = effects_text_cache_initialize(g);
    char line[980];
    const int length = snprintf(line, sizeof(line),
        "{\"kind\":\"shader_capabilities\",\"color_contract\":\"encoded-unorm8-uchar4\",\"group15\":%u,\"ps_fetch17\":%u,\"ps_fetch19\":%u,"
        "\"ps_group19\":%u,\"ps_fetch11\":%u,\"ps_fetch13\":%u,\"plain_sprite22\":%u,\"plain_ps19_23\":%u,\"ps_fetch2_25\":%u,"
        "\"group_patch_status\":%d,\"fetch_patch_status\":%d,\"fetch19_patch_status\":%d,"
        "\"group19_patch_status\":%d,\"fetch11_patch_status\":%d,\"fetch13_patch_status\":%d,"
        "\"plain_sprite_patch_status\":%d,\"plain_ps19_patch_status\":%d,\"fetch2_patch_status\":%d,"
        "\"text_cache24\":%u,\"text_cache_patch_status\":%d,"
        "\"fallback_routes\":\"group19:19/18/17/14;op2:25/17/14;op11:20/17/14;op13:21/17/14;sprite:22/0;op19:23/18/17/14\"}\n",
        (g->shader_capabilities >> 15) & 1U, (g->shader_capabilities >> 17) & 1U,
        (g->shader_capabilities >> 18) & 1U, (g->shader_capabilities >> 19) & 1U,
        (g->shader_capabilities >> 20) & 1U, (g->shader_capabilities >> 21) & 1U,
        (g->shader_capabilities >> 22) & 1U, (g->shader_capabilities >> 23) & 1U,
        (g->shader_capabilities >> 25) & 1U,
        status_by_shader[0], status_by_shader[1], status_by_shader[2], status_by_shader[3],
        status_by_shader[4], status_by_shader[5], status_by_shader[6], status_by_shader[7], status_by_shader[8],
        (g->shader_capabilities >> 24) & 1U, text_cache_status);
    if (length > 0 && (size_t)length < sizeof(line)) (void)kv_perf_console(line, (uint32_t)length);
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
    if (e->constant_vertex) { CHECK(g, sceGxmShaderPatcherReleaseVertexProgram(e->patcher, e->constant_vertex)); e->constant_vertex = NULL; }
    if (e->compact_vertex) { CHECK(g, sceGxmShaderPatcherReleaseVertexProgram(e->patcher, e->compact_vertex)); e->compact_vertex = NULL; }
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
        SCE_GXM_OUTPUT_REGISTER_SIZE_32BIT,
        t->width, t->height, t->pitch, t->pixels.base));
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
    if (async_submission(g)) CHECK(g, wait_submitted(g, KV_PERF_FINISH_SEAL));
    else perf_finish(g->context, KV_PERF_FINISH_SEAL);
    int result = perf_transfer_finish(KV_PERF_TRANSFER_SEAL);
    if (result >= 0) result = texture_target_destroy(g, t);
    if (result < 0) { g->closing = 1; return fail(g, "sealing baked texture retained owner", result); }
    return 0;
}
int32_t kv_gxm_texture_create_ex(KvGxm *g, const uint8_t *data, uint32_t width,
    uint32_t height, uint32_t stride, uint32_t format, uint32_t srgb,
    uint32_t linear, uint32_t render_target, KvGxmTexture **out) {
    if (!active(g) || !out || g->scene_open || !width || !height || width > 2048 || height > 2048
        || format > 1 || srgb != 0 || linear > 1 || render_target > 1
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
    int result = texture_pixels_alloc(g, &t->pixels, t->pitch * height * bpp);
#define EFFECT_TEXTURE_STEP(label, expression) do { if (result >= 0) { stage = label; result = (expression); } } while (0)
    if (result >= 0) {
        texture_initialize_padding(t, render_target ? KV_PERF_ZERO_TARGET : KV_PERF_ZERO_LINEAR, data != NULL);
        const uint64_t copy_started = kv_perf_begin();
        if (data) for (uint32_t y = 0; y < height; ++y)
            memcpy((uint8_t *)t->pixels.base + y * t->pitch * bpp, data + y * stride, width * bpp);
        if (data) kv_perf_end(bpp == 1 ? KV_PERF_UPLOAD_R8 : KV_PERF_UPLOAD_RGBA,
            copy_started, (uint64_t)width * height * bpp, t->pixels.size);
    }
    EFFECT_TEXTURE_STEP("effects texture init", sceGxmTextureInitLinear(&t->descriptor, t->pixels.base,
        format ? SCE_GXM_TEXTURE_FORMAT_U8_RRRR : SCE_GXM_TEXTURE_FORMAT_A8B8G8R8, width, height, 0));
    const SceGxmTextureFilter filter = linear ? SCE_GXM_TEXTURE_FILTER_LINEAR : SCE_GXM_TEXTURE_FILTER_POINT;
    EFFECT_TEXTURE_STEP("effects min filter", sceGxmTextureSetMinFilter(&t->descriptor, filter));
    EFFECT_TEXTURE_STEP("effects mag filter", sceGxmTextureSetMagFilter(&t->descriptor, filter));
    EFFECT_TEXTURE_STEP("effects clamp u", sceGxmTextureSetUAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
    EFFECT_TEXTURE_STEP("effects clamp v", sceGxmTextureSetVAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
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
        || width > 2048 || height > 2048 || srgb != 0 || linear > 1
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
    int result = texture_pixels_alloc(g, &t->pixels, length);
#define COMPRESSED_STEP(label, expression) do { if (result >= 0) { stage = label; result = (expression); } } while (0)
    if (result >= 0) {
        if (!(kv_render_options() & KV_OPT_TEXTURE_POOL))
            perf_zero(KV_PERF_ZERO_GXT, t->pixels.base, t->pixels.size);
        else if (t->pixels.size > length)
            perf_zero(KV_PERF_ZERO_GXT, (uint8_t *)t->pixels.base + length, t->pixels.size - length);
        const uint64_t started = kv_perf_begin();
        memcpy(t->pixels.base, data, length);
        kv_perf_end(KV_PERF_UPLOAD_GXT, started, length, t->pixels.size);
    }
    COMPRESSED_STEP("compressed texture init", sceGxmTextureInitSwizzledArbitrary(
        &t->descriptor, t->pixels.base, (SceGxmTextureFormat)format, width, height, 0));
    const SceGxmTextureFilter filter = linear ? SCE_GXM_TEXTURE_FILTER_LINEAR : SCE_GXM_TEXTURE_FILTER_POINT;
    COMPRESSED_STEP("compressed min filter", sceGxmTextureSetMinFilter(&t->descriptor, filter));
    COMPRESSED_STEP("compressed mag filter", sceGxmTextureSetMagFilter(&t->descriptor, filter));
    COMPRESSED_STEP("compressed clamp u", sceGxmTextureSetUAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
    COMPRESSED_STEP("compressed clamp v", sceGxmTextureSetVAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
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
    const int renamed = texture_rename_for_update(g, t);
    if (renamed < 0) return renamed;
    /* Already completed/currently unreferenced backing needs no context drain.
       A newly renamed source is independent of the retired old backing. */
    if (!async_submission(g)) perf_finish(g->context, KV_PERF_FINISH_UPDATE);
    else if (!(kv_render_options() & KV_OPT_SOURCE_RENAME) || t->last_use > completed_sequence(g))
        CHECK(g, wait_submitted(g, KV_PERF_FINISH_UPDATE));
    if (!renamed && (!(kv_render_options() & KV_OPT_SOURCE_RENAME) || t->target || t->rendered))
        CHECK(g, perf_transfer_finish(KV_PERF_TRANSFER_UPDATE));
    const uint64_t started = kv_perf_begin();
    for (uint32_t row = 0; row < height; ++row)
        memcpy((uint8_t *)t->pixels.base + ((y + row) * t->pitch + x) * t->bpp,
            data + row * stride, width * t->bpp);
    kv_perf_end(KV_PERF_UPDATE, started, (uint64_t)width * height * t->bpp, 0);
    t->last_use = t->last_read = t->last_write = 0;
    return 0;
}
/* Viewport/depth state cached by target size; reset by the present path, which sets a
   different viewport for the display surface. */
static uint32_t kv_bound_width, kv_bound_height;
void kv_reset_effect_state(void) { kv_bound_width = kv_bound_height = 0; }
/* Called by the present path: a new frame starts with an empty vertex ring. */
void kv_reset_effect_ring(KvGxm *g) { if (g->effects) g->effects->used = 0; }

static int effect_scene(KvGxm *g, KvGxmTexture *t) {
    CHECK(g, perf_begin_scene(g->context, scene_flags(g, t, KV_SCENE_OTHER), t->target, NULL, NULL, t->sync, &t->surface, &g->depth));
    g->scene_open = 1; g->current_target = t;
    if (g->effects && !(kv_render_options() & KV_OPT_BATCH_ARENA)) g->effects->used = 0;
    texture_write(g, t);
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
    CHECK(g, ensure_batch(g));
    /* The caller knows the frame's pass dependencies and asks for a drain only when this
       pass samples a target an earlier pass wrote (measured: ~20% of passes). Removing all
       of them black-screened part of the game (r51); keeping all of them wasted ~66 ms of
       driver round trips per 39-pass frame. */
    if (finish_before && !async_submission(g)) {
        const uint64_t t0 = kv_probe_now();
        perf_finish(g->context, KV_PERF_FINISH_TARGET);
        kv_probe_add(&kv_gxm_timing_counters.finish_us, &kv_gxm_timing_counters.finish_calls, t0);
    }
    CHECK(g, effect_scene(g, t));
    if (clear) {
        /* A clear may have nonzero RGB with zero alpha: avoid shader premultiplication. */
        if (!(kv_render_options() & KV_OPT_BATCH_ARENA)) g->quads = 0;
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
    int result = end_submitted_scene(g);
    kv_probe_add(&kv_gxm_timing_counters.end_scene_us, &kv_gxm_timing_counters.end_scene_calls, end_start);
    if (result < 0) { g->closing = 1; return fail(g, "end offscreen scene", result); }
    g->current_target->rendered = 1;
    g->scene_open = 0; g->current_target = NULL;
    /* No duplicate drain at EndScene. Legacy consumers retain their original
       Finish; asynchronous consumers are linked by fragment -> vertex scene
       dependencies and CPU reuse is guarded by fragment completion. */
    return 0;
}
static int packed_integer(float value, float low, float high) {
    return value >= low && value <= high && truncf(value) == value;
}
static int effect_inputs(const KvEffectVertex *v, uint32_t count, uint32_t shader, uint32_t blend,
    const float params[8]) {
    if (!v || !params || !count || count % 3 || count > EFFECT_VERTICES
        || shader >= EFFECT_SHADERS || blend >= EFFECT_BLENDS || shader == 24
        || (effect_fetch_shader(shader) && blend != 5)
        || ((shader == 18 || shader == 19 || shader == 23) && params[0] != 19.0f)
        || (shader == 20 && params[0] != 11.0f)
        || (shader == 21 && params[0] != 13.0f)
        || (shader == 25 && params[0] != 2.0f)) return KV_BAD_ARGUMENT;
    for (uint32_t i = 0; i < 8; ++i)
        if (!isfinite(params[i]) || (shader == 22 && params[i] != 0)) return KV_BAD_ARGUMENT;
    if (((shader >= 19 && shader <= 21) || shader == 25) && (params[1] != 0 || params[4] >= 0 || params[6] != 0
        || params[7] != (shader == 19 ? 1.0f : 0.0f)))
        return KV_BAD_ARGUMENT;
    if (shader == 23 && (!packed_integer(params[1],0,1) || !packed_integer(params[6],0,1)
        || params[4] >= 0 || params[5] != 0 || params[7] != 0)) return KV_BAD_ARGUMENT;
    for (uint32_t i = 0; i < count; ++i) {
        /* The struct has only floats and its ABI is asserted, but pointer
           arithmetic may not legally walk across distinct struct members. */
        float floats[28];
        memcpy(floats, &v[i], sizeof(floats));
        for (uint32_t j = 0; j < 28; ++j) if (!isfinite(floats[j])) return KV_BAD_ARGUMENT;
        for (uint32_t row = 0; row < 2; ++row)
            for (uint32_t j = 0; j < 4; ++j)
                if (v[i].haze[row][j] != v[0].haze[row][j]) return KV_BAD_ARGUMENT;
        if (shader >= 19) {
            // Specialised entries have an exact no-haze/no-soft ABI.
            // Refuse a direct FFI caller which would silently lose haze/soft.
            for (uint32_t row = 0; row < 2; ++row)
                for (uint32_t j = 0; j < 4; ++j)
                    if (v[i].haze[row][j] != 0) return KV_BAD_ARGUMENT;
            for (uint32_t row = 0; row < 3; ++row)
                for (uint32_t j = 0; j < 4; ++j)
                    if (v[i].soft[row][j] != 0) return KV_BAD_ARGUMENT;
        }
        if (v[i].soft[2][0] < 0) return KV_BAD_ARGUMENT;
        if (v[i].soft[2][0] > 0 && (!packed_integer(v[i].soft[2][1],0,17)
            || !packed_integer(v[i].soft[2][2],1,256))) return KV_BAD_ARGUMENT;
    }
    if (shader == 5 && (!packed_integer(v[0].haze[1][3],0,16777215)
        || v[0].haze[1][2] < 0 || v[0].haze[1][2] > 1)) return KV_BAD_ARGUMENT;
    if ((shader == 9 || (shader == 8 && params[2] > 0.5f))
        && !packed_integer(params[1],8,2047)) return KV_BAD_ARGUMENT;
    if (effect_ps_shader(shader) && (!packed_integer(params[0],1,24)
        || (params[4] >= 0 && !packed_integer(params[5],8,2047)))) return KV_BAD_ARGUMENT;
    /* tex2 is either a rule or a same-layout group mask, never both. */
    if (effect_ps_shader(shader) && (!packed_integer(params[7],0,1)
        || (params[7] == 1 && (params[4] >= 0 || params[6] != 0)))) return KV_BAD_ARGUMENT;
    if ((shader == 9 || effect_ps_shader(shader)) && (params[2] <= 0 || params[3] <= 0)) return KV_BAD_ARGUMENT;
    if (shader == 10 && (!packed_integer(params[3],0,2147483520.0f)
        || !packed_integer(params[4],0,2))) return KV_BAD_ARGUMENT;
    return 0;
}
static int32_t draw_effect_inner(KvGxm *g, const KvEffectVertex *vertices, uint32_t count,
    uint32_t shader, uint32_t blend, const KvGxmTexture *const input[4], const float params[8]) {
    if (!active(g) || !g->scene_open || !input) return KV_STATE;
    uint64_t phase = kv_perf_begin();
    const int inputs = effect_inputs(vertices,count,shader,blend,params);
    kv_perf_end(KV_PERF_DRAW_VALIDATE, phase, (uint64_t)count * sizeof(*vertices), inputs < 0);
    if (inputs < 0) return KV_BAD_ARGUMENT;
    const KvGxmTexture *textures[4];
    for (uint32_t i = 0; i < 4; ++i) {
        textures[i] = input[i] ? input[i] : g->white;
        if (!texture_owned(g, textures[i]) || textures[i] == g->current_target) return KV_BAD_ARGUMENT;
    }
    for (uint32_t i = 0; i < 4; ++i) CHECK(g, require_scene_read(g, textures[i]));
    CHECK(g, effects_ensure(g));
    KvEffects *e = g->effects;
    if (!(kv_render_options() & KV_OPT_BATCH_ARENA) && count > EFFECT_VERTICES - e->used)
        return fail(g, "effects scene vertex budget", KV_BUDGET);
    uint32_t layout = EFFECT_STREAM_FULL;
    if ((kv_render_options() & KV_OPT_BATCH_ARENA) && (e->compact_vertex || e->constant_vertex)) {
        layout = effect_stream_layout(vertices, count);
        if (layout == EFFECT_STREAM_ZERO && !e->compact_vertex)
            layout = e->constant_vertex ? EFFECT_STREAM_CONSTANT : EFFECT_STREAM_FULL;
        if (layout == EFFECT_STREAM_CONSTANT && !e->constant_vertex) layout = EFFECT_STREAM_FULL;
    }
    const uint32_t stream_bytes = effect_stream_bytes(count, layout);
    void *stream_vertices = NULL;
    CHECK(g, batch_vertices(g, stream_bytes, &stream_vertices));
    phase = kv_perf_begin();
    const int program = effects_program(g, shader, blend);
    kv_perf_end(KV_PERF_DRAW_PROGRAM, phase, 0, program < 0);
    CHECK(g, program);
    phase = kv_perf_begin();
    const SceGxmProgram *fs = bound_effect_source(e, shader);
    sceGxmSetVertexProgram(g->context, layout == EFFECT_STREAM_CONSTANT ? e->constant_vertex
        : layout == EFFECT_STREAM_ZERO ? e->compact_vertex : e->vertex);
    sceGxmSetFragmentProgram(g->context, e->fragment[shader][blend][output_mode(g)]);
    for (uint32_t i = 0; i < 4; ++i)
        CHECK(g, sceGxmSetFragmentTexture(g->context, i, &textures[i]->descriptor));
    kv_perf_end(KV_PERF_DRAW_BIND, phase, 0, 0);
    phase = kv_perf_begin();
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
    kv_perf_end(KV_PERF_DRAW_UNIFORM, phase, 0, 0);
    KvEffectVertex *destination = stream_vertices;
    if (!(kv_render_options() & KV_OPT_BATCH_ARENA))
        destination = (KvEffectVertex *)e->vertices.base + e->used;
    phase = kv_perf_begin();
    effect_stream_write(destination, vertices, count, layout);
    kv_perf_end(KV_PERF_VERTEX_COPY, phase, stream_bytes, 0);
    if (layout) kv_perf_count(KV_PERF_COMPACT_VERTEX, stream_bytes);
    /* Split the CPU cost of one draw from the submit call itself. */
    const uint64_t cpu_start = kv_probe_now();
    CHECK(g, sceGxmSetVertexStream(g->context, 0, destination));
    if (layout) CHECK(g, sceGxmSetVertexStream(g->context, 1,
        layout == EFFECT_STREAM_CONSTANT ? (uint8_t *)destination + count * sizeof(Vertex)
            : (uint8_t *)g->indices.base + 16));
    kv_gxm_timing_counters.draw_cpu_us += kv_probe_now() - cpu_start;
    const uint64_t gxm_start = kv_probe_now();
    phase = kv_perf_begin();
    for (uint32_t i = 0; i < 4; ++i) texture_use(g, textures[i]);
    const int draw_result = sceGxmDraw(g->context, SCE_GXM_PRIMITIVE_TRIANGLES, SCE_GXM_INDEX_FORMAT_U16, e->indices.base, count);
    kv_perf_end(KV_PERF_DRAW_SUBMIT, phase, 0, draw_result < 0);
    CHECK(g, draw_result);
    if (effect_ps_shader(shader)) scene_ps_use(g, (uint32_t)params[0]);
    kv_gxm_timing_counters.draw_gxm_us += kv_probe_now() - gxm_start;
    ++kv_gxm_timing_counters.draw_calls;
    e->used += count; ++g->stats.draws;
    return 0;
}
int32_t kv_gxm_draw_effect(KvGxm *g, const KvEffectVertex *vertices, uint32_t count,
    uint32_t shader, uint32_t blend, const KvGxmTexture *const input[4], const float params[8]) {
    const uint64_t started = kv_perf_begin();
    const int result = draw_effect_inner(g, vertices, count, shader, blend, input, params);
    const uint64_t finished = kv_perf_begin();
    kv_perf_end_at(KV_PERF_DRAW_TOTAL, started, finished, 0, result < 0);
    if (effect_ps_shader(shader)) kv_perf_end_at(KV_PERF_DRAW_PS, started, finished, 0, result < 0);
    if (result == 0) {
        if (shader == 19) kv_perf_end_at(KV_PERF_DRAW_PS_GROUP19, started, finished, 0, 0);
        if (shader == 20) kv_perf_end_at(KV_PERF_DRAW_PS11, started, finished, 0, 0);
        if (shader == 21) kv_perf_end_at(KV_PERF_DRAW_PS13, started, finished, 0, 0);
        if (shader == 22) kv_perf_end_at(KV_PERF_DRAW_PLAIN_SPRITE, started, finished, 0, 0);
        if (shader == 23) kv_perf_end_at(KV_PERF_DRAW_PS19_PLAIN, started, finished, 0, 0);
        if (shader == 25) kv_perf_end_at(KV_PERF_DRAW_PS2, started, finished, 0, 0);
    }
    if (blend == 8) kv_perf_end_at(KV_PERF_DRAW_MASK_ALPHA, started, finished, 0, result < 0);
    if (shader == 15 || shader == 16) kv_perf_end_at(KV_PERF_DRAW_GROUP, started, finished, 0, result < 0);
    if (shader == 11) kv_perf_end_at(KV_PERF_DRAW_ALTER, started, finished, 0, result < 0);
    if (shader == 10) kv_perf_end_at(KV_PERF_DRAW_BLUR, started, finished, 0, result < 0);
    return result;
}
int32_t kv_gxm_copy_texture(KvGxm *g, const KvGxmTexture *source, KvGxmTexture *destination) {
    if (!active(g) || g->scene_open || source == destination
        || !texture_owned(g,source) || !texture_owned(g,destination)
        || source->storage != KV_TEXTURE_LINEAR || destination->storage != KV_TEXTURE_LINEAR
        || source->width != destination->width || source->height != destination->height
        || (source->bpp != 1 && source->bpp != 4)
        || source->bpp != destination->bpp || source->srgb != destination->srgb) return KV_BAD_ARGUMENT;
    const uint64_t copy_started = kv_perf_begin();
    CHECK(g, ensure_batch(g));
    /* A rendered copy stays on the same ordered GXM context. A transfer uses a
       different engine, so it still requires both producer and transfer drains. */
    if (!async_submission(g) || !(source->target || source->rendered)) {
        if (async_submission(g)) CHECK(g, wait_submitted(g, KV_PERF_FINISH_COPY));
        else perf_finish(g->context, KV_PERF_FINISH_COPY);
        CHECK(g, perf_transfer_finish(KV_PERF_TRANSFER_COPY_PRE));
    }
    if (source->target || source->rendered) {
        /* Keep rendered pixels on the GPU. Some runtimes leave the CPU backing
           stale after Finish; a transfer from that backing loses the backdrop.
           quad_f samples raw RGBA, including alpha, with no premultiplication;
           matching texture/surface gamma modes preserve the stored encoding.
           Sealing a baked image drops its target but retains this GPU origin. */
        const int target_result = texture_target_ensure(g, destination);
        if (target_result < 0) return target_result;
        const uint32_t kind = g->plan_ready && g->plan_target == destination ? g->plan_kind : KV_SCENE_OTHER;
        const KvGxmTexture *read[1] = {source};
        CHECK(g, kv_gxm_scene_plan(g, destination, read, 1, kind));
        CHECK(g, effect_scene(g, destination));
        if (!(kv_render_options() & KV_OPT_BATCH_ARENA)) g->quads = 0;
        const int draw = kv_gxm_draw_quad(g, source, (float[]){0,0,WIDTH,CONTENT_H},
            (float[]){0,0,1,1}, (float[]){1,1,1,1}, 0);
        /* End a begun scene even if submission failed; failed EndScene keeps a
           closing owner so its GPU resources cannot be reused prematurely. */
        const int end = kv_gxm_end_target(g);
        kv_perf_end(KV_PERF_COPY_QUAD, copy_started, (uint64_t)source->width * source->height * source->bpp, end < 0 || draw < 0);
        return end < 0 ? end : draw;
    }
    const SceGxmTransferFormat format = source->bpp == 4 ?
        SCE_GXM_TRANSFER_FORMAT_U8U8U8U8_ABGR : SCE_GXM_TRANSFER_FORMAT_U8_R;
    CHECK(g, sceGxmTransferCopy(source->width, source->height, 0, 0, SCE_GXM_TRANSFER_COLORKEY_NONE,
        format, SCE_GXM_TRANSFER_LINEAR, source->pixels.base, 0, 0, source->pitch * source->bpp,
        format, SCE_GXM_TRANSFER_LINEAR, destination->pixels.base, 0, 0, destination->pitch * destination->bpp,
        NULL, 0, NULL));
    CHECK(g, perf_transfer_finish(KV_PERF_TRANSFER_COPY_POST));
    destination->last_use = destination->last_read = destination->last_write = 0;
    g->plan_ready = 0;
    kv_perf_end(KV_PERF_COPY_TRANSFER, copy_started, (uint64_t)source->width * source->height * source->bpp, 0);
    return 0;
}
static int32_t present_canvas_inner(KvGxm *g, const KvGxmTexture *canvas) {
    if (!active(g) || g->scene_open || !texture_owned(g,canvas) || canvas->bpp != 4) return KV_STATE;
    CHECK(g, effects_ensure(g));
    const KvGxmTexture *read[1] = {canvas};
    CHECK(g, kv_gxm_scene_plan(g, NULL, read, 1, KV_SCENE_PRESENT));
    CHECK(g, kv_gxm_begin(g, (float[]){0,0,0,1}));
    /* Native encoded color: one sampled copy, with no presentation curve.
       Use the existing five-instruction basic FS and its matching vertex ABI. */
    CHECK(g, kv_gxm_draw_quad(g, canvas, (float[]){0,0,WIDTH,CONTENT_H},
        (float[]){0,0,1,1}, (float[]){1,1,1,1}, 0));
    return kv_gxm_present(g);
}
int32_t kv_gxm_present_canvas(KvGxm *g, const KvGxmTexture *canvas) {
    const uint64_t started = kv_perf_begin();
    const int result = present_canvas_inner(g, canvas);
    kv_perf_end(KV_PERF_PRESENT_TOTAL, started, 0, result < 0);
    return result;
}
int32_t kv_gxm_texture_read(KvGxm *g, const KvGxmTexture *texture, uint8_t *data, uint32_t capacity) {
    if (!active(g) || g->scene_open || !texture_owned(g,texture) || !data
        || texture->storage != KV_TEXTURE_LINEAR || (texture->bpp != 1 && texture->bpp != 4)
        || capacity < texture->width * texture->height * texture->bpp) return KV_BAD_ARGUMENT;
    if (async_submission(g)) CHECK(g, wait_submitted(g, KV_PERF_FINISH_READBACK));
    else perf_finish(g->context, KV_PERF_FINISH_READBACK);
    CHECK(g, perf_transfer_finish(KV_PERF_TRANSFER_READBACK));
    const uint64_t copy_started = kv_perf_begin();
    for (uint32_t y = 0; y < texture->height; ++y)
        memcpy(data + y * texture->width * texture->bpp,
            (uint8_t *)texture->pixels.base + y * texture->pitch * texture->bpp,
            texture->width * texture->bpp);
    kv_perf_end(KV_PERF_READBACK, copy_started, (uint64_t)texture->width * texture->height * texture->bpp, 0);
    return 0;
}
