/* CPU fault-injection only: actual backend + installed VitaSDK declarations.
 * SDK memory observations/allocations are stubs; this is not a GPU/Vita run. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#ifdef KV_BUDGET_RED
#define kv_gxm_cdram_available kv_gxm_cdram_available_production
#endif
#include KV_GXM_BACKEND_SOURCE
#ifdef KV_BUDGET_RED
#undef kv_gxm_cdram_available
/* Reproduce the old policy-only admission in a private negative control. */
int32_t kv_gxm_cdram_available(const KvGxm *g, uint32_t *out) {
    if (!g || !out) return KV_BAD_ARGUMENT;
    *out = CDRAM_LIMIT - g->stats.parameter_reserved_bytes - g->stats.cdram_mapped_bytes;
    return 0;
}
#endif

static int free_status, reported_free, memory_queries, allocation_calls;
static SceUID thread_id = 123;
static unsigned checks, failures, cases;
static unsigned char memory[2 * 1024 * 1024];
static void expect(int condition, const char *expression, int line) {
    ++checks;
    if (!condition) {
        ++failures;
        fprintf(stderr, "FAIL case %u line %d: %s\n", cases, line, expression);
    }
}
#define EXPECT(e) expect(!!(e), #e, __LINE__)
SceUID sceKernelGetThreadId(void) { return thread_id; }
int sceKernelGetFreeMemorySize(SceKernelFreeMemorySizeInfo *info) {
    ++memory_queries;
    EXPECT(info->size == (int)sizeof(*info));
    info->size_cdram = reported_free;
    return free_status;
}
SceUID sceKernelAllocMemBlock(const char *name, SceKernelMemBlockType type,
    SceSize size, SceKernelAllocMemBlockOpt *options) {
    ++allocation_calls;
    EXPECT(name && type == SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW);
    EXPECT(size <= sizeof(memory) && options == NULL);
    return 41;
}
int sceKernelGetMemBlockBase(SceUID uid, void **base) { EXPECT(uid == 41); *base = memory; return 0; }
int sceKernelFreeMemBlock(SceUID uid) { EXPECT(uid == 41); return 0; }
int sceGxmMapMemory(void *base, unsigned int size, SceGxmMemoryAttribFlags flags) {
    EXPECT(base == memory && size <= sizeof(memory)); (void)flags; return 0;
}
int sceGxmUnmapMemory(void *base) { EXPECT(base == memory); return 0; }
int sceGxmMapVertexUsseMemory(void *base, unsigned int size, unsigned int *offset) {
    (void)base; (void)size; (void)offset; return -99;
}
int sceGxmMapFragmentUsseMemory(void *base, unsigned int size, unsigned int *offset) {
    (void)base; (void)size; (void)offset; return -99;
}
int sceGxmUnmapVertexUsseMemory(void *base) { (void)base; return -99; }
int sceGxmUnmapFragmentUsseMemory(void *base) { (void)base; return -99; }

int main(void) {
    KvGxm g = {0};
    g.thread = thread_id;
    g.initialized = 1;
    g.stats.parameter_reserved_bytes = 16 * 1024 * 1024;
    g.stats.cdram_mapped_bytes = 23 * 1024 * 1024;
    reported_free = 40 * 1024 * 1024;
    uint32_t out = 777;
    ++cases;
    EXPECT(kv_gxm_cdram_available(&g, &out) == 0);
    EXPECT(out == 25 * 1024 * 1024); /* Policy is the tighter limit. */
    EXPECT(memory_queries == 1);
    ++cases;
    reported_free = 6 * 1024 * 1024 + 17;
    EXPECT(kv_gxm_cdram_available(&g, &out) == 0);
    EXPECT(out == (uint32_t)reported_free); /* Other native users are visible. */
    ++cases;
    reported_free = 0;
    EXPECT(kv_gxm_cdram_available(&g, &out) == 0 && out == 0);
    ++cases;
    free_status = -73; out = 777;
    EXPECT(kv_gxm_cdram_available(&g, &out) == -73 && out == 0);
    ++cases;
    free_status = 0; reported_free = -1; out = 777;
    EXPECT(kv_gxm_cdram_available(&g, &out) == KV_STATE && out == 0);
    ++cases;
    reported_free = 40 * 1024 * 1024;
    int before = memory_queries;
    g.closing = 1; out = 777;
    EXPECT(kv_gxm_cdram_available(&g, &out) == KV_STATE && out == 0);
    EXPECT(memory_queries == before);
    ++cases;
    g.closing = 0; ++thread_id; out = 777;
    EXPECT(kv_gxm_cdram_available(&g, &out) == KV_STATE && out == 0);
    EXPECT(memory_queries == before);
    --thread_id;
    ++cases;
    EXPECT(kv_gxm_cdram_available(NULL, &out) == KV_BAD_ARGUMENT);
    EXPECT(kv_gxm_cdram_available(&g, NULL) == KV_BAD_ARGUMENT);
    ++cases;
    g.stats.parameter_reserved_bytes = CDRAM_LIMIT + 1U; out = 777;
    EXPECT(kv_gxm_cdram_available(&g, &out) == KV_BUDGET && out == 0);
    ++cases;
    g.stats.parameter_reserved_bytes = 16 * 1024 * 1024;
    g.stats.cdram_mapped_bytes = 48 * 1024 * 1024 + 1U; out = 777;
    EXPECT(kv_gxm_cdram_available(&g, &out) == KV_BUDGET && out == 0);
    ++cases;
    g.stats.cdram_mapped_bytes = 48 * 1024 * 1024;
    EXPECT(kv_gxm_cdram_available(&g, &out) == 0 && out == 0);
    ++cases;
    g.stats.cdram_mapped_bytes = 0;
    Block b = {0};
    EXPECT(block_alloc(&g, &b, 256 * 1024 + 1, 1, 1) == 0);
    EXPECT(b.size == 512 * 1024 && g.stats.cdram_mapped_bytes == b.size);
    EXPECT(block_free(&g, &b) == 0 && g.stats.cdram_mapped_bytes == 0);
    ++cases;
    g.stats.cdram_mapped_bytes = 48 * 1024 * 1024 - 256 * 1024;
    EXPECT(block_alloc(&g, &b, 8 * 2 * 4, 1, 1) == 0);
    EXPECT(b.size == 256 * 1024 && g.stats.cdram_mapped_bytes == 48 * 1024 * 1024);
    EXPECT(block_free(&g, &b) == 0);
    ++cases;
    g.stats.cdram_mapped_bytes = 48 * 1024 * 1024;
    before = allocation_calls;
    EXPECT(block_alloc(&g, &b, 4, 1, 1) == KV_BUDGET);
    EXPECT(allocation_calls == before);
    ++cases;
    g.stats.parameter_reserved_bytes = UINT32_MAX;
    EXPECT(block_alloc(&g, &b, 4, 1, 1) == KV_BUDGET);
    EXPECT(allocation_calls == before);
    printf("RESULT %s: %u cases, %u checks, %u failures\n", failures ? "FAIL" : "PASS", cases, checks, failures);
    return failures ? 1 : 0;
}
