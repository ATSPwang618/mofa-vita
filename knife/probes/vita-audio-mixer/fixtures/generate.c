#include <ogg/ogg.h>
#include <opus/opus.h>
#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <stdint.h>

static void packet(FILE *out, ogg_stream_state *state, unsigned char *bytes,
                   long len, int bos, int eos, ogg_int64_t granule, ogg_int64_t number) {
    ogg_packet value={0};
    value.packet=bytes; value.bytes=len; value.b_o_s=bos; value.e_o_s=eos;
    value.granulepos=granule; value.packetno=number;
    assert(ogg_stream_packetin(state,&value)==0);
    ogg_page page;
    while (ogg_stream_flush(state,&page)) {
        assert(fwrite(page.header,1,page.header_len,out)==(size_t)page.header_len);
        assert(fwrite(page.body,1,page.body_len,out)==(size_t)page.body_len);
    }
}
static void generate(const char *path,int channels) {
    FILE *out=fopen(path,"wb"); assert(out);
    ogg_stream_state ogg; assert(ogg_stream_init(&ogg,9009+channels)==0);
    int error=0; OpusEncoder *encoder=opus_encoder_create(48000,channels,OPUS_APPLICATION_AUDIO,&error);
    assert(encoder && error==OPUS_OK);
    unsigned char head[19]={'O','p','u','s','H','e','a','d',1,0,56,1,0x80,0xbb,0,0,0,0,0};
    head[9]=(unsigned char)channels;
    packet(out,&ogg,head,sizeof(head),1,0,0,0);
    unsigned char tags[16]={'O','p','u','s','T','a','g','s',0,0,0,0,0,0,0,0};
    packet(out,&ogg,tags,sizeof(tags),0,0,0,1);
    float pcm[960*2]; unsigned char compressed[4000];
    for (int p=0;p<5;++p) {
        for (int j=0;j<960;++j)
            for (int ch=0;ch<channels;++ch)
                pcm[j*channels+ch]=sinf((float)(p*960+j)*0.0576f)*(ch ? 0.06f : 0.1f);
        int len=opus_encode_float(encoder,pcm,960,compressed,sizeof(compressed)); assert(len>0);
        packet(out,&ogg,compressed,len,0,p==4,(p+1)*960-(p==4?123:0),p+2);
    }
    assert(ogg_stream_clear(&ogg)==0);
    opus_encoder_destroy(encoder);
    assert(fclose(out)==0);
}
int main(int argc,char **argv) {
    if(argc!=3) return 2;
    generate(argv[1],1); generate(argv[2],2);
    return 0;
}
