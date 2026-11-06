/* Actual public effect-input validation, with no live GPU/SDK calls. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#include KV_GXM_BACKEND_SOURCE

static unsigned checks, failures;
static void expect(int value, const char *expression, int line) {
    ++checks;
    if (!value) { ++failures; fprintf(stderr, "FAIL line %d: %s\n", line, expression); }
}
#define EXPECT(value) expect(!!(value), #value, __LINE__)
static int validate(uint32_t shader, uint32_t blend, float params[8], KvEffectVertex vertices[3]) {
    return effect_inputs(vertices, 3, shader, blend, params);
}
static void plain(uint32_t shader, float operation) {
    KvEffectVertex vertices[3] = {0};
    float params[8] = {operation, 0, 960, 540, -1, 0, 0, 0};
    for (unsigned fill = 0; fill < 2; ++fill) for (unsigned noalpha = 0; noalpha < 2; ++noalpha) {
        params[1] = fill; params[6] = noalpha;
        EXPECT(validate(shader, 5, params, vertices) == 0);
    }
    params[1] = 0; params[6] = 0;
    for (unsigned blend = 0; blend < 10; ++blend)
        EXPECT(validate(shader, blend, params, vertices) == (blend == 5 ? 0 : KV_BAD_ARGUMENT));
    for (unsigned op = 1; op <= 24; ++op) {
        params[0] = op;
        EXPECT(validate(shader, 5, params, vertices) == (op == operation ? 0 : KV_BAD_ARGUMENT));
    }
    params[0] = operation;
    const float flags[] = {-1, -.5f, .5f, 1.5f, 2, NAN, INFINITY};
    for (unsigned field = 0; field < 2; ++field) for (unsigned v = 0; v < sizeof(flags)/sizeof(*flags); ++v) {
        params[field ? 6 : 1] = flags[v];
        EXPECT(validate(shader, 5, params, vertices) == KV_BAD_ARGUMENT);
        params[field ? 6 : 1] = 0;
    }
    for (unsigned field = 4; field < 8; ++field) {
        params[field] = field == 4 ? 0 : 1;
        EXPECT(validate(shader, 5, params, vertices) == (field == 6 ? 0 : KV_BAD_ARGUMENT));
        params[field] = field == 4 ? -1 : 0;
    }
    for (unsigned field = 0; field < 20; ++field) {
        for (unsigned vertex = 0; vertex < 3; ++vertex) {
            float values[28]; memcpy(values, vertices+vertex, sizeof(values));
            values[8+field] = .1f; memcpy(vertices+vertex, values, sizeof(values));
        }
        EXPECT(validate(shader, 5, params, vertices) == KV_BAD_ARGUMENT);
        memset(vertices,0,sizeof(*vertices)*3);
    }
}
int main(void) {
    plain(21,13); plain(30,1); plain(31,6); plain(48,7);
    plain(49,1); plain(50,6); plain(51,7); plain(52,13);
    KvEffectVertex vertices[3] = {0}; float params[8] = {1,0,960,540,-1,0,0,0};
    for (unsigned shader = 32; shader < 48; ++shader) {
        EXPECT(validate(shader,5,params,vertices) == KV_BAD_ARGUMENT);
    }
    for (unsigned shader=57;shader<=63;++shader) EXPECT(validate(shader,5,params,vertices)==KV_BAD_ARGUMENT);
    EXPECT(validate(88,5,params,vertices) == KV_BAD_ARGUMENT);
    params[0]=7; params[1]=7;
    EXPECT(validate(27,5,params,vertices)==0);
    for(unsigned op=1;op<=24;++op) {
        params[1]=op; EXPECT(validate(27,5,params,vertices)==(op==7?0:KV_BAD_ARGUMENT));
        params[0]=op;params[1]=7; EXPECT(validate(27,5,params,vertices)==(op==7?0:KV_BAD_ARGUMENT));
        params[0]=7;
    }
    params[0]=1; params[1]=0;
    for (unsigned mode = 0; mode < 3; ++mode) for (unsigned n = 0; n <= 16; ++n) {
        params[3] = n; params[4] = mode;
        EXPECT(validate(53,5,params,vertices) == 0);
    }
    const float nbad[] = {-1, .5f, 16.5f,17,2147483520.0f,NAN,INFINITY};
    for (unsigned n = 0; n < sizeof(nbad)/sizeof(*nbad); ++n) {
        params[3] = nbad[n];
        EXPECT(validate(53,5,params,vertices) == KV_BAD_ARGUMENT);
    }
    params[3] = 16;
    const float mbad[] = {-1,.5f,2.5f,3,NAN,INFINITY};
    for (unsigned m = 0; m < sizeof(mbad)/sizeof(*mbad); ++m) {
        params[4] = mbad[m]; EXPECT(validate(53,5,params,vertices) == KV_BAD_ARGUMENT);
    }
    params[4] = 0; params[3] = 17;
    EXPECT(validate(10,5,params,vertices) == 0); /* wide kernel keeps original path */
    printf("RESULT %s: %u checks, %u failures; production public inputs, no GPU execution\n",
        failures ? "FAIL" : "PASS", checks, failures);
    return failures ? 1 : 0;
}
