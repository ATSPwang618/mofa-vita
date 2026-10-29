#include "gxm_backend.h"
#include "jpeg_hw.h"
#include "shaders/generated/shaders.h"
#include <psp2/display.h>
#include <psp2/gxm.h>
#include <psp2/kernel/processmgr.h>
#include <psp2/kernel/sysmem.h>
#include <psp2/kernel/threadmgr.h>
#include <math.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { WIDTH = 960, HEIGHT = 544, CONTENT_H = 540, CONTENT_Y = 2,
    /* Three scanout buffers, as the reference Vita renderer uses: with two, the display
       queue wait dominated `present` once rendering became cheap. */
    STRIDE = 960, BUFFERS = 3, MAX_QUADS = 1024, PATCHER_BYTES = 64 * 1024,
    /* Leave bounded CDRAM headroom for the independently serialized JPEG engine. */
    CDRAM_LIMIT = 64 * 1024 * 1024 - KV_JPEG_HW_CDRAM_RESERVE,
    MAIN_MAP_LIMIT = 16 * 1024 * 1024 };
enum { KV_BAD_ARGUMENT = -1, KV_STATE = -2, KV_BUDGET = -3, KV_ALLOC = -4 };
/* Defined in effects_backend.inc: invalidates the cached pass state and the vertex ring. */
void kv_reset_effect_state(void);
void kv_reset_effect_ring(KvGxm *g);
enum { KV_TEXTURE_LINEAR = 0, KV_TEXTURE_MOVIE = 1, KV_TEXTURE_COMPRESSED = 2 };
enum { OUTPUT_MODES = 2 };

/* Timing probes: the GXM owner thread is single, so plain counters are enough. */
static KvGxmTiming kv_gxm_timing_counters;
static uint64_t kv_probe_now(void) { return sceKernelGetProcessTimeWide(); }
static void kv_probe_add(uint64_t *total, uint64_t *calls, uint64_t start) {
    *total += kv_probe_now() - start;
    *calls += 1;
}
typedef struct Block { SceUID uid; void *base; uint32_t size, offset, mapping, cdram, accounted; } Block;
#ifdef KV_GXM_DEV_DIAGNOSTICS
static void native_trace(const char *stage, const Block *block) {
    FILE *log = fopen("ux0:data/knife-woh-dev/gxm-native.log", "a");
    if (!log) return;
    if (block) fprintf(log, "%s: uid=%d address=%p bytes=%u mapping=%u cdram=%u\n",
        stage, block->uid, block->base, block->size, block->mapping, block->cdram);
    else fprintf(log, "%s\n", stage);
    /* Closing also publishes stages through Vita3K's buffered host FILE layer. */
    fclose(log);
}
#define TRACE(stage) native_trace(stage, NULL)
#define TRACE_BLOCK(stage, block) native_trace(stage, block)
#else
#define TRACE(stage) ((void)0)
#define TRACE_BLOCK(stage, block) ((void)0)
#endif
typedef struct Vertex { float position[2], uv[2], color[4]; } Vertex;
typedef struct KvEffects KvEffects;
static int effects_destroy(KvGxm *g);
static int texture_target_destroy(KvGxm *g, KvGxmTexture *texture);
_Static_assert(sizeof(Vertex) == 32, "basic vertex ABI");
_Static_assert(sizeof(KvEffectVertex) == 112, "effects vertex ABI");
_Static_assert(sizeof(KvGxmStats) == 48, "stats ABI");
struct KvGxmTexture {
    struct KvGxmTexture *next;
    KvGxm *owner;
    Block pixels;
    SceGxmTexture descriptor;
    SceGxmRenderTarget *target;
    SceGxmSyncObject *sync;
    SceGxmColorSurface surface;
    uint32_t width, height, pitch, bpp, srgb, storage;
    int ready, rendered;
};
typedef struct DisplayData { void *address; volatile int *error; } DisplayData;
struct KvGxm {
    SceUID thread;
    int initialized, scene_open, closing, scanout_submitted;
    char error[128];
    volatile int display_error;
    SceGxmContext *context;
    void *context_host;
    SceGxmRenderTarget *target;
    SceGxmShaderPatcher *patcher;
    SceGxmShaderPatcherId vertex_id, fragment_id;
    SceGxmVertexProgram *vertex_program;
    SceGxmFragmentProgram *opaque_program, *alpha_program;
    SceGxmFragmentProgram *srgb_opaque_program, *srgb_alpha_program;
    SceGxmColorSurface surfaces[BUFFERS];
    SceGxmDepthStencilSurface depth;
    SceGxmSyncObject *sync[BUFFERS];
    Block display[BUFFERS], vdm, vertex_ring, fragment_ring, fragment_usse;
    Block patcher_buffer, vertex_usse, patcher_usse, vertices, indices;
    KvGxmTexture *textures, *white;
    KvGxmTexture *current_target;
#ifdef KV_GXM_DEV_DIAGNOSTICS
    KvGxmTexture *diagnostic_scanout;
#endif
    KvEffects *effects;
    uint32_t back, front, quads;
    KvGxmStats stats;
};

#ifdef KV_GXM_DEV_DIAGNOSTICS
static void native_texture_trace(const KvGxm *g, const KvGxmTexture *t) {
    char line[192];
    snprintf(line, sizeof(line), "texture: created uid=%d width=%u height=%u bpp=%u target=%u cdram_used=%u",
        t->pixels.uid, t->width, t->height, t->bpp, t->target != NULL, g->stats.cdram_mapped_bytes);
    TRACE(line);
}
#define TRACE_TEXTURE(g, texture) native_texture_trace(g, texture)
#else
#define TRACE_TEXTURE(g, texture) ((void)0)
#endif

static int fail(KvGxm *g, const char *stage, int code) {
    snprintf(g->error, sizeof(g->error), "%.112s: 0x%08x", stage, (unsigned int)code);
    return code < 0 ? code : KV_STATE;
}
static int owner_ok(const KvGxm *g) {
    return g && g->thread == sceKernelGetThreadId();
}
static int active(const KvGxm *g) { return owner_ok(g) && !g->closing; }
static int texture_owned(const KvGxm *g, const KvGxmTexture *texture) {
    for (const KvGxmTexture *t = g->textures; t; t = t->next)
        if (t == texture) return t->ready;
    return 0;
}
#define CHECK(g, expression) do { int result_ = (expression); \
    if (result_ < 0) return fail(g, #expression, result_); } while (0)

/* RGBA8 storage is always four bytes per pixel. Native GXM gamma conversion
   requires 64-bit output registers (HALF4); display/linear surfaces use UCHAR4.
   Cache these separately so switching destinations cannot reuse the wrong ABI. */
static uint32_t output_mode(const KvGxm *g) {
    return g->current_target && g->current_target->srgb ? 1U : 0U;
}
static int quad_program(KvGxm *g, uint32_t alpha, SceGxmFragmentProgram **out) {
    const uint32_t srgb = output_mode(g);
    SceGxmFragmentProgram **cached = srgb ?
        (alpha ? &g->srgb_alpha_program : &g->srgb_opaque_program) :
        (alpha ? &g->alpha_program : &g->opaque_program);
    if (!*cached) {
        const SceGxmBlendInfo blend = { .colorMask = SCE_GXM_COLOR_MASK_ALL,
            .colorFunc = SCE_GXM_BLEND_FUNC_ADD, .alphaFunc = SCE_GXM_BLEND_FUNC_ADD,
            .colorSrc = SCE_GXM_BLEND_FACTOR_SRC_ALPHA, .colorDst = SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            .alphaSrc = SCE_GXM_BLEND_FACTOR_ONE, .alphaDst = SCE_GXM_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA };
        CHECK(g, sceGxmShaderPatcherCreateFragmentProgram(g->patcher, g->fragment_id,
            srgb ? SCE_GXM_OUTPUT_REGISTER_FORMAT_HALF4 : SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4,
            SCE_GXM_MULTISAMPLE_NONE, alpha ? &blend : NULL,
            (const SceGxmProgram *)knife_quad_v_gxp, cached));
    }
    *out = *cached;
    return 0;
}

/* Explicit memory classes: no implicit fallback from exhausted CDRAM to RAM. */
static int block_free(KvGxm *g, Block *b);
static int block_alloc(KvGxm *g, Block *b, uint32_t requested, int cdram, uint32_t mapping) {
    const uint32_t alignment = cdram ? 256 * 1024 : 4096;
    if (!requested || requested > UINT32_MAX - alignment + 1) return KV_BAD_ARGUMENT;
    b->size = (requested + alignment - 1) & ~(alignment - 1);
    b->cdram = cdram;
    uint32_t *used = cdram ? &g->stats.cdram_mapped_bytes : &g->stats.main_mapped_bytes;
    const uint32_t reserved = cdram ? g->stats.parameter_reserved_bytes : 0;
    const uint32_t limit = cdram ? CDRAM_LIMIT : MAIN_MAP_LIMIT;
    if (reserved > limit || b->size > limit - reserved || *used > limit - reserved - b->size) return KV_BUDGET;
    b->uid = sceKernelAllocMemBlock("knife-gxm", cdram ?
        SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW : SCE_KERNEL_MEMBLOCK_TYPE_USER_RW_UNCACHE,
        b->size, NULL);
    if (b->uid < 0) return b->uid;
    *used += b->size;
    b->accounted = 1;
    int result = sceKernelGetMemBlockBase(b->uid, &b->base);
    if (result < 0) {
        const int cleanup = block_free(g, b);
        if (cleanup < 0) { g->closing = 1; return fail(g, "memory base rollback retained owner", cleanup); }
        return result;
    }
    if (mapping == 2) result = sceGxmMapVertexUsseMemory(b->base, b->size, &b->offset);
    else if (mapping == 3) result = sceGxmMapFragmentUsseMemory(b->base, b->size, &b->offset);
    else result = sceGxmMapMemory(b->base, b->size, SCE_GXM_MEMORY_ATTRIB_READ | SCE_GXM_MEMORY_ATTRIB_WRITE);
    if (result < 0) {
        const int cleanup = block_free(g, b);
        if (cleanup < 0) { g->closing = 1; return fail(g, "memory mapping rollback retained owner", cleanup); }
        return result;
    }
    b->mapping = mapping;
    TRACE_BLOCK("block: allocated", b);
    return 0;
}
static int block_free(KvGxm *g, Block *b) {
    if (b->uid <= 0) return 0;
    if (b->mapping) {
        TRACE_BLOCK("block: before unmap", b);
        int result;
        if (b->mapping == 2) result = sceGxmUnmapVertexUsseMemory(b->base);
        else if (b->mapping == 3) result = sceGxmUnmapFragmentUsseMemory(b->base);
        else result = sceGxmUnmapMemory(b->base);
        if (result < 0) return result;
        TRACE_BLOCK("block: after unmap", b);
        b->mapping = 0;
    }
    TRACE_BLOCK("block: before free", b);
    int result = sceKernelFreeMemBlock(b->uid);
    if (result < 0) return result;
    TRACE_BLOCK("block: after free", b);
    uint32_t *used = b->cdram ? &g->stats.cdram_mapped_bytes : &g->stats.main_mapped_bytes;
    if (b->accounted) *used -= b->size;
    memset(b, 0, sizeof(*b));
    return 0;
}
static void *patcher_alloc(void *user, unsigned int size) { (void)user; return malloc(size); }
static void patcher_free(void *user, void *memory) { (void)user; free(memory); }
static void display_callback(const void *data) {
    const DisplayData *display = data;
    SceDisplayFrameBuf frame = { .size = sizeof(frame), .base = display->address,
        .pitch = STRIDE, .pixelformat = SCE_DISPLAY_PIXELFORMAT_A8B8G8R8,
        .width = WIDTH, .height = HEIGHT };
    int result = sceDisplaySetFrameBuf(&frame, SCE_DISPLAY_SETBUF_NEXTFRAME);
    if (result >= 0) result = sceDisplayWaitVblankStart();
    if (result < 0) *display->error = result;
}

static int initialize(KvGxm *g) {
    const SceGxmInitializeParams init = { .flags = SCE_GXM_INITIALIZE_FLAG_DEFAULT,
        .displayQueueMaxPendingCount = 1, .displayQueueCallback = display_callback,
        .displayQueueCallbackDataSize = sizeof(DisplayData),
        .parameterBufferSize = SCE_GXM_DEFAULT_PARAMETER_BUFFER_SIZE };
    CHECK(g, sceGxmInitialize(&init));
    g->initialized = 1;
    g->stats.parameter_reserved_bytes = init.parameterBufferSize;
    CHECK(g, block_alloc(g, &g->vdm, SCE_GXM_DEFAULT_VDM_RING_BUFFER_SIZE, 0, 1));
    CHECK(g, block_alloc(g, &g->vertex_ring, SCE_GXM_DEFAULT_VERTEX_RING_BUFFER_SIZE, 0, 1));
    CHECK(g, block_alloc(g, &g->fragment_ring, SCE_GXM_DEFAULT_FRAGMENT_RING_BUFFER_SIZE, 0, 1));
    CHECK(g, block_alloc(g, &g->fragment_usse, SCE_GXM_DEFAULT_FRAGMENT_USSE_RING_BUFFER_SIZE, 0, 3));
    g->context_host = calloc(1, SCE_GXM_MINIMUM_CONTEXT_HOST_MEM_SIZE);
    if (!g->context_host) return fail(g, "context host allocation", KV_ALLOC);
    const SceGxmContextParams context = {
        .hostMem = g->context_host, .hostMemSize = SCE_GXM_MINIMUM_CONTEXT_HOST_MEM_SIZE,
        .vdmRingBufferMem = g->vdm.base, .vdmRingBufferMemSize = g->vdm.size,
        .vertexRingBufferMem = g->vertex_ring.base, .vertexRingBufferMemSize = g->vertex_ring.size,
        .fragmentRingBufferMem = g->fragment_ring.base, .fragmentRingBufferMemSize = g->fragment_ring.size,
        .fragmentUsseRingBufferMem = g->fragment_usse.base,
        .fragmentUsseRingBufferMemSize = g->fragment_usse.size,
        .fragmentUsseRingBufferOffset = g->fragment_usse.offset };
    CHECK(g, sceGxmCreateContext(&context, &g->context));
    const SceGxmRenderTargetParams target = { .width = WIDTH, .height = HEIGHT,
        .scenesPerFrame = 1, .multisampleMode = SCE_GXM_MULTISAMPLE_NONE, .driverMemBlock = -1 };
    CHECK(g, sceGxmCreateRenderTarget(&target, &g->target));
    for (uint32_t i = 0; i < BUFFERS; ++i) {
        CHECK(g, block_alloc(g, &g->display[i], STRIDE * HEIGHT * 4, 1, 1));
        uint32_t *pixels = g->display[i].base;
        for (uint32_t p = 0; p < STRIDE * HEIGHT; ++p) pixels[p] = 0xff000000;
        CHECK(g, sceGxmColorSurfaceInit(&g->surfaces[i], SCE_GXM_COLOR_FORMAT_A8B8G8R8,
            SCE_GXM_COLOR_SURFACE_LINEAR, SCE_GXM_COLOR_SURFACE_SCALE_NONE,
            SCE_GXM_OUTPUT_REGISTER_SIZE_32BIT, WIDTH, HEIGHT, STRIDE, pixels));
        CHECK(g, sceGxmSyncObjectCreate(&g->sync[i]));
    }
    CHECK(g, sceGxmDepthStencilSurfaceInitDisabled(&g->depth));
    CHECK(g, block_alloc(g, &g->patcher_buffer, PATCHER_BYTES, 0, 1));
    CHECK(g, block_alloc(g, &g->vertex_usse, PATCHER_BYTES, 0, 2));
    CHECK(g, block_alloc(g, &g->patcher_usse, PATCHER_BYTES, 0, 3));
    const SceGxmShaderPatcherParams patcher = {
        .hostAllocCallback = patcher_alloc, .hostFreeCallback = patcher_free,
        .bufferMem = g->patcher_buffer.base, .bufferMemSize = g->patcher_buffer.size,
        .vertexUsseMem = g->vertex_usse.base, .vertexUsseMemSize = g->vertex_usse.size,
        .vertexUsseOffset = g->vertex_usse.offset,
        .fragmentUsseMem = g->patcher_usse.base, .fragmentUsseMemSize = g->patcher_usse.size,
        .fragmentUsseOffset = g->patcher_usse.offset };
    CHECK(g, sceGxmShaderPatcherCreate(&patcher, &g->patcher));
    const SceGxmProgram *vs = (const SceGxmProgram *)knife_quad_v_gxp;
    const SceGxmProgram *fs = (const SceGxmProgram *)knife_quad_f_gxp;
    CHECK(g, sceGxmProgramCheck(vs));
    CHECK(g, sceGxmProgramCheck(fs));
    g->stats.program_checks = 2;
    CHECK(g, sceGxmShaderPatcherRegisterProgram(g->patcher, vs, &g->vertex_id));
    CHECK(g, sceGxmShaderPatcherRegisterProgram(g->patcher, fs, &g->fragment_id));
    const char *names[] = { "aPosition", "aTexcoord", "aColor" };
    const uint16_t offsets[] = { offsetof(Vertex, position), offsetof(Vertex, uv), offsetof(Vertex, color) };
    const uint8_t components[] = { 2, 2, 4 };
    SceGxmVertexAttribute attributes[3] = {0};
    for (uint32_t i = 0; i < 3; ++i) {
        const SceGxmProgramParameter *p = sceGxmProgramFindParameterByName(vs, names[i]);
        /* Cg may widen vertex attributes to vec4 in GXP metadata. The stream
           supplies only the source's used components (xy, uv, rgba). */
        if (!p || sceGxmProgramParameterGetCategory(p) != SCE_GXM_PARAMETER_CATEGORY_ATTRIBUTE
            || sceGxmProgramParameterGetComponentCount(p) < components[i]
            || sceGxmProgramParameterGetComponentCount(p) > 4) {
            snprintf(g->error, sizeof(g->error), "%s parameter: present=%d category=%d components=%u expected=%u",
                names[i], p != NULL, p ? (int)sceGxmProgramParameterGetCategory(p) : -1,
                p ? sceGxmProgramParameterGetComponentCount(p) : 0, components[i]);
            return KV_BAD_ARGUMENT;
        }
        attributes[i] = (SceGxmVertexAttribute){ .streamIndex = 0, .offset = offsets[i],
            .format = SCE_GXM_ATTRIBUTE_FORMAT_F32, .componentCount = components[i],
            .regIndex = sceGxmProgramParameterGetResourceIndex(p) };
    }
    const SceGxmProgramParameter *sampler = sceGxmProgramFindParameterByName(fs, "uTexture");
    if (!sampler || sceGxmProgramParameterGetCategory(sampler) != SCE_GXM_PARAMETER_CATEGORY_SAMPLER
        || sceGxmProgramParameterGetResourceIndex(sampler) != 0)
        return fail(g, "fragment sampler TEXUNIT0", KV_BAD_ARGUMENT);
    const SceGxmVertexStream stream = { .stride = sizeof(Vertex), .indexSource = SCE_GXM_INDEX_SOURCE_INDEX_16BIT };
    CHECK(g, sceGxmShaderPatcherCreateVertexProgram(g->patcher, g->vertex_id,
        attributes, 3, &stream, 1, &g->vertex_program));
    int program_result = quad_program(g, 0, &g->opaque_program);
    if (program_result < 0) return program_result;
    program_result = quad_program(g, 1, &g->alpha_program);
    if (program_result < 0) return program_result;
    CHECK(g, block_alloc(g, &g->vertices, MAX_QUADS * 4 * sizeof(Vertex), 0, 1));
    CHECK(g, block_alloc(g, &g->indices, 6 * sizeof(uint16_t), 0, 1));
    const uint16_t indices[] = { 0, 1, 2, 2, 1, 3 };
    memcpy(g->indices.base, indices, sizeof(indices));
    const uint8_t white[] = {255, 255, 255, 255};
    int result = kv_gxm_texture_create(g, white, 1, 1, 4, &g->white);
    if (result < 0) return result; /* Preserve the exact failing SDK operation. */
    g->back = 1;
    g->stats.display_width = WIDTH; g->stats.display_height = HEIGHT;
    g->stats.content_width = WIDTH; g->stats.content_height = CONTENT_H;
    g->stats.content_y = CONTENT_Y; g->stats.display_stride = STRIDE;
    return 0;
}

int32_t kv_gxm_create(KvGxm **out, char *error, uint32_t capacity) {
    if (!out || !error || !capacity) return KV_BAD_ARGUMENT;
    *out = NULL; error[0] = 0;
    KvGxm *g = calloc(1, sizeof(*g));
    if (!g) { snprintf(error, capacity, "backend allocation"); return KV_ALLOC; }
    g->thread = sceKernelGetThreadId();
    int result = initialize(g);
    if (result < 0) {
        snprintf(error, capacity, "%s", g->error);
        int cleanup = kv_gxm_destroy(g);
        if (cleanup < 0) {
            /* A non-null failure output owns cleanup only; it cannot render. */
            *out = g;
            snprintf(error, capacity, "initialization failed; cleanup retained GPU owner: 0x%08x", (unsigned)cleanup);
        }
        return result;
    }
    *out = g;
    return 0;
}

int32_t kv_gxm_texture_create(KvGxm *g, const uint8_t *rgba, uint32_t width,
    uint32_t height, uint32_t stride, KvGxmTexture **out) {
    if (!active(g) || !out || !rgba || !width || width > WIDTH || !height || height > 1080
        || stride < width * 4 || stride > 4 * WIDTH || g->scene_open) return KV_BAD_ARGUMENT;
    *out = NULL;
    KvGxmTexture *t = calloc(1, sizeof(*t));
    if (!t) return fail(g, "texture owner allocation", KV_ALLOC);
    const uint32_t pitch = (width + 7) & ~7U;
    t->width = width; t->height = height; t->pitch = pitch; t->bpp = 4;
    int result = block_alloc(g, &t->pixels, pitch * height * 4, 1, 1);
    if (result < 0) {
        if (t->pixels.uid > 0) {
            t->owner = g; t->next = g->textures; g->textures = t;
            g->closing = 1;
        } else free(t);
        return fail(g, "texture CDRAM allocation", result);
    }
    t->owner = g; t->next = g->textures; g->textures = t;
    memset(t->pixels.base, 0, t->pixels.size);
    for (uint32_t y = 0; y < height; ++y)
        memcpy((uint8_t *)t->pixels.base + y * pitch * 4, rgba + y * stride, width * 4);
    /* LINEAR uses an eight-pixel row alignment. LINEAR_STRIDED stores stride
       bits in the min-filter field and rejects sceGxmTextureSetMinFilter. */
    const char *stage = "sceGxmTextureInitLinear";
    result = sceGxmTextureInitLinear(&t->descriptor, t->pixels.base,
        SCE_GXM_TEXTURE_FORMAT_A8B8G8R8, width, height, 0);
#define TEXTURE_STEP(label, expression) do { if (result >= 0) { stage = label; result = (expression); } } while (0)
    TEXTURE_STEP("sceGxmTextureSetMinFilter", sceGxmTextureSetMinFilter(&t->descriptor, SCE_GXM_TEXTURE_FILTER_POINT));
    TEXTURE_STEP("sceGxmTextureSetMagFilter", sceGxmTextureSetMagFilter(&t->descriptor, SCE_GXM_TEXTURE_FILTER_POINT));
    TEXTURE_STEP("sceGxmTextureSetUAddrMode", sceGxmTextureSetUAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
    TEXTURE_STEP("sceGxmTextureSetVAddrMode", sceGxmTextureSetVAddrMode(&t->descriptor, SCE_GXM_TEXTURE_ADDR_CLAMP));
#undef TEXTURE_STEP
    if (result < 0) {
        const int cleanup = block_free(g, &t->pixels);
        if (cleanup < 0) {
            g->closing = 1; /* Keep the failed allocation reachable for cleanup. */
            return fail(g, "texture initialization rollback retained owner", cleanup);
        }
        g->textures = t->next;
        free(t);
        return fail(g, stage, result);
    }
    t->ready = 1;
    TRACE_TEXTURE(g, t);
    *out = t;
    return 0;
}
int32_t kv_gxm_texture_destroy(KvGxm *g, KvGxmTexture *t) {
    if (!active(g) || !t || !texture_owned(g, t) || t == g->white || g->scene_open) return KV_STATE;
#ifdef KV_GXM_DEV_DIAGNOSTICS
    if (g->diagnostic_scanout == t) return fail(g, "DEV target is still scanned out", KV_STATE);
#endif
    sceGxmFinish(g->context);
    KvGxmTexture **entry = &g->textures;
    while (*entry && *entry != t) entry = &(*entry)->next;
    if (!*entry) return KV_BAD_ARGUMENT;
    t->ready = 0;
    int target_result = texture_target_destroy(g, t);
    if (target_result < 0) { g->closing = 1; return target_result; }
    int result = block_free(g, &t->pixels);
    if (result < 0) { g->closing = 1; return fail(g, "texture release retained owner", result); }
    *entry = t->next; free(t);
    return 0;
}
int32_t kv_gxm_begin(KvGxm *g, const float clear[4]) {
    if (!active(g) || g->scene_open || !clear) return KV_STATE;
    if (g->display_error < 0) return fail(g, "display callback", g->display_error);
    /* Baseline serial submission: arenas are reused only after GPU completion. */
    sceGxmFinish(g->context);
    CHECK(g, sceGxmBeginScene(g->context, 0, g->target, NULL, NULL,
        g->sync[g->back], &g->surfaces[g->back], &g->depth));
    g->scene_open = 1; g->quads = 0; g->current_target = NULL;
    /* A new frame starts with an empty effect vertex ring; the Finish above drained the
       previous frame's readers. */
    kv_reset_effect_ring(g);
    kv_reset_effect_state();
    sceGxmSetViewport(g->context, WIDTH / 2.0f, WIDTH / 2.0f,
        CONTENT_Y + CONTENT_H / 2.0f, -CONTENT_H / 2.0f, 0.5f, 0.5f);
    sceGxmSetCullMode(g->context, SCE_GXM_CULL_NONE);
    sceGxmSetFrontDepthFunc(g->context, SCE_GXM_DEPTH_FUNC_ALWAYS);
    sceGxmSetBackDepthFunc(g->context, SCE_GXM_DEPTH_FUNC_ALWAYS);
    sceGxmSetFrontDepthWriteEnable(g->context, SCE_GXM_DEPTH_WRITE_DISABLED);
    sceGxmSetBackDepthWriteEnable(g->context, SCE_GXM_DEPTH_WRITE_DISABLED);
    const float rect[4] = {0, 0, WIDTH, CONTENT_H}, uv[4] = {0, 0, 1, 1};
    return kv_gxm_draw_quad(g, NULL, rect, uv, clear, 0);
}
int32_t kv_gxm_draw_quad(KvGxm *g, const KvGxmTexture *t, const float rect[4],
    const float uv[4], const float color[4], uint32_t alpha) {
    if (!active(g) || !g->scene_open || !rect || !uv || !color || alpha > 1) return KV_STATE;
    if (g->quads >= MAX_QUADS) return fail(g, "per-frame vertex arena", KV_BUDGET);
    if (!t) t = g->white;
    if (!texture_owned(g, t)) return KV_BAD_ARGUMENT;
    for (uint32_t i = 0; i < 4; ++i)
        if (!isfinite(rect[i]) || !isfinite(uv[i]) || !isfinite(color[i])) return KV_BAD_ARGUMENT;
    SceGxmFragmentProgram *program = NULL;
    const int program_result = quad_program(g, alpha, &program);
    if (program_result < 0) return program_result;
    Vertex *v = (Vertex *)g->vertices.base + g->quads * 4;
    for (uint32_t i = 0; i < 4; ++i) {
        v[i].position[0] = (rect[0] + (i & 1 ? rect[2] : 0)) * (2.0f / WIDTH) - 1.0f;
        v[i].position[1] = 1.0f - (rect[1] + (i & 2 ? rect[3] : 0)) * (2.0f / CONTENT_H);
        v[i].uv[0] = uv[i & 1 ? 2 : 0]; v[i].uv[1] = uv[i & 2 ? 3 : 1];
        memcpy(v[i].color, color, sizeof(v[i].color));
    }
    sceGxmSetVertexProgram(g->context, g->vertex_program);
    sceGxmSetFragmentProgram(g->context, program);
    CHECK(g, sceGxmSetVertexStream(g->context, 0, v));
    CHECK(g, sceGxmSetFragmentTexture(g->context, 0, &t->descriptor));
    CHECK(g, sceGxmDraw(g->context, SCE_GXM_PRIMITIVE_TRIANGLES,
        SCE_GXM_INDEX_FORMAT_U16, g->indices.base, 6));
    ++g->quads; ++g->stats.draws;
    return 0;
}
int32_t kv_gxm_present(KvGxm *g) {
    if (!active(g) || !g->scene_open || g->current_target) return KV_STATE;
    /* A failed submission may already have changed GPU/display ownership.
       Preserve the owner for teardown; never reuse an unconfirmed back buffer. */
#define SUBMIT(expression) do { int result_ = (expression); \
    if (result_ < 0) { g->closing = 1; return fail(g, #expression, result_); } } while (0)
    { const uint64_t t0 = kv_probe_now();
      SUBMIT(sceGxmEndScene(g->context, NULL, NULL));
      kv_probe_add(&kv_gxm_timing_counters.end_scene_us, &kv_gxm_timing_counters.end_scene_calls, t0); }
    g->scene_open = 0;
    SUBMIT(sceGxmPadHeartbeat(&g->surfaces[g->back], g->sync[g->back]));
    const DisplayData data = {g->display[g->back].base, &g->display_error};
    /* Retire scanout even when callback installation succeeds but its wait fails. */
    g->scanout_submitted = 1;
    SUBMIT(sceGxmDisplayQueueAddEntry(g->sync[g->front], g->sync[g->back], &data));
    /* The display entry needs this frame's GPU work retired before the queue can
       withdraw the buffer; EndScene passes no notifications, so the Finish here is
       what makes the scanout signal progress. Removing it black-screens. */
    { const uint64_t t0 = kv_probe_now();
      sceGxmFinish(g->context);
      kv_probe_add(&kv_gxm_timing_counters.finish_us, &kv_gxm_timing_counters.finish_calls, t0); }
    { const uint64_t t0 = kv_probe_now();
      SUBMIT(sceGxmDisplayQueueFinish());
      kv_probe_add(&kv_gxm_timing_counters.display_finish_us,
          &kv_gxm_timing_counters.display_finish_calls, t0); }
#undef SUBMIT
    if (g->display_error < 0) { g->closing = 1; return fail(g, "display callback", g->display_error); }
    g->front = g->back; g->back = (g->back + 1) % BUFFERS;
#ifdef KV_GXM_DEV_DIAGNOSTICS
    g->diagnostic_scanout = NULL;
#endif
    ++g->stats.frames;
    return 0;
}
int32_t kv_gxm_dev_scanout_target(KvGxm *g, KvGxmTexture *t) {
    if (!active(g) || g->scene_open) return KV_STATE;
#ifdef KV_GXM_DEV_DIAGNOSTICS
    if (!t || !texture_owned(g, t) || !t->target || t->bpp != 4 || t->srgb
        || t->width != WIDTH || t->height != HEIGHT || t->pitch != STRIDE) return KV_BAD_ARGUMENT;
    sceGxmFinish(g->context);
    CHECK(g, sceGxmDisplayQueueFinish());
    /* Normal offscreen textures need no display sync. Only this DEV direct
       scanout path hands one to the display queue. */
    if (!t->sync) CHECK(g, sceGxmSyncObjectCreate(&t->sync));
    CHECK(g, sceGxmPadHeartbeat(&t->surface, t->sync));
    /* Keep this target reachable through the native owner until scanout is
       replaced or destroy detaches it. Never free displayed memory early. */
    g->scanout_submitted = 1;
    g->diagnostic_scanout = t;
    const DisplayData data = {t->pixels.base, &g->display_error};
    CHECK(g, sceGxmDisplayQueueAddEntry(g->sync[g->front], t->sync, &data));
    CHECK(g, sceGxmDisplayQueueFinish());
    if (g->display_error < 0) return fail(g, "DEV display callback", g->display_error);
    return 0;
#else
    (void)t;
    return fail(g, "DEV diagnostics are disabled", KV_STATE);
#endif
}
int32_t kv_gxm_dev_swap_display_target(KvGxm *g, KvGxmTexture *t) {
    if (!active(g) || g->scene_open) return KV_STATE;
#ifdef KV_GXM_DEV_DIAGNOSTICS
    if (!t || !texture_owned(g, t) || !t->target || t->width != WIDTH
        || t->height != HEIGHT || t->pitch != STRIDE || t->bpp != 4)
        return KV_BAD_ARGUMENT;
    sceGxmFinish(g->context);
    CHECK(g, sceGxmDisplayQueueFinish());
    SceGxmRenderTarget *previous = g->target;
    g->target = t->target;
    t->target = previous;
    return 0;
#else
    (void)t;
    return fail(g, "DEV diagnostics are disabled", KV_STATE);
#endif
}
int32_t kv_gxm_dev_swap_display_surface(KvGxm *g, KvGxmTexture *t) {
    if (!active(g) || g->scene_open) return KV_STATE;
#ifdef KV_GXM_DEV_DIAGNOSTICS
    if (!t || !texture_owned(g, t) || !t->target || t->width != WIDTH
        || t->height != HEIGHT || t->pitch != STRIDE || t->bpp != 4 || t->srgb
        || g->back == g->front || g->diagnostic_scanout
        || t->pixels.size != g->display[g->back].size) return KV_BAD_ARGUMENT;
    sceGxmFinish(g->context);
    CHECK(g, sceGxmDisplayQueueFinish());
    SceGxmColorSurface *display = &g->surfaces[g->back];
    if (!sceGxmColorSurfaceIsEnabled(display) || !sceGxmColorSurfaceIsEnabled(&t->surface)
        || sceGxmColorSurfaceGetData(display) != g->display[g->back].base
        || sceGxmColorSurfaceGetData(&t->surface) != t->pixels.base
        || sceGxmColorSurfaceGetStrideInPixels(display) != STRIDE
        || sceGxmColorSurfaceGetStrideInPixels(&t->surface) != STRIDE) return KV_STATE;
    TRACE_BLOCK("DEV surface crossover: display before", &g->display[g->back]);
    TRACE_BLOCK("DEV surface crossover: texture before", &t->pixels);
    /* Change the sampler address before committing the infallible swaps. The
       currently scanned-out front buffer is never touched. Equal block sizes
       preserve accounting and each allocation retains exactly one owner. */
    CHECK(g, sceGxmTextureSetData(&t->descriptor, g->display[g->back].base));
    const Block previous_block = g->display[g->back];
    const SceGxmColorSurface previous_surface = *display;
    g->display[g->back] = t->pixels;
    *display = t->surface;
    t->pixels = previous_block;
    t->surface = previous_surface;
    TRACE_BLOCK("DEV surface crossover: display after", &g->display[g->back]);
    TRACE_BLOCK("DEV surface crossover: texture after", &t->pixels);
    return 0;
#else
    (void)t;
    return fail(g, "DEV diagnostics are disabled", KV_STATE);
#endif
}
int32_t kv_gxm_readback(KvGxm *g, uint8_t *rgba, uint32_t capacity) {
    if (!active(g) || g->scene_open || !g->stats.frames || !rgba || capacity < WIDTH * HEIGHT * 4) return KV_STATE;
    sceGxmFinish(g->context);
    CHECK(g, sceGxmDisplayQueueFinish());
    memcpy(rgba, g->display[g->front].base, WIDTH * HEIGHT * 4);
    return 0;
}
int32_t kv_gxm_stats(const KvGxm *g, KvGxmStats *out) {
    if (!g || !out) return KV_BAD_ARGUMENT;
    *out = g->stats; return 0;
}
int32_t kv_gxm_cdram_available(const KvGxm *g, uint32_t *out) {
    if (!out) return KV_BAD_ARGUMENT;
    *out = 0;
    if (!g) return KV_BAD_ARGUMENT;
    if (!active(g)) return KV_STATE;
    const uint32_t reserved = g->stats.parameter_reserved_bytes;
    const uint32_t used = g->stats.cdram_mapped_bytes;
    if (reserved > CDRAM_LIMIT || used > CDRAM_LIMIT - reserved) return KV_BUDGET;
    const uint32_t policy_available = CDRAM_LIMIT - reserved - used;
    SceKernelFreeMemorySizeInfo info = { .size = sizeof(info) };
    const int result = sceKernelGetFreeMemorySize(&info);
    if (result < 0) return result;
    if (info.size_cdram < 0) return KV_STATE;
    const uint32_t kernel_available = (uint32_t)info.size_cdram;
    *out = policy_available < kernel_available ? policy_available : kernel_available;
#ifdef KV_GXM_DEV_DIAGNOSTICS
    if (*out < 4 * 1024 * 1024) {
        char line[192];
        snprintf(line, sizeof(line), "cdram: pressure available=%u policy=%u kernel=%u used=%u reserved=%u",
            *out, policy_available, kernel_available, used, reserved);
        TRACE(line);
    }
#endif
    return 0;
}
const char *kv_gxm_error(const KvGxm *g) { return g ? g->error : "null GXM owner"; }

int32_t kv_gxm_destroy(KvGxm *g) {
    if (!g) return 0;
    if (!owner_ok(g)) return KV_STATE;
    TRACE("destroy: enter");
    g->closing = 1; /* A partial teardown may only be retried, never drawn through. */
    if (g->scene_open) {
        CHECK(g, sceGxmEndScene(g->context, NULL, NULL)); g->scene_open = 0;
    }
    TRACE("destroy: before GPU finish");
    if (g->context) sceGxmFinish(g->context);
    TRACE("destroy: before display queue finish");
    if (g->initialized) CHECK(g, sceGxmDisplayQueueFinish());
    TRACE("destroy: display queue finished");
    /* Stop scan-out before releasing a displayed allocation. */
    if (g->scanout_submitted) {
        TRACE("destroy: before scanout detach");
        CHECK(g, sceDisplaySetFrameBuf(NULL, SCE_DISPLAY_SETBUF_NEXTFRAME));
        TRACE("destroy: before vblank");
        CHECK(g, sceDisplayWaitVblankStart());
        TRACE("destroy: scanout detached");
        g->scanout_submitted = 0;
    }
    while (g->textures) {
        TRACE("destroy: before texture release");
        KvGxmTexture *t = g->textures;
        CHECK(g, texture_target_destroy(g, t));
        CHECK(g, block_free(g, &t->pixels)); g->textures = t->next; free(t);
    }
    TRACE("destroy: textures released");
    CHECK(g, effects_destroy(g));
    if (g->srgb_alpha_program) { CHECK(g, sceGxmShaderPatcherReleaseFragmentProgram(g->patcher, g->srgb_alpha_program)); g->srgb_alpha_program = NULL; }
    if (g->srgb_opaque_program) { CHECK(g, sceGxmShaderPatcherReleaseFragmentProgram(g->patcher, g->srgb_opaque_program)); g->srgb_opaque_program = NULL; }
    if (g->alpha_program) { CHECK(g, sceGxmShaderPatcherReleaseFragmentProgram(g->patcher, g->alpha_program)); g->alpha_program = NULL; }
    if (g->opaque_program) { CHECK(g, sceGxmShaderPatcherReleaseFragmentProgram(g->patcher, g->opaque_program)); g->opaque_program = NULL; }
    if (g->vertex_program) { CHECK(g, sceGxmShaderPatcherReleaseVertexProgram(g->patcher, g->vertex_program)); g->vertex_program = NULL; }
    if (g->fragment_id) { CHECK(g, sceGxmShaderPatcherUnregisterProgram(g->patcher, g->fragment_id)); g->fragment_id = NULL; }
    if (g->vertex_id) { CHECK(g, sceGxmShaderPatcherUnregisterProgram(g->patcher, g->vertex_id)); g->vertex_id = NULL; }
    if (g->patcher) { CHECK(g, sceGxmShaderPatcherDestroy(g->patcher)); g->patcher = NULL; }
    TRACE("destroy: programs released");
    TRACE("destroy: before render target release");
    if (g->target) { CHECK(g, sceGxmDestroyRenderTarget(g->target)); g->target = NULL; }
    TRACE("destroy: before context release");
    if (g->context) { CHECK(g, sceGxmDestroyContext(g->context)); g->context = NULL; }
    TRACE("destroy: target and context released");
    for (uint32_t i = 0; i < BUFFERS; ++i) {
        if (g->sync[i]) { CHECK(g, sceGxmSyncObjectDestroy(g->sync[i])); g->sync[i] = NULL; }
        CHECK(g, block_free(g, &g->display[i]));
    }
    Block *blocks[] = { &g->indices, &g->vertices, &g->patcher_usse, &g->vertex_usse,
        &g->patcher_buffer, &g->fragment_usse, &g->fragment_ring, &g->vertex_ring, &g->vdm };
    for (uint32_t i = 0; i < sizeof(blocks) / sizeof(blocks[0]); ++i) CHECK(g, block_free(g, blocks[i]));
    free(g->context_host); g->context_host = NULL;
    TRACE("destroy: mapped blocks released");
    if (g->initialized) { CHECK(g, sceGxmTerminate()); g->initialized = 0; }
    TRACE("destroy: terminated");
    free(g);
    return 0;
}

/* Cumulative GPU-sync accounting for the performance log. */
int32_t kv_gxm_timing(KvGxm *g, KvGxmTiming *out) {
    if (!active(g) || !out) return KV_BAD_ARGUMENT;
    *out = kv_gxm_timing_counters;
    return 0;
}

#include "effects_backend.inc"

#include "movie_gxm.inc"
