#include "../audio_output.h"
int main(void) {
 int32_t port=-1;
 int status=kv_audio_port_open(&port);
 if (status<0) return status;
 int16_t silence[KV_AUDIO_OUT_FRAMES*2]={0};
 status=kv_audio_port_volume(port,32768,32768);
 if (status>=0) status=kv_audio_port_output(port,silence);
 if (status>=0) status=kv_audio_port_drain(port);
 int close_status=kv_audio_port_close(port);
 return status<0 ? status : close_status;
}
