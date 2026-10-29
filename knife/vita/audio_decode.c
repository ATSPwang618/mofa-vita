#include "audio_decode.h"
#include <limits.h>
#include <opus/opusfile.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct KvAudioDecoder {
 OggOpusFile *opus;
 FILE *file;
 const uint8_t *memory;
 uint32_t length, cursor, channels, cached, consumed;
 uint64_t frame, total;
 float pcm[KV_AUDIO_CHUNK_FRAMES * 2];
};

static int cb_read(void *source, unsigned char *out, int requested) {
 KvAudioDecoder *d=source;
 if (requested<0) return -1;
 if (d->file) {
  size_t count=fread(out,1,(size_t)requested,d->file);
  return ferror(d->file) ? -1 : (int)count;
 }
 uint32_t remaining=d->length-d->cursor;
 uint32_t count=(uint32_t)requested<remaining ? (uint32_t)requested : remaining;
 if (count) memcpy(out,d->memory+d->cursor,count);
 d->cursor+=count;
 return (int)count;
}
static opus_int64 cb_tell(void *source) {
 KvAudioDecoder *d=source;
 return d->file ? (opus_int64)ftell(d->file) : (opus_int64)d->cursor;
}
static int cb_seek(void *source, opus_int64 offset, int whence) {
 KvAudioDecoder *d=source;
 opus_int64 base;
 if (whence==SEEK_SET) base=0;
 else if (whence==SEEK_CUR) base=cb_tell(source);
 else if (whence==SEEK_END) base=d->length;
 else return -1;
 if (base<0 || offset< -base || offset>(opus_int64)d->length-base) return -1;
 opus_int64 target=base+offset;
 if (d->file) return fseek(d->file,(long)target,SEEK_SET);
 d->cursor=(uint32_t)target;
 return 0;
}
static const OpusFileCallbacks CALLBACKS={cb_read,cb_seek,cb_tell,NULL};

/* One complete, CRC-valid final EOS page must end at the physical EOF. */
static int verify_eos(KvAudioDecoder *d) {
 const uint32_t max_page=27U+255U+255U*255U;
 uint32_t n=d->length<max_page ? d->length : max_page;
 if (n<27) return KV_AUDIO_CORRUPT;
 unsigned char *tail=malloc(n);
 if (!tail) return KV_AUDIO_MEMORY;
 if (cb_seek(d,(opus_int64)d->length-n,SEEK_SET)!=0 || cb_read(d,tail,(int)n)!=(int)n) {
  free(tail); return KV_AUDIO_IO;
 }
 int status=KV_AUDIO_CORRUPT;
 for (uint32_t i=n-27;;--i) {
  if (!memcmp(tail+i,"OggS",4) && tail[i+4]==0 && (tail[i+5]&4)) {
   uint32_t header=27U+tail[i+26];
   if (i+header<=n) {
    uint32_t size=header;
    for (uint32_t j=27;j<header;++j) size+=tail[i+j];
    if (size==n-i) {
     uint32_t crc=0;
     uint32_t expected=(uint32_t)tail[i+22]|((uint32_t)tail[i+23]<<8)|
      ((uint32_t)tail[i+24]<<16)|((uint32_t)tail[i+25]<<24);
     for (uint32_t j=0;j<size;++j) {
      uint8_t byte=j>=22 && j<26 ? 0 : tail[i+j];
      crc^=(uint32_t)byte<<24;
      for (int k=0;k<8;++k) crc=(crc<<1)^((crc&0x80000000U)?0x04c11db7U:0);
     }
     if (crc==expected) status=KV_AUDIO_OK;
    }
   }
   break;
  }
  if (!i) break;
 }
 free(tail);
 return status;
}
static int finish_open(KvAudioDecoder *d, KvAudioDecoder **out) {
 int status=verify_eos(d);
 if (status!=KV_AUDIO_OK) goto fail;
 if (cb_seek(d,0,SEEK_SET)!=0) { status=KV_AUDIO_IO; goto fail; }
 int error=0;
 d->opus=op_open_callbacks(d,&CALLBACKS,NULL,0,&error);
 if (!d->opus) {
  status=error==OP_EFAULT ? KV_AUDIO_MEMORY : error==OP_EREAD ? KV_AUDIO_IO : KV_AUDIO_FORMAT;
  goto fail;
 }
 const OpusHead *head=op_head(d->opus,0);
 if (op_link_count(d->opus)!=1 || !head || head->mapping_family!=0 ||
     (head->channel_count!=1 && head->channel_count!=2)) {
  status=KV_AUDIO_UNSUPPORTED; goto fail;
 }
 ogg_int64_t total=op_pcm_total(d->opus,-1);
 if (total<0) { status=KV_AUDIO_CORRUPT; goto fail; }
 d->channels=(uint32_t)head->channel_count;
 d->total=(uint64_t)total;
 *out=d;
 return KV_AUDIO_OK;
fail:
 kv_audio_close(d);
 return status;
}
int32_t kv_audio_open_file(const char *path, KvAudioDecoder **out) {
 if (!out || !path || !*path) return KV_AUDIO_ARGUMENT;
 *out=NULL;
 KvAudioDecoder *d=calloc(1,sizeof(*d));
 if (!d) return KV_AUDIO_MEMORY;
 d->file=fopen(path,"rb");
 if (!d->file) { kv_audio_close(d); return KV_AUDIO_IO; }
 if (fseek(d->file,0,SEEK_END)!=0) { kv_audio_close(d); return KV_AUDIO_IO; }
 long length=ftell(d->file);
 if (length<0) { kv_audio_close(d); return KV_AUDIO_IO; }
 if ((uint64_t)length>KV_AUDIO_MAX_BYTES) { kv_audio_close(d); return KV_AUDIO_RANGE; }
 d->length=(uint32_t)length;
 return finish_open(d,out);
}
int32_t kv_audio_open_memory(const uint8_t *bytes, uint32_t length, KvAudioDecoder **out) {
 if (!out || !bytes || !length) return KV_AUDIO_ARGUMENT;
 *out=NULL;
 if (length>KV_AUDIO_MAX_BYTES) return KV_AUDIO_RANGE;
 KvAudioDecoder *d=calloc(1,sizeof(*d));
 if (!d) return KV_AUDIO_MEMORY;
 d->memory=bytes;
 d->length=length;
 return finish_open(d,out);
}
int32_t kv_audio_read(KvAudioDecoder *d,float *stereo,uint32_t capacity,uint32_t *read) {
 if (!read) return KV_AUDIO_ARGUMENT;
 *read=0;
 if (!d || !stereo || !capacity || capacity>KV_AUDIO_CHUNK_FRAMES) return KV_AUDIO_ARGUMENT;
 while (*read<capacity) {
  if (d->consumed==d->cached) {
   d->consumed=d->cached=0;
   if (d->frame==d->total) break;
   int n=op_read_float(d->opus,d->pcm,KV_AUDIO_CHUNK_FRAMES*(int)d->channels,NULL);
   if (n<0) return n==OP_EFAULT ? KV_AUDIO_MEMORY : n==OP_EREAD ? KV_AUDIO_IO : KV_AUDIO_CORRUPT;
   if (!n || (uint64_t)n>d->total-d->frame || (uint32_t)n>KV_AUDIO_CHUNK_FRAMES) return KV_AUDIO_CORRUPT;
   d->cached=(uint32_t)n;
  }
  uint32_t take=d->cached-d->consumed;
  if (take>capacity-*read) take=capacity-*read;
  for (uint32_t i=0;i<take;++i) {
   float *dst=stereo+(size_t)(*read+i)*2;
   if (d->channels==1) dst[0]=dst[1]=d->pcm[d->consumed+i]*0.7071067811865475f;
   else {
    dst[0]=d->pcm[(size_t)(d->consumed+i)*2]*0.68f;
    dst[1]=d->pcm[(size_t)(d->consumed+i)*2+1]*0.68f;
   }
  }
  d->consumed+=take;
  d->frame+=take;
  *read+=take;
 }
 return KV_AUDIO_OK;
}
int32_t kv_audio_seek(KvAudioDecoder *d,uint64_t frame) {
 if (!d) return KV_AUDIO_ARGUMENT;
 if (frame>d->total || frame>INT64_MAX) return KV_AUDIO_RANGE;
 if (frame==d->total) { d->consumed=d->cached=0; d->frame=frame; return KV_AUDIO_OK; }
 int result=op_pcm_seek(d->opus,(ogg_int64_t)frame);
 if (result) return result==OP_EFAULT ? KV_AUDIO_MEMORY : result==OP_EREAD ? KV_AUDIO_IO : KV_AUDIO_CORRUPT;
 d->consumed=d->cached=0;
 d->frame=frame;
 return KV_AUDIO_OK;
}
uint64_t kv_audio_tell(const KvAudioDecoder *d) { return d ? d->frame : 0; }
uint64_t kv_audio_total(const KvAudioDecoder *d) { return d ? d->total : 0; }
uint32_t kv_audio_channels(const KvAudioDecoder *d) { return d ? d->channels : 0; }
void kv_audio_close(KvAudioDecoder *d) {
 if (!d) return;
 if (d->opus) op_free(d->opus);
 if (d->file) fclose(d->file);
 free(d);
}
