#!/usr/bin/env bash
# Cargo owns the final link; CMake only builds the native STATIC boundary.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export VITASDK="${VITASDK:-/opt/vitasdk}"
export PATH="$VITASDK/bin:/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
export CARGO_TARGET_DIR="${KNIFE_VITA_CACHE:-/root/.cache/knife-vita}"
export CARGO_TARGET_ARMV7_SONY_VITA_NEWLIBEABIHF_LINKER=arm-vita-eabi-gcc
TARGET=armv7-sony-vita-newlibeabihf
TOOLCHAIN=nightly-2026-05-28
MODE="${1:-platform}"
BUILD_KIND="${2:-dev}"
# Release variant: 270p (default, half-size scene assets) or native540
# (960x540 assets, KNIF00540, separate data root and save namespace).
VARIANT="${KNIFE_VITA_VARIANT:-270p}"
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
    export CARGO_TARGET_DIR="${KNIFE_VITA_CACHE:-/root/.cache/knife-vita-text-agent}"
fi
if [ "$MODE" = audio-mixer ]; then
    BINARY=vita-audio-mixer-probe
    PACKAGE=knife-vita-audio-mixer-probe
    MANIFEST="$ROOT/knife/probes/vita-audio-mixer/Cargo.toml"
    export CARGO_TARGET_DIR="${KNIFE_VITA_CACHE:-/root/.cache/knife-vita-audio-mixer-agent}"
fi
if [ "$MODE" = movie ]; then
    BINARY=vita-movie-probe
    PACKAGE=knife-vita-movie-probe
    MANIFEST="$ROOT/knife/probes/vita-movie/Cargo.toml"
    export CARGO_TARGET_DIR="${KNIFE_VITA_CACHE:-/root/.cache/knife-vita-movie-agent}"
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
mkdir -p "$OUTPUT" "$(dirname "$LOG")"
exec > >(tee "$LOG") 2>&1
set -x
rustc +"$TOOLCHAIN" -vV
arm-vita-eabi-gcc --version
cmake --version
cd "$ROOT/knife"
cargo +"$TOOLCHAIN" rustc -Z build-std=std,panic_abort --locked --offline \
    --manifest-path "$MANIFEST" -p "$PACKAGE" --bin "$BINARY" "${FEATURE_ARGS[@]}" --profile vita --target "$TARGET" -- \
    -C "link-arg=-Wl,-Map=$OUTPUT/$BINARY.map" \
    -C link-arg=-Wl,--defsym=__sce_headroom=0x10000
# The VitaSDK armvita linker script reserves this gap before the data segment.
# vita-elf-create appends SCE metadata to the code segment; plain alignment can
# leave fewer bytes than its metadata needs when the linked code grows.
cp "$CARGO_TARGET_DIR/$TARGET/vita/$BINARY.elf" "$OUTPUT/$BINARY.elf"
arm-vita-eabi-readelf -h -A "$OUTPUT/$BINARY.elf" > "$OUTPUT/elf-identity.txt"
arm-vita-eabi-nm -u "$OUTPUT/$BINARY.elf" > "$OUTPUT/undefined-symbols.txt"
vita-elf-create "$OUTPUT/$BINARY.elf" "$OUTPUT/$BINARY.velf" > "$OUTPUT/elf-create.log" 2>&1
vita-make-fself -s "$OUTPUT/$BINARY.velf" "$OUTPUT/eboot.bin"
vita-mksfoex -s TITLE_ID="$TITLE_ID" -s APP_VER=00.01 -d ATTRIBUTE2="${KNIFE_VITA_ATTRIBUTE2:-0}" \
    "$TITLE" "$OUTPUT/param.sfo"
vita-pack-vpk -s "$OUTPUT/param.sfo" -b "$OUTPUT/eboot.bin" \
    -a "$ROOT/knife/vita/sce_sys=sce_sys" -a "$ROOT/knife/LICENSE=LICENSE" \
    "$OUTPUT/$VPK"
python3 "$ROOT/tools/check-vita-package.py" "$OUTPUT/$VPK" "$TITLE_ID" "${CHECK_ARGS[@]}"
python3 "$ROOT/tools/vita-build-receipt.py" "$ROOT" "$OUTPUT" "$TOOLCHAIN" "$TARGET" "$MODE"
# Keep doc/ for documentation only: the machine-readable receipt lives beside the artefacts.
receipt="$ROOT/doc/vita-$MODE-release-540p-build.json"
if [[ "$VARIANT" != native540 ]]; then receipt="$ROOT/doc/vita-$MODE-release-build.json"; fi
if [[ -f "$receipt" ]]; then mv -f "$receipt" "$OUTPUT/"; fi
# Every successful build also publishes the VPK into release/ at the project root, so the
# installable package always sits in one predictable place next to the sources.
RELEASE_DIR="$ROOT/release"
mkdir -p "$RELEASE_DIR"
cp -f "$OUTPUT/$VPK" "$RELEASE_DIR/$VPK"
printf 'published %s -> %s (%s bytes)\n' "$VPK" "$RELEASE_DIR/$VPK" "$(stat -c %s "$RELEASE_DIR/$VPK")"
sha256sum "$RELEASE_DIR/$VPK"
