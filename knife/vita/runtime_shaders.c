/* Cache only authored shaders; the user's compiler module stays on the device.
 * Compilation/loading happens once, before frame recording. Performance output
 * uses sceClibPrintf; the only writes here are validated shader cache artifacts. */
#include "shaders/generated/runtime_source.h"
#include "shader_sha256.h"
#include "shader_module_exports.h"
#include "shader_extensions.h"
#ifdef KV_SHADER_CACHE_HOST_TEST
#include "tests/runtime_shader_cache_stubs.h"
#else
#include "runtime_shaders.h"
#include <psp2/shacccg.h>
#include <psp2/kernel/modulemgr.h>
#include <psp2/kernel/clib.h>
#include <psp2/kernel/processmgr.h>
#include <psp2/io/stat.h>
#endif
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>
#include <errno.h>

enum { CACHE_ABI = 5, MAX_PROGRAM = 1024 * 1024, MIN_PROGRAM = 156 };
#include "diagnostic_log.h"
static struct {
    int attempted;
    SceGxmProgram *programs[48];
} runtime;
#ifndef KV_SHADER_CACHE_HOST_TEST
static SceShaccCgSourceFile source;
#endif
typedef struct {
    unsigned char magic[8];
    uint32_t abi, shader, entry_abi, bytes, compiler_policy, reserved;
    char source_sha256[64];
    char compiler_sha256[64];
    uint64_t checksum;
} CacheHeader;
_Static_assert(sizeof(CacheHeader) == 168, "shader cache header ABI");
#ifndef KV_SHADER_CACHE_ROOT
/* The runtime shader cache lives in the game's own data root, beside assets/ and saves/. */
#define KV_SHADER_CACHE_ROOT "ux0:data/WitchontheHolyNight/shader-cache"
#endif
static const char *const cache_root = KV_SHADER_CACHE_ROOT;
static const char *validation_reason;
static int fetch_shader(uint32_t shader) { return (shader >= 17 && shader <= 21) || shader == 23 || shader == 25 || (shader >= 32 && shader < 48); }
/* ID24 is a checked text-cache capability, never a shader entry. Source SHA
   changes invalidate prior Cg binaries without changing their metadata ABI. */
static const uint32_t runtime_shader_ids[] = { 11, 22, 26, 28, 23, 25, 15, 19, 20, 21, 18, 17,
    32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47 };
static int runtime_shader_id(uint32_t shader) {
    for (unsigned i = 0; i < sizeof(runtime_shader_ids) / sizeof(runtime_shader_ids[0]); ++i)
        if (runtime_shader_ids[i] == shader) return 1;
    return 0;
}
static uint32_t runtime_entry_abi(uint32_t shader) {
    if (shader >= 32 && shader < 48) {
        const uint32_t ops[4] = {2, 11, 13, 19};
        // Cache identity includes the exact compile-time material macros.
        return KNIFE_RUNTIME_ENTRY_ABI | ops[(shader-32)/4] << 8 | ops[(shader-32)%4] << 16;
    }
    return KNIFE_RUNTIME_ENTRY_ABI;
}
static const char *entry_for(uint32_t shader) {
    switch (shader) {
        case 11: return "fs_alter"; /* Current source: encoded RGBA passthrough. */
        case 15: return "fs_group_composite";
        case 17: return "fs_ps_fetch";
        case 18: return "fs_ps19_fetch";
        case 19: return "fs_ps19_group_fetch";
        case 20: return "fs_ps11_fetch";
        case 21: return "fs_ps13_fetch";
        case 22: return "fs_sprite_plain";
        case 26: return "fs_forcealpha_plain";
        case 23: return "fs_ps19_plain_fetch";
        case 25: return "fs_ps2_fetch";
        case 28: return "fs_rim_text_batched";
        default: return shader >= 32 && shader < 48 ? "fs_ps_chain2_fetch" : NULL;
    }
}
static int runtime_any(void) {
    for (unsigned i = 0; i < sizeof(runtime_shader_ids) / sizeof(runtime_shader_ids[0]); ++i)
        if (runtime.programs[runtime_shader_ids[i]]) return 1;
    return 0;
}

static uint32_t word(const unsigned char *data) {
    return (uint32_t)data[0] | (uint32_t)data[1] << 8 | (uint32_t)data[2] << 16 | (uint32_t)data[3] << 24;
}
static uint64_t checksum(const void *data, uint32_t bytes) {
    uint64_t hash = UINT64_C(14695981039346656037);
    const unsigned char *p = data;
    for (uint32_t i = 0; i < bytes; ++i) hash = (hash ^ p[i]) * UINT64_C(1099511628211);
    return hash;
}
static int relative_range(const unsigned char *data, uint32_t bytes, uint32_t field,
    uint32_t length, uint32_t *result) {
    if (field > bytes || bytes - field < 4) return 0;
    const int64_t offset = (int64_t)field + (int32_t)word(data + field);
    if (offset < 0 || offset > bytes || length > bytes - (uint32_t)offset) return 0;
    *result = (uint32_t)offset;
    return 1;
}
static int validate(const void *program, uint32_t bytes, uint32_t shader) {
#define REJECT(reason) do { validation_reason = reason; return 0; } while (0)
    validation_reason = "ok";
    const unsigned char *data = program;
    if (!runtime_shader_id(shader)) REJECT("unknown_shader_id");
    if (!program || bytes < MIN_PROGRAM || bytes > MAX_PROGRAM || word(data) != UINT32_C(0x00505847)) REJECT("magic_or_file_size");
    const uint32_t size = word(data + 8), flags = word(data + 20);
    if (size < MIN_PROGRAM || size > bytes || bytes - size > 3 || !(flags & 1)) REJECT("program_size_or_stage");
    for (uint32_t i = size; i < bytes; ++i) if (data[i]) REJECT("nonzero_padding");
    /* Fragment colour is a distinct program flag, not an interpolated COLOR
       attribute. Require it in addition to the native output register flag. */
    if (fetch_shader(shader) && (flags & UINT32_C(0xc0)) != UINT32_C(0xc0)) REJECT("native_color_read_flags");
    if (!fetch_shader(shader) && (flags & UINT32_C(0xc0))) REJECT("unexpected_native_color");
    uint32_t table, varyings, instructions;
    const uint32_t count = word(data + 36), instruction_count = word(data + 60);
    if (count > 128 || !instruction_count || instruction_count > MAX_PROGRAM / 8
        || !relative_range(data, size, 40, count * 16, &table)
        || !relative_range(data, size, 44, 32, &varyings)
        || !relative_range(data, size, 64, instruction_count * 8, &instructions)) REJECT("table_or_instruction_bounds");
    (void)instructions;
    if (data[varyings + 10] != (fetch_shader(shader) ? 7 : 1) || data[varyings + 11] != 4)
        REJECT(fetch_shader(shader) ? "expected_native_uchar4_output" : "expected_float_color_output");
    const uint32_t uniform_words = word(data + 100);
    unsigned sampler_bits = 0;
    for (uint32_t i = 0; i < count; ++i) {
        const uint32_t at = table + i * 16;
        uint32_t name;
        if (!relative_range(data, size, at, 1, &name)) REJECT("parameter_name_bounds");
        const unsigned char *end = memchr(data + name, 0, size - name);
        if (!end || (uint32_t)(end - data - name) > 96) REJECT("unterminated_parameter_name");
        const unsigned category = data[at + 4] & 15, type = data[at + 4] >> 4;
        const unsigned components = data[at + 5] & 15, container = data[at + 5] >> 4;
        const uint32_t index = word(data + at + 12);
        if (word(data + at + 8) != 1 || category > 2) REJECT("parameter_array_or_category");
        if (category == 0) { if (components < 1 || components > 4 || index > 128) REJECT("attribute_shape"); continue; }
        if (type != 0) REJECT("uniform_or_sampler_not_f32");
        if (category == 2) {
            const char *const names[] = { "tex0", "tex1", "tex2", "soft_lut" };
            if (index >= 4 || strcmp((const char *)data + name, names[index]) || (sampler_bits & (1u << index))) REJECT("sampler_unit_or_name");
            if (fetch_shader(shader) && index == 1) REJECT("fetch_must_not_sample_backdrop");
            sampler_bits |= 1u << index;
        } else {
            const char *const names[] = { "uTex0Size", "uTex1Size", "uSoftLutSize", "uQuadHx", "uQuadHy", "tu.params", "tu.params2" };
            const unsigned counts[] = { 2, 2, 2, 4, 4, 4, 4 };
            unsigned n = 0;
            while (n < 7 && strcmp((const char *)data + name, names[n])) ++n;
            if (n == 7 || components != counts[n] || container != 14 || index > uniform_words || components > uniform_words - index) REJECT("uniform_name_shape_or_bounds");
        }
    }
    if (!(sampler_bits & 1) || (shader == 15 && !(sampler_bits & 2))
        || (shader == 19 && !(sampler_bits & 4))) REJECT("required_sampler_missing");
    if (shader >= 32 && shader < 48 && sampler_bits != 5u) REJECT("chain2_requires_two_sources");
    if (shader == 19 && (sampler_bits & ~5u)) REJECT("group19_unexpected_sampler");
    if ((shader == 11 || shader == 20 || shader == 21 || shader == 22 || shader == 23 || shader == 25 || shader == 26 || shader == 28)
        && (sampler_bits & ~1u)) REJECT("plain_ps_unexpected_sampler");
    if (sceGxmProgramCheck((const SceGxmProgram *)program) < 0) REJECT("sceGxmProgramCheck");
    return 1;
#undef REJECT
}
static void path_for(char *path, size_t bytes, uint32_t shader) {
    snprintf(path, bytes, "%s/effects-%.64s-c%.64s-x%u-a%u-s%u.bin", cache_root,
        KNIFE_RUNTIME_SOURCE_SHA256, KNIFE_RUNTIME_COMPILER_SHA256,
        KV_SHADER_COMPILER_POLICY, CACHE_ABI, (unsigned)shader);
}
static SceGxmProgram *read_cache(uint32_t shader) {
    char path[320]; path_for(path, sizeof(path), shader);
    FILE *file = fopen(path, "rb");
    if (!file) { KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=cache shader=%u result=miss errno=%d\n", (unsigned)shader, errno); return NULL; }
    CacheHeader header;
    int ok = fread(&header, 1, sizeof(header), file) == sizeof(header)
        && !memcmp(header.magic, "KVGXP05", 8) && header.abi == CACHE_ABI && header.shader == shader
        && header.entry_abi == runtime_entry_abi(shader)
        && header.compiler_policy == KV_SHADER_COMPILER_POLICY && !header.reserved
        && !memcmp(header.source_sha256, KNIFE_RUNTIME_SOURCE_SHA256, 64)
        && !memcmp(header.compiler_sha256, KNIFE_RUNTIME_COMPILER_SHA256, 64)
        && header.bytes >= MIN_PROGRAM && header.bytes <= MAX_PROGRAM;
    const char *reason = ok ? "payload_or_checksum" : "header_identity_or_size";
    void *program = ok ? malloc(header.bytes) : NULL;
    ok = program && fread(program, 1, header.bytes, file) == header.bytes && fgetc(file) == EOF && !ferror(file);
    if (ok) ok = checksum(program, header.bytes) == header.checksum;
    if (ok) { ok = validate(program, header.bytes, shader); reason = validation_reason; }
    fclose(file);
    if (!ok) {
        free(program);
        const int removed = remove(path);
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=cache shader=%u result=rejected reason=%s remove_rc=%d\n", (unsigned)shader, reason, removed);
        return NULL;
    }
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=cache shader=%u result=hit bytes=%u abi=%u primary=%u color=encoded-unorm8 output=uchar4\n", (unsigned)shader, (unsigned)header.bytes, CACHE_ABI, (unsigned)word((const unsigned char *)program + 60));
    return program;
}
static void write_cache(uint32_t shader, const void *program, uint32_t bytes) {
    sceIoMkdir("ux0:data/WitchontheHolyNight", 0777);
    sceIoMkdir(cache_root, 0777);
    char path[320], temporary[328]; path_for(path, sizeof(path), shader);
    snprintf(temporary, sizeof(temporary), "%s.tmp", path);
    CacheHeader header = { .magic = "KVGXP05", .abi = CACHE_ABI, .shader = shader, .entry_abi = runtime_entry_abi(shader),
        .compiler_policy = KV_SHADER_COMPILER_POLICY,
        .bytes = bytes, .checksum = checksum(program, bytes) };
    memcpy(header.source_sha256, KNIFE_RUNTIME_SOURCE_SHA256, 64);
    memcpy(header.compiler_sha256, KNIFE_RUNTIME_COMPILER_SHA256, 64);
    FILE *file = fopen(temporary, "wb");
    if (!file) { KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=cache_write shader=%u result=open_failed errno=%d\n", (unsigned)shader, errno); return; }
    int ok = fwrite(&header, 1, sizeof(header), file) == sizeof(header)
        && fwrite(program, 1, bytes, file) == bytes;
    if (fclose(file)) ok = 0;
    if (ok) ok = rename(temporary, path) == 0;
    if (!ok) remove(temporary);
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=cache_write shader=%u bytes=%u result=%s\n", (unsigned)shader, (unsigned)bytes, ok ? "ok" : "failed");
}
#ifndef KV_SHADER_CACHE_HOST_TEST
static struct {
    int (*initialize_options)(SceShaccCgCompileOptions *);
    void (*initialize_callbacks)(SceShaccCgCallbackList *, SceShaccCgCallbackDefaults);
    int (*set_allocator)(void *(*)(unsigned int), void (*)(void *));
    const SceShaccCgCompileOutput *(*compile)(const SceShaccCgCompileOptions *, const SceShaccCgCallbackList *, int);
    void (*destroy_output)(const SceShaccCgCompileOutput *);
    void (*release)(void);
    const char *(*version)(void);
} compiler;
static const uint32_t compiler_nids[] = {
    0x3b58afa0, 0xa8c2c1c8, 0x6f01d573, 0x66814f35, 0xaa82ef0c, 0x95f57a23, 0x7f430ccd
};
static const char *const compiler_names[] = {
    "InitializeCompileOptions", "InitializeCallbackList", "SetDefaultAllocator", "CompileProgram",
    "DestroyCompileOutput", "ReleaseCompiler", "GetVersionString"
};
static int compiler_bindings_valid(SceUID module) {
    SceKernelModuleInfo info = { .size = sizeof(info) };
    const int result = sceKernelGetModuleInfo(module, &info);
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=module_info uid=%08x rc=%08x name=%.28s\n", (unsigned)module,
        (unsigned)result, result >= 0 ? info.module_name : "");
    if (result < 0 || strncmp(info.module_name, "SceShaccCg", sizeof(info.module_name))) return 0;
    KvShaderModuleMemory memory = {0};
    for (unsigned i = 0; i < 4; ++i) memory.segments[i] = (KvShaderSegment){
        (uintptr_t)info.segments[i].vaddr, info.segments[i].memsz, info.segments[i].vaddr };
    uintptr_t entries[7];
    if (!shader_module_resolve(&memory, KNIFE_RUNTIME_COMPILER_INFO_OFFSET, compiler_nids, entries, 7)) {
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=exports uid=%08x result=table_rejected\n", (unsigned)module);
        return 0;
    }
    for (unsigned i = 0; i < 7; ++i) {
        const int owner = sceKernelGetModuleIdByAddr((void *)(entries[i] & ~(uintptr_t)1));
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=export name=%s nid=%08x address=%08x owner=%08x ok=%u\n",
            compiler_names[i], (unsigned)compiler_nids[i], (unsigned)entries[i], (unsigned)owner, owner == module);
        if (owner != module) return 0;
    }
    /* Read the loader-relocated module exports directly. This avoids assuming
       weak imports are rebound or assuming one ARM/Thumb trampoline encoding.
       Every address was checked against the loaded module before conversion. */
#define BIND(field, index) memcpy(&compiler.field, &entries[index], sizeof(compiler.field))
    _Static_assert(sizeof(compiler.compile) == sizeof(uintptr_t), "Vita function pointer ABI");
    BIND(initialize_options, 0); BIND(initialize_callbacks, 1); BIND(set_allocator, 2);
    BIND(compile, 3); BIND(destroy_output, 4); BIND(release, 5); BIND(version, 6);
#undef BIND
    return 1;
}
static SceShaccCgSourceFile *open_source(const char *name, const SceShaccCgSourceLocation *from,
    const SceShaccCgCompileOptions *options, const char **error) {
    (void)name; (void)from; (void)options; (void)error;
    return &source;
}
static void *compiler_alloc(unsigned int bytes) { return malloc(bytes); }
static SceGxmProgram *compile_shader(uint32_t shader) {
    SceShaccCgCompileOptions options;
    int initialized = compiler.initialize_options(&options);
    if (initialized < 0) {
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=compile_options shader=%u rc=%08x\n", (unsigned)shader, (unsigned)initialized);
        return NULL;
    }
    source = (SceShaccCgSourceFile){ "<knife-effects>", knife_runtime_effects_source, sizeof(knife_runtime_effects_source) - 1 };
    options.mainSourceFile = source.fileName;
    options.targetProfile = SCE_SHACCCG_PROFILE_FP;
    options.entryFunctionName = entry_for(shader);
    options.useFx = 0;
    options.optimizationLevel = 3;
    options.useFastmath = options.useFastprecision = options.useFastint = 0;
    options.warningLevel = 3;
    options.warningsAsErrors = 1;
    options.performanceWarnings = 0;
    const char *defines[4] = { "KNIFE_RUNTIME_NATIVE_FETCH=1", "KNIFE_RUNTIME_PS_SPECIALIZED=1", NULL, NULL };
    char chain_a[32], chain_b[32];
    // Non-fetch entries never parse native declarations. Each failure is local.
    options.macroDefinitionCount = fetch_shader(shader) ? (shader >= 19 ? 2 : 1) : 0;
    if (shader >= 32 && shader < 48) {
        const uint32_t ops[4] = {2, 11, 13, 19};
        snprintf(chain_a,sizeof(chain_a),"KNIFE_CHAIN_OP_A=%u",(unsigned)ops[(shader-32)/4]);
        snprintf(chain_b,sizeof(chain_b),"KNIFE_CHAIN_OP_B=%u",(unsigned)ops[(shader-32)%4]);
        defines[2]=chain_a; defines[3]=chain_b; options.macroDefinitionCount=4;
    }
    options.macroDefinitions = defines;
    SceShaccCgCallbackList callbacks;
    compiler.initialize_callbacks(&callbacks, SCE_SHACCCG_TRIVIAL);
    callbacks.openFile = open_source;
    const uint64_t started = sceKernelGetProcessTimeWide();
    const SceShaccCgCompileOutput *output = compiler.compile(&options, &callbacks, 0);
    const uint64_t elapsed = sceKernelGetProcessTimeWide() - started;
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=compile shader=%u entry=%s us=%llu output_bytes=%u diagnostics=%d\n",
        (unsigned)shader, options.entryFunctionName, (unsigned long long)elapsed,
        output ? (unsigned)output->programSize : 0, output ? output->diagnosticCount : -1);
    SceGxmProgram *program = NULL;
    if (output && output->programData) {
        const int accepted = validate(output->programData, output->programSize, shader);
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=metadata shader=%u ok=%d reason=%s flags=%08x bytes=%u primary=%u color=encoded-unorm8 output=uchar4\n",
            (unsigned)shader, accepted, validation_reason,
            output->programSize >= 24 ? (unsigned)word(output->programData + 20) : 0, (unsigned)output->programSize,
            output->programSize >= 64 ? (unsigned)word(output->programData + 60) : 0);
        if (accepted) {
            program = malloc(output->programSize);
            if (program) {
                memcpy(program, output->programData, output->programSize);
                write_cache(shader, program, output->programSize);
            } else KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=program_owner shader=%u result=alloc_failed\n", (unsigned)shader);
        }
    }
    if (output) for (int i = 0; i < output->diagnosticCount && i < 8; ++i) {
        const SceShaccCgDiagnosticMessage *message = &output->diagnostics[i];
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=diagnostic shader=%u code=%u level=%u line=%u %.240s\n",
            (unsigned)shader, (unsigned)message->code, (unsigned)message->level,
            message->location ? (unsigned)message->location->lineNumber : 0, message->message ? message->message : "");
    }
    if (output) compiler.destroy_output(output);
    return program;
}
static int compiler_identity(const char *path, uint32_t expected_bytes) {
    FILE *file = fopen(path, "rb");
    if (!file) {
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=module_identity path=%s result=open_failed errno=%d\n", path, errno);
        return 0;
    }
    KvShaderSha256 hash; shader_sha256_init(&hash);
    unsigned char buffer[16384];
    size_t total = 0, bytes;
    while ((bytes = fread(buffer, 1, sizeof(buffer), file)) != 0) {
        total += bytes;
        if (total > expected_bytes) break;
        shader_sha256_update(&hash, buffer, bytes);
    }
    int read_ok = !ferror(file) && total == expected_bytes;
    if (fclose(file)) read_ok = 0;
    char identity[65]; shader_sha256_finish(&hash, identity);
    int ok = read_ok && !strcmp(identity, KNIFE_RUNTIME_COMPILER_SHA256);
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=module_identity path=%s bytes=%u sha256=%s ok=%d\n",
        path, (unsigned)total, identity, ok);
    return ok;
}
int kv_runtime_shader_initialize(void) {
    if (runtime.attempted) return runtime_any();
    runtime.attempted = 1;
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=initialize color=encoded-unorm8 output=uchar4 cache_abi=%u entry_abi=%u compiler_policy=%u source=%.16s compiler=%.16s extension=%.16s\n",
        CACHE_ABI, KNIFE_RUNTIME_ENTRY_ABI, KV_SHADER_COMPILER_POLICY,
        KNIFE_RUNTIME_SOURCE_SHA256, KNIFE_RUNTIME_COMPILER_SHA256, KV_SHADER_EXTENSION_ARCHIVE_SHA256);
    unsigned cached = 0;
    for (unsigned i = 0; i < sizeof(runtime_shader_ids) / sizeof(runtime_shader_ids[0]); ++i) {
        const uint32_t shader = runtime_shader_ids[i];
        runtime.programs[shader] = read_cache(shader);
        cached += runtime.programs[shader] != NULL;
    }
    if (cached == sizeof(runtime_shader_ids) / sizeof(runtime_shader_ids[0])) return 1;
    const char *const locations[] = { "app0:/sce_module/libshacccg.suprx", "ur0:/data/libshacccg.suprx",
        "ur0:/data/external/libshacccg.suprx", "ux0:/data/ArtemisVita/libshacccg.suprx" };
    int module = -1, first_error = 0, last_error = 0;
    unsigned identity_rejections = 0, export_rejections = 0;
    for (unsigned i = 0; i < sizeof(locations) / sizeof(locations[0]); ++i) {
        SceIoStat stat;
        int result = sceIoGetstat(locations[i], &stat);
        last_error = result;
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=module_stat path=%s rc=%08x bytes=%llu\n", locations[i],
            (unsigned)result, result >= 0 ? (unsigned long long)stat.st_size : 0);
        if (result < 0) {
            if (!first_error && (uint32_t)result != UINT32_C(0x80010002)) first_error = result;
            continue;
        }
        if (stat.st_size != KNIFE_RUNTIME_COMPILER_BYTES || !compiler_identity(locations[i], KNIFE_RUNTIME_COMPILER_BYTES)) {
            ++identity_rejections;
            KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=module_select path=%s result=identity_rejected\n", locations[i]);
            continue;
        }
        int start_status = INT_MIN;
        module = sceKernelLoadStartModule(locations[i], 0, NULL, 0, NULL, &start_status);
        last_error = module < 0 ? module : 0;
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=module_load path=%s uid=%08x start_status=%08x\n",
            locations[i], (unsigned)module, (unsigned)start_status);
        if (module < 0) {
            if (!first_error) first_error = module;
            continue;
        }
        if (compiler_bindings_valid(module)) break;
        ++export_rejections;
        int stopped = sceKernelStopUnloadModule(module, 0, NULL, 0, NULL, NULL);
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=module_reject uid=%08x reason=exports unload_rc=%08x\n", (unsigned)module, (unsigned)stopped);
        memset(&compiler, 0, sizeof(compiler));
        module = -1;
    }
    if (module < 0) {
        KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=compiler result=unavailable first_error=%08x last_error=%08x identity_rejections=%u export_rejections=%u\n",
            (unsigned)first_error, (unsigned)last_error, identity_rejections, export_rejections);
        return runtime_any();
    }
    const int allocator = compiler.set_allocator(compiler_alloc, free);
    const char *version = allocator >= 0 ? compiler.version() : NULL;
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=compiler result=ready allocator_rc=%08x version=%.160s\n",
        (unsigned)allocator, version ? version : "unavailable");
    if (allocator >= 0) {
        /* The PSM compiler strips Vita extension keywords by default. Hazuki's
           same archive restores them; module readiness alone is insufficient.
           Never attempt native declarations when extension admission failed. */
        const int extensions = kv_shader_extensions_enable(module);
        if (extensions == 0) {
            /* Compile hot specialised entries first. Failed new variants leave
               the old entries available; Rust selects only patched capabilities. */
            for (unsigned i = 0; i < sizeof(runtime_shader_ids) / sizeof(runtime_shader_ids[0]); ++i) {
                const uint32_t shader = runtime_shader_ids[i];
                if (!runtime.programs[shader]) runtime.programs[shader] = compile_shader(shader);
            }
        } else KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=compile result=skipped reason=extensions rc=%08x\n", (unsigned)extensions);
        /* Match Hazuki/vitaShaRK's proven order: all outputs destroyed, release
           compiler caches, then remove extension hooks before module unload.
           The archive may also disable its hooks during ReleaseCompiler;
           our subsequent explicit cleanup is deliberately idempotent. */
        compiler.release();
        kv_shader_extensions_disable();
    }
    kv_shader_extensions_disable();
    memset(&compiler, 0, sizeof(compiler));
    const int stopped = sceKernelStopUnloadModule(module, 0, NULL, 0, NULL, NULL);
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=compiler_release uid=%08x rc=%08x\n", (unsigned)module, (unsigned)stopped);
    KV_DIAGNOSTIC_PRINTF("[knife-shader] stage=programs text_passthrough11=%d plain_sprite=%d plain_ps19=%d ps2=%d plain_forcealpha=%d rim_batched=%d ps19_group=%d ps11=%d ps13=%d ps19=%d generic=%d group=%d source=%.16s\n",
        runtime.programs[11] != NULL, runtime.programs[22] != NULL, runtime.programs[23] != NULL,
        runtime.programs[25] != NULL, runtime.programs[26] != NULL, runtime.programs[28] != NULL,
        runtime.programs[19] != NULL, runtime.programs[20] != NULL, runtime.programs[21] != NULL,
        runtime.programs[18] != NULL, runtime.programs[17] != NULL, runtime.programs[15] != NULL, KNIFE_RUNTIME_SOURCE_SHA256);
    return runtime_any();
}
#endif
const SceGxmProgram *kv_runtime_shader_program(uint32_t shader) {
    return runtime_shader_id(shader) ? runtime.programs[shader] : NULL;
}
int kv_runtime_shader_available(uint32_t shader) { return kv_runtime_shader_program(shader) != NULL; }
void kv_runtime_shader_shutdown(void) {
    for (unsigned i = 0; i < sizeof(runtime_shader_ids) / sizeof(runtime_shader_ids[0]); ++i)
        free(runtime.programs[runtime_shader_ids[i]]);
    memset(&runtime, 0, sizeof(runtime));
}
