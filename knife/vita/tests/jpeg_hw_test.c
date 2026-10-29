#include "jpeg_hw.h"
#include "jpeg_hw_driver.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Driver doubles exercise the actual hardware routing/cleanup code. They are
 * not a JPEG hardware emulator or evidence of console speed/colour accuracy. */
static int fail_at, allocations, frees, live, init_calls, info_calls, decode_calls;
static uint32_t current, peak;
static KvJpegHwInfo info;
int kv_jpeg_driver_init(void) { ++init_calls; return fail_at == 1 ? -101 : 0; }
int kv_jpeg_driver_alloc(uint32_t bytes, void **base) {
    ++allocations;
    if (fail_at == 2 || (fail_at == 4 && allocations % 2 == 0)) return -102;
    *base = malloc(bytes); assert(*base); ++live;
    current += bytes; if (current > peak) peak = current;
    return allocations;
}
void kv_jpeg_driver_free(int block, void *base, uint32_t bytes) {
    assert(block >= 0 && base && live > 0 && current >= bytes);
    free(base); ++frees; --live; current -= bytes;
}
int kv_jpeg_driver_info(const uint8_t *jpeg, uint32_t bytes, KvJpegHwInfo *out) {
    assert(jpeg[0] == 0xff && bytes > 20); ++info_calls; *out = info;
    return fail_at == 3 ? -103 : 0;
}
int kv_jpeg_driver_decode(const uint8_t *jpeg, uint32_t bytes, uint8_t *yuv,
    uint32_t yuv_bytes, void *coeff, uint32_t coeff_bytes) {
    assert(jpeg[0] == 0xff && bytes > 20 && yuv && yuv_bytes == info.output_size);
    assert(coeff == yuv + yuv_bytes && coeff_bytes == info.coeff_size);
    ++decode_calls;
    /* Reentrancy simulates foreground/background overlap: no spinning or
     * simultaneous second hardware job is permitted. */
    if (fail_at == 8) {
        KvDecodedImage other = {0};
        assert(kv_jpeg_hw_try(jpeg, bytes, &other, 1048576) == 0);
        assert(!other.pixels);
    }
    if (fail_at == 5) return -105;
    if (fail_at == 6) return (512 << 16) | 1088;
    memset(yuv, 0, yuv_bytes);
    return (int)((info.pitch[0].x << 16) | info.pitch[0].y);
}
int kv_jpeg_driver_csc(uint8_t *rgba, const uint8_t *yuv, uint32_t packed,
    uint32_t pitch, uint32_t sampling) {
    assert(yuv && packed == ((info.pitch[0].x << 16) | info.pitch[0].y));
    assert(pitch == info.pitch[0].x && sampling == (info.color_space & 65535));
    if (fail_at == 7) return -107;
    for (uint32_t y = 0; y < info.pitch[0].y; ++y)
        for (uint32_t x = 0; x < pitch; ++x) {
            uint8_t *p = rgba + (y * pitch + x) * 4;
            p[0] = (uint8_t)x; p[1] = (uint8_t)y; p[2] = 37; p[3] = 255;
        }
    return 0;
}
static uint8_t *load(const char *root, const char *name, uint32_t *size) {
    char path[4096]; snprintf(path, sizeof(path), "%s/%s", root, name);
    FILE *f = fopen(path, "rb"); assert(f); fseek(f, 0, SEEK_END);
    long len = ftell(f); assert(len > 0); rewind(f);
    uint8_t *p = malloc((size_t)len); assert(p);
    assert(fread(p, 1, (size_t)len, f) == (size_t)len); fclose(f); *size = (uint32_t)len; return p;
}
static void defaults(int sampling) {
    memset(&info, 0, sizeof(info));
    info.color_space = sampling == 2 ? 0x20202 : 0x20201;
    info.width = 479; info.height = 269;
    info.pitch[0].x = 480; info.pitch[0].y = sampling == 2 ? 272 : 272;
    info.pitch[1].x = info.pitch[2].x = 240;
    info.pitch[1].y = info.pitch[2].y = sampling == 2 ? 136 : 272;
    info.output_size = 480 * 272 + 2 * 240 * info.pitch[1].y;
    info.output_size = (info.output_size + 255) & ~255U;
    info.coeff_size = 256;
}
int main(int argc, char **argv) {
    assert(argc == 2);
    uint32_t len; uint8_t *jpeg = load(argv[1], "hw-420.jpg", &len);
    KvJpegHwProfile profile = {0};
    assert(kv_jpeg_hw_profile(jpeg, len, &profile));
    assert(profile.width == 479 && profile.height == 269 && profile.sampling == 0x20202);
    for (uint32_t n = 0; n < len; ++n) assert(!kv_jpeg_hw_profile(jpeg, n, &profile));
    defaults(2);
    if (getenv("KV_HW_TEST_INIT_FAILURE")) {
        fail_at = 1; KvDecodedImage out = {0};
        assert(!kv_jpeg_hw_try(jpeg, len, &out, 1048576));
        assert(!kv_jpeg_hw_try(jpeg, len, &out, 1048576));
        assert(init_calls == 1 && live == 0 && allocations == 0);
        free(jpeg); puts("PASS hardware init failure is bounded and remembered"); return 0;
    }
    KvDecodedImage out = {0};
    assert(!kv_jpeg_hw_try(jpeg, len, &out, 1024)); assert(allocations == 0);
    assert(kv_jpeg_hw_try(jpeg, len, &out, 1048576));
    assert(out.width == 479 && out.height == 269 && out.stride == 479 * 4);
    for (uint32_t y = 0; y < out.height; ++y)
        for (uint32_t x = 0; x < out.width; ++x) {
            const uint8_t *p = out.pixels + (y * out.width + x) * 4;
            assert(p[0] == (uint8_t)x && p[1] == (uint8_t)y && p[2] == 37 && p[3] == 255);
        }
    kv_image_free(&out); assert(live == 0 && current == 0);
    for (int fail = 2; fail <= 8; ++fail) {
        fail_at = fail;
        int result = kv_jpeg_hw_try(jpeg, len, &out, 1048576);
        assert(result == (fail == 8)); kv_image_free(&out);
        assert(live == 0 && current == 0);
    }
    fail_at = 0;
    for (int bad = 0; bad < 7; ++bad) {
        defaults(2);
        switch (bad) {
            case 0: info.width++; break;
            case 1: info.pitch[0].x = 1024; break;
            case 2: info.output_size = 1; break;
            case 3: info.coeff_size = UINT32_MAX; break;
            case 4: info.pitch[2].y--; break;
            case 5: info.color_space = 0x20101; break;
            case 6: info.pitch[3].x = 1; break;
        }
        assert(!kv_jpeg_hw_try(jpeg, len, &out, 1048576)); assert(!out.pixels && live == 0);
    }
    free(jpeg);
    jpeg = load(argv[1], "hw-422.jpg", &len); defaults(1);
    assert(kv_jpeg_hw_try(jpeg, len, &out, 1048576)); kv_image_free(&out); free(jpeg);
    for (int sampling = 1; sampling <= 2; ++sampling) {
        jpeg = load(argv[1], sampling == 2 ? "hw-chroma-420.jpg" : "hw-chroma-422.jpg", &len);
        defaults(sampling); info.width = 463; info.pitch[0].x = 464;
        info.output_size = (464 * 272 + 2 * 240 * info.pitch[1].y + 255) & ~255U;
        assert(kv_jpeg_hw_try(jpeg, len, &out, 1048576));
        assert(out.width == 463 && out.stride == 463 * 4);
        assert(out.pixels[(268 * out.width + 462) * 4] == (uint8_t)462);
        kv_image_free(&out); free(jpeg); assert(!live && !current);
    }
    for (size_t i = 0; i < 5; ++i) {
        const char *names[] = {"rgb.jpg", "progressive.jpg", "gray.jpg", "cmyk.jpg", "canvas.jpg"};
        jpeg = load(argv[1], names[i], &len);
        assert(!kv_jpeg_hw_profile(jpeg, len, &profile)); free(jpeg);
    }
    KvImageDecodeStats stats = {0}; kv_image_decode_stats(&stats);
    assert(stats.jpeg_hardware_ok == 5 && stats.jpeg_fallback_busy == 1);
    assert(stats.jpeg_fallback_contract >= 8 && stats.jpeg_fallback_budget >= 1);
    assert(stats.hw_current_cdram == 0 && stats.hw_peak_cdram == peak && peak <= KV_JPEG_HW_CDRAM_RESERVE);
    assert(init_calls == 1 && info_calls > 10 && decode_calls >= 5 && live == 0);
    printf("PASS hardware routing, pitch crop, 420/422, limits, failures, busy serialization; peak=%u allocations=%d frees=%d\n",
        peak, allocations, frees);
    return 0;
}
