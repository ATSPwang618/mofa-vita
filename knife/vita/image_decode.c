#include "image_decode.h"
#include "jpeg_hw.h"

#include <limits.h>
#include <malloc.h>
#include <setjmp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <png.h>
#include <jpeglib.h>
#include <jerror.h>

#if !defined(PNG_USER_MEM_SUPPORTED) || !defined(PNG_SET_USER_LIMITS_SUPPORTED)
#error "Bounded decoding requires libpng user memory and dimension limits"
#endif
#if !defined(JCS_ALPHA_EXTENSIONS) || BITS_IN_JSAMPLE != 8
#error "Bounded decoding requires 8-bit libjpeg-turbo with RGBA output"
#endif

#if defined(__vita__)
_Static_assert(sizeof(KvDecodedImage) == 20, "Vita image ABI must be 20 bytes");
_Static_assert(_Alignof(KvDecodedImage) == 4, "Vita image ABI must align to 4");
_Static_assert(offsetof(KvDecodedImage, pixels) == 16, "Vita pixel pointer offset");
#endif

/* libjpeg initializes its original permanent pool before a public memory
 * manager can be installed. Reserve 64 KiB for that small bootstrap pool.
 * Subsequent codec allocations, including progressive coefficient arrays,
 * are charged to the same budget and never use a disk backing store.
 * malloc's own metadata and the borrowed encoded input are outside this cap.
 */
#define JPEG_BOOTSTRAP_RESERVE (64U * 1024U)
#define ALLOCATION_ALIGNMENT 64U

typedef struct Allocation {
    struct Allocation *next;
    size_t charge;
    int pool;
} Allocation;

#define ALLOCATION_HEADER \
    ((sizeof(Allocation) + ALLOCATION_ALIGNMENT - 1U) & ~(size_t)(ALLOCATION_ALIGNMENT - 1U))

typedef struct DecodeMemory {
    Allocation *allocations;
    uint8_t *pixels;
    size_t used;
    int32_t error;
} DecodeMemory;

static int checked_product(size_t left, size_t right, size_t *result) {
    if (left != 0 && right > SIZE_MAX / left) return 0;
    *result = left * right;
    return 1;
}

static int aligned_size(size_t bytes, size_t *result) {
    if (bytes > SIZE_MAX - (ALLOCATION_ALIGNMENT - 1U)) return 0;
    *result = (bytes + ALLOCATION_ALIGNMENT - 1U) & ~(size_t)(ALLOCATION_ALIGNMENT - 1U);
    return 1;
}

static void *decode_allocate(DecodeMemory *memory, size_t bytes, int pool) {
    size_t payload, charge;
    if (!aligned_size(bytes, &payload) || payload > SIZE_MAX - ALLOCATION_HEADER) {
        memory->error = KV_IMAGE_BUDGET;
        return NULL;
    }
    charge = ALLOCATION_HEADER + payload;
    if (charge > KV_IMAGE_WORKING_BUDGET - memory->used) {
        memory->error = KV_IMAGE_BUDGET;
        return NULL;
    }
    Allocation *allocation = memalign(ALLOCATION_ALIGNMENT, charge);
    if (allocation == NULL) {
        memory->error = KV_IMAGE_MEMORY;
        return NULL;
    }
    allocation->next = memory->allocations;
    allocation->charge = charge;
    allocation->pool = pool;
    memory->allocations = allocation;
    memory->used += charge;
    void *result = (uint8_t *)allocation + ALLOCATION_HEADER;
    memset(result, 0, payload);
    return result;
}

static void decode_release(DecodeMemory *memory, void *pointer) {
    if (pointer == NULL) return;
    Allocation *allocation = (Allocation *)((uint8_t *)pointer - ALLOCATION_HEADER);
    Allocation **link = &memory->allocations;
    while (*link != NULL && *link != allocation) link = &(*link)->next;
    if (*link == NULL) return;
    *link = allocation->next;
    memory->used -= allocation->charge;
    free(allocation);
}

static void decode_release_pool(DecodeMemory *memory, int pool) {
    Allocation **link = &memory->allocations;
    while (*link != NULL) {
        Allocation *allocation = *link;
        if (pool < 0 || allocation->pool == pool) {
            *link = allocation->next;
            memory->used -= allocation->charge;
            free(allocation);
        } else {
            link = &allocation->next;
        }
    }
}

static int image_shape(uint32_t width, uint32_t height, KvDecodedImage *shape) {
    size_t stride, length;
    if (width == 0 || height == 0 || width > KV_IMAGE_WIDTH_LIMIT ||
        height > KV_IMAGE_HEIGHT_LIMIT || !checked_product(width, 4, &stride) ||
        !checked_product(stride, height, &length) || stride > UINT32_MAX ||
        length > UINT32_MAX) return KV_IMAGE_DIMENSIONS;
    shape->width = width;
    shape->height = height;
    shape->stride = (uint32_t)stride;
    shape->length = (uint32_t)length;
    return KV_IMAGE_OK;
}

static int allocate_pixels(DecodeMemory *memory, uint32_t length) {
    if (length > KV_IMAGE_WORKING_BUDGET - memory->used) {
        memory->error = KV_IMAGE_BUDGET;
        return 0;
    }
    memory->pixels = malloc(length);
    if (memory->pixels == NULL) {
        memory->error = KV_IMAGE_MEMORY;
        return 0;
    }
    memory->used += length;
    memset(memory->pixels, 0, length);
    return 1;
}

typedef struct PngDecode {
    DecodeMemory memory;
    png_structp png;
    png_infop info;
    const uint8_t *encoded;
    size_t length, offset;
    KvDecodedImage shape;
} PngDecode;

static void png_failure(png_structp png, png_const_charp message) {
    PngDecode *context = png_get_error_ptr(png);
    (void)message;
    if (context->memory.error == KV_IMAGE_OK) context->memory.error = KV_IMAGE_MALFORMED;
    png_longjmp(png, 1);
}

static void png_warning_ignored(png_structp png, png_const_charp message) {
    (void)png;
    (void)message;
}

static png_voidp png_allocate(png_structp png, png_alloc_size_t bytes) {
    PngDecode *context = png_get_mem_ptr(png);
    return decode_allocate(&context->memory, bytes, 0);
}

static void png_release(png_structp png, png_voidp pointer) {
    PngDecode *context = png_get_mem_ptr(png);
    decode_release(&context->memory, pointer);
}

static void png_read_memory(png_structp png, png_bytep output, size_t bytes) {
    PngDecode *context = png_get_io_ptr(png);
    if (bytes > context->length - context->offset) png_error(png, "Truncated PNG");
    memcpy(output, context->encoded + context->offset, bytes);
    context->offset += bytes;
}

static uint32_t read_be32(const uint8_t *bytes) {
    return ((uint32_t)bytes[0] << 24) | ((uint32_t)bytes[1] << 16) |
           ((uint32_t)bytes[2] << 8) | bytes[3];
}

static int32_t decode_png(const uint8_t *encoded, uint32_t length, KvDecodedImage *out, uint32_t max_rgba) {
    /* Classify oversized IHDRs before libpng's generic user-limit error. */
    KvDecodedImage preliminary = {0};
    if (length >= 24 && memcmp(encoded + 12, "IHDR", 4) == 0 &&
        image_shape(read_be32(encoded + 16), read_be32(encoded + 20), &preliminary) != KV_IMAGE_OK)
        return KV_IMAGE_DIMENSIONS;
    if (preliminary.length > max_rgba) return KV_IMAGE_BUDGET;
    PngDecode *const context = calloc(1, sizeof(*context));
    if (context == NULL) return KV_IMAGE_MEMORY;
    context->memory.used = sizeof(*context);
    context->encoded = encoded;
    context->length = length;
    context->png = png_create_read_struct_2(PNG_LIBPNG_VER_STRING, context,
        png_failure, png_warning_ignored, context, png_allocate, png_release);
    if (context->png == NULL) {
        if (context->memory.error == KV_IMAGE_OK) context->memory.error = KV_IMAGE_MEMORY;
        goto cleanup;
    }
    /* All state modified after setjmp lives in the heap context. */
    if (setjmp(png_jmpbuf(context->png)) != 0) goto cleanup;
    context->info = png_create_info_struct(context->png);
    if (context->info == NULL) {
        if (context->memory.error == KV_IMAGE_OK) context->memory.error = KV_IMAGE_MEMORY;
        goto cleanup;
    }
    png_set_read_fn(context->png, context, png_read_memory);
    png_set_user_limits(context->png, KV_IMAGE_WIDTH_LIMIT, KV_IMAGE_HEIGHT_LIMIT);
    png_set_chunk_cache_max(context->png, 16);
    png_set_chunk_malloc_max(context->png, 128U * 1024U);
    png_set_keep_unknown_chunks(context->png, PNG_HANDLE_CHUNK_NEVER, NULL, 0);
    png_set_crc_action(context->png, PNG_CRC_ERROR_QUIT, PNG_CRC_ERROR_QUIT);
    png_read_info(context->png, context->info);
    context->memory.error = image_shape(png_get_image_width(context->png, context->info),
        png_get_image_height(context->png, context->info), &context->shape);
    if (context->memory.error != KV_IMAGE_OK) goto cleanup;
    if (context->shape.length > max_rgba) { context->memory.error = KV_IMAGE_BUDGET; goto cleanup; }
    int color = png_get_color_type(context->png, context->info);
    int depth = png_get_bit_depth(context->png, context->info);
    int has_transparency = png_get_valid(context->png, context->info, PNG_INFO_tRNS) != 0;
    if (depth == 16) png_set_strip_16(context->png);
    if (color == PNG_COLOR_TYPE_PALETTE) png_set_palette_to_rgb(context->png);
    if (color == PNG_COLOR_TYPE_GRAY && depth < 8) png_set_expand_gray_1_2_4_to_8(context->png);
    if (has_transparency) png_set_tRNS_to_alpha(context->png);
    if (color == PNG_COLOR_TYPE_GRAY || color == PNG_COLOR_TYPE_GRAY_ALPHA)
        png_set_gray_to_rgb(context->png);
    if ((color & PNG_COLOR_MASK_ALPHA) == 0 && !has_transparency)
        png_set_add_alpha(context->png, 255, PNG_FILLER_AFTER);
    int passes = png_set_interlace_handling(context->png);
    png_read_update_info(context->png, context->info);
    if (png_get_bit_depth(context->png, context->info) != 8 ||
        png_get_channels(context->png, context->info) != 4 ||
        png_get_rowbytes(context->png, context->info) != context->shape.stride) {
        context->memory.error = KV_IMAGE_UNSUPPORTED;
        goto cleanup;
    }
    if (!allocate_pixels(&context->memory, context->shape.length)) goto cleanup;
    for (int pass = 0; pass < passes; pass++) {
        for (uint32_t row = 0; row < context->shape.height; row++)
            png_read_row(context->png,
                context->memory.pixels + (size_t)row * context->shape.stride, NULL);
    }
    png_read_end(context->png, context->info);

cleanup:
    if (context->png != NULL) png_destroy_read_struct(&context->png, &context->info, NULL);
    decode_release_pool(&context->memory, -1);
    int32_t result = context->memory.error;
    if (result == KV_IMAGE_OK) {
        *out = context->shape;
        out->pixels = context->memory.pixels;
        context->memory.pixels = NULL;
    }
    free(context->memory.pixels);
    free(context);
    return result;
}

/* libjpeg treats these public pointer targets as opaque. Our virtual arrays
 * are fully resident and charged before use; there is no implicit swap file.
 */
struct jvirt_sarray_control {
    struct jvirt_sarray_control *next;
    JSAMPARRAY rows;
    JDIMENSION width, height, max_access;
};

struct jvirt_barray_control {
    struct jvirt_barray_control *next;
    JBLOCKARRAY rows;
    JDIMENSION width, height, max_access;
};

typedef struct JpegDecode {
    DecodeMemory memory;
    struct jpeg_decompress_struct jpeg;
    struct jpeg_error_mgr errors;
    jmp_buf jump;
    struct jpeg_source_mgr source;
    struct jpeg_memory_mgr bounded_memory;
    struct jpeg_memory_mgr *original_memory;
    jvirt_sarray_ptr sample_arrays;
    jvirt_barray_ptr block_arrays;
    KvDecodedImage shape;
} JpegDecode;

static JpegDecode *jpeg_context(j_common_ptr jpeg) {
    return (JpegDecode *)jpeg->client_data;
}

static void jpeg_failure(j_common_ptr jpeg) {
    JpegDecode *context = jpeg_context(jpeg);
    if (context->memory.error == KV_IMAGE_OK) context->memory.error = KV_IMAGE_MALFORMED;
    longjmp(context->jump, 1);
}

static void jpeg_message(j_common_ptr jpeg, int level) {
    /* libjpeg normally repairs truncated streams and reports only a warning.
     * Reject those cases so callers cannot mistake partial pixels for success.
     */
    if (level < 0) jpeg_failure(jpeg);
}

static _Noreturn void jpeg_memory_failure(j_common_ptr jpeg) {
    ERREXIT1(jpeg, JERR_OUT_OF_MEMORY, 0);
    /* The installed error_exit longjmps. Guard against a returning callback. */
    abort();
}

static void *jpeg_allocate(j_common_ptr jpeg, int pool, size_t bytes) {
    if (pool < 0 || pool >= JPOOL_NUMPOOLS) ERREXIT1(jpeg, JERR_BAD_POOL_ID, pool);
    void *result = decode_allocate(&jpeg_context(jpeg)->memory, bytes, pool);
    if (result == NULL) jpeg_memory_failure(jpeg);
    return result;
}

static size_t jpeg_array_bytes(j_common_ptr jpeg, size_t width, size_t element,
                               size_t rows, size_t *stride) {
    size_t row_bytes, total;
    if (!checked_product(width, element, &row_bytes) || !aligned_size(row_bytes, stride) ||
        !checked_product(*stride, rows, &total)) {
        jpeg_context(jpeg)->memory.error = KV_IMAGE_BUDGET;
        jpeg_memory_failure(jpeg);
    }
    return total;
}

static JSAMPARRAY jpeg_allocate_samples(j_common_ptr jpeg, int pool,
                                       JDIMENSION width, JDIMENSION rows) {
    size_t stride, pointer_bytes;
    size_t bytes = jpeg_array_bytes(jpeg, width, sizeof(JSAMPLE), rows, &stride);
    if (!checked_product(rows, sizeof(JSAMPROW), &pointer_bytes)) {
        jpeg_context(jpeg)->memory.error = KV_IMAGE_BUDGET;
        jpeg_memory_failure(jpeg);
    }
    JSAMPARRAY result = jpeg_allocate(jpeg, pool, pointer_bytes);
    uint8_t *data = jpeg_allocate(jpeg, pool, bytes);
    for (JDIMENSION row = 0; row < rows; row++) result[row] = data + (size_t)row * stride;
    return result;
}

static JBLOCKARRAY jpeg_allocate_blocks(j_common_ptr jpeg, int pool,
                                       JDIMENSION width, JDIMENSION rows) {
    size_t stride, pointer_bytes;
    size_t bytes = jpeg_array_bytes(jpeg, width, sizeof(JBLOCK), rows, &stride);
    if (!checked_product(rows, sizeof(JBLOCKROW), &pointer_bytes)) {
        jpeg_context(jpeg)->memory.error = KV_IMAGE_BUDGET;
        jpeg_memory_failure(jpeg);
    }
    JBLOCKARRAY result = jpeg_allocate(jpeg, pool, pointer_bytes);
    uint8_t *data = jpeg_allocate(jpeg, pool, bytes);
    for (JDIMENSION row = 0; row < rows; row++)
        result[row] = (JBLOCKROW)(data + (size_t)row * stride);
    return result;
}

static jvirt_sarray_ptr jpeg_request_samples(j_common_ptr jpeg, int pool, boolean zero,
    JDIMENSION width, JDIMENSION height, JDIMENSION max_access) {
    JpegDecode *context = jpeg_context(jpeg);
    if (pool != JPOOL_IMAGE) ERREXIT1(jpeg, JERR_BAD_POOL_ID, pool);
    jvirt_sarray_ptr result = jpeg_allocate(jpeg, pool, sizeof(*result));
    (void)zero; /* All allocations are zero initialized, including pre_zero. */
    result->width = width;
    result->height = height;
    result->max_access = max_access;
    result->next = context->sample_arrays;
    context->sample_arrays = result;
    return result;
}

static jvirt_barray_ptr jpeg_request_blocks(j_common_ptr jpeg, int pool, boolean zero,
    JDIMENSION width, JDIMENSION height, JDIMENSION max_access) {
    JpegDecode *context = jpeg_context(jpeg);
    if (pool != JPOOL_IMAGE) ERREXIT1(jpeg, JERR_BAD_POOL_ID, pool);
    jvirt_barray_ptr result = jpeg_allocate(jpeg, pool, sizeof(*result));
    (void)zero;
    result->width = width;
    result->height = height;
    result->max_access = max_access;
    result->next = context->block_arrays;
    context->block_arrays = result;
    return result;
}

static void jpeg_realize_arrays(j_common_ptr jpeg) {
    JpegDecode *context = jpeg_context(jpeg);
    for (jvirt_sarray_ptr array = context->sample_arrays; array != NULL; array = array->next)
        if (array->rows == NULL)
            array->rows = jpeg_allocate_samples(jpeg, JPOOL_IMAGE, array->width, array->height);
    for (jvirt_barray_ptr array = context->block_arrays; array != NULL; array = array->next)
        if (array->rows == NULL)
            array->rows = jpeg_allocate_blocks(jpeg, JPOOL_IMAGE, array->width, array->height);
}

static JSAMPARRAY jpeg_access_samples(j_common_ptr jpeg, jvirt_sarray_ptr array,
    JDIMENSION start, JDIMENSION rows, boolean writable) {
    (void)writable;
    if (array == NULL || array->rows == NULL || rows > array->max_access ||
        start > array->height || rows > array->height - start)
        ERREXIT(jpeg, JERR_BAD_VIRTUAL_ACCESS);
    return array->rows + start;
}

static JBLOCKARRAY jpeg_access_blocks(j_common_ptr jpeg, jvirt_barray_ptr array,
    JDIMENSION start, JDIMENSION rows, boolean writable) {
    (void)writable;
    if (array == NULL || array->rows == NULL || rows > array->max_access ||
        start > array->height || rows > array->height - start)
        ERREXIT(jpeg, JERR_BAD_VIRTUAL_ACCESS);
    return array->rows + start;
}

static void jpeg_release_pool(j_common_ptr jpeg, int pool) {
    JpegDecode *context = jpeg_context(jpeg);
    if (pool < 0 || pool >= JPOOL_NUMPOOLS) ERREXIT1(jpeg, JERR_BAD_POOL_ID, pool);
    if (pool == JPOOL_IMAGE) {
        context->sample_arrays = NULL;
        context->block_arrays = NULL;
    }
    decode_release_pool(&context->memory, pool);
}

static void jpeg_destroy_memory(j_common_ptr jpeg) {
    JpegDecode *context = jpeg_context(jpeg);
    decode_release_pool(&context->memory, -1);
    jpeg->mem = context->original_memory;
    context->original_memory->self_destruct(jpeg);
}

static void jpeg_source_noop(j_decompress_ptr jpeg) {
    (void)jpeg;
}

static boolean jpeg_source_eof(j_decompress_ptr jpeg) {
    ERREXIT(jpeg, JERR_INPUT_EOF);
    return FALSE;
}

static void jpeg_source_skip(j_decompress_ptr jpeg, long bytes) {
    if (bytes <= 0) return;
    if ((unsigned long)bytes > jpeg->src->bytes_in_buffer) ERREXIT(jpeg, JERR_INPUT_EOF);
    jpeg->src->next_input_byte += (size_t)bytes;
    jpeg->src->bytes_in_buffer -= (size_t)bytes;
}

static int32_t decode_jpeg(const uint8_t *encoded, uint32_t length, KvDecodedImage *out, uint32_t max_rgba) {
    JpegDecode *const context = calloc(1, sizeof(*context));
    if (context == NULL) return KV_IMAGE_MEMORY;
    context->memory.used = sizeof(*context) + JPEG_BOOTSTRAP_RESERVE;
    context->jpeg.err = jpeg_std_error(&context->errors);
    context->errors.error_exit = jpeg_failure;
    context->errors.emit_message = jpeg_message;
    context->jpeg.client_data = context;
    if (setjmp(context->jump) != 0) goto cleanup;
    jpeg_create_decompress(&context->jpeg);
    context->jpeg.client_data = context;
    context->original_memory = context->jpeg.mem;
    context->bounded_memory = *context->original_memory;
    context->bounded_memory.alloc_small = jpeg_allocate;
    context->bounded_memory.alloc_large = jpeg_allocate;
    context->bounded_memory.alloc_sarray = jpeg_allocate_samples;
    context->bounded_memory.alloc_barray = jpeg_allocate_blocks;
    context->bounded_memory.request_virt_sarray = jpeg_request_samples;
    context->bounded_memory.request_virt_barray = jpeg_request_blocks;
    context->bounded_memory.realize_virt_arrays = jpeg_realize_arrays;
    context->bounded_memory.access_virt_sarray = jpeg_access_samples;
    context->bounded_memory.access_virt_barray = jpeg_access_blocks;
    context->bounded_memory.free_pool = jpeg_release_pool;
    context->bounded_memory.self_destruct = jpeg_destroy_memory;
    context->bounded_memory.max_memory_to_use = KV_IMAGE_WORKING_BUDGET;
    context->bounded_memory.max_alloc_chunk = KV_IMAGE_WORKING_BUDGET;
    context->jpeg.mem = &context->bounded_memory;
    context->source.init_source = jpeg_source_noop;
    context->source.fill_input_buffer = jpeg_source_eof;
    context->source.skip_input_data = jpeg_source_skip;
    context->source.resync_to_restart = jpeg_resync_to_restart;
    context->source.term_source = jpeg_source_noop;
    context->source.next_input_byte = encoded;
    context->source.bytes_in_buffer = length;
    context->jpeg.src = &context->source;
    if (jpeg_read_header(&context->jpeg, TRUE) != JPEG_HEADER_OK) {
        context->memory.error = KV_IMAGE_MALFORMED;
        goto cleanup;
    }
    context->memory.error = image_shape(context->jpeg.image_width, context->jpeg.image_height,
                                        &context->shape);
    if (context->memory.error != KV_IMAGE_OK) goto cleanup;
    if (context->shape.length > max_rgba) { context->memory.error = KV_IMAGE_BUDGET; goto cleanup; }
    if (context->jpeg.data_precision != 8 ||
        (context->jpeg.jpeg_color_space != JCS_GRAYSCALE &&
         context->jpeg.jpeg_color_space != JCS_RGB && context->jpeg.jpeg_color_space != JCS_YCbCr)) {
        context->memory.error = KV_IMAGE_UNSUPPORTED;
        goto cleanup;
    }
    context->jpeg.out_color_space = JCS_EXT_RGBA;
    context->jpeg.scale_num = 1;
    context->jpeg.scale_denom = 1;
    context->jpeg.dct_method = JDCT_ISLOW;
    context->jpeg.quantize_colors = FALSE;
    jpeg_calc_output_dimensions(&context->jpeg);
    if (context->jpeg.output_width != context->shape.width ||
        context->jpeg.output_height != context->shape.height) {
        context->memory.error = KV_IMAGE_UNSUPPORTED;
        goto cleanup;
    }
    if (!allocate_pixels(&context->memory, context->shape.length)) goto cleanup;
    if (!jpeg_start_decompress(&context->jpeg) || context->jpeg.output_components != 4) {
        context->memory.error = KV_IMAGE_MALFORMED;
        goto cleanup;
    }
    while (context->jpeg.output_scanline < context->jpeg.output_height) {
        JSAMPROW row = context->memory.pixels +
            (size_t)context->jpeg.output_scanline * context->shape.stride;
        if (jpeg_read_scanlines(&context->jpeg, &row, 1) != 1) {
            context->memory.error = KV_IMAGE_MALFORMED;
            goto cleanup;
        }
    }
    if (!jpeg_finish_decompress(&context->jpeg)) context->memory.error = KV_IMAGE_MALFORMED;

cleanup:
    if (context->jpeg.mem != NULL) jpeg_destroy_decompress(&context->jpeg);
    decode_release_pool(&context->memory, -1);
    int32_t result = context->memory.error;
    if (result == KV_IMAGE_OK) {
        *out = context->shape;
        out->pixels = context->memory.pixels;
        context->memory.pixels = NULL;
    }
    free(context->memory.pixels);
    free(context);
    return result;
}

int32_t kv_image_decode(const uint8_t *encoded, uint32_t length, KvDecodedImage *out) {
    return kv_image_decode_bounded(encoded, length, out, KV_IMAGE_WIDTH_LIMIT * KV_IMAGE_HEIGHT_LIMIT * 4U);
}

int32_t kv_image_decode_bounded(const uint8_t *encoded, uint32_t length, KvDecodedImage *out, uint32_t max_rgba) {
    static const uint8_t signature[] = {0x89, 'P', 'N', 'G', 13, 10, 26, 10};
    if (out == NULL || out->pixels != NULL) return KV_IMAGE_ARGUMENT;
    memset(out, 0, sizeof(*out));
    if (encoded == NULL || length == 0 || max_rgba == 0) return KV_IMAGE_ARGUMENT;
    if (length > KV_IMAGE_ENCODED_LIMIT) return KV_IMAGE_INPUT_LIMIT;
    if (length >= sizeof(signature) && memcmp(encoded, signature, sizeof(signature)) == 0) {
        int32_t rc = decode_png(encoded, length, out, max_rgba);
        kv_image_record_software(1, rc == KV_IMAGE_OK); return rc;
    }
    if (length >= 2 && encoded[0] == 0xff && encoded[1] == 0xd8) {
        if (kv_jpeg_hw_try(encoded, length, out, max_rgba)) return KV_IMAGE_OK;
        int32_t rc = decode_jpeg(encoded, length, out, max_rgba);
        kv_image_record_software(0, rc == KV_IMAGE_OK); return rc;
    }
    return KV_IMAGE_FORMAT;
}

void kv_image_free(KvDecodedImage *image) {
    if (image == NULL) return;
    free(image->pixels);
    memset(image, 0, sizeof(*image));
}
