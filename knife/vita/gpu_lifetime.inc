/* Ownership protocol shared by clear/copy/effect/present, not a guessed frame delay.
 * GXM scene dependency flags order fragment -> next vertex work on this context.
 * Fragment notifications only establish when CPU storage/owners may be reused.
 * Reference: VitaSDK gxm.h SceGxmSceneFlags; vitaGL 9bf3e3f4 source/gxm.c
 * scene_end / scene_reset and buffers.c query_fence. Transfers keep explicit drains. */
struct PoolSpan { PoolSpan *next; uint32_t offset, size, available; };
struct TexturePage { TexturePage *next; Block storage; PoolSpan *spans; uint32_t used; };
struct RetiredPixels { RetiredPixels *next; Block pixels; uint32_t last_use, charged; uint64_t identity; };
static int require_scene_read(KvGxm *g, const KvGxmTexture *texture);
static uint32_t completed_sequence(const KvGxm *g);
static int collect_retired_pixels(KvGxm *g, int force) {
    RetiredPixels **entry = &g->retired_pixels;
    while (*entry) {
        RetiredPixels *old = *entry;
        if (!force && old->last_use > completed_sequence(g)) { entry = &old->next; continue; }
        const uint32_t bytes = old->charged;
        const uint64_t started = kv_perf_begin();
        const int result = block_free(g, &old->pixels);
        kv_perf_end(KV_PERF_RENAME_RETIRE, started, result >= 0 ? bytes : 0, result < 0);
        if (result < 0) { g->closing = 1; return fail(g, "renamed pixels retained owner", result); }
        g->rename_bytes -= bytes; --g->rename_owners;
        *entry = old->next; free(old);
    }
    return 0;
}
static int retired_pixels_destroy(KvGxm *g) { return collect_retired_pixels(g, 1); }

static int async_submission(const KvGxm *g) {
    const uint32_t required = KV_OPT_BATCH_ARENA | KV_OPT_ASYNC_SUBMIT;
    return (kv_render_options() & required) == required && g->completion != NULL;
}
static uint32_t completed_sequence(const KvGxm *g) {
    if (!g->completion) return 0;
    /* Each slot owns a contiguous submitted sequence range. The earliest
       incomplete range limits the global retirement frontier, even if the
       other slot has completed a later batch. Never use max(words). */
    uint32_t covered = g->sequence;
    for (uint32_t i = 0; i < BATCH_SLOTS; ++i) {
        const BatchSlot *slot = &g->batches[i];
        if (!slot->first_sequence || slot->last_sequence < slot->first_sequence) continue;
        const uint32_t observed = g->completion[i];
        if (observed >= slot->last_sequence) continue;
        const uint32_t before_pending = observed >= slot->first_sequence
            ? observed : slot->first_sequence - 1;
        if (before_pending < covered) covered = before_pending;
    }
    return covered;
}
static void texture_use(KvGxm *g, const KvGxmTexture *texture) {
    if (kv_render_options() & KV_OPT_BATCH_ARENA)
    {
        ((KvGxmTexture *)texture)->last_use = g->sequence + 1;
        ((KvGxmTexture *)texture)->last_read = g->sequence + 1;
    }
}
static int wait_slot(KvGxm *g, uint32_t index, enum KvPerfMetric metric, int reuse) {
    BatchSlot *slot = &g->batches[index];
    if (!g->completion || !slot->last_sequence || g->completion[index] >= slot->last_sequence) return 0;
    /* Only this slot writes its word. No newer scene can overtake this final
       value while the owner thread waits; the other slot uses another word. */
    const SceGxmNotification notification = { g->completion + index, slot->last_sequence };
    const uint64_t started = kv_perf_begin();
    if (reuse) {
        kv_perf_end_at(KV_PERF_SLOT_WAIT_NEEDED, started, started, 0, slot->last_sequence);
        kv_perf_end_at(KV_PERF_SLOT_WAIT_LATEST, started, started, 0, g->sequence);
        kv_perf_end_at(KV_PERF_SLOT_WAIT_COVERED, started, started, 0, completed_sequence(g));
    }
    kv_perf_native_enter(KV_NATIVE_NOTIFICATION_WAIT, slot->last_sequence);
    const int result = sceGxmNotificationWait(&notification);
    kv_perf_native_leave();
    kv_perf_end(metric, started, 0, result < 0);
    if (result < 0) { g->closing = 1; return fail(g, "GPU slot completion retained owner", result); }
    return 0;
}
static int wait_submitted(KvGxm *g, enum KvPerfMetric metric) {
    /* Explicit context drains retain their full scope. Slot reuse alone uses
       wait_slot directly, never turning an old-slot need into a newest wait. */
    for (uint32_t n = 0; n < BATCH_SLOTS; ++n)
        CHECK(g, wait_slot(g, (g->batch_index + n) % BATCH_SLOTS, metric, 0));
    return 0;
}
static void texture_write(KvGxm *g, KvGxmTexture *texture) {
    if (texture && (kv_render_options() & KV_OPT_BATCH_ARENA)) {
        texture->last_use = g->sequence + 1;
        texture->last_write = g->sequence + 1;
    }
}
int32_t kv_gxm_scene_plan(KvGxm *g, KvGxmTexture *target,
        const KvGxmTexture *const *reads, uint32_t count, uint32_t kind) {
    if (!active(g) || g->scene_open || kind > KV_SCENE_BAKE || count > 262144U
        || (count && !reads) || (target && !texture_owned(g, target))) return KV_BAD_ARGUMENT;
    uint32_t need = 0, hazards = 0;
    const uint32_t complete = completed_sequence(g);
    if (target) {
        if (target->last_read > complete) { hazards |= 2; need = target->last_read; }
        if (target->last_write > complete) { hazards |= 4; if (target->last_write > need) need = target->last_write; }
    }
    for (uint32_t i = 0; i < count; ++i) {
        const KvGxmTexture *t = reads[i];
        if (!t) continue;
        if (!texture_owned(g, t) || t == target) return KV_BAD_ARGUMENT;
        if (t->last_write > complete) { hazards |= 1; if (t->last_write > need) need = t->last_write; }
    }
    g->plan_ready = 1; g->plan_target = target; g->plan_kind = kind;
    g->plan_need_sequence = need; g->plan_hazards = hazards;
    return 0;
}
static unsigned int scene_flags(KvGxm *g, KvGxmTexture *target, uint32_t kind) {
    const int planned = g->plan_ready && g->plan_target == target;
    g->scene_kind = planned ? g->plan_kind : kind;
    g->scene_ps_ops = 0;
    if (!async_submission(g)) { g->plan_ready = 0; return 0; }
    unsigned int flags = SCE_GXM_SCENE_FRAGMENT_SET_DEPENDENCY;
    int wait = g->dependency_pending != 0;
    if (kv_render_options() & KV_OPT_DEPENDENCY_GRAPH) {
        uint32_t need = planned ? g->plan_need_sequence : g->sequence;
        const uint32_t complete = completed_sequence(g);
        if (complete > g->gpu_covered_sequence) g->gpu_covered_sequence = complete;
        wait = need > g->gpu_covered_sequence;
        if (wait) {
            const uint64_t observed = kv_perf_begin();
            kv_perf_end_at(KV_PERF_SCENE_BARRIER_NEED, observed, observed, 0, need);
            kv_perf_end_at(KV_PERF_SCENE_BARRIER_LATEST, observed, observed, 0, g->sequence);
            kv_perf_end_at(KV_PERF_SCENE_BARRIER_COVERED, observed, observed, 0, g->gpu_covered_sequence);
            enum KvPerfMetric metric = KV_PERF_SCENE_BARRIER_OTHER;
            switch (g->scene_kind) {
                case KV_SCENE_STAGE: metric = KV_PERF_SCENE_BARRIER_STAGE; break;
                case KV_SCENE_GROUP_PIXEL: metric = KV_PERF_SCENE_BARRIER_PIXEL; break;
                case KV_SCENE_GROUP_MASK: metric = KV_PERF_SCENE_BARRIER_MASK; break;
                case KV_SCENE_CANVAS: metric = KV_PERF_SCENE_BARRIER_CANVAS; break;
                case KV_SCENE_PRESENT: metric = KV_PERF_SCENE_BARRIER_PRESENT; break;
            }
            kv_perf_end_at(metric, observed, observed, 0, g->sequence - need);
        }
        if (planned && wait) {
            if (g->plan_hazards & 1) kv_perf_count(KV_PERF_SCENE_WAIT_RAW, 0);
            if (g->plan_hazards & 2) kv_perf_count(KV_PERF_SCENE_WAIT_WAR, 0);
            if (g->plan_hazards & 4) kv_perf_count(KV_PERF_SCENE_WAIT_WAW, 0);
        } else if (!planned) kv_perf_count(KV_PERF_SCENE_PLAN_FALLBACK, 0);
        if (!wait) kv_perf_count(KV_PERF_SCENE_INDEPENDENT, 0);
    }
    if (wait) {
        flags |= SCE_GXM_SCENE_VERTEX_WAIT_FOR_DEPENDENCY;
        /* Every submitted fragment producer SETs the dependency. Waiting on
           the latest one covers all earlier fragment jobs on this context. */
        g->gpu_covered_sequence = g->sequence;
    }
    g->plan_ready = 0; g->dependency_pending = 1;
    kv_perf_count(KV_PERF_SCENE_DEPENDENCY, 0);
    return flags;
}
static void scene_ps_use(KvGxm *g, uint32_t op) {
    if (op >= 1 && op <= 24) g->scene_ps_ops |= 1U << (op - 1);
}
static int end_submitted_scene(KvGxm *g) {
    const int arena = (kv_render_options() & KV_OPT_BATCH_ARENA) != 0;
    if (arena && (!g->completion || g->sequence == UINT32_MAX)) return KV_STATE;
    const SceGxmNotification notification = { arena ? g->completion + g->batch_index : NULL,
        g->sequence + 1 };
    const int result = perf_end_scene_kind(g->context, NULL, arena ? &notification : NULL,
        g->scene_kind, g->previous_scene_ps_ops);
    if (result < 0) { g->closing = 1; return result; }
    if (arena) {
        ++g->sequence;
        g->batches[g->batch_index].last_sequence = g->sequence;
    }
    g->previous_scene_ps_ops = g->scene_ps_ops;
    return 0;
}
static int collect_retired_textures(KvGxm *g) {
    CHECK(g, collect_retired_pixels(g, 0));
    const uint32_t completed = completed_sequence(g);
    KvGxmTexture **entry = &g->textures;
    while (*entry) {
        KvGxmTexture *t = *entry;
        if (!t->retiring || t->last_use > completed || t == g->white) { entry = &t->next; continue; }
        const uint64_t started = kv_perf_begin();
        const uint32_t bytes = t->pixels.size;
        int result = texture_target_destroy(g, t);
        if (result >= 0) result = block_free(g, &t->pixels);
        kv_perf_end(KV_PERF_RETIRE_TEXTURE, started, result >= 0 ? bytes : 0, result < 0);
        if (result < 0) { g->closing = 1; return fail(g, "retired texture retained owner", result); }
        *entry = t->next;
        free(t);
    }
    return 0;
}
int32_t kv_gxm_reclaim(KvGxm *g, uint32_t wait) {
    if (!active(g) || g->scene_open || wait > 1) return KV_STATE;
    if (!(kv_render_options() & KV_OPT_BATCH_ARENA)) return 0;
    if (wait) CHECK(g, wait_submitted(g, KV_PERF_BATCH_WAIT));
    return collect_retired_textures(g);
}
int32_t kv_gxm_begin_batch(KvGxm *g, uint32_t basic_quads, uint32_t effect_vertices) {
    if (!active(g) || g->scene_open) return KV_STATE;
    if (!(kv_render_options() & KV_OPT_BATCH_ARENA)) return 0;
    if ((uint64_t)effect_vertices * sizeof(KvEffectVertex) > 16U * 1024U * 1024U
        || basic_quads > (16U * 1024U * 1024U) / (4U * sizeof(Vertex))) return KV_BUDGET;
    if (!g->completion) {
        g->completion = sceGxmGetNotificationRegion();
        if (!g->completion) return fail(g, "GXM notification region", KV_ALLOC);
        for (uint32_t i = 0; i < BATCH_SLOTS; ++i) g->completion[i] = 0;
    }
    if (g->sequence > UINT32_MAX - 65536U) {
        /* All identities are reset only after actual completion; never compare
           wrapped generations or leave a retired owner carrying an old value. */
        CHECK(g, wait_submitted(g, KV_PERF_BATCH_WAIT));
        CHECK(g, collect_retired_textures(g));
        for (KvGxmTexture *t = g->textures; t; t = t->next) t->last_use = t->last_read = t->last_write = 0;
        for (uint32_t i = 0; i < BATCH_SLOTS; ++i) {
            g->batches[i].first_sequence = g->batches[i].last_sequence = 0;
            g->completion[i] = 0;
        }
        g->sequence = 0; g->gpu_covered_sequence = 0; g->plan_ready = 0;
    }
    const uint32_t next = (g->batch_index + 1) % BATCH_SLOTS;
    BatchSlot *slot = &g->batches[next];
    CHECK(g, wait_slot(g, next, KV_PERF_BATCH_WAIT, 1));
    CHECK(g, collect_retired_textures(g));
    uint64_t planned = (uint64_t)basic_quads * 4U * sizeof(Vertex)
        + (uint64_t)effect_vertices * sizeof(KvEffectVertex);
    if (planned < 64U * 1024U) planned = 64U * 1024U;
    if (planned > BATCH_MAX_BYTES) planned = BATCH_MAX_BYTES;
    const uint32_t wanted = ((uint32_t)planned + 65535U) & ~65535U;
    if (slot->vertices.size < wanted) {
        CHECK(g, block_free(g, &slot->vertices));
        CHECK(g, block_alloc(g, &slot->vertices, wanted, 0, 1));
    }
    slot->used = 0;
    slot->first_sequence = g->sequence + 1;
    g->batch_index = next;
    g->batch_active = 1;
    g->quads = 0;
    return 0;
}
static int ensure_batch(KvGxm *g) {
    if (!(kv_render_options() & KV_OPT_BATCH_ARENA) || g->batch_active) return 0;
    return kv_gxm_begin_batch(g, MAX_QUADS, 4096);
}
/* A controlled boundary is shared by genuine capacity overflow and missed
 * dependency plans. Both complete readers before memory reuse; their counters
 * remain distinct so a missing edge is never reported as insufficient capacity. */
static int batch_reopen(KvGxm *g, uint32_t required, enum KvPerfMetric metric) {
    BatchSlot *slot = &g->batches[g->batch_index];
    const uint64_t started = kv_perf_begin();
    KvGxmTexture *target = g->current_target;
    const uint32_t kind = g->scene_kind;
    CHECK(g, end_submitted_scene(g));
    g->scene_open = 0;
    CHECK(g, wait_submitted(g, KV_PERF_BATCH_WAIT));
    if (slot->vertices.size < required) {
        CHECK(g, block_free(g, &slot->vertices));
        CHECK(g, block_alloc(g, &slot->vertices, (required + 65535U) & ~65535U, 0, 1));
    }
    slot->used = 0;
    /* Actual CPU completion covers every prior resource. A precise empty plan
       avoids classifying the defensive reopen itself as another missing plan. */
    CHECK(g, kv_gxm_scene_plan(g, target, NULL, 0, kind));
    const int result = target ? perf_begin_scene(g->context, scene_flags(g, target, kind), target->target,
        NULL, NULL, target->sync, &target->surface, &g->depth) :
        perf_begin_scene(g->context, scene_flags(g, NULL, kind), g->target, NULL, NULL,
            g->sync[g->back], &g->surfaces[g->back], &g->depth);
    if (result < 0) { g->closing = 1; return fail(g, "arena segment reopen retained owner", result); }
    g->scene_open = 1;
    if (target) texture_write(g, target);
    const float w = target ? target->width : WIDTH;
    const float h = target ? target->height : CONTENT_H;
    const float y = target ? 0.0f : CONTENT_Y;
    sceGxmSetViewport(g->context, w / 2.0f, w / 2.0f, y + h / 2.0f, -h / 2.0f, 0.5f, 0.5f);
    sceGxmSetCullMode(g->context, SCE_GXM_CULL_NONE);
    sceGxmSetFrontDepthFunc(g->context, SCE_GXM_DEPTH_FUNC_ALWAYS);
    sceGxmSetBackDepthFunc(g->context, SCE_GXM_DEPTH_FUNC_ALWAYS);
    sceGxmSetFrontDepthWriteEnable(g->context, SCE_GXM_DEPTH_WRITE_DISABLED);
    sceGxmSetBackDepthWriteEnable(g->context, SCE_GXM_DEPTH_WRITE_DISABLED);
    kv_perf_end(metric, started, slot->vertices.size, 0);
    return 0;
}
static int batch_vertices(KvGxm *g, uint32_t bytes, void **out) {
    *out = NULL;
    if (!(kv_render_options() & KV_OPT_BATCH_ARENA)) return 0;
    if (!g->batch_active || !g->scene_open || !bytes || bytes > BATCH_MAX_BYTES) return KV_STATE;
    BatchSlot *slot = &g->batches[g->batch_index];
    bytes = (bytes + 15U) & ~15U;
    if (bytes > slot->vertices.size - slot->used) CHECK(g, batch_reopen(g, bytes, KV_PERF_BATCH_DRAIN));
    *out = (uint8_t *)slot->vertices.base + slot->used;
    slot->used += bytes;
    return 0;
}
/* Actual draws defend against an incomplete caller plan before submitting a
 * texture read. The missed edge goes through a real completion boundary. */
static int require_scene_read(KvGxm *g, const KvGxmTexture *texture) {
    if (!async_submission(g) || !(kv_render_options() & KV_OPT_DEPENDENCY_GRAPH)
        || texture->last_write <= g->gpu_covered_sequence
        || texture->last_write <= completed_sequence(g)) return 0;
    CHECK(g, batch_reopen(g, 0, KV_PERF_SCENE_PLAN_FALLBACK));
    g->gpu_covered_sequence = g->sequence;
    return 0;
}
int32_t kv_gxm_texture_charge(uint32_t payload_bytes, uint32_t *out) {
    if (!out || !payload_bytes) return KV_BAD_ARGUMENT;
    /* Small glyph/UI slices share 256 KiB pages. A larger image owns its
       complete kernel page: a tiny long-lived glyph in its padding must not
       keep a mostly empty 4 MiB portrait page mapped after that image retires. */
    const uint32_t alignment = (kv_render_options() & KV_OPT_TEXTURE_POOL)
        && payload_bytes < 256U * 1024U ? 64U : 256U * 1024U;
    if (payload_bytes > UINT32_MAX - alignment + 1U) return KV_BAD_ARGUMENT;
    *out = (payload_bytes + alignment - 1U) & ~(alignment - 1U);
    return 0;
}
static int pool_remove_page(KvGxm *g, TexturePage **entry) {
    TexturePage *page = *entry;
    const int result = block_free(g, &page->storage);
    if (result < 0) return result;
    *entry = page->next;
    while (page->spans) { PoolSpan *next = page->spans->next; free(page->spans); page->spans = next; }
    free(page);
    return 0;
}
static int pool_trim(KvGxm *g, uint32_t keep_bytes) {
    uint32_t kept = 0;
    TexturePage **entry = &g->texture_pages;
    while (*entry) {
        TexturePage *page = *entry;
        if (!page->used && page->storage.size <= keep_bytes - kept) { kept += page->storage.size; entry = &page->next; }
        else if (!page->used) { const int result = pool_remove_page(g, entry); if (result < 0) return result; }
        else entry = &page->next;
    }
    return 0;
}
static int pool_slice_free(KvGxm *g, Block *b) {
    TexturePage *page = b->page;
    if (!page || !b->span || b->span->available || page->used < b->size) return KV_STATE;
    page->used -= b->size;
    b->span->available = 1;
    memset(b, 0, sizeof(*b));
    for (PoolSpan *span = page->spans; span && span->next;) {
        if (span->available && span->next->available) {
            PoolSpan *next = span->next;
            span->size += next->size; span->next = next->next; free(next);
        } else span = span->next;
    }
    /* Empty, already retired pages are bounded; active slices are never moved. */
    return pool_trim(g, 2U * 1024U * 1024U);
}
static int texture_pixels_alloc(KvGxm *g, Block *out, uint32_t requested) {
    if (!(kv_render_options() & KV_OPT_TEXTURE_POOL)) return block_alloc(g, out, requested, 1, 1);
    uint32_t bytes = 0;
    CHECK(g, kv_gxm_texture_charge(requested, &bytes));
    CHECK(g, collect_retired_textures(g));
    TexturePage *page = NULL; PoolSpan *span = NULL;
    for (page = g->texture_pages; page; page = page->next) {
        /* A retired large page may remain cached. Do not let a later glyph
           split it, or let a smaller image leave a tail for that glyph.
           Large requests reuse only an empty page of the exact charged size. */
        if (bytes < 256U * 1024U) {
            if (page->storage.size != 256U * 1024U) continue;
        } else if (page->used || page->storage.size != bytes) continue;
        for (span = page->spans; span; span = span->next)
            if (span->available && span->size >= bytes) break;
        if (span) break;
    }
    if (!page) {
        CHECK(g, pool_trim(g, 0));
        page = calloc(1, sizeof(*page)); span = calloc(1, sizeof(*span));
        if (!page || !span) { free(page); free(span); return KV_ALLOC; }
        page->spans = span; span->available = 1;
        page->next = g->texture_pages; g->texture_pages = page;
        const uint32_t page_bytes = bytes > 256U * 1024U ? bytes : 256U * 1024U;
        const int result = block_alloc(g, &page->storage, page_bytes, 1, 1);
        if (result < 0) {
            if (page->storage.uid <= 0) { g->texture_pages = page->next; free(span); free(page); }
            return result;
        }
        span->size = page->storage.size;
        kv_perf_count(KV_PERF_POOL_PAGE, span->size);
    } else kv_perf_count(KV_PERF_POOL_HIT, bytes);
    if (span->size > bytes) {
        PoolSpan *tail = calloc(1, sizeof(*tail));
        if (!tail) return KV_ALLOC;
        *tail = (PoolSpan){ .next = span->next, .offset = span->offset + bytes,
            .size = span->size - bytes, .available = 1 };
        span->next = tail; span->size = bytes;
    }
    span->available = 0; page->used += bytes;
    *out = (Block){ .uid = page->storage.uid, .base = (uint8_t *)page->storage.base + span->offset,
        .size = bytes, .cdram = 1, .page = page, .span = span };
    return 0;
}
static void texture_initialize_padding(const KvGxmTexture *t, enum KvPerfMetric metric, int overwritten) {
    if (!(kv_render_options() & KV_OPT_TEXTURE_POOL) || !overwritten) {
        perf_zero(metric, t->pixels.base, t->pixels.size);
        return;
    }
    /* Source upload overwrites every logical pixel. Clear only row padding and
       the suballocation tail; never clear neighboring slices in the CDRAM page. */
    const uint32_t row_bytes = t->width * t->bpp, pitch_bytes = t->pitch * t->bpp;
    const uint64_t started = kv_perf_begin();
    uint64_t cleared = 0;
    if (pitch_bytes > row_bytes) for (uint32_t y = 0; y < t->height; ++y) {
        memset((uint8_t *)t->pixels.base + y * pitch_bytes + row_bytes, 0, pitch_bytes - row_bytes);
        cleared += pitch_bytes - row_bytes;
    }
    const uint32_t payload = pitch_bytes * t->height;
    if (t->pixels.size > payload) {
        memset((uint8_t *)t->pixels.base + payload, 0, t->pixels.size - payload);
        cleared += t->pixels.size - payload;
    }
    kv_perf_end(metric, started, cleared, 0);
}
static int pool_destroy(KvGxm *g) {
    while (g->texture_pages) {
        if (g->texture_pages->used) return KV_STATE;
        CHECK(g, pool_remove_page(g, &g->texture_pages));
    }
    return 0;
}

/* Only immutable CPU-origin pixels may be copied while the GPU reads them.
 * RT/readback owners require completion, not a CPU copy of potentially stale
 * storage. A logical source has at most one pending old version (two backings). */
static int texture_rename_for_update(KvGxm *g, KvGxmTexture *t) {
    if (!async_submission(g) || !(kv_render_options() & KV_OPT_SOURCE_RENAME)
        || t->last_use <= completed_sequence(g)) return 0;
    const uint64_t started = kv_perf_begin();
    uint32_t reason = 0;
    const uint32_t limit = 8U * 1024U * 1024U;
    if (t->target || t->rendered || t->storage != KV_TEXTURE_LINEAR || !t->pixels.base
        || t->pixels.size > 2U * 1024U * 1024U) reason = 1;
    CHECK(g, collect_retired_pixels(g, 0));
    if (!reason && (g->rename_owners >= 32 || t->pixels.size > limit - g->rename_bytes)) reason = 2;
    if (!t->rename_identity) {
        if (g->next_rename_identity == UINT64_MAX) reason = 3;
        else t->rename_identity = ++g->next_rename_identity;
    }
    for (const RetiredPixels *old = g->retired_pixels; !reason && old; old = old->next)
        if (old->identity == t->rename_identity) reason = 4;
    if (reason) { kv_perf_end(KV_PERF_RENAME_FALLBACK, started, 0, reason); return 0; }
    RetiredPixels *old = calloc(1, sizeof(*old));
    if (!old) { kv_perf_end(KV_PERF_RENAME_FALLBACK, started, 0, 5); return 0; }
    Block replacement = {0};
    int result = texture_pixels_alloc(g, &replacement, t->pitch * t->height * t->bpp);
    if (result >= 0) {
        SceGxmTexture descriptor = t->descriptor;
        result = sceGxmTextureSetData(&descriptor, replacement.base);
        if (result >= 0) result = sceGxmTextureValidate(&descriptor);
        if (result >= 0) {
            const uint64_t copy_started = kv_perf_begin();
            const uint32_t payload = t->pitch * t->height * t->bpp;
            memcpy(replacement.base, t->pixels.base, payload);
            if (replacement.size > payload) memset((uint8_t *)replacement.base + payload, 0, replacement.size - payload);
            kv_perf_end(KV_PERF_RENAME_COPY, copy_started, payload, 0);
            old->pixels = t->pixels; old->charged = t->pixels.size; old->last_use = t->last_use; old->identity = t->rename_identity;
            old->next = g->retired_pixels; g->retired_pixels = old;
            g->rename_bytes += old->pixels.size; ++g->rename_owners;
            t->pixels = replacement; t->descriptor = descriptor;
            t->last_use = t->last_read = t->last_write = 0;
            kv_perf_end(KV_PERF_RENAME_UPDATE, started, 0, old->pixels.size);
            return 1;
        }
    }
    /* Allocation/descriptor admission is reversible. If cleanup is uncertain,
       retain its exact mapped owner for teardown instead of losing a stack Block. */
    const int cleanup = block_free(g, &replacement);
    if (cleanup < 0 || g->closing) {
        if (replacement.uid > 0) {
            old->pixels = replacement; old->charged = replacement.size; old->last_use = 0;
            old->next = g->retired_pixels; g->retired_pixels = old;
            g->rename_bytes += replacement.size; ++g->rename_owners;
        } else free(old);
        g->closing = 1;
        return fail(g, "source rename rollback retained owner", cleanup < 0 ? cleanup : result);
    }
    free(old);
    kv_perf_end(KV_PERF_RENAME_FALLBACK, started, 0, 6);
    return 0;
}
