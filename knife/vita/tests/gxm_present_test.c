/* Host-only submission-state regression. This does not execute a GPU or an SDK
 * library. The production C implementation and unmodified VitaSDK declarations
 * are compiled together; --gc-sections discards unused backend entry points.
 * Keep the fault stubs limited to present, owner validation, and its callback. */
#ifndef KV_GXM_BACKEND_SOURCE
#define KV_GXM_BACKEND_SOURCE "../gxm_backend.c"
#endif
#include KV_GXM_BACKEND_SOURCE

static volatile unsigned int host_completion[BATCH_SLOTS];
volatile unsigned int *sceGxmGetNotificationRegion(void) { return host_completion; }
int sceGxmNotificationWait(const SceGxmNotification *n) { *n->address = n->value; return 0; }

enum Fault {
    FAULT_NONE, FAULT_END_SCENE, FAULT_HEARTBEAT, FAULT_QUEUE_ADD,
    FAULT_QUEUE_FINISH, FAULT_SET_FRAME, FAULT_VBLANK
};
static const char *const fault_names[] = {
    "success", "EndScene", "PadHeartbeat", "QueueAddEntry",
    "QueueFinish", "callback.SetFrameBuf", "callback.WaitVblankStart"
};
static const char *const expected_traces[] = {
    "EHQFDSW", "E", "EH", "EHQ", "EHQFD", "EHQFDS", "EHQFDSW"
};
static enum Fault injected_fault;
static KvGxm *expected_owner;
static DisplayData queued_display;
static int queue_pending;
static SceUID calling_thread = 123;
static unsigned failures, cases, checks;
static char case_name[96], trace[64];
static size_t trace_length;
static _Alignas(16) unsigned char context_token[16], sync_tokens[2][16];
static uint32_t display_tokens[2];

static void expect(int condition, const char *expression, int line) {
    ++checks;
    if (condition) return;
    ++failures;
    fprintf(stderr, "FAIL %s line %d: %s\n", case_name, line, expression);
}
#define EXPECT(expression) expect(!!(expression), #expression, __LINE__)

static void event(char marker) {
    EXPECT(trace_length + 1 < sizeof(trace));
    if (trace_length + 1 < sizeof(trace)) {
        trace[trace_length++] = marker;
        trace[trace_length] = 0;
    }
}
static int fault_status(enum Fault point) {
    return injected_fault == point ? -100 - (int)point : 0;
}
static void reset(enum Fault point, KvGxm *g, uint32_t frames) {
    memset(g, 0, sizeof(*g));
    injected_fault = point;
    expected_owner = g;
    memset(&queued_display, 0, sizeof(queued_display));
    queue_pending = 0;
    trace_length = 0;
    trace[0] = 0;
    g->thread = calling_thread;
    g->initialized = 1;
    g->scene_open = 1;
    g->context = (SceGxmContext *)(void *)context_token;
    g->sync[0] = (SceGxmSyncObject *)(void *)sync_tokens[0];
    g->sync[1] = (SceGxmSyncObject *)(void *)sync_tokens[1];
    g->display[0].base = &display_tokens[0];
    g->display[1].base = &display_tokens[1];
    g->front = 0;
    g->back = 1;
    g->stats.frames = frames;
    g->stats.draws = 37;
}

/* Exact SDK prototypes come from the production source's real SDK includes. */
SceUID sceKernelGetThreadId(void) { return calling_thread; }
int sceGxmEndScene(SceGxmContext *context, const SceGxmNotification *vertex,
    const SceGxmNotification *fragment) {
    event('E');
    EXPECT(context == expected_owner->context);
    EXPECT(vertex == NULL && fragment == NULL);
    return fault_status(FAULT_END_SCENE);
}
int sceGxmPadHeartbeat(const SceGxmColorSurface *surface, SceGxmSyncObject *sync) {
    event('H');
    EXPECT(surface == &expected_owner->surfaces[expected_owner->back]);
    EXPECT(sync == expected_owner->sync[expected_owner->back]);
    return fault_status(FAULT_HEARTBEAT);
}
int sceGxmDisplayQueueAddEntry(SceGxmSyncObject *old_sync,
    SceGxmSyncObject *new_sync, const void *callback_data) {
    event('Q');
    EXPECT(old_sync == expected_owner->sync[expected_owner->front]);
    EXPECT(new_sync == expected_owner->sync[expected_owner->back]);
    EXPECT(callback_data != NULL);
    if (injected_fault == FAULT_QUEUE_ADD) return fault_status(FAULT_QUEUE_ADD);
    /* Copy callback data like the SDK queue, rather than retaining present's
     * stack address. Callback execution is synchronous only in this host test. */
    queued_display = *(const DisplayData *)callback_data;
    EXPECT(queued_display.address == expected_owner->display[expected_owner->back].base);
    EXPECT(queued_display.error == &expected_owner->display_error);
    queue_pending = 1;
    return 0;
}
void sceGxmFinish(SceGxmContext *context) {
    event('F');
    EXPECT(context == expected_owner->context);
}
int sceGxmDisplayQueueFinish(void) {
    event('D');
    EXPECT(queue_pending == 1);
    if (injected_fault == FAULT_QUEUE_FINISH) return fault_status(FAULT_QUEUE_FINISH);
    display_callback(&queued_display);
    queue_pending = 0;
    return 0;
}
int sceDisplaySetFrameBuf(const SceDisplayFrameBuf *frame, SceDisplaySetBufSync sync) {
    event('S');
    EXPECT(frame != NULL);
    if (frame) {
        EXPECT(frame->size == sizeof(*frame));
        EXPECT(frame->base == queued_display.address);
        EXPECT(frame->pitch == STRIDE && frame->width == WIDTH && frame->height == HEIGHT);
        EXPECT(frame->pixelformat == SCE_DISPLAY_PIXELFORMAT_A8B8G8R8);
    }
    EXPECT(sync == SCE_DISPLAY_SETBUF_NEXTFRAME);
    return fault_status(FAULT_SET_FRAME);
}
int sceDisplayWaitVblankStart(void) {
    event('W');
    return fault_status(FAULT_VBLANK);
}

static void report_case(unsigned before) {
    ++cases;
    printf("%s %s trace=%s\n", failures == before ? "PASS" : "FAIL", case_name, trace);
}
static void rejected_preconditions(void) {
    for (unsigned condition = 0; condition < 4; ++condition) {
        KvGxm g;
        reset(FAULT_NONE, &g, 0);
        snprintf(case_name, sizeof(case_name), "precondition.%s",
            (const char *const[]){"null", "no-scene", "wrong-thread", "closing"}[condition]);
        if (condition == 1) g.scene_open = 0;
        if (condition == 2) ++g.thread;
        if (condition == 3) g.closing = 1;
        const KvGxm before_owner = g;
        const unsigned before = failures;
        EXPECT(kv_gxm_present(condition == 0 ? NULL : &g) == KV_STATE);
        EXPECT(trace_length == 0);
        EXPECT(memcmp(&g, &before_owner, sizeof(g)) == 0);
        report_case(before);
    }
}
static void failed_submission(enum Fault point, uint32_t frames) {
    KvGxm g;
    reset(point, &g, frames);
    snprintf(case_name, sizeof(case_name), "fault.%s.frames=%u", fault_names[point], frames);
    const unsigned before = failures;
    EXPECT(active(&g));
    EXPECT(g.closing == 0 && g.scene_open == 1 && g.scanout_submitted == 0);
    EXPECT(g.front == 0 && g.back == 1 && g.stats.frames == frames);
    EXPECT(kv_gxm_present(&g) == fault_status(point));
    EXPECT(g.closing == 1);
    EXPECT(!active(&g));
    EXPECT(g.front == 0 && g.back == 1 && g.stats.frames == frames);
    EXPECT(g.stats.draws == 37 && g.initialized == 1);
    EXPECT(g.scene_open == (point == FAULT_END_SCENE));
    EXPECT(strcmp(trace, expected_traces[point]) == 0);
    EXPECT(g.error[0] != 0);
    if (point >= FAULT_QUEUE_FINISH) EXPECT(g.scanout_submitted == 1);
    if (point < FAULT_QUEUE_ADD) EXPECT(g.scanout_submitted == 0);
    if (point == FAULT_QUEUE_FINISH) EXPECT(queue_pending == 1);
    if (point == FAULT_SET_FRAME || point == FAULT_VBLANK)
        EXPECT(g.display_error == fault_status(point));
    const size_t before_retry = trace_length;
    EXPECT(kv_gxm_present(&g) == KV_STATE);
    EXPECT(trace_length == before_retry);
    report_case(before);
}
static void successful_submission(uint32_t frames) {
    KvGxm g;
    reset(FAULT_NONE, &g, frames);
    snprintf(case_name, sizeof(case_name), "success.frames=%u", frames);
    const unsigned before = failures;
    EXPECT(active(&g));
    EXPECT(g.closing == 0 && g.scene_open == 1 && g.scanout_submitted == 0);
    EXPECT(kv_gxm_present(&g) == 0);
    EXPECT(g.closing == 0 && active(&g));
    EXPECT(g.scene_open == 0 && g.scanout_submitted == 1);
    EXPECT(g.front == 1 && g.back == 2 && g.stats.frames == frames + 1);
    EXPECT(g.stats.draws == 37 && g.initialized == 1);
    EXPECT(g.error[0] == 0 && g.display_error == 0 && queue_pending == 0);
    EXPECT(strcmp(trace, expected_traces[FAULT_NONE]) == 0);
    const size_t before_retry = trace_length;
    EXPECT(kv_gxm_present(&g) == KV_STATE); /* No scene means no second advance. */
    EXPECT(g.stats.frames == frames + 1 && trace_length == before_retry);
    report_case(before);
}
int main(void) {
    puts("HOST-ONLY GXM present fault injection; SDK calls are stubs, no GPU execution.");
    rejected_preconditions();
    const uint32_t starting_frames[] = {0, 17};
    for (unsigned i = 0; i < sizeof(starting_frames) / sizeof(starting_frames[0]); ++i) {
        for (enum Fault point = FAULT_END_SCENE; point <= FAULT_VBLANK; ++point)
            failed_submission(point, starting_frames[i]);
        successful_submission(starting_frames[i]);
    }
    printf("RESULT %s: %u cases, %u checks, %u failures; host submission state only\n",
        failures ? "FAIL" : "PASS", cases, checks, failures);
    return failures ? 1 : 0;
}

/* Deterministic clock for legacy timing sites; no device timing claim. */
SceUInt64 sceKernelGetProcessTimeWide(void) { static SceUInt64 t; return ++t; }
