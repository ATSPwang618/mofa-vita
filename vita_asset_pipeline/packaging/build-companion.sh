#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
export PATH="/root/.cargo/bin:/opt/vitasdk/bin:/usr/local/bin:/usr/bin:/bin"
export KNIFE_BUILD_TAG="${KNIFE_BUILD_TAG:-asset-pipeline-main70-20261118}"
export KNIFE_VITA_OPTIMIZATIONS='hardware-test,viewport-reject,group-mask-reject,ps-normal-multiply,live-alpha-coverage,cold-cohort,save-bgm-focus,shader-cap-batch,transition-textures,group-mask-clip,coverage-materials,group-opaque-mask,stage-opaque-cover,group-opaque-overwrite,transparent-draw-reject,stage-alpha-region'
export KNIFE_PERF_CONSOLE=1
export KNIFE_VITA_QUIET=0
export KNIFE_VITA_OFFLINE=1
export KNIFE_VITA_BUILD_KIND=release
# Compile, link and sign with the maintained main-branch path. Package the
# resulting engine without embedding a Sony runtime compiler module.
awk -v project="$ROOT" '/^vita-mksfoex / {exit} /^ROOT=/ {print "ROOT=\"" project "\"";next} {print}' tools/build-vita.sh | bash -s -- game release
OUTPUT="$ROOT/.work/vita/game-release-540p"
vita-mksfoex -s TITLE_ID=KNIF00540 -s APP_VER=00.01 -d ATTRIBUTE2=12 \
    'Witch on the Holy Night' "$OUTPUT/param.sfo"
python3 vita_asset_pipeline/packaging/finish_companion.py
python3 tools/vita-build-receipt.py "$ROOT" "$OUTPUT" nightly-2026-05-28 armv7-sony-vita-newlibeabihf game
