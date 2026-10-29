#ifndef KNIFE_VITA_AUDIO_DECODE_H
#define KNIFE_VITA_AUDIO_DECODE_H
#include <stdint.h>
#define KV_AUDIO_RATE 48000U
#define KV_AUDIO_CHUNK_FRAMES 960U
#define KV_AUDIO_MAX_BYTES (1024U * 1024U * 1024U)
typedef struct KvAudioDecoder KvAudioDecoder;
typedef enum KvAudioStatus {
 KV_AUDIO_OK=0, KV_AUDIO_ARGUMENT=-1, KV_AUDIO_IO=-2, KV_AUDIO_FORMAT=-3,
 KV_AUDIO_CORRUPT=-4, KV_AUDIO_MEMORY=-5, KV_AUDIO_UNSUPPORTED=-6, KV_AUDIO_RANGE=-7
} KvAudioStatus;
#ifdef __cplusplus
extern "C" {
#endif
/* C owns and closes the file; memory input stays borrowed and immutable until close. */
int32_t kv_audio_open_file(const char *path, KvAudioDecoder **out);
int32_t kv_audio_open_memory(const uint8_t *bytes, uint32_t length, KvAudioDecoder **out);
/* Interleaved stereo f32; mono centre-panned, stereo gain 0.68. */
int32_t kv_audio_read(KvAudioDecoder *decoder, float *stereo, uint32_t capacity_frames, uint32_t *frames_read);
int32_t kv_audio_seek(KvAudioDecoder *decoder, uint64_t frame);
uint64_t kv_audio_tell(const KvAudioDecoder *decoder);
uint64_t kv_audio_total(const KvAudioDecoder *decoder);
uint32_t kv_audio_channels(const KvAudioDecoder *decoder);
void kv_audio_close(KvAudioDecoder *decoder);
#ifdef __cplusplus
}
#endif
#endif
