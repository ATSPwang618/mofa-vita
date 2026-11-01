#include "perf_audit.h"
#include <stdatomic.h>
#include <stddef.h>
#ifdef __vita__
#include <psp2/kernel/clib.h>
#include <psp2/kernel/processmgr.h>
static uint64_t now_us(void) { return sceKernelGetProcessTimeWide(); }
#else
#include <time.h>
#include <stdio.h>
static uint64_t now_us(void) {
    struct timespec ts;
    timespec_get(&ts, TIME_UTC);
    return (uint64_t)ts.tv_sec * 1000000 + (uint64_t)ts.tv_nsec / 1000;
}
#endif
static atomic_bool enabled;
static _Atomic uint32_t native_phase, native_detail;
void kv_perf_native_enter(uint32_t phase, uint32_t detail) {
    if (!atomic_load_explicit(&enabled, memory_order_relaxed)) return;
    atomic_store_explicit(&native_detail, detail, memory_order_relaxed);
    atomic_store_explicit(&native_phase, phase, memory_order_relaxed);
}
void kv_perf_native_leave(void) {
    if (atomic_load_explicit(&enabled, memory_order_relaxed))
        atomic_store_explicit(&native_phase, KV_NATIVE_IDLE, memory_order_relaxed);
}
void kv_perf_native_state(uint32_t out[2]) {
    if (!out) return;
    out[0] = atomic_load_explicit(&native_phase, memory_order_relaxed);
    out[1] = atomic_load_explicit(&native_detail, memory_order_relaxed);
}
static _Atomic uint32_t render_options;
void kv_render_set_options(uint32_t options) { atomic_store_explicit(&render_options, options, memory_order_relaxed); }
uint32_t kv_render_options(void) { return atomic_load_explicit(&render_options, memory_order_relaxed); }
typedef struct KvPerfCounter { _Atomic uint64_t calls, us, bytes, aux; } KvPerfCounter;
static KvPerfCounter counters[KV_PERF_COUNT];
_Static_assert(KV_PERF_COUNT <= 32 * 8, "console audit bank index bound");
_Static_assert(sizeof(KvPerfSample) == 32, "performance counter ABI");
void kv_perf_set_enabled(uint32_t value) { atomic_store_explicit(&enabled, value != 0, memory_order_relaxed); }
uint64_t kv_perf_begin(void) {
    return atomic_load_explicit(&enabled, memory_order_relaxed) ? now_us() + 1 : 0;
}
void kv_perf_end(enum KvPerfMetric metric, uint64_t started, uint64_t bytes, uint64_t aux) {
    if (!started) return;
    kv_perf_end_at(metric, started, now_us() + 1, bytes, aux);
}
void kv_perf_end_at(enum KvPerfMetric metric, uint64_t started, uint64_t finished, uint64_t bytes, uint64_t aux) {
    if (!started || !finished || (unsigned)metric >= KV_PERF_COUNT) return;
    KvPerfCounter *c = &counters[metric];
    atomic_fetch_add_explicit(&c->us, finished >= started ? finished - started : 0, memory_order_relaxed);
    atomic_fetch_add_explicit(&c->bytes, bytes, memory_order_relaxed);
    atomic_fetch_add_explicit(&c->aux, aux, memory_order_relaxed);
    atomic_fetch_add_explicit(&c->calls, 1, memory_order_relaxed);
}
void kv_perf_count(enum KvPerfMetric metric, uint64_t bytes) {
    if (!atomic_load_explicit(&enabled, memory_order_relaxed) || (unsigned)metric >= KV_PERF_COUNT) return;
    atomic_fetch_add_explicit(&counters[metric].bytes, bytes, memory_order_relaxed);
    atomic_fetch_add_explicit(&counters[metric].calls, 1, memory_order_relaxed);
}
int32_t kv_perf_snapshot(KvPerfSample *out, uint32_t count) {
    if (!out || count != KV_PERF_COUNT) return -1;
    for (unsigned i = 0; i < count; ++i) {
        out[i] = (KvPerfSample){atomic_load_explicit(&counters[i].calls, memory_order_relaxed),
            atomic_load_explicit(&counters[i].us, memory_order_relaxed),
            atomic_load_explicit(&counters[i].bytes, memory_order_relaxed),
            atomic_load_explicit(&counters[i].aux, memory_order_relaxed)};
    }
    return 0;
}
int32_t kv_perf_console(const char *line, uint32_t length) {
#if defined(KV_VITA_QUIET) && KV_VITA_QUIET
    (void)line; (void)length;
    return 0;
#else
    if (!line || !length || length > 900) return -1;
#ifdef __vita__
    return sceClibPrintf("%.*s", (int)length, line);
#else
    return printf("%.*s", (int)length, line);
#endif
#endif
}
