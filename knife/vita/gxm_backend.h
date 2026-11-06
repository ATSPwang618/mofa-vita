#ifndef KNIFE_VITA_GXM_BACKEND_H
#define KNIFE_VITA_GXM_BACKEND_H
#include <stdint.h>

typedef struct KvGxm KvGxm;
typedef struct KvGxmTexture KvGxmTexture;
enum KvSceneKind { KV_SCENE_OTHER = 0, KV_SCENE_STAGE = 1, KV_SCENE_GROUP_PIXEL = 2,
    KV_SCENE_GROUP_MASK = 3, KV_SCENE_SCRATCH = 4, KV_SCENE_CANVAS = 5,
    KV_SCENE_COPY_PS = 6, KV_SCENE_COPY_GROUP = 7, KV_SCENE_PRESENT = 8, KV_SCENE_BAKE = 9 };
/* Same C ABI as knife_render_api::Vertex; positions are clip coordinates. */
typedef struct KvEffectVertex {
    float position[2], uv[2], color[4], haze[2][4], soft[3][4];
} KvEffectVertex;

/* Cumulative GPU-sync accounting. All values are microseconds or counts on the
   single GXM owner thread; a zero means the site never ran. */
typedef struct KvGxmTiming {
    uint64_t finish_us, finish_calls;
    uint64_t end_scene_us, end_scene_calls;
    uint64_t display_finish_us, display_finish_calls;
    uint64_t target_create_us, target_create_calls;
    /* Pass and draw submission breakdown: where the CPU time actually goes. */
    uint64_t begin_target_us, begin_target_calls;
    uint64_t clear_us;
    uint64_t draw_cpu_us, draw_calls;
    uint64_t draw_gxm_us;
} KvGxmTiming;

typedef struct KvGxmStats {
    uint32_t main_mapped_bytes, cdram_mapped_bytes, parameter_reserved_bytes;
    uint32_t frames, draws, program_checks, display_width, display_height;
    uint32_t content_width, content_height, content_y, display_stride;
} KvGxmStats;

/* All operations run on the creating thread. Coordinates are 960x540 pixels.
   Basic straight-alpha RGBA8 only; unsupported game effects are not accepted.
   Caller owns encoded/decoded buffers; texture upload copies before returning. */
/* On failed initialization, non-null *out is a retained cleanup-only owner. */
int32_t kv_gxm_create(KvGxm **out, char *error, uint32_t error_capacity);
/* Optional runtime program passed compiler output/metadata checks at startup. */
int32_t kv_gxm_shader_available(KvGxm *g, uint32_t shader);
/* Exact startup-patched, cost-admitted material pair; no compilation on draw. */
int32_t kv_gxm_chain_pair_available(KvGxm *g, uint32_t first, uint32_t second);
int32_t kv_gxm_texture_create(KvGxm *g, const uint8_t *rgba, uint32_t width,
    uint32_t height, uint32_t stride, KvGxmTexture **out);
int32_t kv_gxm_texture_destroy(KvGxm *g, KvGxmTexture *texture);
/* Borrows an AvPlayer GPU-mapped YVU420P2 frame. No CPU layout is inferred.
   Caller keeps frame allocation alive through the final GXM submission and finish. */
int32_t kv_gxm_movie_texture_create(KvGxm *g, const void *frame, uint32_t width,
    uint32_t height, KvGxmTexture **out);
int32_t kv_gxm_movie_texture_rebind(KvGxm *g, KvGxmTexture *texture,
    const void *frame, uint32_t width, uint32_t height);
int32_t kv_gxm_movie_gpu_finish(KvGxm *g);
/* format: 0=RGBA8, 1=R8. Image decoding has its own, smaller size bound.
   Generic allocations admit 1..2048 per axis, subject to the native budget.
   data=NULL creates a zeroed allocation. RTs require RGBA8. */
int32_t kv_gxm_texture_create_ex(KvGxm *g, const uint8_t *data, uint32_t width,
    uint32_t height, uint32_t stride, uint32_t format, uint32_t srgb,
    uint32_t linear, uint32_t render_target, KvGxmTexture **out);
/* Immutable, already encoded/swizzled single-level texture. format is a full
   GXM PVRT4BPP_ABGR, UBC1_ABGR or UBC3_ABGR enum, not create_ex's format ID.
   Logical dimensions are 1..2048; length must include next-power-of-two
   backing (minimum 8x8 PVRTC or 4x4 DXT). Copies synchronously to owned CDRAM.
   Compressed textures cannot be CPU-updated/read, copied or used as RTs. */
int32_t kv_gxm_texture_create_compressed(KvGxm *g, const uint8_t *data,
    uint32_t length, uint32_t width, uint32_t height, uint32_t format,
    uint32_t srgb, uint32_t linear, KvGxmTexture **out);
int32_t kv_gxm_texture_update(KvGxm *g, KvGxmTexture *texture, uint32_t x,
    uint32_t y, uint32_t width, uint32_t height, const uint8_t *data, uint32_t stride);
/* finish_before: 1 drains the GPU before starting (needed when this pass samples a
   target written earlier in the same frame); 0 skips the driver round trip. */
int32_t kv_gxm_begin_target(KvGxm *g, KvGxmTexture *texture, const float *clear, int32_t finish_before);
int32_t kv_gxm_end_target(KvGxm *g);
/* After a completed bake, release RT/DEV-sync attachments while retaining the
   sampled texture and its pixels. No active scene or displayed DEV target. */
int32_t kv_gxm_texture_seal(KvGxm *g, KvGxmTexture *texture);
/* Triangle-list vertices follow the selected shader ABI: ordinary uniforms are
   equal across the draw; RimText28 has per-triangle hx/xyz and shared RGB24. IDs match
   Rust EffectShader / EffectBlend; params is two complete float4 uniforms. */
/* Copies one <=33-element F32 group; the next BlurWeights draw validates and consumes it. */
int32_t kv_gxm_set_blur_weights(KvGxm *g, float sigma, uint32_t half_taps,
    const float *weights, uint32_t count);
int32_t kv_gxm_draw_effect(KvGxm *g, const KvEffectVertex *vertices, uint32_t count,
    uint32_t shader, uint32_t blend, const KvGxmTexture *const textures[4], const float params[8]);
int32_t kv_gxm_copy_texture(KvGxm *g, const KvGxmTexture *source, KvGxmTexture *destination);
int32_t kv_gxm_present_canvas(KvGxm *g, const KvGxmTexture *canvas);
int32_t kv_gxm_timing(KvGxm *g, KvGxmTiming *out);
int32_t kv_gxm_texture_read(KvGxm *g, const KvGxmTexture *texture, uint8_t *data, uint32_t capacity);
/* Call once before a recorded batch, while no scene is open. Counts include
   clear/copy/present basic quads; allocation is capped and safely segmented. */
int32_t kv_gxm_begin_batch(KvGxm *g, uint32_t basic_quads, uint32_t effect_vertices);
/* Before Begin/Copy, provide all sampled textures for this one scene. Metadata
   is consumed synchronously; no pointer array is retained. NULL target=display.
   Omitted plans retain conservative ordering. Draw validates missed hazards. */
int32_t kv_gxm_scene_plan(KvGxm *g, KvGxmTexture *target,
    const KvGxmTexture *const *reads, uint32_t count, uint32_t kind);
/* Small pooled slices charge 64-byte granules; >=256 KiB payloads own whole
   256 KiB-aligned pages so their tail cannot pin large retired image pages. */
int32_t kv_gxm_texture_charge(uint32_t payload_bytes, uint32_t *out);
/* Explicit memory-pressure reclamation; wait=0 only polls completed owners. */
int32_t kv_gxm_reclaim(KvGxm *g, uint32_t wait);
int32_t kv_gxm_begin(KvGxm *g, const float clear[4]);
/* rect={x,y,width,height}; uv={left,top,right,bottom}; texture=NULL uses white. */
int32_t kv_gxm_draw_quad(KvGxm *g, const KvGxmTexture *texture,
    const float rect[4], const float uv[4], const float color[4], uint32_t alpha_blend);
int32_t kv_gxm_present(KvGxm *g);
/* DEV-only direct scanout isolates RT drawing from texture sampling. The
   960x544 non-sRGB target stays owned until normal present or backend close.
   Returns an error when built without KV_GXM_DEV_DIAGNOSTICS. */
int32_t kv_gxm_dev_scanout_target(KvGxm *g, KvGxmTexture *texture);
/* DEV-only crossover: exchange equal-sized target objects, retaining both
   owners and their color surfaces/storage. Isolates target creation from RT data. */
int32_t kv_gxm_dev_swap_display_target(KvGxm *g, KvGxmTexture *texture);
/* DEV-only crossover of equal-sized color surfaces and their backing blocks.
   Only the unused back buffer may be exchanged; descriptor/owners stay valid. */
int32_t kv_gxm_dev_swap_display_surface(KvGxm *g, KvGxmTexture *texture);
/* Copies the completed front buffer, tightly packed RGBA8, 960x544. */
int32_t kv_gxm_readback(KvGxm *g, uint8_t *rgba, uint32_t capacity);
int32_t kv_gxm_stats(const KvGxm *g, KvGxmStats *out);
/* Creating thread only. Includes fully empty pool pages reclaimable before
   growth and the largest contiguous reusable span, without summing live-page
   holes. New mapping remains bounded by both policy and kernel free bytes.
   A query/owner/accounting failure returns an error, never a
   fabricated allowance; no reservation against concurrent OS users is implied. */
int32_t kv_gxm_cdram_available(const KvGxm *g, uint32_t *out);
/* Last texture create failed specifically at CDRAM budget/kernel allocation.
   Validation, mapping, descriptor and unsafe rollback errors are not retryable. */
uint32_t kv_gxm_texture_allocation_failed(const KvGxm *g);
const char *kv_gxm_error(const KvGxm *g);
/* Drains GPU/display work before retiring resources. Failure retains the owner
   for another destroy call only; all rendering is disabled after the attempt. */
int32_t kv_gxm_destroy(KvGxm *g);
#endif
