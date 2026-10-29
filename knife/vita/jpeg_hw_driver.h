#ifndef KNIFE_VITA_JPEG_HW_DRIVER_H
#define KNIFE_VITA_JPEG_HW_DRIVER_H
#include <stdint.h>
/* Portable mirror of the 0x34-byte SceJpegOutputInfo, so the admission and
 * cleanup state machine can also run with a hostile driver double under ASan. */
typedef struct KvJpegHwInfo {
    uint32_t color_space;
    uint16_t width, height;
    uint32_t output_size, temp_size, coeff_size;
    struct { uint32_t x, y; } pitch[4];
} KvJpegHwInfo;
int kv_jpeg_driver_init(void);
int kv_jpeg_driver_alloc(uint32_t bytes, void **base);
void kv_jpeg_driver_free(int block, void *base, uint32_t bytes);
int kv_jpeg_driver_info(const uint8_t *jpeg, uint32_t bytes, KvJpegHwInfo *out);
int kv_jpeg_driver_decode(const uint8_t *jpeg, uint32_t bytes, uint8_t *yuv,
    uint32_t yuv_bytes, void *coeff, uint32_t coeff_bytes);
int kv_jpeg_driver_csc(uint8_t *rgba, const uint8_t *yuv, uint32_t packed,
    uint32_t pitch, uint32_t sampling);
#endif
