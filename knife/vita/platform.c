#include "platform.h"
#include <stdlib.h>
#include <psp2/io/fcntl.h>
#include <psp2/io/stat.h>
#include <psp2/kernel/processmgr.h>
#include <psp2/kernel/sysmem.h>
#include <psp2/kernel/threadmgr.h>
#include <psp2/power.h>

/* The loader reads these by name, so the sections must survive --gc-sections:
   only weak references exist, and -ffunction-sections/-fdata-sections dropped
   the definitions in earlier builds (the ELF listed _newlib_heap_size_user as a
   weak undefined symbol). "used" keeps them in the image.

   The heap is 96 MiB, which fits the 365 MiB MAIN budget granted by
   ATTRIBUTE2=0x0C: eboot ~8 MiB, code+rodata ~6 MiB, GXM MAIN mappings 16 MiB,
   stacks ~4 MiB, SceJpeg/SceAvPlayer pools a few MiB. GXM CDRAM (61 MiB) is a
   separate pool and is unaffected. */
/* 192 MiB: the font atlases, audio decodes and per-frame scratch together pushed
   the 96 MiB heap into allocator asserts on hardware (abort with no panic text).
   ATTRIBUTE2=0x0C grants 365 MiB MAIN, so this still leaves >150 MiB headroom. */
__attribute__((used)) unsigned int _newlib_heap_size_user = 192 * 1024 * 1024;
__attribute__((used)) unsigned int sceLibcHeapSize = 4 * 1024 * 1024;
/* Not const: a const initializer is inlined and the symbol disappears, which
   leaves the loader using its own default stack. */
__attribute__((used)) unsigned int sceUserMainThreadStackSize = 2 * 1024 * 1024;
__attribute__((used)) int sceUserMainThreadPriority = 0x60;

_Static_assert(sizeof(KvIoRequest) == 24, "Vita IO ABI must be 24 bytes");
_Static_assert(_Alignof(KvIoRequest) == 8, "Vita u64 alignment must be 8");
_Static_assert(sizeof(KvAbiLayout) == 36, "Layout report has fixed u32 fields");
_Static_assert(sizeof(KvMemoryRoundTrip) == 64, "Memory probe report has fixed 32-bit fields");

void kv_abi_layout(KvAbiLayout *out) {
    *out = (KvAbiLayout){1, sizeof(void*) * 8, sizeof(KvIoRequest), _Alignof(KvIoRequest), {
        offsetof(KvIoRequest, offset), offsetof(KvIoRequest, buffer),
        offsetof(KvIoRequest, length), offsetof(KvIoRequest, actual), offsetof(KvIoRequest, status)}};
}

/* The port never raised the clocks, so the GPU stayed at the 111 MHz default and
   every render pass measured 12-20 ms. These are the standard game clocks; a unit
   that refuses them keeps its current setting, which kv_clock_mhz reports. */
void kv_set_performance_clock(int *arm, int *bus, int *gpu, int *xbar) {
    /* The GPU clock is refused while the crossbar is low on some units, so raise
       the crossbar first and report every return code for the log header. */
    const int a = scePowerSetArmClockFrequency(444);
    const int x = scePowerSetGpuXbarClockFrequency(166);
    const int g = scePowerSetGpuClockFrequency(222);
    const int b = scePowerSetBusClockFrequency(222);
    if (arm) *arm = a;
    if (bus) *bus = b;
    if (gpu) *gpu = g;
    if (xbar) *xbar = x;
}

void kv_clock_mhz(uint32_t *arm, uint32_t *bus, uint32_t *gpu) {
    if (arm) *arm = (uint32_t)scePowerGetArmClockFrequency();
    if (bus) *bus = (uint32_t)scePowerGetBusClockFrequency();
    if (gpu) *gpu = (uint32_t)scePowerGetGpuClockFrequency();
}

void *kv_alloc(uint32_t length) { return malloc(length); }
void kv_free(void *buffer) { free(buffer); }

int32_t kv_init_data_dir(void) {
    const char *paths[] = {"ux0:data", "ux0:data/knife-woh-dev"};
    for (size_t i = 0; i < sizeof(paths) / sizeof(paths[0]); i++) {
        SceIoStat stat;
        int result = sceIoGetstat(paths[i], &stat);
        if (result < 0) {
            result = sceIoMkdir(paths[i], 0777);
            if (result < 0) return result;
        } else if (!SCE_S_ISDIR(stat.st_mode)) {
            return -1;
        }
    }
    return 0;
}

int32_t kv_read_range(const char *path, KvIoRequest *request) {
    request->actual = 0;
    SceUID fd = sceIoOpen(path, SCE_O_RDONLY, 0);
    if (fd < 0) return request->status = fd;
    SceOff position = sceIoLseek(fd, (SceOff)request->offset, SCE_SEEK_SET);
    int result;
    if (position < 0 || (uint64_t)position != request->offset) {
        result = position < 0 ? (int)position : -1;
    } else {
        result = sceIoRead(fd, request->buffer, request->length);
        if (result >= 0) {
            request->actual = (uint32_t)result;
            result = 0;
        }
    }
    int close_result = sceIoClose(fd);
    if (result >= 0 && close_result < 0) result = close_result;
    return request->status = result;
}

uint64_t kv_clock_us(void) { return sceKernelGetProcessTimeWide(); }

/* A real reference keeps the surrounding object pulled from the static library
 * and the data section alive under --gc-sections. */
void kv_runtime_memory(KvRuntimeMemory *out) {
    out->newlib_heap_bytes = _newlib_heap_size_user;
    out->libc_heap_bytes = sceLibcHeapSize;
    out->main_stack_bytes = sceUserMainThreadStackSize;
    out->main_priority = (uint32_t)sceUserMainThreadPriority;
}

static int worker(SceSize size, void *data) {
    if (size != sizeof(uint32_t)) return -1;
    uint32_t input = *(const uint32_t *)data;
    return (int)((input ^ 0x5a5aU) & 0x7fffffffU);
}

int32_t kv_join_worker(uint32_t input, int32_t *result) {
    SceUID thread = sceKernelCreateThread("knife-abi-worker", worker, 0x10000100,
        64 * 1024, 0, SCE_KERNEL_CPU_MASK_USER_ALL, NULL);
    if (thread < 0) return thread;
    int status = sceKernelStartThread(thread, sizeof(input), &input);
    if (status < 0) {
        sceKernelDeleteThread(thread);
        return status;
    }
    /* No timeout: the caller's stack data must not outlive an unjoined worker. */
    /* The installed Vita3K build does not write WaitThreadEnd's stat pointer.
       Join first, then use the SDK's explicit exit-status query on both targets. */
    status = sceKernelWaitThreadEnd(thread, NULL, NULL);
    if (status < 0) return status;
    status = sceKernelGetThreadExitStatus(thread, result);
    if (status < 0) {
        sceKernelDeleteThread(thread);
        return status;
    }
    return sceKernelDeleteThread(thread);
}

static int memory_snapshot(uint32_t out[3]) {
    SceKernelFreeMemorySizeInfo info = { .size = sizeof(info) };
    int result = sceKernelGetFreeMemorySize(&info);
    if (result >= 0) {
        if (info.size_user < 0 || info.size_cdram < 0 || info.size_phycont < 0) return -1;
        out[0] = (uint32_t)info.size_user;
        out[1] = (uint32_t)info.size_cdram;
        out[2] = (uint32_t)info.size_phycont;
    }
    return result;
}

int32_t kv_probe_memory_round_trip(uint32_t pool, KvMemoryRoundTrip *out) {
    static const SceKernelMemBlockType types[3] = {
        SCE_KERNEL_MEMBLOCK_TYPE_USER_RW, SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW,
        SCE_KERNEL_MEMBLOCK_TYPE_USER_MAIN_PHYCONT_RW
    };
    if (!out || pool >= 3) return -1;
    *out = (KvMemoryRoundTrip){ .pool = pool, .bytes = pool == 1 ? 1024 * 1024 : 64 * 1024 };
    for (unsigned i = 0; i < 5; ++i) out->status[i] = -1;
    out->status[0] = memory_snapshot(out->before);
    if (out->status[0] < 0) return out->status[0];
    out->status[1] = sceKernelAllocMemBlock("knife-memory-reclaim-probe", types[pool], out->bytes, NULL);
    if (out->status[1] < 0) return out->status[1];
    out->status[2] = memory_snapshot(out->during);
    /* Cleanup is mandatory even if the middle query failed. */
    out->status[3] = sceKernelFreeMemBlock(out->status[1]);
    out->status[4] = memory_snapshot(out->after);
    for (unsigned i = 2; i < 5; ++i) if (out->status[i] < 0) return out->status[i];
    return 0;
}
