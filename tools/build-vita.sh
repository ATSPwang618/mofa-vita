#!/usr/bin/env bash
# Cargo owns the final link; CMake only builds the native STATIC boundary.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export VITASDK="${VITASDK:-/opt/vitasdk}"
export PATH="$VITASDK/bin:${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
VITA_CACHE_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}"
export CARGO_TARGET_DIR="${KNIFE_VITA_CACHE:-${CARGO_TARGET_DIR:-$VITA_CACHE_ROOT/knife-vita}}"
export CARGO_TARGET_ARMV7_SONY_VITA_NEWLIBEABIHF_LINKER=arm-vita-eabi-gcc
TARGET=armv7-sony-vita-newlibeabihf
TOOLCHAIN=nightly-2026-05-28
MODE="${1:-platform}"
BUILD_KIND="${2:-dev}"
# Release variant: native540 by default; 270p remains a legacy build profile
# (960x540 assets, KNIF00540, separate data root and save namespace).
VARIANT="${KNIFE_VITA_VARIANT:-native540}"
if [[ "$VARIANT" != 270p && "$VARIANT" != native540 ]]; then
    echo "Unknown KNIFE_VITA_VARIANT: $VARIANT (expected 270p or native540)" >&2
    exit 2
fi
if [[ "$BUILD_KIND" != dev && "$BUILD_KIND" != release ]] || [[ "$BUILD_KIND" = release && "$MODE" != game ]]; then
    echo 'Usage: build-vita.sh [probe-mode] [dev] | build-vita.sh game release' >&2
    exit 2
fi
export KNIFE_VITA_BUILD_KIND="$BUILD_KIND"
# Release runs indefinitely and omits the synchronous diagnostic file writes.
# DEV remains bounded and writes to its separate save namespace.
if [[ "$BUILD_KIND" = release ]]; then
    export KNIFE_VITA_DEV_DIAGNOSTICS=0
    export KNIFE_VITA_QUIET="${KNIFE_VITA_QUIET:-1}"
    export KNIFE_VITA_SAVE_NAMESPACE=production
    unset KNIFE_VITA_DEV_MAX_FRAMES
else
    export KNIFE_VITA_DEV_DIAGNOSTICS=1
    export KNIFE_VITA_SAVE_NAMESPACE=dev
fi
case "$MODE" in
    platform) TITLE="Knife Vita DEV platform probe"; TITLE_ID=KNIF00002 ;;
    gxm) TITLE="Knife Vita DEV direct GXM probe"; TITLE_ID=KNIF00003 ;;
    image) TITLE="Knife Vita DEV PNG/JPEG image probe"; TITLE_ID=KNIF00004 ;;
    effects) TITLE="Knife Vita DEV offline effects probe"; TITLE_ID=KNIF00005 ;;
    audio) TITLE="Knife Vita DEV AudioOut probe"; TITLE_ID=KNIF00006 ;;
    pipeline) TITLE="Knife Vita DEV pipeline isolation"; TITLE_ID=KNIF00007 ;;
    text) TITLE="Knife Vita DEV original bilingual text"; TITLE_ID=KNIF00008 ;;
    audio-mixer) TITLE="Knife Vita DEV streaming mixer"; TITLE_ID=KNIF00009 ;;
    game) TITLE="Knife Vita DEV Witch native game"; TITLE_ID=KNIF00010 ;;
    movie) TITLE="Knife Vita DEV native MP4 playback"; TITLE_ID=KNIF00011 ;;
    compressed) TITLE="Knife Vita DEV compressed GXM textures"; TITLE_ID=KNIF00012 ;;
    ui-color) TITLE="Knife Vita DEV production UI color"; TITLE_ID=KNIF00013 ;;
    *) echo "Unknown Vita DEV build mode: $MODE" >&2; exit 2 ;;
esac
BINARY="knife-vita-$MODE-probe"
PACKAGE=knife-platform-vita
MANIFEST="$ROOT/knife/Cargo.toml"
FEATURE_ARGS=()
if [ "$MODE" = game ]; then
    BINARY=knife
    PACKAGE=knife
    FEATURE_ARGS=(--features vita-gxm)
    # Bounded ordinary Game/Ui execution with normal scope-based teardown.
    # This DEV package never claims to be the unrestricted final release.
    if [[ "$BUILD_KIND" = dev ]]; then
        export KNIFE_VITA_DEV_MAX_FRAMES="${KNIFE_VITA_DEV_MAX_FRAMES:-360}"
    fi
fi
if [ "$MODE" = text ]; then
    BINARY=vita-text-probe
    PACKAGE=knife-vita-text-probe
    MANIFEST="$ROOT/knife/probes/vita-text/Cargo.toml"
    export CARGO_TARGET_DIR="${KNIFE_VITA_CACHE:-$VITA_CACHE_ROOT/knife-vita-text}"
fi
if [ "$MODE" = audio-mixer ]; then
    BINARY=vita-audio-mixer-probe
    PACKAGE=knife-vita-audio-mixer-probe
    MANIFEST="$ROOT/knife/probes/vita-audio-mixer/Cargo.toml"
    export CARGO_TARGET_DIR="${KNIFE_VITA_CACHE:-$VITA_CACHE_ROOT/knife-vita-audio-mixer}"
fi
if [ "$MODE" = movie ]; then
    BINARY=vita-movie-probe
    PACKAGE=knife-vita-movie-probe
    MANIFEST="$ROOT/knife/probes/vita-movie/Cargo.toml"
    export CARGO_TARGET_DIR="${KNIFE_VITA_CACHE:-$VITA_CACHE_ROOT/knife-vita-movie}"
fi
OUTPUT="$ROOT/.work/vita/$MODE"
LOG="$ROOT/.work/logs/vita/$MODE-build.log"
VPK="knife-vita-$MODE-dev.vpk"
CHECK_ARGS=()
if [[ "$BUILD_KIND" = release ]]; then
    if [[ "$VARIANT" = native540 ]]; then
        OUTPUT="$ROOT/.work/vita/$MODE-release-540p"
        LOG="$ROOT/.work/logs/vita/$MODE-release-540p-build.log"
        TITLE='Witch on the Holy Night 540p'
        TITLE_ID=KNIF00540
        VPK=knife-vita-game-540p.vpk
        export KNIFE_VITA_SCENE_PIXELS=960,540
        export KNIFE_VITA_TITLE_ID=KNIF00540
    else
        OUTPUT="$ROOT/.work/vita/$MODE-release"
        LOG="$ROOT/.work/logs/vita/$MODE-release-build.log"
        TITLE='Witch on the Holy Night 270p'
        TITLE_ID=KNIF00270
        VPK=knife-vita-game-270p.vpk
    fi
    CHECK_ARGS=(--release)
fi
if [[ "$BUILD_KIND" = release && "$MODE" = game && "$VARIANT" = native540 ]]; then
    FEATURE_ARGS=(--features vita-gxm,assets-540p)
fi
# The native 540p variant always requests the enlarged MAIN budget (0x0C,
# +109 MiB). Override only for a deliberate A/B: KNIFE_VITA_ATTRIBUTE2=0|4|12.
if [[ -z "${KNIFE_VITA_ATTRIBUTE2:-}" ]]; then
    if [[ "$VARIANT" = native540 ]]; then export KNIFE_VITA_ATTRIBUTE2=12
    else export KNIFE_VITA_ATTRIBUTE2=0; fi
fi
export KNIFE_VITA_TITLE_ID="${KNIFE_VITA_TITLE_ID:-$TITLE_ID}"
# Baked into the perf log header so a hardware log always names its package.
export KNIFE_BUILD_TAG="${KNIFE_BUILD_TAG:-$VARIANT}"
# Validate the checked-in packaging input before compiling anything.
if [[ "$MODE" = game ]]; then
    export KNIFE_VITA_SHADER_COMPILER="$ROOT/knife/vita/vendor/shacccg/libshacccg.suprx"
    python3 - "$KNIFE_VITA_SHADER_COMPILER" <<'CHECK_COMPILER'
import hashlib, sys
from pathlib import Path
p = Path(sys.argv[1])
expected = "0c58b5d3e1cc47d9837b8c59ab6f3c299bfe7adf55a83f547d553f9fbb776552"
if not p.is_file() or p.stat().st_size != 1776523 or hashlib.sha256(p.read_bytes()).hexdigest() != expected:
    raise SystemExit("Missing or modified bundled compiler: " + str(p) + " (expected SHA-256 " + expected + ")")
CHECK_COMPILER
fi
for build_tool in rustc cargo arm-vita-eabi-gcc arm-vita-eabi-strip cmake ninja python3 vita-elf-create vita-make-fself vita-mksfoex vita-pack-vpk; do
    command -v "$build_tool" >/dev/null || { echo "Missing build tool: $build_tool (see doc/03-构建与工具链.md)" >&2; exit 1; }
done
NETWORK_ARGS=()
if [[ "${KNIFE_VITA_OFFLINE:-0}" = 1 ]]; then NETWORK_ARGS=(--offline); fi
# Apply to every crate, including build-std: panic locations and DWARF must not
# publish the builder's home, registry, SDK or checkout directories in the SELF.
export CARGO_ENCODED_RUSTFLAGS="$(python3 - "$ROOT" <<'REMAP_RUST_PATHS'
import os, sys
from pathlib import Path
root = Path(sys.argv[1]).absolute()
if "CARGO_ENCODED_RUSTFLAGS" in os.environ:
    flags = os.environ["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
else:
    flags = os.environ.get("RUSTFLAGS", "").split()
home = Path.home()
target = Path(os.environ["CARGO_TARGET_DIR"])
if not target.is_absolute():
    target = root / "knife" / target
paths = ((home, "/home/builder"),
         (Path(os.environ.get("CARGO_HOME", str(home / ".cargo"))), "/cargo"),
         (Path(os.environ.get("RUSTUP_HOME", str(home / ".rustup"))), "/rustup"),
         (Path(os.environ["VITASDK"]), "/vitasdk"),
         (root, "/source"), (target, "/build/target"))
flags = [flag for flag in flags if flag]
for prefix, replacement in paths:
    for spelling in dict.fromkeys((str(prefix.absolute()), str(prefix.resolve()))):
        flags.append("--remap-path-prefix=" + spelling + "=" + replacement)
print("\x1f".join(flags), end="")
REMAP_RUST_PATHS
)"
mkdir -p "$OUTPUT" "$(dirname "$LOG")"
exec > >(tee "$LOG") 2>&1
set -x
rustc +"$TOOLCHAIN" -vV
arm-vita-eabi-gcc --version
cmake --version
cd "$ROOT/knife"
cargo +"$TOOLCHAIN" rustc -Z build-std=std,panic_abort --locked ${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"} \
    --manifest-path "$MANIFEST" -p "$PACKAGE" --bin "$BINARY" ${FEATURE_ARGS[@]+"${FEATURE_ARGS[@]}"} --profile vita --target "$TARGET" -- \
    -C "link-arg=-Wl,-Map=$OUTPUT/$BINARY.map" \
    -C link-arg=-Wl,--undefined=_newlib_heap_size_user \
    -C link-arg=-Wl,--undefined=sceLibcHeapSize \
    -C link-arg=-Wl,--undefined=sceUserMainThreadStackSize \
    -C link-arg=-Wl,--undefined=sceUserMainThreadPriority \
    -C link-arg=-Wl,--defsym=__sce_headroom=0x10000
# The VitaSDK armvita linker script reserves this gap before the data segment.
# vita-elf-create appends SCE metadata to the code segment; plain alignment can
# leave fewer bytes than its metadata needs when the linked code grows.
cp "$CARGO_TARGET_DIR/$TARGET/vita/$BINARY.elf" "$OUTPUT/$BINARY.elf"
# SDK archives may predate path remapping. Drop inherited DWARF and sanitize
# only source-filename metadata before SELF signing; keep the Cargo ELF local.
arm-vita-eabi-strip --strip-debug "$OUTPUT/$BINARY.elf"
python3 "$ROOT/tools/sanitize-vita-elf.py" "$OUTPUT/$BINARY.elf"
arm-vita-eabi-readelf -h -A "$OUTPUT/$BINARY.elf" > "$OUTPUT/elf-identity.txt"
arm-vita-eabi-nm -u "$OUTPUT/$BINARY.elf" > "$OUTPUT/undefined-symbols.txt"
vita-elf-create "$OUTPUT/$BINARY.elf" "$OUTPUT/$BINARY.velf" > "$OUTPUT/elf-create.log" 2>&1
vita-make-fself -s "$OUTPUT/$BINARY.velf" "$OUTPUT/eboot.bin"
vita-mksfoex -s TITLE_ID="$TITLE_ID" -s APP_VER=00.01 -d ATTRIBUTE2="${KNIFE_VITA_ATTRIBUTE2:-0}" \
    "$TITLE" "$OUTPUT/param.sfo"
python3 - "$ROOT/knife/vita/vendor/shacccg-ext" "$OUTPUT/SceShaccCgExt-NOTICE.txt" <<'WRITE_NOTICE'
from pathlib import Path
import sys
source, output = map(Path, sys.argv[1:])
output.write_text((source / "NOTICE").read_text() + "\n" + (source / "LICENSE").read_text())
WRITE_NOTICE
PACKAGE_EXTRA=(-a "$OUTPUT/SceShaccCgExt-NOTICE.txt=sce_module/SceShaccCgExt-NOTICE.txt"
    -a "$ROOT/knife/vita/vendor/dependencies/licenses=licenses/native-dependencies")
if [[ "$MODE" = game ]]; then
    PACKAGE_EXTRA+=(-a "$KNIFE_VITA_SHADER_COMPILER=sce_module/libshacccg.suprx")
fi
vita-pack-vpk -s "$OUTPUT/param.sfo" -b "$OUTPUT/eboot.bin" \
    -a "$ROOT/knife/vita/sce_sys=sce_sys" -a "$ROOT/knife/LICENSE=LICENSE" \
    "${PACKAGE_EXTRA[@]}" "$OUTPUT/$VPK"
python3 "$ROOT/tools/check-vita-package.py" "$OUTPUT/$VPK" "$TITLE_ID" ${CHECK_ARGS[@]+"${CHECK_ARGS[@]}"}
python3 "$ROOT/tools/vita-build-receipt.py" "$ROOT" "$OUTPUT" "$TOOLCHAIN" "$TARGET" "$MODE"
# Every successful build also publishes the VPK into release/ at the project root, so the
# installable package always sits in one predictable place next to the sources.
RELEASE_DIR="$ROOT/release"
mkdir -p "$RELEASE_DIR"
cp -f "$OUTPUT/$VPK" "$RELEASE_DIR/$VPK"
python3 - "$RELEASE_DIR/$VPK" <<'PRINT_PACKAGE'
import hashlib, sys
from pathlib import Path
p = Path(sys.argv[1])
print("published", p, "(%s bytes)" % p.stat().st_size)
print(hashlib.sha256(p.read_bytes()).hexdigest(), p)
PRINT_PACKAGE
