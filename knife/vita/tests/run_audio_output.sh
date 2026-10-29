#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/../.."
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
cc -std=c11 -Wall -Wextra -Werror -DKV_AUDIO_OUT_TEST \
  -fsanitize=address,undefined -fno-omit-frame-pointer -c vita/audio_output.c -o "$out/output.o"
cc -std=c11 -Wall -Wextra -Werror -fsanitize=address,undefined \
  -fno-omit-frame-pointer -c vita/tests/audio_output_mock.c -o "$out/mock.o"
cc -std=c11 -Wall -Wextra -Werror -I/usr/include/opus \
  -fsanitize=address,undefined -fno-omit-frame-pointer -c vita/audio_decode.c -o "$out/decode.o"
rustc --edition 2024 --test knife-platform-vita/src/audio_output.rs \
  -C "link-arg=$out/output.o" -C "link-arg=$out/mock.o" \
  -C link-arg=-fsanitize=address -l asan -l ubsan -o "$out/output_test"
ASAN_OPTIONS=detect_leaks=1 "$out/output_test" --test-threads=1
rustc --edition 2024 -A dead_code vita/tests/audio_output_host.rs \
  -C "link-arg=$out/decode.o" -C "link-arg=$out/output.o" -C "link-arg=$out/mock.o" \
  -C link-arg=-fsanitize=address -l asan -l ubsan -l opusfile -l opus -l ogg -l m -o "$out/full_probe"
ASAN_OPTIONS=detect_leaks=1 "$out/full_probe"
arm-vita-eabi-gcc -std=c11 -Wall -Wextra -Werror \
  -I/opt/vitasdk/arm-vita-eabi/include/opus \
  vita/audio_output.c vita/audio_decode.c vita/tests/audio_output_arm_link.c \
  -lSceAudio_stub -lopusfile -lopus -logg -lm -o "$out/output_arm.elf"
file "$out/output_arm.elf"
arm-vita-eabi-nm "$out/output_arm.elf" | grep ' T kv_audio_port_open$'
