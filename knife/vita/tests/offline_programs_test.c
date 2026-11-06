/* Real PC-produced authored GXP through production validation. No GPU execution. */
#define KV_SHADER_CACHE_HOST_TEST 1
#define KV_OFFLINE_REAL_TEST 1
#include "../runtime_shaders.c"
#include <sys/stat.h>
int sceGxmProgramCheck(const SceGxmProgram *p) { (void)p; return 0; }
int sceIoMkdir(const char *p,int mode) { return mkdir(p,mode); }
int main(void) {
    unsigned count=0;
    for(unsigned i=0;i<sizeof(runtime_shader_ids)/sizeof(runtime_shader_ids[0]);++i){
        unsigned id=runtime_shader_ids[i],bytes=0;
        const unsigned char *blob=knife_offline_program(id,&bytes);
        if(!blob || !validate(blob,bytes,id)) {
            fprintf(stderr,"OFFLINE REJECT id=%u bytes=%u reason=%s\n",id,bytes,validation_reason);return 1;
        }
        SceGxmProgram *loaded=read_offline(id);
        if(!loaded || memcmp(loaded,blob,bytes)){free(loaded);return 2;}
        free(loaded);++count;
    }
    if(count!=85 || read_offline(63)!=NULL || read_offline(UINT32_MAX)!=NULL)return 3;
    printf("PASS: %u real PC GXP programs, embedded identities and production validator; ProgramCheck mocked, GPU unverified\n",count);
    return 0;
}
