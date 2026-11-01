#include "movie_player.h"
#include <psp2/avplayer.h>
#include <psp2/sysmodule.h>
#ifdef KV_MOVIE_TEST
#include "movie_native.h"
#else
#include <psp2/gxm.h>
#include <psp2/kernel/sysmem.h>
#include <psp2/kernel/threadmgr/mutex.h>
#include <psp2/kernel/threadmgr/thread.h>
#endif
#include <limits.h>
#include <malloc.h>
#include <math.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define KV_MOVIE_ALLOCATIONS 64
#define KV_MOVIE_MAX_ALLOCATION (16u * 1024u * 1024u)
#define KV_MOVIE_PHYCONT_ALIGNMENT (1024u * 1024u)
#define KV_MOVIE_MAX_PATH 1024
#define KV_MOVIE_MAX_VIDEO_WIDTH 960u
#define KV_MOVIE_MAX_VIDEO_HEIGHT 544u
#define KV_MOVIE_MAX_STREAMS 64
#define KV_MOVIE_READY_EVENT 2
#define KV_MOVIE_READY_POLL_US 1000u
/* These two exports are in VitaSDK's AvPlayer import library but are missing
 * from its avplayer.h. Their firmware signatures use the same 32-bit handle
 * representation as the declarations already supplied by VitaSDK. */
extern int sceAvPlayerStreamCount(SceAvPlayerHandle handle);
extern int sceAvPlayerEnableStream(SceAvPlayerHandle handle, uint32_t stream_id);
/* The Ex export is also present in VitaSDK's import library, but its public
 * header omits the record. This is an independent ABI projection of the 80-byte
 * video details in the native 96-byte ARM frame; no SDK implementation is used.
 * Host fixtures use the same declared C types, with host pointer alignment. */
typedef struct KvNativeMovieVideoEx {
    uint32_t width, height;
    float aspect;
    uint8_t language[4];
    uint32_t framerate, crop_left, crop_right, crop_top, crop_bottom;
    uint8_t reserved[44];
} KvNativeMovieVideoEx;
typedef struct KvNativeMovieFrameEx {
    void *data;
    uint8_t reserved[4];
    uint64_t timestamp;
    KvNativeMovieVideoEx video;
} KvNativeMovieFrameEx;
extern SceBool sceAvPlayerGetVideoDataEx(SceAvPlayerHandle handle, KvNativeMovieFrameEx *frame);
_Static_assert(sizeof(KvNativeMovieVideoEx) == 80, "native video Ex details size");
_Static_assert(offsetof(KvNativeMovieVideoEx, crop_left) == 20, "native Ex left crop offset");
_Static_assert(offsetof(KvNativeMovieVideoEx, crop_bottom) == 32, "native Ex bottom crop offset");
#if UINTPTR_MAX == UINT32_MAX
_Static_assert(sizeof(KvNativeMovieFrameEx) == 96, "native ARM frame Ex size");
_Static_assert(offsetof(KvNativeMovieFrameEx, timestamp) == 8, "native ARM frame timestamp");
_Static_assert(offsetof(KvNativeMovieFrameEx, video) == 16, "native ARM frame Ex video offset");
_Static_assert(sizeof(KvMovieVideo) == 32, "legacy ARM movie frame ABI stays 32 bytes");
_Static_assert(sizeof(KvMovieVideoEx) == 48, "extended ARM movie frame ABI size");
_Static_assert(offsetof(KvMovieVideoEx, crop_left) == 32, "extended ARM movie crop ABI offset");
#endif
/* The firmware API returns an opaque pointer and NULL on Init failure.
 * VitaSDK stores its 32-bit ABI bits in a signed integer: a high-bit handle
 * such as 0x840010a0 is valid and must reach Close before callbacks are freed. */
#define KV_MOVIE_NO_HANDLE ((SceAvPlayerHandle)0)

typedef struct KvMovieAllocation {
    void *pointer;
    uint32_t size;
    SceUID block;
    unsigned texture : 1;
    unsigned mapped : 1;
    unsigned occupied : 1;
} KvMovieAllocation;
struct KvMovie {
    SceAvPlayerHandle handle;
    SceUID io_mutex, allocation_mutex;
    FILE *file;
    const uint8_t *encoded;
    uint32_t length, allocated_bytes;
    char path[KV_MOVIE_MAX_PATH];
    KvMovieAllocation allocations[KV_MOVIE_ALLOCATIONS];
    atomic_int callback_error;
    atomic_int last_event;
    atomic_int source_ready;
    unsigned module_owned : 1;
    unsigned started : 1;
    unsigned paused : 1;
    unsigned stopped : 1;
    unsigned seen_active : 1;
    unsigned seen_frame : 1;
    unsigned streams_selected : 1;
    uint32_t pending_frames, pending_position, pending_channels;
    uint64_t pending_timestamp;
    int16_t pending[KV_MOVIE_AUDIO_PENDING_FRAMES * 2];
};
static atomic_int player_owned;

static int remember(KvMovie *movie, int error) {
    int expected = 0;
    if (error) atomic_compare_exchange_strong(&movie->callback_error, &expected, error);
    return error;
}
static int checked(KvMovie *movie) {
    if (!movie || movie->handle == KV_MOVIE_NO_HANDLE) return KV_MOVIE_STATE;
    return atomic_load(&movie->callback_error);
}
static int native_status(int code) { return code <= 0 ? code : KV_MOVIE_OUTPUT; }
static int lock(KvMovie *movie, SceUID mutex) {
    int code = native_status(sceKernelLockMutex(mutex, 1, NULL));
    return code ? remember(movie, code) : 0;
}
static int unlock(KvMovie *movie, SceUID mutex) {
    int code = native_status(sceKernelUnlockMutex(mutex, 1));
    return code ? remember(movie, code) : 0;
}
static int io_close(void *object) {
    KvMovie *movie = object;
    int code = lock(movie, movie->io_mutex);
    if (code) return code;
    if (movie->file) {
        if (fclose(movie->file)) code = remember(movie, KV_MOVIE_IO);
        movie->file = NULL;
    }
    int released = unlock(movie, movie->io_mutex);
    return code ? code : released;
}
static int io_open(void *object, const char *filename) {
    KvMovie *movie = object;
    if (!filename || strcmp(filename, movie->path)) return remember(movie, KV_MOVIE_IO);
    int code = lock(movie, movie->io_mutex);
    if (code) return code;
    if (!movie->encoded && !movie->file) {
        movie->file = fopen(movie->path, "rb");
        if (!movie->file) code = remember(movie, KV_MOVIE_IO);
        else if (setvbuf(movie->file, NULL, _IONBF, 0)) code = remember(movie, KV_MOVIE_IO);
    }
    int released = unlock(movie, movie->io_mutex);
    return code ? code : released;
}
static uint64_t io_size(void *object) { return ((KvMovie *)object)->length; }
static int io_read(void *object, uint8_t *buffer, uint64_t position, uint32_t length) {
    KvMovie *movie = object;
    if (!buffer && length) return remember(movie, KV_MOVIE_ARGUMENT);
    if (length > KV_MOVIE_MAX_READ_BYTES) return remember(movie, KV_MOVIE_BUDGET);
    if (position > movie->length || position > UINT64_MAX - length)
        return remember(movie, KV_MOVIE_IO);
    uint32_t remaining = movie->length - (uint32_t)position;
    uint32_t wanted = length < remaining ? length : remaining;
    if (!wanted) return 0;
    int code = lock(movie, movie->io_mutex);
    if (code) return code;
    if (movie->encoded) memcpy(buffer, movie->encoded + (uint32_t)position, wanted);
    else if (!movie->file || position > LONG_MAX || fseek(movie->file, (long)position, SEEK_SET)
             || fread(buffer, 1, wanted, movie->file) != wanted) code = remember(movie, KV_MOVIE_IO);
    int released = unlock(movie, movie->io_mutex);
    return code ? code : released ? released : (int)wanted;
}
static void *allocate(KvMovie *movie, uint32_t alignment, uint32_t size, int texture) {
    if (!alignment || (alignment & (alignment - 1)) || alignment > KV_MOVIE_PHYCONT_ALIGNMENT)
        { remember(movie, KV_MOVIE_ARGUMENT); return NULL; }
    if (!size || size > KV_MOVIE_MAX_ALLOCATION)
        { remember(movie, KV_MOVIE_BUDGET); return NULL; }
    if (alignment < sizeof(void *)) alignment = sizeof(void *);
    /* Charge the worst-case CPU alignment padding as well as the payload;
     * otherwise many highly aligned small allocations evade the budget. */
    if (size > UINT32_MAX - (alignment - 1))
        { remember(movie, KV_MOVIE_BUDGET); return NULL; }
    uint32_t charge = size + alignment - 1;
    if (texture) charge = (size + KV_MOVIE_PHYCONT_ALIGNMENT - 1) & ~(KV_MOVIE_PHYCONT_ALIGNMENT - 1);
    if (lock(movie, movie->allocation_mutex)) return NULL;
    KvMovieAllocation *slot = NULL;
    for (uint32_t i = 0; i < KV_MOVIE_ALLOCATIONS; i++) {
        if (!movie->allocations[i].occupied) { slot = &movie->allocations[i]; break; }
    }
    if (!slot || charge > KV_MOVIE_MAX_DECODE_BYTES - movie->allocated_bytes) {
        remember(movie, KV_MOVIE_BUDGET);
        unlock(movie, movie->allocation_mutex);
        return NULL;
    }
    int code = 0;
    void *pointer = NULL;
    SceUID block = -1;
    if (!texture) {
        pointer = memalign(alignment, size);
        if (!pointer) code = KV_MOVIE_MEMORY;
    } else {
        block = sceKernelAllocMemBlock("knife movie frame", SCE_KERNEL_MEMBLOCK_TYPE_USER_MAIN_PHYCONT_NC_RW, charge, NULL);
        if (block < 0) code = block;
        else {
            code = native_status(sceKernelGetMemBlockBase(block, &pointer));
            if (!code && (!pointer || (uintptr_t)pointer % alignment)) code = KV_MOVIE_OUTPUT;
            if (!code) code = native_status(sceGxmMapMemory(pointer, charge, SCE_GXM_MEMORY_ATTRIB_RW));
            if (code) {
                int freed = native_status(sceKernelFreeMemBlock(block));
                if (freed) {
                    /* Retain a failed cleanup allocation for an explicit Close retry. */
                    slot->pointer = pointer; slot->block = block; slot->size = charge;
                    slot->texture = 1; slot->occupied = 1; movie->allocated_bytes += charge;
                    remember(movie, freed);
                }
                pointer = NULL;
            }
        }
    }
    if (code) remember(movie, code);
    else {
        slot->pointer = pointer; slot->size = charge; slot->block = block;
        slot->texture = texture != 0; slot->mapped = texture != 0; slot->occupied = 1;
        movie->allocated_bytes += charge;
    }
    if (unlock(movie, movie->allocation_mutex)) return NULL;
    return pointer;
}
static int free_slot(KvMovie *movie, KvMovieAllocation *slot) {
    if (slot->texture) {
        if (slot->mapped) {
            int code = native_status(sceGxmUnmapMemory(slot->pointer));
            if (code) return remember(movie, code);
            slot->mapped = 0;
        }
        int code = native_status(sceKernelFreeMemBlock(slot->block));
        if (code) return remember(movie, code);
    } else free(slot->pointer);
    movie->allocated_bytes -= slot->size;
    memset(slot, 0, sizeof(*slot));
    return 0;
}
static void deallocate(KvMovie *movie, void *pointer, int texture) {
    if (!pointer) return;
    if (lock(movie, movie->allocation_mutex)) return;
    uint32_t i;
    for (i = 0; i < KV_MOVIE_ALLOCATIONS; i++) {
        KvMovieAllocation *slot = &movie->allocations[i];
        if (slot->occupied && slot->pointer == pointer) {
            if (slot->texture != (unsigned)texture) remember(movie, KV_MOVIE_OUTPUT);
            else free_slot(movie, slot);
            break;
        }
    }
    if (i == KV_MOVIE_ALLOCATIONS) remember(movie, KV_MOVIE_OUTPUT);
    unlock(movie, movie->allocation_mutex);
}
static void *allocate_cpu(void *object, uint32_t alignment, uint32_t size) { return allocate(object, alignment, size, 0); }
static void *allocate_gpu(void *object, uint32_t alignment, uint32_t size) { return allocate(object, alignment, size, 1); }
static void free_cpu(void *object, void *pointer) { deallocate(object, pointer, 0); }
static void free_gpu(void *object, void *pointer) { deallocate(object, pointer, 1); }
static void event(void *object, int32_t id, int32_t source, void *data) {
    (void)source; (void)data;
    KvMovie *movie = object;
    atomic_store(&movie->last_event, id);
    /* READY can be followed by BUFFERING before the owner thread wakes.
     * Latch it independently; callbacks never issue player control calls. */
    if (id == KV_MOVIE_READY_EVENT) atomic_store(&movie->source_ready, 1);
    /* The public header supplies no event-payload ABI. Never dereference it. */
    if (id < 0) remember(movie, id);
}
int kv_movie_close(KvMovie **owner) {
    if (!owner) return KV_MOVIE_ARGUMENT;
    KvMovie *movie = *owner;
    if (!movie) return 0;
    if (movie->handle != KV_MOVIE_NO_HANDLE) {
        int code = native_status(sceAvPlayerClose(movie->handle));
        if (code) return code; /* Callbacks/encoded bytes must remain alive. */
        movie->handle = KV_MOVIE_NO_HANDLE;
    }
    if (movie->io_mutex >= 0) {
        int code = io_close(movie);
        if (code) return code;
    }
    if (movie->allocation_mutex >= 0) {
        int code = lock(movie, movie->allocation_mutex);
        if (code) return code;
        for (uint32_t i = 0; i < KV_MOVIE_ALLOCATIONS; i++) {
            if (movie->allocations[i].occupied) {
                code = free_slot(movie, &movie->allocations[i]);
                if (code) break;
            }
        }
        int released = unlock(movie, movie->allocation_mutex);
        if (code || released) return code ? code : released;
    }
    if (movie->module_owned) {
        int code = native_status(sceSysmoduleUnloadModule(SCE_SYSMODULE_AVPLAYER));
        if (code) return code;
        movie->module_owned = 0;
    }
    if (movie->io_mutex >= 0) {
        int code = native_status(sceKernelDeleteMutex(movie->io_mutex));
        if (code) return code;
        movie->io_mutex = -1;
    }
    if (movie->allocation_mutex >= 0) {
        int code = native_status(sceKernelDeleteMutex(movie->allocation_mutex));
        if (code) return code;
        movie->allocation_mutex = -1;
    }
    free(movie);
    *owner = NULL;
    atomic_store(&player_owned, 0);
    return 0;
}
static int format_header(const uint8_t *bytes, uint32_t length) {
    if (length < 16 || memcmp(bytes + 4, "ftyp", 4)) return KV_MOVIE_FORMAT;
    uint32_t size = (uint32_t)bytes[0] << 24 | (uint32_t)bytes[1] << 16 | (uint32_t)bytes[2] << 8 | bytes[3];
    return size < 16 || size > length ? KV_MOVIE_FORMAT : 0;
}
static int create(KvMovie **out) {
    int expected = 0;
    if (!atomic_compare_exchange_strong(&player_owned, &expected, 1)) return KV_MOVIE_BUSY;
    KvMovie *movie = calloc(1, sizeof(*movie));
    if (!movie) { atomic_store(&player_owned, 0); return KV_MOVIE_MEMORY; }
    movie->handle = KV_MOVIE_NO_HANDLE; movie->io_mutex = movie->allocation_mutex = -1;
    atomic_init(&movie->callback_error, 0); atomic_init(&movie->last_event, 0);
    atomic_init(&movie->source_ready, 0);
    *out = movie;
    movie->io_mutex = sceKernelCreateMutex("knife movie io", 0, 0, NULL);
    if (movie->io_mutex < 0) return movie->io_mutex;
    movie->allocation_mutex = sceKernelCreateMutex("knife movie memory", 0, 0, NULL);
    if (movie->allocation_mutex < 0) return movie->allocation_mutex;
    return 0;
}
static int open_native(KvMovie *movie) {
    int code = sceSysmoduleIsLoaded(SCE_SYSMODULE_AVPLAYER);
    if (code == (int)SCE_SYSMODULE_ERROR_UNLOADED) {
        code = native_status(sceSysmoduleLoadModule(SCE_SYSMODULE_AVPLAYER));
        if (code) return code;
        movie->module_owned = 1;
    } else if (code) return native_status(code);
    SceAvPlayerInitData init;
    memset(&init, 0, sizeof(init));
    init.memoryReplacement = (SceAvPlayerMemReplacement){movie, allocate_cpu, free_cpu, allocate_gpu, free_gpu};
    init.fileReplacement = (SceAvPlayerFileReplacement){movie, io_open, io_close, io_read, io_size};
    init.eventReplacement = (SceAvPlayerEventReplacement){movie, event};
    init.basePriority = 0xA0;
    init.numOutputVideoFrameBuffers = 5;
    init.autoStart = SCE_FALSE;
    init.defaultLanguage = "eng";
    movie->handle = sceAvPlayerInit(&init);
    if (movie->handle == KV_MOVIE_NO_HANDLE) {
        int error = atomic_load(&movie->callback_error);
        return error ? error : KV_MOVIE_MEMORY;
    }
    code = native_status(sceAvPlayerAddSource(movie->handle, movie->path));
    return code ? code : atomic_load(&movie->callback_error);
}
static int finish_open(int code, KvMovie **out) {
    if (code) { int cleanup = kv_movie_close(out); if (cleanup) return cleanup; }
    return code;
}
int kv_movie_open_memory(const uint8_t *bytes, uint32_t length, KvMovie **out) {
    if (!out) return KV_MOVIE_ARGUMENT;
    *out = NULL;
    if (!bytes || !length) return KV_MOVIE_ARGUMENT;
    if (length > KV_MOVIE_MAX_ENCODED_BYTES) return KV_MOVIE_BUDGET;
    int code = format_header(bytes, length);
    if (code) return code;
    code = create(out);
    if (!code) {
        KvMovie *movie = *out;
        movie->encoded = bytes; movie->length = length;
        strcpy(movie->path, "knife-memory.mp4");
        code = open_native(movie);
    }
    return finish_open(code, out);
}
int kv_movie_open_file(const char *path, KvMovie **out) {
    if (!out) return KV_MOVIE_ARGUMENT;
    *out = NULL;
    if (!path || !*path) return KV_MOVIE_ARGUMENT;
    size_t path_length = 0;
    while (path_length < KV_MOVIE_MAX_PATH && path[path_length]) path_length++;
    if (path_length == KV_MOVIE_MAX_PATH) return KV_MOVIE_ARGUMENT;
    FILE *file = fopen(path, "rb");
    if (!file) return KV_MOVIE_IO;
    long length = -1;
    uint8_t header[16];
    int code = 0;
    if (setvbuf(file, NULL, _IONBF, 0) || fseek(file, 0, SEEK_END) || (length = ftell(file)) < 0
        || fseek(file, 0, SEEK_SET)) code = KV_MOVIE_IO;
    else if ((uint64_t)length > KV_MOVIE_MAX_ENCODED_BYTES) code = KV_MOVIE_BUDGET;
    else if (length < 16) code = KV_MOVIE_FORMAT;
    else if (fread(header, 1, 16, file) != 16) code = KV_MOVIE_IO;
    else code = format_header(header, (uint32_t)length);
    if (code) { fclose(file); return code; }
    code = create(out);
    if (code) { fclose(file); return finish_open(code, out); }
    KvMovie *movie = *out;
    movie->file = file; movie->length = (uint32_t)length;
    memcpy(movie->path, path, path_length + 1);
    code = open_native(movie);
    return finish_open(code, out);
}
static int wait_ready(KvMovie *movie) {
    uint64_t started = (uint64_t)sceKernelGetSystemTimeWide();
    /* The iteration bound also prevents an unbounded wait if a broken clock
     * stops advancing. Delay yields to AvPlayer's asynchronous source parser. */
    for (uint32_t waits = 0; ; waits++) {
        int code = checked(movie);
        if (code) return code;
        if (atomic_load(&movie->source_ready)) return 0;
        uint64_t now = (uint64_t)sceKernelGetSystemTimeWide();
        if (waits >= KV_MOVIE_READY_TIMEOUT_US / KV_MOVIE_READY_POLL_US
            || now - started >= KV_MOVIE_READY_TIMEOUT_US) return KV_MOVIE_TIMEOUT;
        code = native_status(sceKernelDelayThread(KV_MOVIE_READY_POLL_US));
        if (code) return code;
    }
}
static int select_streams(KvMovie *movie) {
    if (movie->streams_selected) return 0;
    int count = sceAvPlayerStreamCount(movie->handle);
    if (count < 0) return count;
    if (count > KV_MOVIE_MAX_STREAMS) return KV_MOVIE_BUDGET;
    int video = -1, audio = -1, has_audio = 0;
    /* Validate the full bounded inventory before enabling either stream. The
     * game supplies one source per player, with separately selected EN/ZC files. */
    for (int id = 0; id < count; id++) {
        SceAvPlayerStreamInfo info;
        memset(&info, 0, sizeof(info));
        int code = checked(movie);
        if (!code) code = native_status(sceAvPlayerGetStreamInfo(movie->handle, (uint32_t)id, &info));
        if (code) return code;
        if (info.type == SCE_AVPLAYER_VIDEO) {
            const SceAvPlayerVideo *v = &info.details.video;
            if (video < 0 && v->width && v->height && v->width <= KV_MOVIE_MAX_VIDEO_WIDTH
                && v->height <= KV_MOVIE_MAX_VIDEO_HEIGHT && !(v->width & 1) && !(v->height & 1)
                && isfinite(v->aspectRatio) && v->aspectRatio > 0.0f) video = id;
        } else if (info.type == SCE_AVPLAYER_AUDIO) {
            const SceAvPlayerAudio *a = &info.details.audio;
            has_audio = 1;
            if (audio < 0 && a->sampleRate == 48000 && (a->channelCount == 1 || a->channelCount == 2)) audio = id;
        }
    }
    if (video < 0 || (has_audio && audio < 0)) return KV_MOVIE_UNSUPPORTED;
    int code = checked(movie);
    if (!code) code = native_status(sceAvPlayerEnableStream(movie->handle, (uint32_t)video));
    if (!code && audio >= 0) code = native_status(sceAvPlayerEnableStream(movie->handle, (uint32_t)audio));
    /* An enable failure can leave one stream enabled. Retain the owner and
     * require Close rather than repeating a partially completed selection. */
    if (code) return remember(movie, code);
    movie->streams_selected = 1;
    return atomic_load(&movie->callback_error);
}
/* Poll the asynchronous source parser without sleeping on the game thread.
 * All control still executes on the player's creating thread. */
int kv_movie_source_ready(KvMovie *movie, uint32_t *ready) {
    if (!ready) return KV_MOVIE_ARGUMENT;
    *ready = 0;
    int code = checked(movie);
    if (code) return code;
    *ready = movie->started || atomic_load(&movie->source_ready) ? 1 : 0;
    return 0;
}
int kv_movie_select_streams(KvMovie *movie) {
    int code = checked(movie);
    if (code) return code;
    if (!atomic_load(&movie->source_ready)) return KV_MOVIE_STATE;
    return select_streams(movie);
}
int kv_movie_start_selected(KvMovie *movie) {
    int code = checked(movie);
    if (code) return code;
    if (movie->started) return 0;
    if (!movie->streams_selected) return KV_MOVIE_STATE;
    code = native_status(sceAvPlayerStart(movie->handle));
    if (!code) { movie->started = 1; movie->stopped = 0; }
    return code ? code : atomic_load(&movie->callback_error);
}
int kv_movie_poll_start(KvMovie *movie, uint32_t *ready) {
    int code = kv_movie_source_ready(movie, ready);
    if (code || !*ready) return code;
    if (movie->started) return 0;
    *ready = 0;
    code = kv_movie_select_streams(movie);
    if (!code) code = kv_movie_start_selected(movie);
    if (!code) { movie->started = 1; movie->stopped = 0; *ready = 1; }
    return code ? code : atomic_load(&movie->callback_error);
}
int kv_movie_start(KvMovie *movie) {
    int code = checked(movie);
    if (code) return code;
    if (movie->started) return KV_MOVIE_STATE;
    code = wait_ready(movie);
    if (code) return code;
    uint32_t ready = 0;
    code = kv_movie_poll_start(movie, &ready);
    return code ? code : ready ? 0 : KV_MOVIE_STATE;
}
int kv_movie_pause(KvMovie *movie) {
    int code = checked(movie);
    if (code) return code;
    if (!movie->started || movie->stopped || movie->paused) return KV_MOVIE_STATE;
    code = native_status(sceAvPlayerPause(movie->handle));
    if (!code) movie->paused = 1;
    return code ? code : atomic_load(&movie->callback_error);
}
int kv_movie_resume(KvMovie *movie) {
    int code = checked(movie);
    if (code) return code;
    if (!movie->paused || movie->stopped) return KV_MOVIE_STATE;
    code = native_status(sceAvPlayerResume(movie->handle));
    if (!code) movie->paused = 0;
    return code ? code : atomic_load(&movie->callback_error);
}
int kv_movie_stop(KvMovie *movie) {
    if (!movie || movie->handle == KV_MOVIE_NO_HANDLE) return KV_MOVIE_STATE;
    int code = native_status(sceAvPlayerStop(movie->handle));
    if (!code) {
        movie->stopped = 1; movie->paused = 0;
        movie->pending_frames = movie->pending_position = 0;
    }
    return code ? code : atomic_load(&movie->callback_error);
}
int kv_movie_seek_ms(KvMovie *movie, uint64_t timestamp) {
    int code = checked(movie);
    if (code) return code;
    if (!movie->started || movie->stopped) return KV_MOVIE_STATE;
    code = native_status(sceAvPlayerJumpToTime(movie->handle, timestamp));
    if (!code) { movie->pending_frames = movie->pending_position = 0; movie->seen_frame = 0; }
    return code ? code : atomic_load(&movie->callback_error);
}
int kv_movie_time_ms(KvMovie *movie, uint64_t *timestamp) {
    if (!timestamp) return KV_MOVIE_ARGUMENT;
    int code = checked(movie);
    if (code) return code;
    *timestamp = sceAvPlayerCurrentTime(movie->handle);
    return atomic_load(&movie->callback_error);
}
int kv_movie_poll_state(KvMovie *movie, uint32_t *state) {
    if (!state) return KV_MOVIE_ARGUMENT;
    int code = checked(movie);
    if (code) return code;
    int active = sceAvPlayerIsActive(movie->handle) != 0;
    if (active) movie->seen_active = 1;
    if (movie->stopped) *state = KV_MOVIE_STOPPED;
    else if (movie->paused) *state = KV_MOVIE_PAUSED;
    else if (!movie->started) *state = KV_MOVIE_READY;
    /* This follows the native vitaGL sample's completion observation. The
     * public SDK has no typed event payload that would distinguish a late
     * asynchronous decode error from normal completion. */
    else if (!active && movie->seen_active && movie->seen_frame && !movie->pending_frames) *state = KV_MOVIE_EOF;
    else *state = KV_MOVIE_PLAYING;
    return atomic_load(&movie->callback_error);
}
int kv_movie_next_video(KvMovie *movie, KvMovieVideo *out, uint32_t *available) {
    if (!out || !available) return KV_MOVIE_ARGUMENT;
    memset(out, 0, sizeof(*out)); *available = 0;
    int code = checked(movie);
    if (code) return code;
    if (!movie->started || movie->stopped) return KV_MOVIE_STATE;
    if (movie->paused || !sceAvPlayerIsActive(movie->handle)) return 0;
    movie->seen_active = 1;
    SceAvPlayerFrameInfo frame;
    memset(&frame, 0, sizeof(frame));
    if (sceAvPlayerGetVideoData(movie->handle, &frame)) {
        uint32_t width = frame.details.video.width, height = frame.details.video.height;
        float aspect = frame.details.video.aspectRatio;
        if (!frame.pData || !width || !height || width > KV_MOVIE_MAX_VIDEO_WIDTH || height > KV_MOVIE_MAX_VIDEO_HEIGHT
            || (width & 1) || (height & 1) || !isfinite(aspect) || aspect <= 0)
            return remember(movie, KV_MOVIE_OUTPUT);
        out->data = frame.pData; out->width = width; out->height = height;
        out->aspect_ratio = aspect; out->timestamp_ms = frame.timeStamp;
        out->format = KV_MOVIE_VIDEO_FORMAT_GXM_YVU420P2_CSC1;
        movie->seen_frame = 1; *available = 1;
    }
    return atomic_load(&movie->callback_error);
}
int kv_movie_next_video_ex(KvMovie *movie, KvMovieVideoEx *out, uint32_t *available) {
    if (!out || !available) return KV_MOVIE_ARGUMENT;
    memset(out, 0, sizeof(*out)); *available = 0;
    int code = checked(movie);
    if (code) return code;
    if (!movie->started || movie->stopped) return KV_MOVIE_STATE;
    if (movie->paused || !sceAvPlayerIsActive(movie->handle)) return 0;
    movie->seen_active = 1;
    KvNativeMovieFrameEx frame;
    memset(&frame, 0, sizeof(frame));
    if (sceAvPlayerGetVideoDataEx(movie->handle, &frame)) {
        const KvNativeMovieVideoEx *v = &frame.video;
        if (!frame.data || !v->width || !v->height || v->width > KV_MOVIE_MAX_VIDEO_WIDTH
            || v->height > KV_MOVIE_MAX_VIDEO_HEIGHT || (v->width & 1) || (v->height & 1)
            || !isfinite(v->aspect) || v->aspect <= 0)
            return remember(movie, KV_MOVIE_OUTPUT);
        /* A crop unit cannot represent fewer than one pixel. Even before the
         * caller resolves native units, reject impossible/overflowing extents
         * without adding untrusted u32 offsets or changing their values. */
        if (v->crop_left >= v->width || v->crop_right >= v->width - v->crop_left
            || v->crop_top >= v->height || v->crop_bottom >= v->height - v->crop_top)
            return remember(movie, KV_MOVIE_OUTPUT);
        out->frame.data = frame.data; out->frame.width = v->width; out->frame.height = v->height;
        out->frame.aspect_ratio = v->aspect; out->frame.timestamp_ms = frame.timestamp;
        out->frame.format = KV_MOVIE_VIDEO_FORMAT_GXM_YVU420P2_CSC1;
        out->crop_left = v->crop_left; out->crop_right = v->crop_right;
        out->crop_top = v->crop_top; out->crop_bottom = v->crop_bottom;
        movie->seen_frame = 1; *available = 1;
    }
    return atomic_load(&movie->callback_error);
}
int kv_movie_read_audio(KvMovie *movie, float *stereo, uint32_t capacity, KvMovieAudioRead *read) {
    if (!read || !stereo || !capacity || capacity > KV_MOVIE_AUDIO_CHUNK_FRAMES) return KV_MOVIE_ARGUMENT;
    memset(read, 0, sizeof(*read));
    int code = checked(movie);
    if (code) return code;
    if (!movie->started || movie->stopped) return KV_MOVIE_STATE;
    if (movie->paused) return 0;
    if (!movie->pending_frames) {
        if (!sceAvPlayerIsActive(movie->handle)) return 0;
        movie->seen_active = 1;
        SceAvPlayerFrameInfo frame;
        memset(&frame, 0, sizeof(frame));
        if (!sceAvPlayerGetAudioData(movie->handle, &frame)) return atomic_load(&movie->callback_error);
        uint32_t channels = frame.details.audio.channelCount;
        if ((channels != 1 && channels != 2) || frame.details.audio.sampleRate != 48000)
            return remember(movie, KV_MOVIE_UNSUPPORTED);
        uint32_t size = frame.details.audio.size;
        uint32_t frame_size = channels * sizeof(int16_t);
        if (!frame.pData || !size || size % frame_size || size / frame_size > KV_MOVIE_AUDIO_PENDING_FRAMES)
            return remember(movie, KV_MOVIE_OUTPUT);
        /* SDK owns pData. Copy this one bounded frame before another SDK pull. */
        memcpy(movie->pending, frame.pData, size);
        movie->pending_channels = channels; movie->pending_frames = size / frame_size;
        movie->pending_position = 0; movie->pending_timestamp = frame.timeStamp;
        movie->seen_frame = 1;
    }
    uint32_t count = capacity < movie->pending_frames ? capacity : movie->pending_frames;
    uint32_t channels = movie->pending_channels;
    uint64_t increment = (uint64_t)movie->pending_position * 1000 / 48000;
    if (increment > UINT64_MAX - movie->pending_timestamp) return remember(movie, KV_MOVIE_OUTPUT);
    read->timestamp_ms = movie->pending_timestamp + increment;
    for (uint32_t i = 0; i < count; i++) {
        uint32_t sample = (movie->pending_position + i) * channels;
        stereo[i * 2] = (float)movie->pending[sample] / 32768.0f;
        stereo[i * 2 + 1] = (float)movie->pending[sample + (channels == 2)] / 32768.0f;
    }
    movie->pending_position += count; movie->pending_frames -= count;
    read->frames = count;
    return atomic_load(&movie->callback_error);
}
