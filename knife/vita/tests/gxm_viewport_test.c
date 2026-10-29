/* CPU regression for the production effect_scene's SDK viewport arguments.
 * Real VitaSDK declarations; only the called scene/state SDK functions are
 * replaced. No GPU, Vulkan or pixel-rendering claim is made by this test. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#include KV_GXM_BACKEND_SOURCE

static KvGxm *owner;
static KvGxmTexture *texture;
static unsigned checks, failures, cases, state_calls, viewport_calls;
static int begin_status;
static float viewport[6];

static void expect(int condition, const char *expression, int line) {
    ++checks;
    if (condition) return;
    ++failures;
    fprintf(stderr, "FAIL case %u line %d: %s\n", cases, line, expression);
}
#define EXPECT(value) expect(!!(value), #value, __LINE__)

int sceGxmBeginScene(SceGxmContext *context, unsigned int flags,
    const SceGxmRenderTarget *target, const SceGxmValidRegion *region,
    SceGxmSyncObject *vertex_sync, SceGxmSyncObject *fragment_sync,
    const SceGxmColorSurface *color, const SceGxmDepthStencilSurface *depth) {
    EXPECT(context == owner->context && flags == 0);
    EXPECT(target == texture->target && region == NULL && vertex_sync == NULL);
    EXPECT(fragment_sync == texture->sync && color == &texture->surface);
    EXPECT(depth == &owner->depth);
    return begin_status;
}
void sceGxmSetViewport(SceGxmContext *context, float x, float sx, float y,
    float sy, float z, float sz) {
    EXPECT(context == owner->context);
    const float values[6] = {x, sx, y, sy, z, sz};
    memcpy(viewport, values, sizeof(viewport));
    ++viewport_calls;
}
void sceGxmSetCullMode(SceGxmContext *context, SceGxmCullMode mode) {
    EXPECT(context == owner->context && mode == SCE_GXM_CULL_NONE);
    ++state_calls;
}
void sceGxmSetFrontDepthFunc(SceGxmContext *context, SceGxmDepthFunc function) {
    EXPECT(context == owner->context && function == SCE_GXM_DEPTH_FUNC_ALWAYS);
    ++state_calls;
}
void sceGxmSetBackDepthFunc(SceGxmContext *context, SceGxmDepthFunc function) {
    EXPECT(context == owner->context && function == SCE_GXM_DEPTH_FUNC_ALWAYS);
    ++state_calls;
}
void sceGxmSetFrontDepthWriteEnable(SceGxmContext *context, SceGxmDepthWriteMode mode) {
    EXPECT(context == owner->context && mode == SCE_GXM_DEPTH_WRITE_DISABLED);
    ++state_calls;
}
void sceGxmSetBackDepthWriteEnable(SceGxmContext *context, SceGxmDepthWriteMode mode) {
    EXPECT(context == owner->context && mode == SCE_GXM_DEPTH_WRITE_DISABLED);
    ++state_calls;
}

static void run(uint32_t width, uint32_t height, int status) {
    KvEffects effects = {.used = 18};
    KvGxm g = {.effects = &effects};
    KvGxmTexture t = {.width = width, .height = height};
    /* Opaque pointers are compared only; the host never dereferences them. */
    g.context = (SceGxmContext *)(void *)&g;
    t.target = (SceGxmRenderTarget *)(void *)&t;
    t.sync = (SceGxmSyncObject *)(void *)&effects;
    owner = &g; texture = &t; begin_status = status;
    state_calls = 0; viewport_calls = 0;
    memset(viewport, 0, sizeof(viewport));
    const unsigned before = failures;
    ++cases;
    EXPECT(effect_scene(&g, &t) == status);
    if (status < 0) {
        EXPECT(!g.scene_open && g.current_target == NULL);
        EXPECT(effects.used == 18 && state_calls == 0 && viewport_calls == 0);
    } else {
        EXPECT(g.scene_open && g.current_target == &t && effects.used == 0);
        EXPECT(viewport_calls == 1 && state_calls == 5);
        for (unsigned i = 0; i < 6; ++i) EXPECT(isfinite(viewport[i]));
        EXPECT(viewport[1] > 0 && viewport[3] < 0);
        /* Check the geometric contract, independently of how the C expression
         * computes it: NDC corners must span exactly the target rectangle. */
        EXPECT(viewport[0] - viewport[1] == 0.0f);
        EXPECT(viewport[0] + viewport[1] == (float)width);
        EXPECT(viewport[2] + viewport[3] == 0.0f);
        EXPECT(viewport[2] - viewport[3] == (float)height);
        EXPECT(viewport[4] - viewport[5] == 0.0f);
        EXPECT(viewport[4] + viewport[5] == 1.0f);
    }
    printf("%s %ux%u begin=%d viewport=(%g,%g,%g,%g,%g,%g)\n",
        failures == before ? "PASS" : "FAIL", width, height, status,
        viewport[0], viewport[1], viewport[2], viewport[3], viewport[4], viewport[5]);
}
int main(void) {
    const uint32_t sizes[][2] = {
        {1,1}, {2,2}, {64,64}, {960,540}, {960,544}, {2048,2048}, {3,17}, {1025,769}
    };
    for (unsigned i = 0; i < sizeof(sizes) / sizeof(*sizes); ++i)
        run(sizes[i][0], sizes[i][1], 0);
    run(960,540,-73);
    printf("RESULT %s: %u cases, %u checks, %u failures\n",
        failures ? "FAIL" : "PASS", cases, checks, failures);
    return failures ? 1 : 0;
}
