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
    /* Generic17 retains its required-tex0 contract. Full-source op7 additionally
       requires tex2 and soft_lut so a truncated material source cannot admit. */
    for (unsigned bits = 1; bits < 16; bits += 4) {
        fixture(data, bits);
        EXPECT(validate(data, sizeof(data), 17) == 1);
        EXPECT(validate(data, sizeof(data), 29) == (bits == 13));
    }
    fixture(data, 13); EXPECT(validate(data, sizeof(data), 29) == 1);
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
int main(void) {
    (void)knife_runtime_effects_source;
    puts("HOST-ONLY production runtime shader metadata/cache; synthetic GXP, no shader execution.");
    metadata(); cache_identity();
    printf("RESULT %s: %u cases, %u checks, %u failures\n", failures ? "FAIL" : "PASS", cases, checks, failures);
    return failures ? 1 : 0;
}
