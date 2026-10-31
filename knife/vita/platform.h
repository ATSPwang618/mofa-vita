#ifndef KNIFE_VITA_PLATFORM_H
#define KNIFE_VITA_PLATFORM_H
#include <stdint.h>
#include <stddef.h>

typedef struct KvIoRequest {
    uint64_t offset;
    uint8_t *buffer;
    uint32_t length;
    uint32_t actual;
    int32_t status;
} KvIoRequest;

typedef struct KvAbiLayout {
    uint32_t version, pointer_bits, size, alignment;
    uint32_t offsets[5];
} KvAbiLayout;

void kv_abi_layout(KvAbiLayout *out);
/* Only kv_free releases a successful kv_alloc result. */
void *kv_alloc(uint32_t length);
void kv_free(void *buffer);
int32_t kv_init_data_dir(void);
int32_t kv_read_range(const char *path, KvIoRequest *request);
uint64_t kv_clock_us(void);
/* Runtime view of the loader's heap/stack sizes. These read the same symbols the
 * newlib loader consumes, so they prove the constants were linked, not guessed. */
typedef struct KvRuntimeMemory {
    uint32_t newlib_heap_bytes, libc_heap_bytes, main_stack_bytes, main_priority;
} KvRuntimeMemory;
int32_t kv_memory_available(uint32_t out[4]);
void kv_runtime_memory(KvRuntimeMemory *out);
void kv_set_performance_clock(int *arm, int *bus, int *gpu, int *xbar);
void kv_clock_mhz(uint32_t *arm, uint32_t *bus, uint32_t *gpu);
int32_t kv_join_worker(uint32_t input, int32_t *result);

/* DEV probe only: three SDK pools, five call results, three complete snapshots.
 * Every successful allocation is freed before this synchronous call returns. */
typedef struct KvMemoryRoundTrip {
    uint32_t pool, bytes;
    int32_t status[5]; /* query-before, allocation UID, query-during, free, query-after */
    uint32_t before[3], during[3], after[3]; /* user, CDRAM, physical-contiguous */
} KvMemoryRoundTrip;
int32_t kv_probe_memory_round_trip(uint32_t pool, KvMemoryRoundTrip *out);
#endif
