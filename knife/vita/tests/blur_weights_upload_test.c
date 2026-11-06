/* Host-only test of the production FFI copy/match/upload path. A recording
   SDK boundary replaces only thread identity and the actual GPU uniform call. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#include KV_GXM_BACKEND_SOURCE

static unsigned checks, failures, uploads;
static uint32_t uploaded_count;
static float uploaded[33];
static int upload_status;
SceUID sceKernelGetThreadId(void) { return 7; }
int sceGxmSetUniformDataF(void *buffer, const SceGxmProgramParameter *parameter,
    unsigned int componentOffset, unsigned int componentCount, const float *data) {
    if (buffer != uploaded || parameter != (const SceGxmProgramParameter *)(uintptr_t)1
        || componentOffset || componentCount > 33) return KV_BAD_ARGUMENT;
    ++uploads; uploaded_count = componentCount;
    memcpy(uploaded, data, componentCount * sizeof(*data));
    return upload_status;
}
static void expect(int value, const char *text, int line) {
    ++checks;
    if (!value) { ++failures; fprintf(stderr, "FAIL %d: %s\n", line, text); }
}
#define EXPECT(value) expect(!!(value), #value, __LINE__)
int main(void) {
    KvEffects effects = {0}; KvGxm owner = { .thread = 7, .effects = &effects };
    effects.blur_uniform[5][0] = (const SceGxmProgramParameter *)(uintptr_t)1;
    float weights[33], original[33], params[8] = {1,0,5.0f,16,2,0,0,0};
    for (unsigned i = 0; i < 33; ++i) weights[i] = original[i] = (float)(i+1) / 100.0f;
    EXPECT(kv_gxm_set_blur_weights(NULL,5,16,weights,33) == KV_STATE);
    EXPECT(kv_gxm_set_blur_weights(&owner,5,17,weights,33) == KV_BAD_ARGUMENT);
    EXPECT(kv_gxm_set_blur_weights(&owner,5,16,weights,32) == KV_BAD_ARGUMENT);
    EXPECT(kv_gxm_set_blur_weights(&owner,5,16,weights,33) == 0);
    EXPECT(effect_blur_weights_ready(&effects,params));
    weights[0] = 99;
    EXPECT(effects.blur_weights[0] == original[0]);
    params[2] = nextafterf(5,6); EXPECT(!effect_blur_weights_ready(&effects,params));
    params[2] = 5; params[3] = 15; EXPECT(!effect_blur_weights_ready(&effects,params));
    params[3] = 16;
    EXPECT(effect_blur_weights_upload(&owner,uploaded,5,0) == 0);
    EXPECT(uploads == 1 && uploaded_count == 33);
    EXPECT(!memcmp(uploaded,original,sizeof(original)));
    EXPECT(!effect_blur_weights_ready(&effects,params));
    EXPECT(effect_blur_weights_upload(&owner,uploaded,5,0) == KV_STATE);
    EXPECT(uploads == 1);
    EXPECT(kv_gxm_set_blur_weights(&owner,.01f,0,original,1) == 0);
    params[2] = 0; params[3] = 0;
    EXPECT(effect_blur_weights_ready(&effects,params));
    upload_status = -4;
    EXPECT(effect_blur_weights_upload(&owner,uploaded,5,0) == -4);
    EXPECT(effects.blur_weight_count == 0);
    EXPECT(effect_blur_weights_upload(&owner,uploaded,5,0) == KV_STATE);
    original[1] = NAN; EXPECT(kv_gxm_set_blur_weights(&owner,5,1,original,3) == KV_BAD_ARGUMENT);
    original[1] = -1; EXPECT(kv_gxm_set_blur_weights(&owner,5,1,original,3) == KV_BAD_ARGUMENT);
    EXPECT(effects.blur_weight_count == 0);
    printf("RESULT %s: %u checks, %u failures; production FFI and upload, no GPU execution\n",
        failures ? "FAIL" : "PASS", checks, failures);
    return failures ? 1 : 0;
}
