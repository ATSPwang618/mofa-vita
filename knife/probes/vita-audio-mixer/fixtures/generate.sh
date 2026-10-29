#!/usr/bin/env bash
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cc -std=c11 -Wall -Wextra -Werror -fsanitize=address,undefined -g "$here/generate.c" -o "${TMPDIR:-/tmp}/knife-audio-mixer-generate-host" $(pkg-config --cflags --libs opus ogg) -lm
"${TMPDIR:-/tmp}/knife-audio-mixer-generate-host" "$here/mono.opus" "$here/stereo.opus"
sha256sum "$here/mono.opus" "$here/stereo.opus"
