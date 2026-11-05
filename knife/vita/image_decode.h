#ifndef KNIFE_VITA_IMAGE_DECODE_H
#define KNIFE_VITA_IMAGE_DECODE_H

#include <stddef.h>
#include <stdint.h>

#define KV_IMAGE_ENCODED_LIMIT (32U * 1024U * 1024U)
#define KV_IMAGE_WIDTH_LIMIT 960U
#define KV_IMAGE_HEIGHT_LIMIT 1080U
/* Whole RGBA output plus decoder allocations; malloc bookkeeping is external. */
#define KV_IMAGE_WORKING_BUDGET (4U * 1024U * 1024U + 256U * 1024U)

typedef enum KvImageResult {
    KV_IMAGE_OK = 0,
    KV_IMAGE_ARGUMENT = -1,
    KV_IMAGE_INPUT_LIMIT = -2,
    KV_IMAGE_FORMAT = -3,
    KV_IMAGE_DIMENSIONS = -4,
    KV_IMAGE_MALFORMED = -5,
    KV_IMAGE_MEMORY = -6,
    KV_IMAGE_UNSUPPORTED = -7,
    KV_IMAGE_BUDGET = -8
} KvImageResult;

typedef struct KvDecodedImage {
    uint32_t width, height, stride, length;
    uint8_t *pixels;
} KvDecodedImage;

/* Cumulative wrapping u32 counters. Current/peak cover JPEG temporary CDRAM,
 * not the whole process heap. last_hw_error preserves the driver's u32 bits. */
typedef struct KvImageDecodeStats {
    uint32_t jpeg_hardware_candidates, jpeg_hardware_ok, jpeg_fallback_profile;
    uint32_t jpeg_fallback_busy, jpeg_fallback_budget, jpeg_fallback_init;
    uint32_t jpeg_fallback_alloc, jpeg_fallback_driver, jpeg_fallback_contract;
    uint32_t software_png_ok, software_jpeg_ok;
    uint32_t hw_current_cdram, hw_peak_cdram, last_hw_error;
    /* Last rejected hardware contract (bits: size=1, colour=2, Y=4,
     * chroma=8, plane4=16, output=32, coeff=64, decode pitch=128).
     * Packed sizes/pitches put width in the high 16 bits, height in low. */
    uint32_t last_contract_mask, last_info_size, last_info_y_pitch;
    uint32_t last_info_chroma_pitch, last_info_color_space;
} KvImageDecodeStats;

#ifdef __cplusplus
extern "C" {
#endif

/*
 * out must be zero initialized or previously freed. On a decoding failure it
 * stays empty; an already-owned output is rejected without changing ownership.
 * encoded is borrowed only for this call. PNG/JPEG only, no resizing/transcoding.
 * Success owns a complete tightly packed, straight-alpha RGBA8 buffer. Both
 * 960x540 images and the existing 960x1080 language strips are admitted.
 */
int32_t kv_image_decode(const uint8_t *encoded, uint32_t length, KvDecodedImage *out);
/* A smaller per-job output cap, checked at the codec header before pixels are allocated. */
int32_t kv_image_decode_bounded(const uint8_t *encoded, uint32_t length, KvDecodedImage *out, uint32_t max_rgba);
/* Only this function releases pixels. It clears all fields; NULL/repeated calls are safe. */
int32_t kv_image_decode_gray_bounded(const uint8_t *encoded, uint32_t length, KvDecodedImage *out, uint32_t max_bytes);
void kv_image_free(KvDecodedImage *image);
void kv_image_decode_stats(KvImageDecodeStats *out);
/* Allocation-free identity check for immutable offline image coverage metadata. */
int32_t kv_image_sha256(const uint8_t *bytes, uint32_t length, char hex[65]);

#ifdef __cplusplus
}
#endif

#endif
