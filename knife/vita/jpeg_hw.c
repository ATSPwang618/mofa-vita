#include "jpeg_hw.h"
#include "jpeg_hw_driver.h"
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

#define KV_STATS(X) \
    X(jpeg_hardware_candidates) X(jpeg_hardware_ok) X(jpeg_fallback_profile) \
    X(jpeg_fallback_busy) X(jpeg_fallback_budget) X(jpeg_fallback_init) \
    X(jpeg_fallback_alloc) X(jpeg_fallback_driver) X(jpeg_fallback_contract) \
    X(software_png_ok) X(software_jpeg_ok) X(hw_current_cdram) X(hw_peak_cdram) X(last_hw_error) \
    X(last_contract_mask) X(last_info_size) X(last_info_y_pitch) \
    X(last_info_chroma_pitch) X(last_info_color_space)
#define DECLARE_COUNTER(name) static _Atomic uint32_t name;
KV_STATS(DECLARE_COUNTER)
#undef DECLARE_COUNTER
#define INC(name) ((void)atomic_fetch_add_explicit(&(name), 1, memory_order_relaxed))

void kv_image_decode_stats(KvImageDecodeStats *out) {
    if (!out) return;
    /* Approximate cross-thread snapshot; counters are individually atomic. */
#define LOAD_COUNTER(name) out->name = atomic_load_explicit(&(name), memory_order_relaxed);
    KV_STATS(LOAD_COUNTER)
#undef LOAD_COUNTER
}
void kv_image_record_software(int png, int success) {
    if (success) {
        if (png) INC(software_png_ok); else INC(software_jpeg_ok);
    }
}

static uint32_t be16(const uint8_t *p) { return ((uint32_t)p[0] << 8) | p[1]; }
int kv_jpeg_hw_profile(const uint8_t *p, uint32_t n, KvJpegHwProfile *out) {
    if (!p || !out || n < 4 || n > KV_JPEG_HW_INPUT_LIMIT ||
        p[0] != 0xff || p[1] != 0xd8 || p[n-2] != 0xff || p[n-1] != 0xd9) return 0;
    KvJpegHwProfile shape = {0};
    uint32_t pos = 2; int dht = 0, sof = 0;
    while (pos < n) {
        if (p[pos++] != 0xff) return 0;
        while (pos < n && p[pos] == 0xff) ++pos;
        if (pos == n) return 0;
        uint8_t marker = p[pos++];
        if (marker == 0 || marker == 0xd8 || marker == 0xd9 || marker == 1 ||
            (marker >= 0xd0 && marker <= 0xd7) || n-pos < 2) return 0;
        uint32_t size = be16(p + pos);
        if (size < 2 || size > n-pos) return 0;
        const uint8_t *body = p + pos + 2; uint32_t len = size - 2;
        if (marker == 0xc0) {
            if (sof || len != 15 || body[0] != 8 || body[5] != 3 ||
                body[6] != 1 || body[9] != 2 || body[12] != 3 ||
                body[10] != 0x11 || body[13] != 0x11) return 0;
            shape.height = be16(body+1); shape.width = be16(body+3);
            if (shape.width < 64 || shape.width > 480 || shape.height < 64 || shape.height > 540) return 0;
            if (body[7] == 0x22) shape.sampling = 0x20202;
            else if (body[7] == 0x21) shape.sampling = 0x20201;
            else return 0;
            sof = 1;
        } else if (marker == 0xc4) dht = 1;
        else if (marker >= 0xc0 && marker <= 0xcf) return 0; /* progressive/arithmetic/etc. */
        else if (marker == 0xee && len >= 12 && !memcmp(body, "Adobe", 5) && body[11] != 1) return 0;
        else if (marker == 0xda) {
            if (!sof || !dht || len != 10 || body[0] != 3 || body[1] != 1 ||
                body[3] != 2 || body[5] != 3 || body[7] != 0 || body[8] != 63 || body[9] != 0) return 0;
            *out = shape; return 1;
        }
        pos += size;
    }
    return 0;
}

#if defined(__vita__) || defined(KV_JPEG_HW_MOCK)
static atomic_flag hardware_busy = ATOMIC_FLAG_INIT;
static int initialized; /* accessed only while hardware_busy is owned */
#define ALIGN_CDRAM(n) (((n) + 262143U) & ~262143U)
#define ALIGN64(n) (((n) + 63U) & ~63U)
typedef struct HwBlock { int uid; void *base; uint32_t size; } HwBlock;
static int block_create(HwBlock *b, uint32_t bytes) {
    b->uid = kv_jpeg_driver_alloc(bytes, &b->base);
    if (b->uid < 0) {
        atomic_store_explicit(&last_hw_error, (uint32_t)b->uid, memory_order_relaxed); return 0;
    }
    b->size = bytes;
    uint32_t now = atomic_load_explicit(&hw_current_cdram, memory_order_relaxed) + bytes;
    atomic_store_explicit(&hw_current_cdram, now, memory_order_relaxed);
    if (now > atomic_load_explicit(&hw_peak_cdram, memory_order_relaxed))
        atomic_store_explicit(&hw_peak_cdram, now, memory_order_relaxed);
    return 1;
}
static void block_release(HwBlock *b) {
    if (b->uid < 0) return;
    kv_jpeg_driver_free(b->uid, b->base, b->size);
    atomic_fetch_sub_explicit(&hw_current_cdram, b->size, memory_order_relaxed);
    b->uid = -1; b->base = NULL; b->size = 0;
}
static uint32_t info_contract_errors(const KvJpegHwProfile *p, const KvJpegHwInfo *i) {
    uint32_t w = i->pitch[0].x, h = i->pitch[0].y;
    uint32_t cy = p->sampling == 0x20202 ? 2 : 1;
    uint32_t errors = 0;
    if (i->width != p->width || i->height != p->height) errors |= 1;
    if (i->color_space != p->sampling) errors |= 2;
    if (w < p->width || h < p->height || w > 512 || h > 544 || (w % 16) || (h % (8*cy))) errors |= 4;
    /* Standard MJPEG aligns chroma rows independently to 16 bytes. For a
     * 464-byte Y pitch this is 240, so halving Y would reject valid output. */
    uint32_t chroma = ((w / 2) + 15U) & ~15U;
    if (i->pitch[1].x != chroma || i->pitch[2].x != chroma ||
        i->pitch[1].y != h/cy || i->pitch[2].y != h/cy) errors |= 8;
    if (i->pitch[3].x || i->pitch[3].y) errors |= 16;
    uint64_t planes = (uint64_t)w*h + 2*(uint64_t)i->pitch[1].x*i->pitch[1].y;
    if (i->output_size < planes || i->output_size > 1024*1024) errors |= 32;
    if (i->coeff_size > 1024*1024) errors |= 64;
    return errors;
}
static void record_contract(const KvJpegHwInfo *i, uint32_t mask) {
    atomic_store_explicit(&last_info_size, ((uint32_t)i->width << 16) | i->height, memory_order_relaxed);
    atomic_store_explicit(&last_info_y_pitch, (i->pitch[0].x << 16) | i->pitch[0].y, memory_order_relaxed);
    atomic_store_explicit(&last_info_chroma_pitch, (i->pitch[1].x << 16) | i->pitch[1].y, memory_order_relaxed);
    atomic_store_explicit(&last_info_color_space, i->color_space, memory_order_relaxed);
    atomic_store_explicit(&last_contract_mask, mask, memory_order_relaxed);
}
#endif

int kv_jpeg_hw_try(const uint8_t *jpeg, uint32_t bytes, KvDecodedImage *out, uint32_t max_rgba) {
#if defined(__vita__) || defined(KV_JPEG_HW_MOCK)
    KvJpegHwProfile profile = {0};
    if (!out || out->pixels) return 0;
    if (!kv_jpeg_hw_profile(jpeg, bytes, &profile)) { INC(jpeg_fallback_profile); return 0; }
    INC(jpeg_hardware_candidates);
    uint32_t rgba_bytes = profile.width * profile.height * 4;
    if (rgba_bytes > max_rgba) { INC(jpeg_fallback_budget); return 0; }
    /* Foreground demand may coincide with background prefetch. Never spin,
     * block the renderer, or let two callers share the global JPEG engine. */
    if (atomic_flag_test_and_set_explicit(&hardware_busy, memory_order_acquire)) {
        INC(jpeg_fallback_busy); return 0;
    }
    int success = 0, rc;
    HwBlock stream = {.uid = -1}, work = {.uid = -1};
    uint8_t *pixels = NULL;
    if (!initialized) {
        rc = kv_jpeg_driver_init(); initialized = rc < 0 ? -1 : 1;
        if (rc < 0) atomic_store_explicit(&last_hw_error, (uint32_t)rc, memory_order_relaxed);
    }
    if (initialized < 0) { INC(jpeg_fallback_init); goto cleanup; }
    uint32_t stream_bytes = ALIGN_CDRAM(bytes);
    if (!block_create(&stream, stream_bytes)) { INC(jpeg_fallback_alloc); goto cleanup; }
    memcpy(stream.base, jpeg, bytes);
    KvJpegHwInfo info = {0};
    rc = kv_jpeg_driver_info(stream.base, bytes, &info);
    if (rc < 0) goto driver_failure;
    uint32_t contract_errors = info_contract_errors(&profile, &info);
    if (contract_errors) {
        record_contract(&info, contract_errors); INC(jpeg_fallback_contract); goto cleanup;
    }
    uint32_t csc_offset = ALIGN64(info.output_size + info.coeff_size);
    uint32_t pitch_bytes = info.pitch[0].x * info.pitch[0].y * 4;
    uint32_t work_bytes = ALIGN_CDRAM(csc_offset + pitch_bytes);
    if (stream_bytes + work_bytes > KV_JPEG_HW_CDRAM_RESERVE ||
        stream_bytes + work_bytes + rgba_bytes > KV_IMAGE_WORKING_BUDGET) {
        INC(jpeg_fallback_budget); goto cleanup;
    }
    if (!block_create(&work, work_bytes)) { INC(jpeg_fallback_alloc); goto cleanup; }
    uint8_t *yuv = work.base, *rgba = yuv + csc_offset;
    rc = kv_jpeg_driver_decode(stream.base, bytes, yuv, info.output_size,
        info.coeff_size ? yuv + info.output_size : NULL, info.coeff_size);
    if (rc < 0) goto driver_failure;
    uint32_t packed = (info.pitch[0].x << 16) | info.pitch[0].y;
    if ((uint32_t)rc != packed) {
        record_contract(&info, 128); INC(jpeg_fallback_contract); goto cleanup;
    }
    block_release(&stream);
    rc = kv_jpeg_driver_csc(rgba, yuv, packed, info.pitch[0].x, info.color_space & 65535);
    if (rc < 0) goto driver_failure;
    pixels = malloc(rgba_bytes);
    if (!pixels) { INC(jpeg_fallback_alloc); goto cleanup; }
    for (uint32_t y = 0; y < profile.height; ++y)
        memcpy(pixels + y * profile.width * 4, rgba + y * info.pitch[0].x * 4, profile.width * 4);
    *out = (KvDecodedImage){profile.width, profile.height, profile.width*4, rgba_bytes, pixels};
    pixels = NULL; INC(jpeg_hardware_ok); success = 1; goto cleanup;
driver_failure:
    atomic_store_explicit(&last_hw_error, (uint32_t)rc, memory_order_relaxed);
    INC(jpeg_fallback_driver);
cleanup:
    free(pixels); block_release(&work); block_release(&stream);
    atomic_flag_clear_explicit(&hardware_busy, memory_order_release);
    return success;
#else
    (void)jpeg; (void)bytes; (void)out; (void)max_rgba;
    return 0;
#endif
}

#if defined(__vita__)
#include <psp2/jpeg.h>
#include <psp2/kernel/sysmem.h>
_Static_assert(sizeof(KvJpegHwInfo) == sizeof(SceJpegOutputInfo), "JPEG output ABI");
_Static_assert(offsetof(SceJpegOutputInfo, pitch) == 20, "JPEG pitch ABI");
/* VitaSDK's jpeg.h currently places mode before output. The exported function
 * ABI used by libvita2d_sys and Vita3K has output, outputSize, mode in slots
 * 3..5. Use a private, correctly typed symbol alias; never edit global SDK files.
 * CSC's third argument is packed pitch dimensions, NOT a byte count.
 */
extern int kv_sce_jpeg_decode_abi(const uint8_t *, SceSize, uint8_t *, SceSize,
    SceInt32, void *, SceSize) __asm__("sceJpegDecodeMJpegYCbCr");
int kv_jpeg_driver_init(void) {
    /* Default CDRAM mode. No guessed LPDDR option and no ARM software module. */
    return sceJpegInitMJpeg(0);
}
int kv_jpeg_driver_alloc(uint32_t bytes, void **base) {
    int block = sceKernelAllocMemBlock("knife-jpeg-work", SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW, bytes, NULL);
    if (block < 0) return block;
    int rc = sceKernelGetMemBlockBase(block, base);
    if (rc < 0 || !*base) { sceKernelFreeMemBlock(block); return rc < 0 ? rc : -1; }
    return block;
}
void kv_jpeg_driver_free(int block, void *base, uint32_t bytes) {
    (void)base; (void)bytes; sceKernelFreeMemBlock(block);
}
int kv_jpeg_driver_info(const uint8_t *jpeg, uint32_t bytes, KvJpegHwInfo *out) {
    SceJpegOutputInfo native = {0};
    int rc = sceJpegGetOutputInfo(jpeg, bytes, -1, 0, &native);
    if (rc >= 0) memcpy(out, &native, sizeof(native));
    return rc;
}
int kv_jpeg_driver_decode(const uint8_t *jpeg, uint32_t bytes, uint8_t *yuv,
    uint32_t yuv_bytes, void *coeff, uint32_t coeff_bytes) {
    return kv_sce_jpeg_decode_abi(jpeg, bytes, yuv, yuv_bytes, 0, coeff, coeff_bytes);
}
int kv_jpeg_driver_csc(uint8_t *rgba, const uint8_t *yuv, uint32_t packed,
    uint32_t pitch, uint32_t sampling) {
    return sceJpegMJpegCsc(rgba, yuv, packed, (int)pitch, 0, (int)sampling);
}
#endif
