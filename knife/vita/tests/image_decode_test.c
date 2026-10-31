#include "image_decode.h"
#include "perf_audit.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <png.h>
#include <jpeglib.h>

#define STRINGIFY_INNER(x) #x
#define STRINGIFY(x) STRINGIFY_INNER(x)

static uint8_t *load(const char *root, const char *name, uint32_t *length) {
    char path[4096];
    assert(snprintf(path, sizeof(path), "%s/%s", root, name) > 0);
    FILE *file = fopen(path, "rb");
    assert(file != NULL);
    assert(fseek(file, 0, SEEK_END) == 0);
    long size = ftell(file);
    assert(size > 0 && (unsigned long)size <= UINT32_MAX);
    assert(fseek(file, 0, SEEK_SET) == 0);
    uint8_t *data = malloc((size_t)size);
    assert(data != NULL);
    assert(fread(data, 1, (size_t)size, file) == (size_t)size);
    assert(fclose(file) == 0);
    *length = (uint32_t)size;
    return data;
}

static void empty(const KvDecodedImage *image) {
    assert(image->pixels == NULL && image->width == 0 && image->height == 0);
    assert(image->stride == 0 && image->length == 0);
}

static void shape(const KvDecodedImage *image, uint32_t width, uint32_t height) {
    assert(image->pixels != NULL && image->width == width && image->height == height);
    assert(image->stride == width * 4 && image->length == width * height * 4);
}

static void test_rgba(const char *root, const char *file_name) {
    uint32_t length, expected_length;
    uint8_t *data = load(root, file_name, &length);
    uint8_t *expected = load(root, "rgba.expected", &expected_length);
    KvDecodedImage image = {0};
    assert(kv_image_decode(data, length, &image) == KV_IMAGE_OK);
    shape(&image, 3, 2);
    assert(image.length == expected_length);
    assert(memcmp(image.pixels, expected, expected_length) == 0);
    uint8_t *owned = image.pixels;
    assert(kv_image_decode(data, length, &image) == KV_IMAGE_ARGUMENT);
    assert(image.pixels == owned);
    kv_image_free(&image);
    kv_image_free(&image);
    empty(&image);
    free(expected);
    free(data);
}

static void test_transform(const char *root, const char *name,
                           uint32_t width, uint32_t height, const uint8_t *expected) {
    uint32_t length;
    uint8_t *data = load(root, name, &length);
    KvDecodedImage image = {0};
    assert(kv_image_decode(data, length, &image) == KV_IMAGE_OK);
    shape(&image, width, height);
    assert(memcmp(image.pixels, expected, image.length) == 0);
    kv_image_free(&image);
    free(data);
}

static void test_jpeg(const char *root, const char *name) {
    uint32_t length;
    uint8_t *data = load(root, name, &length);
    KvDecodedImage image = {0};
    assert(kv_image_decode(data, length, &image) == KV_IMAGE_OK);
    shape(&image, 32, 16);
    for (uint32_t y = 0; y < image.height; y++) {
        for (uint32_t x = 0; x < image.width; x++) {
            const uint8_t left[] = {240, 20, 10};
            const uint8_t right[] = {15, 225, 35};
            const uint8_t *expected = x < 16 ? left : right;
            const uint8_t *actual = image.pixels + y * image.stride + x * 4;
            for (size_t component = 0; component < 3; component++) {
                assert(abs((int)actual[component] - expected[component]) <= 3);
            }
            assert(actual[3] == 255);
        }
    }
    kv_image_free(&image);
    free(data);
}

static void test_failure(const char *root, const char *name, int32_t result) {
    uint32_t length;
    uint8_t *data = load(root, name, &length);
    for (size_t iteration = 0; iteration < 16; iteration++) {
        KvDecodedImage image = {0};
        assert(kv_image_decode(data, length, &image) == result);
        empty(&image);
        kv_image_free(&image);
    }
    free(data);
}

static void test_solid_jpeg(const char *root, const char *name, uint32_t width, uint32_t height) {
    uint32_t length;
    uint8_t *data = load(root, name, &length);
    KvDecodedImage image = {0};
    assert(kv_image_decode(data, length, &image) == KV_IMAGE_OK);
    shape(&image, width, height);
    const uint8_t expected[] = {40, 70, 100};
    for (uint32_t index = 0; index < image.length; index += 4) {
        for (size_t component = 0; component < 3; component++)
            assert(abs((int)image.pixels[index + component] - expected[component]) <= 3);
        assert(image.pixels[index + 3] == 255);
    }
    kv_image_free(&image);
    free(data);
}

static void test_prefixes(const char *root, const char *name) {
    uint32_t length;
    uint8_t *data = load(root, name, &length);
    /* Every missing suffix, including missing end markers after pixel decode. */
    for (uint32_t prefix = 0; prefix < length; prefix++) {
        KvDecodedImage image = {0};
        assert(kv_image_decode(data, prefix, &image) < 0);
        empty(&image);
        kv_image_free(&image);
    }
    free(data);
}

static void test_gray_direct(const char *root, const char *name) {
    uint32_t length;
    uint8_t *data = load(root, name, &length);
    KvDecodedImage rgba = {0}, gray = {0};
    assert(kv_image_decode(data, length, &rgba) == KV_IMAGE_OK);
    uint32_t pixels = rgba.width * rgba.height;
    assert(kv_image_decode_gray_bounded(data, length, &gray, pixels - 1) == KV_IMAGE_BUDGET);
    empty(&gray);
    assert(kv_image_decode_gray_bounded(data, length, &gray, pixels) == KV_IMAGE_OK);
    assert(gray.width == rgba.width && gray.height == rgba.height);
    assert(gray.stride == gray.width && gray.length == pixels);
    for (uint32_t i = 0; i < pixels; ++i) assert(gray.pixels[i] == rgba.pixels[i * 4]);
    kv_image_free(&rgba); kv_image_free(&gray); free(data);
}

int main(int argc, char **argv) {
    assert(argc == 2);
    const char *root = argv[1];
    printf("libpng header=%s runtime=%s; libjpeg ABI=%d turbo=%s\n",
           PNG_LIBPNG_VER_STRING, png_get_libpng_ver(NULL), JPEG_LIB_VERSION,
           STRINGIFY(LIBJPEG_TURBO_VERSION));
    {
        uint32_t audit_length;
        uint8_t *audit_input = load(root, "rgba.png", &audit_length);
        KvDecodedImage audit_image = {0};
        KvPerfSample before[KV_PERF_COUNT], after[KV_PERF_COUNT];
        kv_perf_set_enabled(1);
        assert(kv_perf_snapshot(before, KV_PERF_COUNT) == 0);
        assert(kv_image_decode(audit_input, audit_length, &audit_image) == KV_IMAGE_OK);
        assert(kv_perf_snapshot(after, KV_PERF_COUNT) == 0);
        assert(after[KV_PERF_PNG_DECODE].calls == before[KV_PERF_PNG_DECODE].calls + 1);
        assert(after[KV_PERF_PNG_DECODE].bytes - before[KV_PERF_PNG_DECODE].bytes == 24);
        assert(after[KV_PERF_DECODE_ZERO].bytes - before[KV_PERF_DECODE_ZERO].bytes == 24);
        assert(after[KV_PERF_PNG_INPUT_COPY].bytes > before[KV_PERF_PNG_INPUT_COPY].bytes);
        kv_image_free(&audit_image); free(audit_input);
        kv_perf_set_enabled(0);
        puts("PASS enabled decoder audit: one successful 3x2 RGBA PNG, 24 output/zero bytes, input callback bytes");
    }
    test_rgba(root, "rgba.png");
    test_rgba(root, "interlaced.png");
    const uint8_t palette[] = {255, 0, 0, 0, 0, 255, 0, 91,
                               0, 0, 255, 255, 0, 255, 0, 91};
    const uint8_t ga[] = {17, 17, 17, 91, 201, 201, 201, 255};
    const uint8_t gray16[] = {0x12, 0x12, 0x12, 255, 0xab, 0xab, 0xab, 255};
    test_transform(root, "palette.png", 2, 2, palette);
    test_transform(root, "gray-alpha.png", 2, 1, ga);
    test_transform(root, "gray16.png", 2, 1, gray16);
    test_gray_direct(root, "gray-alpha.png");
    test_gray_direct(root, "gray16.png");
    {
        uint32_t length;
        uint8_t *data = load(root, "rgba.png", &length);
        KvDecodedImage gray = {0};
        assert(kv_image_decode_gray_bounded(data, length, &gray, 64) == KV_IMAGE_UNSUPPORTED);
        empty(&gray); free(data);
    }
    puts("PASS direct R8 grayscale: exact prior RGBA red samples, alpha stripping, 16-bit truncation, byte cap and color rejection");
    test_jpeg(root, "rgb.jpg");
    test_jpeg(root, "progressive.jpg");
    test_solid_jpeg(root, "canvas.jpg", 960, 540);
    test_solid_jpeg(root, "large-baseline.jpg", 960, 1080);
    for (size_t i = 0; i < 2; i++) {
        uint32_t bytes;
        uint8_t *encoded = load(root, i == 0 ? "strip.png" : "canvas.jpg", &bytes);
        KvDecodedImage bounded = {0};
        assert(kv_image_decode_bounded(encoded, bytes, &bounded, 1024U * 1024U) == KV_IMAGE_BUDGET);
        empty(&bounded);
        assert(kv_image_decode(encoded, bytes, &bounded) == KV_IMAGE_OK);
        kv_image_free(&bounded);
        free(encoded);
    }
    test_failure(root, "oversize-width.png", KV_IMAGE_DIMENSIONS);
    test_failure(root, "oversize-height.png", KV_IMAGE_DIMENSIONS);
    test_failure(root, "huge-dimensions.png", KV_IMAGE_DIMENSIONS);
    test_failure(root, "oversize.jpg", KV_IMAGE_DIMENSIONS);
    test_failure(root, "cmyk.jpg", KV_IMAGE_UNSUPPORTED);
    test_failure(root, "bad-end-crc.png", KV_IMAGE_MALFORMED);
    test_failure(root, "large-progressive.jpg", KV_IMAGE_BUDGET);
    test_prefixes(root, "rgba.png");
    test_prefixes(root, "rgb.jpg");
    test_prefixes(root, "progressive.jpg");

    uint32_t length;
    uint8_t *data = load(root, "strip.png", &length);
    KvDecodedImage image = {0};
    assert(kv_image_decode(data, length, &image) == KV_IMAGE_OK);
    shape(&image, 960, 1080);
    for (uint32_t i = 0; i < image.length; i += 4) {
        const uint8_t pixel[] = {37, 61, 83, 127};
        assert(memcmp(image.pixels + i, pixel, 4) == 0);
    }
    kv_image_free(&image);
    free(data);
    data = load(root, "gray.jpg", &length);
    assert(kv_image_decode(data, length, &image) == KV_IMAGE_OK);
    shape(&image, 7, 3);
    for (uint32_t i = 0; i < image.length; i += 4) {
        assert(image.pixels[i] == 117 && image.pixels[i + 1] == 117);
        assert(image.pixels[i + 2] == 117 && image.pixels[i + 3] == 255);
    }
    kv_image_free(&image);
    free(data);
    const uint8_t signature[] = {0x89, 'P', 'N', 'G', 13, 10, 26, 10};
    assert(kv_image_decode(signature, KV_IMAGE_ENCODED_LIMIT + 1, &image) == KV_IMAGE_INPUT_LIMIT);
    assert(kv_image_decode(signature, 8, NULL) == KV_IMAGE_ARGUMENT);
    assert(kv_image_decode(NULL, 8, &image) == KV_IMAGE_ARGUMENT);
    empty(&image);
    kv_image_free(NULL);
    puts("RESULT PASS: alpha/Adam7/palette/gray16, JPEG RGB/progressive/gray/stride, full 960x1080, corruption/truncation, budgets and ownership");
    return 0;
}
