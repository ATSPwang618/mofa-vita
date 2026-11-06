#ifndef KNIFE_PERF_AUDIT_H
#define KNIFE_PERF_AUDIT_H
#include <stdint.h>
enum KvPerfMetric {
#define KV_PERF(symbol, label) KV_PERF_##symbol,
#include "perf_counters.def"
#undef KV_PERF
    KV_PERF_COUNT
};
typedef struct KvPerfSample { uint64_t calls, us, bytes, aux; } KvPerfSample;
/* Optimization switches are independent of whether timing probes are enabled. */
void kv_render_set_options(uint64_t options);
uint64_t kv_render_options(void);
void kv_perf_set_enabled(uint32_t enabled);
uint64_t kv_perf_begin(void); /* 0 when disabled: no clock or atomic counter update */
void kv_perf_count(enum KvPerfMetric metric, uint64_t bytes);
void kv_perf_end(enum KvPerfMetric metric, uint64_t started, uint64_t bytes, uint64_t aux);
void kv_perf_end_at(enum KvPerfMetric metric, uint64_t started, uint64_t finished, uint64_t bytes, uint64_t aux);
int32_t kv_perf_snapshot(KvPerfSample *out, uint32_t count);
/* Only the GXM owner writes these; the watchdog reads without any GXM call. */
enum KvPerfNativePhase {
    KV_NATIVE_IDLE, KV_NATIVE_BEGIN_SCENE, KV_NATIVE_END_SCENE, KV_NATIVE_FINISH,
    KV_NATIVE_TRANSFER_FINISH, KV_NATIVE_DISPLAY_FINISH, KV_NATIVE_DISPLAY_ENQUEUE,
    KV_NATIVE_NOTIFICATION_WAIT
};
void kv_perf_native_enter(uint32_t phase, uint32_t detail);
void kv_perf_native_leave(void);
void kv_perf_native_state(uint32_t out[2]);
/* Console only. No fopen/write/sync. Caller sends one bounded line at a time. */
int32_t kv_perf_console(const char *line, uint32_t length);
#endif
