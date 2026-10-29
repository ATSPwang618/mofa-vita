#define _POSIX_C_SOURCE 200809L
#include "../audio_output.h"
#include <assert.h>
#include <stdatomic.h>
#include <stdint.h>
#include <time.h>
static atomic_int next_port=ATOMIC_VAR_INIT(40);
static atomic_int fail_open=ATOMIC_VAR_INIT(0);
static atomic_int fail_output=ATOMIC_VAR_INIT(0);
static atomic_int fail_release=ATOMIC_VAR_INIT(0);
static atomic_int opened=ATOMIC_VAR_INIT(0);
static atomic_int closed=ATOMIC_VAR_INIT(0);
static atomic_int drained=ATOMIC_VAR_INIT(0);
/* Model an AudioOut driver retaining the latest DMA buffer until the next
 * blocking output returns, or until drain/release completes. ASan makes an
 * early drop/recycle a reproducible host failure instead of a random pop. */
static const int16_t *pending_samples;
static int16_t pending_first, pending_last;
static void consume_pending(void) {
 if (pending_samples) {
  assert(pending_samples[0]==pending_first);
  assert(pending_samples[1919]==pending_last);
  pending_samples=0;
 }
}
void kv_test_fail_open(int count) { atomic_store(&fail_open,count); }
void kv_test_fail_output(int count) { atomic_store(&fail_output,count); }
void kv_test_fail_release(int count) { atomic_store(&fail_release,count); }
int kv_test_opened(void) { return atomic_load(&opened); }
int kv_test_closed(void) { return atomic_load(&closed); }
int kv_test_drained(void) { return atomic_load(&drained); }
int sceAudioOutOpenPort(int type,int frames,int rate,int mode) {
 assert(type==0 && frames==960 && rate==48000 && mode==1);
 if (atomic_exchange(&fail_open,0)) return -44;
 atomic_fetch_add(&opened,1);
 return atomic_fetch_add(&next_port,1);
}
int sceAudioOutOutput(int port,const void *samples) {
 assert(port>=40);
 consume_pending();
 if (!samples) { atomic_fetch_add(&drained,1); return 0; }
 if (atomic_exchange(&fail_output,0)) return -45;
 struct timespec delay={0,1000000}; nanosleep(&delay,0);
 pending_samples=samples;
 pending_first=pending_samples[0]; pending_last=pending_samples[1919];
 return 0;
}
int sceAudioOutSetVolume(int port,int flags,int *volume) {
 assert(port>=40 && flags==3 && volume[0]>=0 && volume[0]<=32768 && volume[1]>=0 && volume[1]<=32768);
 return 0;
}
int sceAudioOutReleasePort(int port) {
 assert(port>=40);
 if (atomic_exchange(&fail_release,0)) return -46;
 consume_pending();
 atomic_fetch_add(&closed,1);
 return 0;
}
