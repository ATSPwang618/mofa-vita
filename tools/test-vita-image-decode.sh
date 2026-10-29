#!/usr/bin/env bash
# Host verification and ARM compilation use the exact same implementation.
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
build="$project/.work/vita/image-decode"
logs="$project/doc/logs/vita"
mkdir -p "$build" "$logs"

{
    cc --version | head -n 1
    pkg-config --modversion libpng libjpeg
    cc -std=c11 -Wall -Wextra -Werror -g -O1 \
        -fsanitize=address,undefined -fno-omit-frame-pointer \
        -I"$project/knife/vita" "$project/knife/vita/image_decode.c" "$project/knife/vita/jpeg_hw.c" \
        "$project/knife/vita/tests/image_decode_test.c" \
        $(pkg-config --cflags --libs libpng libjpeg) \
        -o "$build/test-image-decode"
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 \
        "$build/test-image-decode" "$build/fixtures"
    cc -std=c11 -Wall -Wextra -Werror -g -O1 \
        -fsanitize=address,undefined -fno-omit-frame-pointer \
        -Wl,--wrap=malloc,--wrap=calloc,--wrap=memalign,--wrap=free \
        -I"$project/knife/vita" "$project/knife/vita/image_decode.c" "$project/knife/vita/jpeg_hw.c" \
        "$project/knife/vita/tests/image_decode_allocation_test.c" \
        $(pkg-config --cflags --libs libpng libjpeg) \
        -o "$build/test-image-allocation"
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 \
        "$build/test-image-allocation" "$build/fixtures"
    cc -std=c11 -Wall -Wextra -Werror -g -O1 -DKV_JPEG_HW_MOCK=1 \
        -fsanitize=address,undefined -fno-omit-frame-pointer \
        -I"$project/knife/vita" "$project/knife/vita/image_decode.c" "$project/knife/vita/jpeg_hw.c" \
        "$project/knife/vita/tests/jpeg_hw_test.c" \
        $(pkg-config --cflags --libs libpng libjpeg) -o "$build/test-jpeg-hw"
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 \
        "$build/test-jpeg-hw" "$build/fixtures"
    KV_HW_TEST_INIT_FAILURE=1 ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 \
        "$build/test-jpeg-hw" "$build/fixtures"
} 2>&1 | tee "$logs/image-decode-host.log"

{
    /opt/vitasdk/bin/arm-vita-eabi-gcc --version | head -n 1
    /opt/vitasdk/bin/arm-vita-eabi-gcc -std=c11 -Wall -Wextra -Werror -O2 \
        -I"$project/knife/vita" -c "$project/knife/vita/image_decode.c" \
        -o "$build/image_decode.o"
    /opt/vitasdk/bin/arm-vita-eabi-gcc -std=c11 -Wall -Wextra -Werror -O2 \
        -I"$project/knife/vita" -c "$project/knife/vita/jpeg_hw.c" -o "$build/jpeg_hw.o"
    /opt/vitasdk/bin/arm-vita-eabi-readelf -h -A "$build/image_decode.o"
    /opt/vitasdk/bin/arm-vita-eabi-nm -u "$build/image_decode.o"
    # Force the complete object through the actual VitaSDK static libraries.
    cat > "$build/link-probe.c" <<'PROBE'
#include "image_decode.h"
int main(void) {
    KvDecodedImage image = {0};
    int result = kv_image_decode((const uint8_t *)"invalid", 7, &image);
    kv_image_free(&image);
    return result == KV_IMAGE_FORMAT ? 0 : 1;
}
PROBE
    /opt/vitasdk/bin/arm-vita-eabi-gcc -std=c11 -Wall -Wextra -Werror -O2 \
        -I"$project/knife/vita" "$build/link-probe.c" "$build/image_decode.o" "$build/jpeg_hw.o" \
        -lpng -ljpeg -lz -lm -lSceJpeg_stub -lSceSysmem_stub -Wl,-Map,"$build/link-probe.map" \
        -o "$build/link-probe.elf"
    /opt/vitasdk/bin/arm-vita-eabi-readelf -h "$build/link-probe.elf"
    grep 'PNG_LIBPNG_VER_STRING' /opt/vitasdk/arm-vita-eabi/include/png.h
    grep -E '^#define (JPEG_LIB_VERSION|LIBJPEG_TURBO_VERSION |BITS_IN_JSAMPLE )' \
        /opt/vitasdk/arm-vita-eabi/include/jconfig.h
    sha256sum "$build/image_decode.o" \
        "$build/jpeg_hw.o" \
        /opt/vitasdk/arm-vita-eabi/lib/libpng.a \
        /opt/vitasdk/arm-vita-eabi/lib/libjpeg.a \
        /opt/vitasdk/arm-vita-eabi/lib/libz.a
    printf '%s\n' 'RESULT PASS: Vita ARM C11 -Wall -Wextra -Werror compilation and static library link (not executed)'
} 2>&1 | tee "$logs/image-decode-arm.log"
