#include "shader_extensions.h"
#ifdef KV_SHADER_EXTENSIONS_HOST_TEST
#include "tests/shader_extensions_stubs.h"
#else
#include <psp2/kernel/clib.h>
#include <taihen.h>
#endif

/* These are ordinary functions in the pinned, already-used Hazuki archive.
 * That archive imports modulemgr/clib/taihen, never weak SceShaccCg exports. */
extern int sceShaccCgExtEnableExtensions(void);
extern void sceShaccCgExtDisableExtensions(void);

#include "diagnostic_log.h"
static struct { int attempted, enabled, module; } extensions;

void kv_shader_extensions_disable(void) {
    if (!extensions.attempted) return;
    sceShaccCgExtDisableExtensions();
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=extensions action=disable uid=%08x enabled=%d result=done\n",
        (unsigned)extensions.module, extensions.enabled);
    extensions.attempted = extensions.enabled = extensions.module = 0;
}

int kv_shader_extensions_enable(int expected_module) {
    if (extensions.attempted) {
        if (extensions.enabled && extensions.module == expected_module) return 0;
        return -1;
    }
    tai_module_info_t info = { .size = sizeof(info) };
    const int found = taiGetModuleInfo("SceShaccCg", &info);
    const int matched = found >= 0 && info.modid == expected_module;
    const int supported = found >= 0 && (info.module_nid == 0x6c3c7547u || info.module_nid == 0xee15880du);
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=extensions action=preflight uid=%08x found_uid=%08x rc=%08x nid=%08x matched=%d supported=%d policy=%u\n",
        (unsigned)expected_module, found >= 0 ? (unsigned)info.modid : 0,
        (unsigned)found, found >= 0 ? (unsigned)info.module_nid : 0, matched, supported,
        KV_SHADER_COMPILER_POLICY);
    if (!matched || !supported) return -1;
    extensions.attempted = 1;
    extensions.module = expected_module;
    const int result = sceShaccCgExtEnableExtensions();
    extensions.enabled = result == 0;
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=extensions action=enable uid=%08x rc=%08x policy=%u implementation=%.16s\n",
        (unsigned)expected_module, (unsigned)result, KV_SHADER_COMPILER_POLICY,
        KV_SHADER_EXTENSION_ARCHIVE_SHA256);
    if (result != 0) kv_shader_extensions_disable();
    return result;
}
