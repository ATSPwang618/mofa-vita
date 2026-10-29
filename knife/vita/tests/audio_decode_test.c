#include "../audio_decode.h"
#include <assert.h>
#include <math.h>
#include <ogg/ogg.h>
#include <opus/opus.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int fail_calloc;
static int fail_malloc;
void *__real_calloc(size_t count,size_t size);
void *__real_malloc(size_t size);
void *__wrap_calloc(size_t count,size_t size) {
 if (fail_calloc) { --fail_calloc; return NULL; }
 return __real_calloc(count,size);
}
void *__wrap_malloc(size_t size) {
 if (fail_malloc) { --fail_malloc; return NULL; }
 return __real_malloc(size);
}
static void write_packet(FILE *file, ogg_stream_state *state, unsigned char *data,
                         long size, int bos, int eos, ogg_int64_t gp, ogg_int64_t number) {
 ogg_packet packet={0};
 packet.packet=data; packet.bytes=size; packet.b_o_s=bos; packet.e_o_s=eos;
 packet.granulepos=gp; packet.packetno=number;
 assert(ogg_stream_packetin(state,&packet)==0);
 ogg_page page;
 while (ogg_stream_flush(state,&page)) {
  assert(fwrite(page.header,1,page.header_len,file)==(size_t)page.header_len);
  assert(fwrite(page.body,1,page.body_len,file)==(size_t)page.body_len);
 }
}
static void fixture(const char *path,int channels) {
 FILE *file=fopen(path,"wb"); assert(file);
 ogg_stream_state state; assert(ogg_stream_init(&state,77)==0);
 int error=0;
 OpusEncoder *encoder=opus_encoder_create(48000,channels,OPUS_APPLICATION_AUDIO,&error);
 assert(encoder && error==OPUS_OK);
 unsigned char head[19]={ 'O','p','u','s','H','e','a','d',1,0,56,1,0x80,0xbb,0,0,0,0,0 };
 head[9]=(unsigned char)channels;
 write_packet(file,&state,head,sizeof(head),1,0,0,0);
 unsigned char tags[16]={ 'O','p','u','s','T','a','g','s',0,0,0,0,0,0,0,0 };
 write_packet(file,&state,tags,sizeof(tags),0,0,0,1);
 float samples[960*2]; unsigned char encoded[4000];
 for (int p=0;p<5;++p) {
  for (int j=0;j<960;++j)
   for (int ch=0;ch<channels;++ch)
    samples[j*channels+ch]=sinf((float)(p*960+j)*0.0576f)*(ch ? 0.06f : 0.1f);
  int size=opus_encode_float(encoder,samples,960,encoded,sizeof(encoded));
  assert(size>0);
  write_packet(file,&state,encoded,size,0,p==4,(p+1)*960-(p==4 ? 123 : 0),p+2);
 }
 assert(ogg_stream_clear(&state)==0);
 opus_encoder_destroy(encoder);
 assert(fclose(file)==0);
}
static unsigned char *load(const char *path,size_t *length) {
 FILE *f=fopen(path,"rb"); assert(f);
 assert(fseek(f,0,SEEK_END)==0);
 long n=ftell(f); assert(n>0);
 assert(fseek(f,0,SEEK_SET)==0);
 unsigned char *bytes=malloc((size_t)n+1); assert(bytes);
 assert(fread(bytes,1,(size_t)n,f)==(size_t)n);
 assert(fclose(f)==0);
 *length=(size_t)n;
 return bytes;
}
static void verify(const char *path,int channels) {
 fixture(path,channels);
 size_t length=0; unsigned char *encoded=load(path,&length);
 KvAudioDecoder *file=NULL, *memory=NULL;
 assert(kv_audio_open_file(path,&file)==KV_AUDIO_OK && file);
 assert(kv_audio_open_memory(encoded,(uint32_t)length,&memory)==KV_AUDIO_OK && memory);
 assert(kv_audio_channels(file)==(uint32_t)channels);
 assert(kv_audio_total(file)==4365);
 assert(kv_audio_total(memory)==kv_audio_total(file));
 float from_file[960*2], from_memory[960*2];
 for (uint64_t pos=0;pos<4365;) {
  uint32_t a=0,b=0;
  assert(kv_audio_read(file,from_file,137,&a)==KV_AUDIO_OK);
  assert(kv_audio_read(memory,from_memory,137,&b)==KV_AUDIO_OK);
  assert(a==b && a>0);
  for (uint32_t i=0;i<a*2;++i) assert(fabsf(from_file[i]-from_memory[i])<1e-6f);
  if (channels==1) for (uint32_t i=0;i<a;++i) assert(from_file[2*i]==from_file[2*i+1]);
  pos+=a;
  assert(kv_audio_tell(file)==pos);
 }
 uint32_t read=9;
 assert(kv_audio_read(file,from_file,960,&read)==KV_AUDIO_OK && read==0);
 assert(kv_audio_seek(file,1001)==KV_AUDIO_OK && kv_audio_tell(file)==1001);
 assert(kv_audio_seek(memory,1001)==KV_AUDIO_OK);
 assert(kv_audio_read(file,from_file,960,&read)==KV_AUDIO_OK && read==960);
 uint32_t other=0;
 assert(kv_audio_read(memory,from_memory,960,&other)==KV_AUDIO_OK && other==read);
 for (uint32_t i=0;i<other*2;++i) assert(fabsf(from_file[i]-from_memory[i])<1e-6f);
 assert(kv_audio_seek(file,4366)==KV_AUDIO_RANGE && kv_audio_tell(file)==1961);
 assert(kv_audio_seek(file,4365)==KV_AUDIO_OK);
 assert(kv_audio_read(file,from_file,960,&read)==KV_AUDIO_OK && read==0);
 assert(kv_audio_read(file,from_file,961,&read)==KV_AUDIO_ARGUMENT && read==0);
 kv_audio_close(file); kv_audio_close(memory);
 KvAudioDecoder *bad=NULL;
 assert(kv_audio_open_memory(encoded,(uint32_t)length-1,&bad)==KV_AUDIO_CORRUPT && !bad);
 encoded[length-1]^=1;
 assert(kv_audio_open_memory(encoded,(uint32_t)length,&bad)==KV_AUDIO_CORRUPT && !bad);
 assert(kv_audio_open_memory((const uint8_t *)"OggS",4,&bad)==KV_AUDIO_CORRUPT && !bad);
 assert(kv_audio_open_memory(encoded,KV_AUDIO_MAX_BYTES+1U,&bad)==KV_AUDIO_RANGE && !bad);
 /* The production wrapper must leave its output empty on either owned allocation failure. */
 encoded[length-1]^=1;
 fail_calloc=1;
 assert(kv_audio_open_memory(encoded,(uint32_t)length,&bad)==KV_AUDIO_MEMORY && !bad);
 fail_malloc=1;
 assert(kv_audio_open_memory(encoded,(uint32_t)length,&bad)==KV_AUDIO_MEMORY && !bad);
 /* A damaged earlier page is visible during streaming, after a valid EOS tail. */
 unsigned char *middle=encoded+length/2;
 *middle^=1;
 if (kv_audio_open_memory(encoded,(uint32_t)length,&bad)==KV_AUDIO_OK) {
  int failure=0;
  float block[960*2]; uint32_t count=0;
  for (int attempt=0;attempt<12;++attempt) {
   int result=kv_audio_read(bad,block,960,&count);
   if (result!=KV_AUDIO_OK) { failure=1; break; }
   if (!count) break;
  }
  assert(failure);
  kv_audio_close(bad);
 } else assert(!bad);
 free(encoded);
}
int main(int argc,char **argv) {
 assert(argc==3);
 verify(argv[1],1); verify(argv[2],2);
 puts("audio_decode: mono/stereo, EOF, seek/tell, file/memory, CRC/truncation, range OK");
 return 0;
}
