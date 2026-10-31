#ifndef KNIFE_DIAGNOSTIC_LOG_H
#define KNIFE_DIAGNOSTIC_LOG_H
/* Keep argument evaluation and shader/cache control flow identical. */
#if defined(KV_VITA_QUIET) && KV_VITA_QUIET
static inline int kv_quiet_diagnostic(const char *format, ...) {
    (void)format;
    return 0;
}
#define KV_DIAGNOSTIC_PRINTF kv_quiet_diagnostic
#else
#define KV_DIAGNOSTIC_PRINTF sceClibPrintf
#endif
#endif
