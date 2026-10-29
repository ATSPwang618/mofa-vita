/* Host-only contract tests for the actual, private effect_inputs implementation.
 * The complete production backend is included with real VitaSDK declarations;
 * uncalled GPU functions are removed by section garbage collection. No SDK
 * stubs or alternate implementation of effect_inputs are used. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#include KV_GXM_BACKEND_SOURCE
#include <float.h>

enum { TEST_VERTEX_FLOATS = 28, TEST_VERTEX_LIMIT = 32760,
    TEST_SHADER_COUNT = 16, TEST_BLEND_COUNT = 9 };
typedef struct Input { KvEffectVertex v[3]; float p[8]; } Input;
static unsigned cases, checks, failures, accepted_cases, rejected_cases;

static Input fresh(void) {
    Input in = {0};
    /* A valid baseline for every shader: Rule has positive dimensions and
     * packed flags, PS selects operation 1, and Blur has valid integer modes. */
    const float p[8] = {1, 8, 1, 1, 0, 8, 0, 0};
    memcpy(in.p, p, sizeof(p));
    for (unsigned i = 0; i < 3; ++i) {
        in.v[i].color[0] = .8f; in.v[i].color[1] = .7f;
        in.v[i].color[2] = .6f; in.v[i].color[3] = .75f;
    }
    return in;
}
static void expect(int condition, const char *name, const char *detail) {
    ++checks;
    if (condition) return;
    ++failures;
    fprintf(stderr, "FAIL %s: %s\n", name, detail);
}
static void check(const char *name, KvEffectVertex *v, size_t vertex_bytes,
    float p[8], uint32_t count, uint32_t shader, uint32_t blend, int expected,
    unsigned nulls) {
    const unsigned before = failures;
    unsigned char *copy = malloc(vertex_bytes);
    if (!copy) abort();
    memcpy(copy, v, vertex_bytes);
    float params_copy[8];
    memcpy(params_copy, p, sizeof(params_copy));
    const int actual = effect_inputs(nulls & 1 ? NULL : v, count, shader, blend,
        nulls & 2 ? NULL : p);
    char detail[96];
    snprintf(detail, sizeof(detail), "expected status %d, got %d", expected, actual);
    expect(actual == expected, name, detail);
    expect(memcmp(v, copy, vertex_bytes) == 0, name, "vertex bytes were modified");
    expect(memcmp(p, params_copy, sizeof(params_copy)) == 0, name, "parameter bytes were modified");
    free(copy);
    ++cases;
    if (expected == 0) ++accepted_cases; else ++rejected_cases;
    printf("%s %s\n", failures == before ? "PASS" : "FAIL", name);
}
static void run(const char *name, Input *in, uint32_t shader, int expected) {
    check(name, in->v, sizeof(in->v), in->p, 3, shader, 0, expected, 0);
}
/* Construct float-slot fixtures through memcpy, without pointer arithmetic
 * across distinct members of KvEffectVertex in the test itself. */
static void set_float(KvEffectVertex *v, unsigned index, float value) {
    float values[TEST_VERTEX_FLOATS];
    memcpy(values, v, sizeof(values));
    values[index] = value;
    memcpy(v, values, sizeof(values));
}
static void uniform_haze(Input *in, unsigned row, unsigned column, float value) {
    for (unsigned i = 0; i < 3; ++i) in->v[i].haze[row][column] = value;
}
static void preconditions(void) {
    Input in = fresh();
    check("precondition.null-vertices", in.v, sizeof(in.v), in.p, 3, 0, 0, KV_BAD_ARGUMENT, 1);
    check("precondition.null-params", in.v, sizeof(in.v), in.p, 3, 0, 0, KV_BAD_ARGUMENT, 2);
    const uint32_t bad_counts[] = {0, 1, 2, 4, TEST_VERTEX_LIMIT + 1,
        TEST_VERTEX_LIMIT + 3, UINT32_MAX};
    char name[128];
    for (unsigned i = 0; i < sizeof(bad_counts) / sizeof(*bad_counts); ++i) {
        snprintf(name, sizeof(name), "precondition.vertex-count.%u", bad_counts[i]);
        check(name, in.v, sizeof(in.v), in.p, bad_counts[i], 0, 0, KV_BAD_ARGUMENT, 0);
    }
    const uint32_t bad_shader[] = {TEST_SHADER_COUNT, UINT32_MAX};
    const uint32_t bad_blend[] = {TEST_BLEND_COUNT, UINT32_MAX};
    for (unsigned i = 0; i < 2; ++i) {
        snprintf(name, sizeof(name), "precondition.shader.%u", bad_shader[i]);
        check(name, in.v, sizeof(in.v), in.p, 3, bad_shader[i], 0, KV_BAD_ARGUMENT, 0);
        snprintf(name, sizeof(name), "precondition.blend.%u", bad_blend[i]);
        check(name, in.v, sizeof(in.v), in.p, 3, 0, bad_blend[i], KV_BAD_ARGUMENT, 0);
    }
    /* Exercise every legal ID, including the last shader and blend, so the
     * negative enum tests cannot pass through blanket rejection. */
    for (uint32_t shader = 0; shader < TEST_SHADER_COUNT; ++shader)
        for (uint32_t blend = 0; blend < TEST_BLEND_COUNT; ++blend) {
            snprintf(name, sizeof(name), "valid.shader.%u.blend.%u", shader, blend);
            check(name, in.v, sizeof(in.v), in.p, 3, shader, blend, 0, 0);
        }
    KvEffectVertex *large = calloc(TEST_VERTEX_LIMIT, sizeof(*large));
    if (!large) abort();
    const size_t bytes = TEST_VERTEX_LIMIT * sizeof(*large);
    check("valid.vertex-count.maximum", large, bytes, in.p, TEST_VERTEX_LIMIT, 0, 0, 0, 0);
    large[TEST_VERTEX_LIMIT - 1].position[0] = INFINITY;
    check("invalid.maximum-count-last-vertex.infinity", large, bytes, in.p,
        TEST_VERTEX_LIMIT, 0, 0, KV_BAD_ARGUMENT, 0);
    free(large);
}
static void finite_values(void) {
    const float bad[] = {NAN, INFINITY, -INFINITY};
    const char *const labels[] = {"nan", "infinity", "negative-infinity"};
    char name[128];
    for (unsigned b = 0; b < 3; ++b) {
        for (unsigned p = 0; p < 8; ++p) {
            Input in = fresh();
            in.p[p] = bad[b];
            snprintf(name, sizeof(name), "finite.params.%u.%s", p, labels[b]);
            run(name, &in, 0, KV_BAD_ARGUMENT);
        }
        for (unsigned vertex = 0; vertex < 3; ++vertex)
            for (unsigned field = 0; field < TEST_VERTEX_FLOATS; ++field) {
                Input in = fresh();
                set_float(&in.v[vertex], field, bad[b]);
                snprintf(name, sizeof(name), "finite.vertex.%u.float.%u.%s", vertex, field, labels[b]);
                run(name, &in, 0, KV_BAD_ARGUMENT);
            }
    }
    /* Finite positions/colors are not clamped by this API. Preserve that
     * contract, while the specialized packed-parameter cases test their bounds. */
    Input in = fresh();
    in.v[0].position[0] = -FLT_MAX;
    in.v[2].color[0] = FLT_MAX;
    in.p[6] = -FLT_MAX; in.p[7] = FLT_MAX;
    run("valid.finite-extremes-without-packed-semantics", &in, 0, 0);
}
static void haze_and_rim(void) {
    char name[128];
    for (unsigned vertex = 1; vertex < 3; ++vertex)
        for (unsigned row = 0; row < 2; ++row)
            for (unsigned column = 0; column < 4; ++column) {
                Input in = fresh();
                in.v[vertex].haze[row][column] = .25f;
                snprintf(name, sizeof(name), "haze.mismatch.vertex.%u.row.%u.column.%u", vertex, row, column);
                run(name, &in, 0, KV_BAD_ARGUMENT);
            }
    Input in = fresh();
    uniform_haze(&in, 0, 0, .25f);
    uniform_haze(&in, 1, 1, -100.f);
    run("valid.haze.uniform", &in, 0, 0);
    const float rim_levels[] = {0, 1, -.0001f, 1.0001f};
    for (unsigned i = 0; i < 4; ++i) {
        in = fresh(); uniform_haze(&in, 1, 2, rim_levels[i]);
        snprintf(name, sizeof(name), "rim.level.%u", i);
        run(name, &in, 5, i < 2 ? 0 : KV_BAD_ARGUMENT);
    }
    const float rim_flags[] = {0, 16777215.f, -1, .5f, 16777216.f};
    for (unsigned i = 0; i < 5; ++i) {
        in = fresh(); uniform_haze(&in, 1, 3, rim_flags[i]);
        snprintf(name, sizeof(name), "rim.flags.%u", i);
        run(name, &in, 5, i < 2 ? 0 : KV_BAD_ARGUMENT);
    }
    in = fresh(); uniform_haze(&in, 1, 2, 2); uniform_haze(&in, 1, 3, -.5f);
    run("valid.sprite-does-not-interpret-rim-fields", &in, 0, 0);
}
static void soft_lut(void) {
    char name[128];
    const float radius[] = {-1, -.0001f, -0.0f, 0, FLT_MIN, 10000};
    for (unsigned i = 0; i < sizeof(radius) / sizeof(*radius); ++i) {
        Input in = fresh();
        in.v[2].soft[2][0] = radius[i];
        in.v[2].soft[2][1] = 17; in.v[2].soft[2][2] = 256;
        snprintf(name, sizeof(name), "soft.radius.%u", i);
        run(name, &in, 0, i < 2 ? KV_BAD_ARGUMENT : 0);
    }
    const float rows[] = {0, 17, -1, .5f, 18};
    const float resolutions[] = {1, 256, 0, -1, 1.5f, 257};
    for (unsigned vertex = 0; vertex < 3; ++vertex) {
        for (unsigned i = 0; i < sizeof(rows) / sizeof(*rows); ++i) {
            Input in = fresh();
            in.v[vertex].soft[2][0] = 1;
            in.v[vertex].soft[2][1] = rows[i]; in.v[vertex].soft[2][2] = 1;
            snprintf(name, sizeof(name), "soft.row.vertex.%u.value.%u", vertex, i);
            run(name, &in, 0, i < 2 ? 0 : KV_BAD_ARGUMENT);
        }
        for (unsigned i = 0; i < sizeof(resolutions) / sizeof(*resolutions); ++i) {
            Input in = fresh();
            in.v[vertex].soft[2][0] = 1;
            in.v[vertex].soft[2][1] = 0; in.v[vertex].soft[2][2] = resolutions[i];
            snprintf(name, sizeof(name), "soft.resolution.vertex.%u.value.%u", vertex, i);
            run(name, &in, 0, i < 2 ? 0 : KV_BAD_ARGUMENT);
        }
    }
    Input in = fresh();
    in.v[2].soft[2][1] = -999; in.v[2].soft[2][2] = -999;
    run("valid.disabled-soft-does-not-use-lut-parameters", &in, 0, 0);
}
static void rule_and_transition(void) {
    char name[128];
    const float flags[] = {8, 2047, 7, 2048, 8.5f, -1};
    for (unsigned shader = 8; shader <= 9; ++shader)
        for (unsigned i = 0; i < sizeof(flags) / sizeof(*flags); ++i) {
            Input in = fresh(); in.p[1] = flags[i];
            snprintf(name, sizeof(name), "rule.flags.shader.%u.value.%u", shader, i);
            run(name, &in, shader, i < 2 ? 0 : KV_BAD_ARGUMENT);
        }
    Input in = fresh(); in.p[1] = -1; in.p[2] = .5f;
    run("valid.transition.mode-half-does-not-use-rule-flags", &in, 8, 0);
    in.p[2] = nextafterf(.5f, 1.f);
    run("invalid.transition.mode-above-half-requires-rule-flags", &in, 8, KV_BAD_ARGUMENT);
    const float dimensions[] = {0, -1, FLT_MIN, 960};
    for (unsigned field = 2; field <= 3; ++field)
        for (unsigned i = 0; i < sizeof(dimensions) / sizeof(*dimensions); ++i) {
            in = fresh(); in.p[field] = dimensions[i];
            snprintf(name, sizeof(name), "rule.dimension.%u.value.%u", field, i);
            run(name, &in, 9, i < 2 ? KV_BAD_ARGUMENT : 0);
        }
}
static void ps_and_blur(void) {
    char name[128];
    Input in = fresh();
    for (unsigned op = 1; op <= 24; ++op) {
        in.p[0] = (float)op;
        snprintf(name, sizeof(name), "valid.ps.operation.%u", op);
        run(name, &in, 14, 0);
    }
    const float operations[] = {0, 25, 1.5f, -1};
    for (unsigned i = 0; i < sizeof(operations) / sizeof(*operations); ++i) {
        in = fresh(); in.p[0] = operations[i];
        snprintf(name, sizeof(name), "ps.invalid-operation.%u", i);
        run(name, &in, 14, KV_BAD_ARGUMENT);
    }
    const float flags[] = {8, 2047, 7, 2048, 8.5f};
    for (unsigned i = 0; i < sizeof(flags) / sizeof(*flags); ++i) {
        in = fresh(); in.p[4] = 0; in.p[5] = flags[i];
        snprintf(name, sizeof(name), "ps.enabled-rule.flags.%u", i);
        run(name, &in, 14, i < 2 ? 0 : KV_BAD_ARGUMENT);
    }
    in = fresh(); in.p[4] = -.0001f; in.p[5] = -.5f;
    run("valid.ps.disabled-rule-ignores-packed-flags", &in, 14, 0);
    in.p[4] = -0.0f;
    run("invalid.ps.negative-zero-enables-rule", &in, 14, KV_BAD_ARGUMENT);
    for (unsigned op = 1; op <= 24; ++op) {
        in = fresh(); in.p[0] = (float)op; in.p[4] = -1; in.p[7] = 1;
        snprintf(name, sizeof(name), "valid.ps.group-mask.operation.%u", op);
        run(name, &in, 14, 0);
    }
    const float group_modes[] = {-1, .5f, 2};
    for (unsigned i = 0; i < sizeof(group_modes) / sizeof(*group_modes); ++i) {
        in = fresh(); in.p[4] = -1; in.p[7] = group_modes[i];
        snprintf(name, sizeof(name), "ps.invalid-group-mode.%u", i);
        run(name, &in, 14, KV_BAD_ARGUMENT);
    }
    in = fresh(); in.p[7] = 1;
    run("invalid.ps.group-mask-with-rule", &in, 14, KV_BAD_ARGUMENT);
    in.p[4] = -0.0f;
    run("invalid.ps.group-mask-with-negative-zero-rule", &in, 14, KV_BAD_ARGUMENT);
    in.p[4] = -1; in.p[6] = 1;
    run("invalid.ps.group-mask-with-no-alpha", &in, 14, KV_BAD_ARGUMENT);
    in.p[6] = -.0001f;
    run("invalid.ps.group-mask-with-nonzero-no-alpha", &in, 14, KV_BAD_ARGUMENT);
    const float dimensions[] = {0, -1, FLT_MIN, 540};
    for (unsigned field = 2; field <= 3; ++field)
        for (unsigned i = 0; i < sizeof(dimensions) / sizeof(*dimensions); ++i) {
            in = fresh(); in.p[field] = dimensions[i];
            snprintf(name, sizeof(name), "ps.dimension.%u.value.%u", field, i);
            run(name, &in, 14, i < 2 ? KV_BAD_ARGUMENT : 0);
        }
    const float phases[] = {0, 2147483520.0f, -1, .5f, 2147483648.0f};
    for (unsigned i = 0; i < sizeof(phases) / sizeof(*phases); ++i) {
        in = fresh(); in.p[3] = phases[i];
        snprintf(name, sizeof(name), "blur.phase.%u", i);
        run(name, &in, 10, i < 2 ? 0 : KV_BAD_ARGUMENT);
    }
    const float modes[] = {0, 1, 2, -1, .5f, 3};
    for (unsigned i = 0; i < sizeof(modes) / sizeof(*modes); ++i) {
        in = fresh(); in.p[4] = modes[i];
        snprintf(name, sizeof(name), "blur.mode.%u", i);
        run(name, &in, 10, i < 3 ? 0 : KV_BAD_ARGUMENT);
    }
}
int main(void) {
    /* Keep combined stdout/stderr logs on complete lines, even when a mutant
     * produces enough failures to cross the stdio output buffer boundary. */
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("HOST-ONLY actual effect_inputs contract; no GPU, SDK library, or emulator execution.");
    _Static_assert(sizeof(KvEffectVertex) == TEST_VERTEX_FLOATS * sizeof(float), "test float slots match ABI");
    preconditions();
    finite_values();
    haze_and_rim();
    soft_lut();
    rule_and_transition();
    ps_and_blur();
    printf("RESULT %s: %u cases, %u checks, %u failures; %u valid, %u rejected inputs\n",
        failures ? "FAIL" : "PASS", cases, checks, failures, accepted_cases, rejected_cases);
    return failures ? 1 : 0;
}
