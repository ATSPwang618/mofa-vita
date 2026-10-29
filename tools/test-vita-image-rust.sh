#!/usr/bin/env bash
# Execute the Vita-only Rust ownership boundary against the exact C decoder on
# the host. This standalone harness does not add host codecs to the Cargo crate.
set -euo pipefail
export PATH=/root/.cargo/bin:/usr/bin:/bin
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
build="$root/.work/vita/image-rust"
log="$root/doc/logs/vita/${KNIFE_IMAGE_RUST_LOG:-image-rust-host.log}"
mkdir -p "$build" "$(dirname "$log")"
exec > >(tee "$log") 2>&1
set -x
cc --version | head -n 1
pkg-config --modversion libpng libjpeg
cc -std=c11 -Wall -Wextra -Werror -g -O1 -I"$root/knife/vita" \
    $(pkg-config --cflags libpng libjpeg) -c "$root/knife/vita/image_decode.c" \
    -o "$build/image_decode.o"
cc -std=c11 -Wall -Wextra -Werror -g -O1 -I"$root/knife/vita" \
    -c "$root/knife/vita/tests/image_decode_rust_support.c" -o "$build/image_support.o"
cc -std=c11 -Wall -Wextra -Werror -g -O1 -I"$root/knife/vita" \
    -c "$root/knife/vita/jpeg_hw.c" -o "$build/jpeg_hw.o"
ar rcs "$build/libknife_image_decoder.a" "$build/image_decode.o" "$build/image_support.o" "$build/jpeg_hw.o"
rustc +1.99.0 --edition=2024 --test "$root/knife/vita/tests/image_decode_rust.rs" \
    -L "native=$build" -l static=knife_image_decoder -l png -l jpeg -l z -l m \
    -C link-arg=-Wl,--wrap=kv_image_free -o "$build/test-image-rust"
"$build/test-image-rust" --nocapture --test-threads=1
