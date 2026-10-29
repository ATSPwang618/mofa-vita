#include "image_decode.h"

/* Test-only link wrapper counts calls from Rust to the real C destructor. */
static uint32_t calls, uncleared;
void __real_kv_image_free(KvDecodedImage *image);

void __wrap_kv_image_free(KvDecodedImage *image) {
    calls++;
    __real_kv_image_free(image);
    if (image != NULL && (image->width != 0 || image->height != 0 ||
        image->stride != 0 || image->length != 0 || image->pixels != NULL)) uncleared++;
}

uint32_t kv_test_free_calls(void) { return calls; }
uint32_t kv_test_uncleared_frees(void) { return uncleared; }
