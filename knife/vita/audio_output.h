#ifndef KNIFE_VITA_AUDIO_OUTPUT_H
#define KNIFE_VITA_AUDIO_OUTPUT_H
#include <stdint.h>
#define KV_AUDIO_OUT_FRAMES 960U
#define KV_AUDIO_OUT_RATE 48000U
#define KV_AUDIO_OUT_CHANNELS 2U
#define KV_AUDIO_OUT_VOLUME_MAX 32768U
/* Negative SDK errors are returned unchanged. -1=argument, -2=occupied/stale. */
#ifdef __cplusplus
extern "C" {
#endif
int32_t kv_audio_port_open(int32_t *port);
int32_t kv_audio_port_output(int32_t port, const int16_t *stereo_960);
int32_t kv_audio_port_volume(int32_t port, uint32_t left, uint32_t right);
int32_t kv_audio_port_drain(int32_t port);
int32_t kv_audio_port_close(int32_t port);
#ifdef __cplusplus
}
#endif
#endif
