/* SceAvPlayer texture allocations are owned and GPU-mapped by movie_player.c.
 * The SDK frame descriptor has width/height but no stride or byte count; never
 * treat the pointer as a CPU NV12 plane array. YVU420P2_CSC1 matches the
 * upstream VitaGL AvPlayer binding. Exact device output needs runtime checks. */
static int movie_descriptor(SceGxmTexture *descriptor, const void *frame,
        uint32_t width, uint32_t height) {
    if (!descriptor || !frame || ((uintptr_t)frame & 63) || !width || !height
        || width > 1920 || height > 1088 || (width & 1) || (height & 1))
        return KV_BAD_ARGUMENT;
    int rc = sceGxmTextureInitLinear(descriptor, frame,
        SCE_GXM_TEXTURE_FORMAT_YVU420P2_CSC1, width, height, 0);
    if (rc < 0) return rc;
    return sceGxmTextureValidate(descriptor);
}
int32_t kv_gxm_movie_texture_create(KvGxm *g, const void *frame,
        uint32_t width, uint32_t height, KvGxmTexture **out) {
    if (!active(g) || g->scene_open || !out) return KV_STATE;
    *out = NULL;
    KvGxmTexture *t = calloc(1, sizeof(*t));
    if (!t) return fail(g, "movie descriptor allocation", KV_ALLOC);
    const int rc = movie_descriptor(&t->descriptor, frame, width, height);
    if (rc < 0) { free(t); return fail(g, "movie YVU420P2 descriptor", rc); }
    t->owner = g; t->width = width; t->height = height; t->pitch = width;
    t->bpp = 0; t->storage = KV_TEXTURE_MOVIE;
    /* borrowed native planar buffer; never CPU-update as pixels */
    t->next = g->textures; g->textures = t; t->ready = 1;
    *out = t;
    return 0;
}
int32_t kv_gxm_movie_texture_rebind(KvGxm *g, KvGxmTexture *t,
        const void *frame, uint32_t width, uint32_t height) {
    if (!active(g) || g->scene_open || !t || !texture_owned(g,t) || t->bpp
        || t->storage != KV_TEXTURE_MOVIE
        || t->width != width || t->height != height) return KV_BAD_ARGUMENT;
    /* A previous movie frame might still be sampled by commands submitted
       since present; finish before replacing the borrowed pointer. */
    sceGxmFinish(g->context);
    SceGxmTexture next = t->descriptor;
    int rc = movie_descriptor(&next, frame, width, height);
    if (rc < 0) return fail(g, "movie YVU420P2 rebind", rc);
    t->descriptor = next;
    return 0;
}
int32_t kv_gxm_movie_gpu_finish(KvGxm *g) {
    if (!active(g) || g->scene_open) return KV_STATE;
    sceGxmFinish(g->context);
    return 0;
}
