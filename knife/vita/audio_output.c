#include "audio_output.h"
#include <stdatomic.h>
#ifdef KV_AUDIO_OUT_TEST
/* Host tests provide deterministic SDK mocks with the same signatures. */
int sceAudioOutOpenPort(int type,int frames,int rate,int mode);
int sceAudioOutOutput(int port,const void *buffer);
int sceAudioOutSetVolume(int port,int flags,int *volume);
int sceAudioOutReleasePort(int port);
#define SCE_AUDIO_OUT_PORT_TYPE_MAIN 0
#define SCE_AUDIO_OUT_MODE_STEREO 1
#else
#include <psp2/audioout.h>
#endif
/* -1 free, -2 opening, -3 closing. The worker alone uses a published port. */
static atomic_int owner=-1;
int32_t kv_audio_port_open(int32_t *out) {
 if (!out) return -1;
 *out=-1;
 int expected=-1;
 if (!atomic_compare_exchange_strong(&owner,&expected,-2)) return -2;
 int port=sceAudioOutOpenPort(SCE_AUDIO_OUT_PORT_TYPE_MAIN,KV_AUDIO_OUT_FRAMES,
                              KV_AUDIO_OUT_RATE,SCE_AUDIO_OUT_MODE_STEREO);
 if (port<0) { atomic_store(&owner,-1); return port; }
 atomic_store(&owner,port);
 *out=port;
 return 0;
}
int32_t kv_audio_port_output(int32_t port,const int16_t *pcm) {
 if (!pcm) return -1;
 if (port<0 || atomic_load(&owner)!=port) return -2;
 return sceAudioOutOutput(port,pcm);
}
int32_t kv_audio_port_volume(int32_t port,uint32_t left,uint32_t right) {
 if (left>KV_AUDIO_OUT_VOLUME_MAX || right>KV_AUDIO_OUT_VOLUME_MAX) return -1;
 if (port<0 || atomic_load(&owner)!=port) return -2;
 int volumes[2]={(int)left,(int)right};
 return sceAudioOutSetVolume(port,3,volumes);
}
int32_t kv_audio_port_drain(int32_t port) {
 if (port<0 || atomic_load(&owner)!=port) return -2;
 return sceAudioOutOutput(port,0);
}
int32_t kv_audio_port_close(int32_t port) {
 if (port<0) return -1;
 int expected=port;
 if (!atomic_compare_exchange_strong(&owner,&expected,-3)) return -2;
 int result=sceAudioOutReleasePort(port);
 atomic_store(&owner,result<0 ? port : -1);
 return result;
}
