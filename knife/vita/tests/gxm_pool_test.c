/* Real allocator implementation with simulated SDK memory, no GPU execution. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#include KV_GXM_BACKEND_SOURCE

static struct { void *base; uint32_t bytes; } blocks[128];
static unsigned checks, failures, cases, allocations, frees;
static int map_failure, free_failure;
static void expect(int ok, const char *expression, int line) {
    ++checks;
    if (!ok) { ++failures; fprintf(stderr,"FAIL case %u line %d: %s\n",cases,line,expression); }
}
#define EXPECT(e) expect(!!(e),#e,__LINE__)
SceUID sceKernelGetThreadId(void) { return 7; }
int sceKernelGetFreeMemorySize(SceKernelFreeMemorySizeInfo *info) {
    EXPECT(info && info->size==(int)sizeof(*info)); info->size_cdram=112*1024*1024; return 0;
}
SceUID sceKernelAllocMemBlock(const char *name,SceKernelMemBlockType type,SceSize bytes,SceKernelAllocMemBlockOpt *opt) {
    EXPECT(name && type==SCE_KERNEL_MEMBLOCK_TYPE_USER_CDRAM_RW && opt==NULL);
    for(unsigned i=1;i<128;++i) if(!blocks[i].base) {
        blocks[i].base=malloc(bytes); blocks[i].bytes=bytes;
        EXPECT(blocks[i].base!=NULL); ++allocations; return (SceUID)i;
    }
    return -73;
}
int sceKernelGetMemBlockBase(SceUID uid,void **out) {
    EXPECT(uid>0 && uid<128 && blocks[uid].base); *out=blocks[uid].base; return 0;
}
int sceKernelFreeMemBlock(SceUID uid) {
    EXPECT(uid>0 && uid<128 && blocks[uid].base);
    if(free_failure) return -81;
    free(blocks[uid].base); memset(&blocks[uid],0,sizeof(blocks[uid])); ++frees; return 0;
}
int sceGxmMapMemory(void *p,unsigned n,SceGxmMemoryAttribFlags flags) {
    EXPECT(p && n && flags==(SCE_GXM_MEMORY_ATTRIB_READ|SCE_GXM_MEMORY_ATTRIB_WRITE)); return map_failure ? -82 : 0;
}
int sceGxmUnmapMemory(void *p) { EXPECT(p!=NULL); return 0; }
int sceGxmMapVertexUsseMemory(void *p,unsigned n,unsigned *o) { (void)p;(void)n;(void)o;return -99; }
int sceGxmMapFragmentUsseMemory(void *p,unsigned n,unsigned *o) { (void)p;(void)n;(void)o;return -99; }
int sceGxmUnmapVertexUsseMemory(void *p) { (void)p;return -99; }
int sceGxmUnmapFragmentUsseMemory(void *p) { (void)p;return -99; }
int sceGxmDestroyRenderTarget(SceGxmRenderTarget *p) { (void)p; EXPECT(0);return -99; }
int sceGxmSyncObjectDestroy(SceGxmSyncObject *p) { (void)p; EXPECT(0);return -99; }

static void fragmentation(void) {
    ++cases;
    KvGxm g={0};g.thread=7;g.initialized=1;g.stats.parameter_reserved_bytes=16*1024*1024;
    const uint32_t initial=CDRAM_LIMIT-g.stats.parameter_reserved_bytes-5*1024*1024;
    g.stats.cdram_mapped_bytes=initial;
    Block portrait={0},glyph={0},next={0};
    EXPECT(texture_pixels_alloc(&g,&portrait,4128768)==0);
    memset(portrait.base,0x6a,4128768);
    EXPECT(texture_pixels_alloc(&g,&glyph,65536)==0);
    memset(glyph.base,0x39,65536);
    EXPECT(block_free(&g,&portrait)==0);
    uint32_t available=0;
    EXPECT(kv_gxm_cdram_available(&g,&available)==0 && available>=4171776);
    EXPECT(texture_pixels_alloc(&g,&next,4171776)==0);
    if(next.base) memset(next.base,0x7b,4171776);
    int intact=1;for(unsigned i=0;i<65536;++i) if(((uint8_t*)glyph.base)[i]!=0x39) intact=0;
    EXPECT(intact && "portrait replacement must preserve live glyphs");
    EXPECT(block_free(&g,&next)==0 && block_free(&g,&glyph)==0);
    EXPECT(pool_trim(&g,0)==0 && !g.texture_pages);
    EXPECT(g.stats.cdram_mapped_bytes==initial && !g.closing);
}
static void small_textures(void) {
    ++cases;
    KvGxm g={0};g.thread=7;g.initialized=1;Block a={0},b={0};
    const unsigned before=allocations;
    EXPECT(texture_pixels_alloc(&g,&a,4096)==0 && texture_pixels_alloc(&g,&b,8192)==0);
    EXPECT(a.page==b.page && allocations==before+1 && a.size==4096 && b.size==8192);
    EXPECT(block_free(&g,&a)==0 && block_free(&g,&b)==0 && pool_trim(&g,0)==0);
    EXPECT(!g.texture_pages && g.stats.cdram_mapped_bytes==0);
}
static void large_charge(void) {
    ++cases;uint32_t out=0;
    EXPECT(kv_gxm_texture_charge(4128768,&out)==0 && out==4*1024*1024);
    EXPECT(kv_gxm_texture_charge(4171776,&out)==0 && out==4*1024*1024);
    EXPECT(kv_gxm_texture_charge(262145,&out)==0 && out==512*1024);
    EXPECT(kv_gxm_texture_charge(65,&out)==0 && out==128);
    EXPECT(kv_gxm_texture_charge(UINT32_MAX,&out)==KV_BAD_ARGUMENT);
}
static void empty_pages_are_reclaimable_before_admission(void) {
    ++cases;
    KvGxm g={0};g.thread=7;g.initialized=1;g.stats.parameter_reserved_bytes=16*1024*1024;
    const uint32_t initial=CDRAM_LIMIT-g.stats.parameter_reserved_bytes-3*1024*1024;
    g.stats.cdram_mapped_bytes=initial;Block old={0},next={0};
    EXPECT(texture_pixels_alloc(&g,&old,2*1024*1024)==0 && block_free(&g,&old)==0);
    uint32_t available=0;
    EXPECT(kv_gxm_cdram_available(&g,&available)==0 && available>=5*512*1024);
    EXPECT(texture_pixels_alloc(&g,&next,5*512*1024)==0);
    EXPECT(block_free(&g,&next)==0 && pool_trim(&g,0)==0);
    EXPECT(g.stats.cdram_mapped_bytes==initial && !g.texture_pages);
}
static void small_texture_cannot_pin_a_retired_large_page(void) {
    ++cases;
    KvGxm g={0};g.thread=7;g.initialized=1;g.stats.parameter_reserved_bytes=16*1024*1024;
    const uint32_t initial=CDRAM_LIMIT-g.stats.parameter_reserved_bytes-3*1024*1024;
    g.stats.cdram_mapped_bytes=initial;Block old={0},glyph={0},next={0};
    EXPECT(texture_pixels_alloc(&g,&old,2*1024*1024)==0 && block_free(&g,&old)==0);
    EXPECT(texture_pixels_alloc(&g,&glyph,65536)==0);
    EXPECT(glyph.page && glyph.page->storage.size==256*1024);
    memset(glyph.base,0x39,65536);
    uint32_t available=0;
    EXPECT(kv_gxm_cdram_available(&g,&available)==0 && available>=5*512*1024);
    EXPECT(texture_pixels_alloc(&g,&next,5*512*1024)==0);
    int intact=1;for(unsigned i=0;i<65536;++i) if(((uint8_t*)glyph.base)[i]!=0x39) intact=0;
    EXPECT(intact);
    EXPECT(block_free(&g,&next)==0 && block_free(&g,&glyph)==0 && pool_trim(&g,0)==0);
    EXPECT(g.stats.cdram_mapped_bytes==initial && !g.texture_pages);
}
int main(void) {
    kv_render_set_options(KV_OPT_TEXTURE_POOL);
    fragmentation();small_textures();large_charge();empty_pages_are_reclaimable_before_admission();
    small_texture_cannot_pin_a_retired_large_page();
    EXPECT(allocations==frees);
    printf("RESULT %s: %u cases, %u checks, %u failures\n",failures?"FAIL":"PASS",cases,checks,failures);
    return failures ? 1 : 0;
}
