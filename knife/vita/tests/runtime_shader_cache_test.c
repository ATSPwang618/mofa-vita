#define KV_SHADER_CACHE_HOST_TEST 1
#ifndef KV_RUNTIME_SHADER_SOURCE
#define KV_RUNTIME_SHADER_SOURCE "../runtime_shaders.c"
#endif
#include KV_RUNTIME_SHADER_SOURCE
#include <sys/stat.h>
#include <unistd.h>

static unsigned cases, checks, failures;
static int program_check_status;
int sceGxmProgramCheck(const SceGxmProgram *program) { (void)program; return program_check_status; }
int sceIoMkdir(const char *path, int mode) {
    if (!strcmp(path, "ux0:data/WitchontheHolyNight")) return 0;
    return mkdir(path, (mode_t)mode);
}
static void expect(int condition, const char *expression, int line) {
    ++checks;
    if (!condition) { ++failures; fprintf(stderr, "FAIL case %u line %d: %s\n", cases, line, expression); }
}
#define EXPECT(value) expect(!!(value), #value, __LINE__)
static void put(unsigned char *data, unsigned offset, uint32_t value) {
    for (unsigned i = 0; i < 4; ++i) data[offset+i] = (unsigned char)(value >> (8*i));
}
/* Hand-authored structural GXP fixture, not executable GPU bytecode. */
static void fixture(unsigned char data[512], unsigned sampler_bits) {
    memset(data, 0, 512);
    put(data, 0, 0x00505847); put(data, 8, 512); put(data, 20, 0xc1);
    put(data, 40, 160-40); put(data, 44, 112-44);
    data[122] = 7; data[123] = 4;
    put(data, 60, 1); put(data, 64, 144-64);
    unsigned count = 0, name_at = 256;
    const char *const names[] = {"tex0", "tex1", "tex2", "soft_lut"};
    for (unsigned unit = 0; unit < 4; ++unit) if (sampler_bits & (1U << unit)) {
        const unsigned at = 160 + count++ * 16;
        put(data, at, name_at-at); data[at+4] = 2;
        put(data, at+8, 1); put(data, at+12, unit);
        strcpy((char *)data+name_at, names[unit]); name_at += (unsigned)strlen(names[unit])+1;
    }
    put(data, 36, count);
}
static void metadata(void) {
    unsigned char data[512];
    ++cases;
    EXPECT(runtime_shader_id(29));
    const char *entry = entry_for(29);
    EXPECT(entry && !strcmp(entry, "fs_ps7_fetch"));
    EXPECT(fetch_shader(29));
    EXPECT(runtime_shader_id(30));
    EXPECT(!strcmp(entry_for(30), "fs_ps1_plain_fetch"));
    EXPECT(runtime_shader_id(31));
    EXPECT(!strcmp(entry_for(31), "fs_ps6_plain_fetch"));
    EXPECT(fetch_shader(30) && fetch_shader(31));
    /* Plain OP1/OP6 candidates follow their independent option bit.  A
       baseline profile must not read or compile their cache entries, and a
       Half/Blur bit must not accidentally enable them. */
    EXPECT(!runtime_shader_enabled(30, 0));
    EXPECT(!runtime_shader_enabled(31, 0));
    EXPECT(!runtime_shader_enabled(30, UINT64_C(1) << 32));
    EXPECT(!runtime_shader_enabled(31, UINT64_C(1) << 34));
    EXPECT(runtime_shader_enabled(30, UINT64_C(1) << 30));
    EXPECT(runtime_shader_enabled(31, UINT64_C(1) << 30));
    const char *const full_entries[]={"fs_ps1_full_fetch","fs_ps6_full_fetch","fs_ps13_full_fetch"};
    for(unsigned shader=54;shader<=56;++shader) {
        EXPECT(runtime_shader_id(shader));
        const char *full_entry=entry_for(shader);
        EXPECT(full_entry && !strcmp(full_entry,full_entries[shader-54]));
        EXPECT(fetch_shader(shader));
        EXPECT(!runtime_shader_enabled(shader,0));
        for(unsigned bit=0;bit<64;++bit)
            EXPECT(runtime_shader_enabled(shader,UINT64_C(1)<<bit)==(bit==37));
        for(unsigned bits=0;bits<16;++bits) {
            fixture(data,bits);
            EXPECT(validate(data,sizeof(data),shader)==(bits==13));
        }
        fixture(data,13); data[20]=1; EXPECT(!validate(data,sizeof(data),shader));
        fixture(data,13); data[122]=1; EXPECT(!validate(data,sizeof(data),shader));
        fixture(data,13); data[123]=3; EXPECT(!validate(data,sizeof(data),shader));
        fixture(data,13); put(data,160+16+12,0); EXPECT(!validate(data,sizeof(data),shader));
    }
    const char *const candidates[] = { "fs_ps1_plain_half_fetch", "fs_ps6_plain_half_fetch",
        "fs_ps7_plain_half_fetch", "fs_ps13_plain_half_fetch", "fs_blur_weights" };
    for (unsigned shader = 49; shader <= 53; ++shader) {
        EXPECT(runtime_shader_id(shader));
        const char *candidate = entry_for(shader);
        EXPECT(candidate && !strcmp(candidate, candidates[shader-49]));
        EXPECT(fetch_shader(shader) == (shader < 53));
    }
    for (unsigned shader = 49; shader <= 52; ++shader) {
        EXPECT(!runtime_shader_enabled(shader, 0));
        EXPECT(!runtime_shader_enabled(shader, UINT64_C(1) << 34));
        EXPECT(runtime_shader_enabled(shader, UINT64_C(1) << 32));
        fixture(data, 1); EXPECT(validate(data, sizeof(data), shader));
        fixture(data, 5); EXPECT(!validate(data, sizeof(data), shader));
    }
    EXPECT(!runtime_shader_enabled(53, 0));
    EXPECT(!runtime_shader_enabled(53, UINT64_C(1) << 32));
    EXPECT(runtime_shader_enabled(53, UINT64_C(1) << 34));
    /* Generic17 retains its required-tex0 contract. Full-source op7 additionally
       requires tex2 and soft_lut so a truncated material source cannot admit. */
    for (unsigned bits = 1; bits < 16; bits += 4) {
        fixture(data, bits);
        EXPECT(validate(data, sizeof(data), 17) == 1);
        EXPECT(validate(data, sizeof(data), 29) == (bits == 13));
    }
    fixture(data, 13); EXPECT(validate(data, sizeof(data), 29) == 1);
    fixture(data, 1); EXPECT(validate(data, sizeof(data), 30) == 1);
    fixture(data, 1); EXPECT(validate(data, sizeof(data), 31) == 1);
    fixture(data, 5); EXPECT(validate(data, sizeof(data), 29) == 0);
    EXPECT(!strcmp(validation_reason, "ps7_requires_full_source_samplers"));
    fixture(data, 9); EXPECT(validate(data, sizeof(data), 29) == 0);
    EXPECT(!strcmp(validation_reason, "ps7_requires_full_source_samplers"));
    fixture(data, 15); EXPECT(validate(data, sizeof(data), 29) == 0);
    EXPECT(!strcmp(validation_reason, "fetch_must_not_sample_backdrop"));
    fixture(data, 12); EXPECT(validate(data, sizeof(data), 29) == 0);
    EXPECT(!strcmp(validation_reason, "required_sampler_missing"));
    fixture(data, 13); data[20] = 1; EXPECT(validate(data, sizeof(data), 29) == 0);
    EXPECT(!strcmp(validation_reason, "native_color_read_flags"));
    fixture(data, 13); data[122] = 1; EXPECT(validate(data, sizeof(data), 29) == 0);
    fixture(data, 13); data[123] = 3; EXPECT(validate(data, sizeof(data), 29) == 0);
    fixture(data, 13); put(data, 160+16+12, 0); EXPECT(validate(data, sizeof(data), 29) == 0);
    fixture(data, 13); data[261] = 'X'; EXPECT(validate(data, sizeof(data), 29) == 0);
    fixture(data, 13); program_check_status = -1; EXPECT(validate(data, sizeof(data), 29) == 0);
    EXPECT(!strcmp(validation_reason, "sceGxmProgramCheck")); program_check_status = 0;
}
static void blur_array_metadata(void) {
    unsigned char data[512];
    ++cases;
    fixture(data, 1);
    data[20] = 1; data[122] = 1;
    EXPECT(!validate(data, sizeof(data), 53));
    EXPECT(!strcmp(validation_reason, "blur_weight_array_missing"));
    /* One exact scalar[33] F32 uniform is permitted only on entry 53. */
    put(data, 36, 2); put(data, 100, 33);
    put(data, 176, 320-176); data[180] = 1; data[181] = 0xe1;
    put(data, 184, 33); put(data, 188, 0); strcpy((char *)data+320, "uBlurWeights");
    EXPECT(validate(data, sizeof(data), 53));
    EXPECT(!validate(data, sizeof(data), 22));
    for (unsigned shader = 49; shader <= 52; ++shader) {
        data[20] = 0xc1; data[122] = 7;
        EXPECT(!validate(data, sizeof(data), shader));
    }
    data[20] = 1; data[122] = 1;
    for (unsigned size = 0; size <= 34; ++size) if (size != 33) {
        put(data, 184, size); EXPECT(!validate(data, sizeof(data), 53));
    }
    put(data, 184, 33); put(data, 100, 32); EXPECT(!validate(data, sizeof(data), 53));
    put(data, 100, 33); data[181] = 0xe2; EXPECT(!validate(data, sizeof(data), 53));
    data[181] = 0xe1; data[180] = 0x11; EXPECT(!validate(data, sizeof(data), 53));
}
static void all_operation_metadata(void) {
    unsigned char data[512];
    for(unsigned shader=64;shader<=87;++shader) {
        ++cases;
        EXPECT(runtime_shader_id(shader));
        const char *entry=entry_for(shader);
        EXPECT(entry && !strcmp(entry,"fs_ps_operation_fetch"));
        EXPECT(fetch_shader(shader));
        EXPECT(runtime_entry_abi(shader)==(KNIFE_RUNTIME_ENTRY_ABI|((shader-63)<<24)));
        const char *defines[4]; char macro_values[2][32], expected[32];
        EXPECT(runtime_macro_definitions(shader,defines,macro_values)==3);
        EXPECT(!strcmp(defines[0],"KNIFE_RUNTIME_NATIVE_FETCH=1"));
        EXPECT(!strcmp(defines[1],"KNIFE_RUNTIME_PS_SPECIALIZED=1"));
        snprintf(expected,sizeof(expected),"KNIFE_PS_FULL_OP=%u",shader-63);
        EXPECT(defines[2] && !strcmp(defines[2],expected) && !defines[3]);
        EXPECT(!runtime_shader_enabled(shader,0));
        for(unsigned bit=0;bit<64;++bit)
            EXPECT(runtime_shader_enabled(shader,UINT64_C(1)<<bit)==(bit==38));
        for(unsigned bits=0;bits<16;++bits) {
            fixture(data,bits);
            if(shader==81) data[20]=0x41; /* real OP18 compiler output reads FRAGCOLOR but needs no native-output bit */
            EXPECT(validate(data,sizeof(data),shader)==(bits==(shader==81?9u:13u)));
        }
        fixture(data,13); data[20]=1; EXPECT(!validate(data,sizeof(data),shader));
        fixture(data,13); data[122]=1; EXPECT(!validate(data,sizeof(data),shader));
        fixture(data,13); data[123]=3; EXPECT(!validate(data,sizeof(data),shader));
    }
    for(unsigned shader=58;shader<=63;++shader) EXPECT(!runtime_shader_id(shader));
    for(unsigned bits=0;bits<16;++bits) {
        fixture(data,bits);
        EXPECT(validate(data,sizeof(data),48)==(bits==1));
        EXPECT(validate(data,sizeof(data),57)==(bits==5));
    }
    EXPECT(runtime_shader_id(48) && runtime_shader_id(57));
    EXPECT(!strcmp(entry_for(48),"fs_ps7_plain_fetch") && !strcmp(entry_for(57),"fs_ps_chain2_fetch"));
    EXPECT(!runtime_shader_enabled(48,0) && runtime_shader_enabled(48,UINT64_C(1)<<39));
    EXPECT(!runtime_shader_enabled(57,UINT64_C(1)<<39) && !runtime_shader_enabled(57,UINT64_C(1)<<40));
    EXPECT(runtime_shader_enabled(57,UINT64_C(3)<<39));
    EXPECT(runtime_entry_abi(57)==(KNIFE_RUNTIME_ENTRY_ABI|7U<<8|7U<<16));
    EXPECT(!runtime_shader_id(88) && !runtime_shader_id(127) && !runtime_shader_id(UINT32_MAX));
    const char *defines[4]; char macro_values[2][32];
    EXPECT(runtime_macro_definitions(17,defines,macro_values)==1);
    EXPECT(runtime_macro_definitions(29,defines,macro_values)==2);
    EXPECT(runtime_macro_definitions(48,defines,macro_values)==2);
    EXPECT(runtime_macro_definitions(57,defines,macro_values)==4
        && !strcmp(defines[2],"KNIFE_CHAIN_OP_A=7") && !strcmp(defines[3],"KNIFE_CHAIN_OP_B=7"));
    EXPECT(runtime_macro_definitions(49,defines,macro_values)==3 && !strcmp(defines[2],"KNIFE_RUNTIME_HALF_COLOR=1"));
    EXPECT(runtime_macro_definitions(53,defines,macro_values)==1 && !strcmp(defines[0],"KNIFE_RUNTIME_BLUR_WEIGHTS=1"));
    EXPECT(runtime_macro_definitions(32,defines,macro_values)==4
        && !strcmp(defines[2],"KNIFE_CHAIN_OP_A=2") && !strcmp(defines[3],"KNIFE_CHAIN_OP_B=2"));
}
static int replace_header(uint32_t shader, unsigned fault) {
    char path[320]; path_for(path, sizeof(path), shader);
    FILE *file = fopen(path, "r+b");
    if (!file) return 0;
    CacheHeader header;
    int ok = fread(&header, 1, sizeof(header), file) == sizeof(header);
    if (fault == 0) header.shader = 17;
    if (fault == 1) ++header.entry_abi;
    if (fault == 2) header.source_sha256[0] ^= 1;
    if (fault == 3) header.compiler_sha256[0] ^= 1;
    if (fault == 4) ++header.compiler_policy;
    if (fault == 5) header.checksum ^= 1;
    rewind(file); ok = ok && fwrite(&header, 1, sizeof(header), file) == sizeof(header);
    return fclose(file) == 0 && ok;
}
static void cache_identity(void) {
    unsigned char data[512]; fixture(data, 13);
    char path29[320], path17[320];
    path_for(path29, sizeof(path29), 29); path_for(path17, sizeof(path17), 17);
    ++cases; EXPECT(strcmp(path29, path17) != 0);
    write_cache(29, data, sizeof(data));
    SceGxmProgram *loaded = read_cache(29);
    EXPECT(loaded && !memcmp(loaded, data, sizeof(data))); free(loaded);
    EXPECT(read_cache(17) == NULL);
    for (unsigned fault = 0; fault < 6; ++fault) {
        ++cases; write_cache(29, data, sizeof(data)); EXPECT(replace_header(29, fault));
        loaded = read_cache(29); EXPECT(loaded == NULL); free(loaded);
        EXPECT(access(path29, F_OK) != 0);
    }
    ++cases; write_cache(29, data, sizeof(data));
    FILE *file = fopen(path29, "r+b"); EXPECT(file != NULL);
    if (file) { EXPECT(fseek(file, sizeof(CacheHeader)+20, SEEK_SET) == 0); EXPECT(fputc(1, file) != EOF); EXPECT(fclose(file) == 0); }
    loaded = read_cache(29); EXPECT(loaded == NULL); free(loaded);
    EXPECT(access(path29, F_OK) != 0);
    ++cases; write_cache(29, data, sizeof(data));
    EXPECT(rename(path29, path17) == 0); loaded = read_cache(17); EXPECT(loaded == NULL); free(loaded);
    EXPECT(access(path17, F_OK) != 0);
}
static void full_cache_identity(void) {
    unsigned char data[512]; fixture(data,13);
    char previous[320]; path_for(previous,sizeof(previous),29);
    for(unsigned shader=54;shader<=56;++shader) {
        ++cases;
        char current[320]; path_for(current,sizeof(current),shader);
        EXPECT(strcmp(current,previous)!=0);
        write_cache(shader,data,sizeof(data));
        runtime.programs[shader]=read_cache(shader);
        EXPECT(kv_runtime_shader_available(shader));
        EXPECT(runtime.programs[shader] && !memcmp(runtime.programs[shader],data,sizeof(data)));
        strcpy(previous,current);
        /* A header naming another operation cannot reuse this blob. */
        EXPECT(replace_header(shader,0));
        SceGxmProgram *rejected=read_cache(shader); EXPECT(!rejected); free(rejected);
        EXPECT(access(current,F_OK)!=0);
    }
    kv_runtime_shader_shutdown();
    for(unsigned shader=54;shader<=56;++shader) EXPECT(!kv_runtime_shader_available(shader));
}
int main(void) {
    (void)knife_runtime_effects_source;
    puts("HOST-ONLY production runtime shader metadata/cache; synthetic GXP, no shader execution.");
    metadata(); blur_array_metadata(); all_operation_metadata(); cache_identity(); full_cache_identity();
    printf("RESULT %s: %u cases, %u checks, %u failures\n", failures ? "FAIL" : "PASS", cases, checks, failures);
    return failures ? 1 : 0;
}
