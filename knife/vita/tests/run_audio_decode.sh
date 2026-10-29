#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
cc -std=c11 -Wall -Wextra -Werror -I/usr/include/opus \
  -fsanitize=address,undefined -fno-omit-frame-pointer \
  vita/audio_decode.c vita/tests/audio_decode_test.c \
  -Wl,--wrap=calloc -Wl,--wrap=malloc -lopusfile -lopus -logg -lm -o "$out/audio_test"
ASAN_OPTIONS=detect_leaks=1 "$out/audio_test" "$out/mono.opus" "$out/stereo.opus"
cc -std=c11 -Wall -Wextra -Werror -I/usr/include/opus -c vita/audio_decode.c -o "$out/audio_ffi.o"
rustc --edition 2024 --test knife-platform-vita/src/audio_decode.rs \
  -C "link-arg=$out/audio_ffi.o" -l opusfile -l opus -l ogg -l m -o "$out/rust_test"
KNIFE_AUDIO_FIXTURE_MONO="$out/mono.opus" KNIFE_AUDIO_FIXTURE_STEREO="$out/stereo.opus" "$out/rust_test"
arm-vita-eabi-gcc -std=c11 -Wall -Wextra -Werror \
  -I/opt/vitasdk/arm-vita-eabi/include/opus \
  vita/audio_decode.c vita/tests/audio_decode_test.c \
  -Wl,--wrap=calloc -Wl,--wrap=malloc -lopusfile -lopus -logg -lm -o "$out/audio_arm.elf"
file "$out/audio_arm.elf"
arm-vita-eabi-nm "$out/audio_arm.elf" | grep ' T kv_audio_open_file$'
