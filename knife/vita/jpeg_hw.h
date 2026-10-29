#ifndef KNIFE_VITA_JPEG_HW_H
#define KNIFE_VITA_JPEG_HW_H
#include "image_decode.h"

/* Dedicated temporary CDRAM, excluded from the renderer's allocation ceiling.
 * One hardware operation at a time; no image/stream cache is retained here. */
#define KV_JPEG_HW_CDRAM_RESERVE (3U * 1024U * 1024U)
#define KV_JPEG_HW_INPUT_LIMIT (512U * 1024U)
typedef struct KvJpegHwProfile {
    uint32_t width, height, sampling;
} KvJpegHwProfile;

/* Allocation-free, conservative admission for this port's 270p JPEGs.
 * This is a profile check, not a complete entropy-stream validator. */
int kv_jpeg_hw_profile(const uint8_t *jpeg, uint32_t bytes, KvJpegHwProfile *out);
/* Returns 1 only for an owned complete RGBA output, otherwise 0 for software
 * fallback after releasing all temporary blocks. Never publishes partial data. */
int kv_jpeg_hw_try(const uint8_t *jpeg, uint32_t bytes, KvDecodedImage *out, uint32_t max_rgba);
void kv_image_record_software(int png, int success);
#endif
