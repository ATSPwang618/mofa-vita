#ifndef KNIFE_VITA_MOVIE_PLAYER_H
#define KNIFE_VITA_MOVIE_PLAYER_H
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

enum {
    KV_MOVIE_ARGUMENT = -1, KV_MOVIE_IO = -2, KV_MOVIE_FORMAT = -3,
    KV_MOVIE_BUDGET = -4, KV_MOVIE_MEMORY = -5, KV_MOVIE_UNSUPPORTED = -6,
    KV_MOVIE_STATE = -7, KV_MOVIE_BUSY = -8, KV_MOVIE_OUTPUT = -9,
    KV_MOVIE_TIMEOUT = -10,
    KV_MOVIE_READY_TIMEOUT_US = 5000000,
    KV_MOVIE_MAX_ENCODED_BYTES = 1024 * 1024 * 1024,
    KV_MOVIE_MAX_READ_BYTES = 1024 * 1024,
    KV_MOVIE_MAX_DECODE_BYTES = 32 * 1024 * 1024,
    KV_MOVIE_AUDIO_CHUNK_FRAMES = 960,
    KV_MOVIE_AUDIO_PENDING_FRAMES = 4096,
    KV_MOVIE_VIDEO_FORMAT_GXM_YVU420P2_CSC1 = 1
};
typedef struct KvMovie KvMovie;
typedef struct KvMovieVideo {
    const uint8_t *data;
    uint32_t width, height, format;
    float aspect_ratio;
    uint32_t reserved;
    uint64_t timestamp_ms;
} KvMovieVideo;
/* Width/height in frame describe native texture storage, including decoder
 * alignment. The native aspect value and four Ex crop offsets are preserved
 * verbatim. The SDK declaration does not specify crop-offset units, so these
 * fields alone must not be relabelled as a pixel rectangle or a display aspect
 * ratio. Validate against the source's coded/display geometry before sampling.
 * In particular, a 960x544 storage frame can contain a 960x540 picture. */
typedef struct KvMovieVideoEx {
    KvMovieVideo frame;
    uint32_t crop_left, crop_right, crop_top, crop_bottom;
} KvMovieVideoEx;
typedef struct KvMovieAudioRead { uint64_t timestamp_ms; uint32_t frames, reserved; } KvMovieAudioRead;
enum { KV_MOVIE_READY = 0, KV_MOVIE_PLAYING = 1, KV_MOVIE_PAUSED = 2,
       KV_MOVIE_STOPPED = 3, KV_MOVIE_EOF = 4 };

/* Open failure can leave *out non-NULL only if native Close also failed. The
 * owner and (for memory sources) the encoded storage must then remain alive. */
int kv_movie_open_file(const char *path, KvMovie **out);
/* Immutable bytes must remain alive through successful Close; no encoded copy. */
int kv_movie_open_memory(const uint8_t *bytes, uint32_t length, KvMovie **out);
/* Manual playback: wait at most 5 s for READY, inspect <=64 streams, enable
 * the first supported video (<=960x544) and optional 48 kHz mono/stereo audio,
 * then Start. Unsupported metadata and native failures are returned unchanged.
 * A timeout retains the player and may be retried; an enable failure requires
 * Close because a partially selected stream may already be active. */
int kv_movie_start(KvMovie *movie);
/* Nonblocking counterpart: ready=0 until source READY, ready=1 after selecting
 * the same supported streams and starting. No sleeps or implicit EOF. The
 * creating-thread caller owns its deadline and keeps the player until Close. */
int kv_movie_poll_start(KvMovie *movie, uint32_t *ready);
/* Split owner-thread startup lets callers identify native Select versus Start
 * latency. source_ready only observes parser state; the others require READY. */
int kv_movie_source_ready(KvMovie *movie, uint32_t *ready);
int kv_movie_select_streams(KvMovie *movie);
int kv_movie_start_selected(KvMovie *movie);
int kv_movie_pause(KvMovie *movie);
int kv_movie_resume(KvMovie *movie);
int kv_movie_stop(KvMovie *movie);
int kv_movie_seek_ms(KvMovie *movie, uint64_t timestamp_ms);
int kv_movie_time_ms(KvMovie *movie, uint64_t *timestamp_ms);
/* EOF is the native sample's inactive-after-a-valid-frame observation; the
 * public SDK does not distinguish late asynchronous failures via a typed ABI. */
int kv_movie_poll_state(KvMovie *movie, uint32_t *state);
/* Frame data belongs to AvPlayer. CPU stride/byte length are NOT provided by
 * this ABI. Complete all GPU reads before the next video pull/control/Close. */
int kv_movie_next_video(KvMovie *movie, KvMovieVideo *frame, uint32_t *available);
/* Extended pull consumes one frame, just like next_video; do not call both for
 * the same presentation. The borrowed data and GPU-fence rules are identical.
 * No padding rows are removed and no native CPU stride is inferred. */
int kv_movie_next_video_ex(KvMovie *movie, KvMovieVideoEx *frame, uint32_t *available);
/* Only interleaved signed-16-bit 48 kHz mono/stereo SDK audio is accepted.
 * Copy/convert at most 960 stereo float frames, retaining <=4096 input frames. */
int kv_movie_read_audio(KvMovie *movie, float *stereo, uint32_t capacity, KvMovieAudioRead *read);
/* NULLs *movie only after native shutdown, callback cleanup and module release.
 * On error a non-NULL owner is intentionally retained, and may be retried. */
int kv_movie_close(KvMovie **movie);
#ifdef __cplusplus
}
#endif
#endif
