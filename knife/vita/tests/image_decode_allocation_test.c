#include "image_decode.h"

#include <assert.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Link --wrap intercepts allocations made by this implementation. Dynamically
 * linked codecs are not interposed; PNG uses its injected allocator and JPEG
 * uses its injected manager after the library's fixed bootstrap allocations.
 */
void *__real_malloc(size_t bytes);
void *__real_calloc(size_t count, size_t bytes);
void *__real_memalign(size_t alignment, size_t bytes);
void __real_free(void *pointer);

static void *owned[4096];
static size_t calls, fail_at, live;
static int active;

static int reject(void) {
    if (!active) return 0;
    calls++;
    if (calls == fail_at) {
        errno = ENOMEM;
        return 1;
    }
    return 0;
}

static void *remember(void *pointer) {
    if (!active || pointer == NULL) return pointer;
    assert(live < sizeof(owned) / sizeof(owned[0]));
    owned[live++] = pointer;
    return pointer;
}

void *__wrap_malloc(size_t bytes) {
    if (reject()) return NULL;
    return remember(__real_malloc(bytes));
}

void *__wrap_calloc(size_t count, size_t bytes) {
    if (reject()) return NULL;
    return remember(__real_calloc(count, bytes));
}

void *__wrap_memalign(size_t alignment, size_t bytes) {
    if (reject()) return NULL;
    return remember(__real_memalign(alignment, bytes));
}

void __wrap_free(void *pointer) {
    if (active && pointer != NULL) {
        size_t index;
        for (index = 0; index < live; index++) {
            if (owned[index] == pointer) break;
        }
        assert(index < live);
        owned[index] = owned[--live];
    }
    __real_free(pointer);
}

static void exercise(const char *root, const char *name) {
    char path[4096];
    assert(snprintf(path, sizeof(path), "%s/%s", root, name) > 0);
    FILE *file = fopen(path, "rb");
    assert(file != NULL);
    assert(fseek(file, 0, SEEK_END) == 0);
    long size = ftell(file);
    assert(size > 0 && size <= KV_IMAGE_ENCODED_LIMIT);
    rewind(file);
    uint8_t *data = malloc((size_t)size);
    assert(data != NULL);
    assert(fread(data, 1, (size_t)size, file) == (size_t)size);
    assert(fclose(file) == 0);

    KvDecodedImage image = {0};
    active = 1;
    calls = 0;
    fail_at = 0;
    assert(kv_image_decode(data, (uint32_t)size, &image) == KV_IMAGE_OK);
    size_t total_calls = calls;
    kv_image_free(&image);
    assert(live == 0);
    active = 0;
    for (size_t fail = 1; fail <= total_calls; fail++) {
        active = 1;
        calls = 0;
        fail_at = fail;
        assert(kv_image_decode(data, (uint32_t)size, &image) == KV_IMAGE_MEMORY);
        assert(image.pixels == NULL && image.length == 0);
        kv_image_free(&image);
        assert(live == 0);
        active = 0;
    }
    printf("Allocation failures: %s, %zu individual allocation points, all released\n",
           name, total_calls);
    free(data);
}

int main(int argc, char **argv) {
    assert(argc == 2);
    exercise(argv[1], "rgba.png");
    exercise(argv[1], "interlaced.png");
    exercise(argv[1], "rgb.jpg");
    exercise(argv[1], "progressive.jpg");
    puts("RESULT PASS: failure injection at every implementation allocation point");
    return 0;
}
