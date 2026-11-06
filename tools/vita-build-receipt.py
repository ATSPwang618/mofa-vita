"""Record actual built artifacts and selected tool identities (never runtime proof)."""
import datetime
import hashlib
import json
import os
import re
import struct
import subprocess
import sys
import zipfile
from pathlib import Path

root, output = map(Path, sys.argv[1:3])
toolchain, target = sys.argv[3:5]
mode = sys.argv[5] if len(sys.argv) > 5 else "platform"
release = os.environ.get("KNIFE_VITA_BUILD_KIND") == "release"
if mode not in ("platform", "gxm", "image", "effects", "audio", "pipeline", "text", "audio-mixer", "game", "movie", "compressed", "ui-color"):
    raise ValueError("unsupported DEV build mode")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def sfo_attribute2(path):
    """Decode ATTRIBUTE2 from a param.sfo so a receipt never guesses the memory mode."""
    data = path.read_bytes()
    magic, _version, key_table, data_table, count = struct.unpack_from("<IIIII", data, 0)
    if magic != 0x46535000:
        return None
    for i in range(count):
        key_offset, _fmt, _len, _max, _offset = struct.unpack_from("<HHIII", data, 20 + i * 16)
        start = key_table + key_offset
        end = data.find(b"\x00", start)
        if end < 0:
            continue
        if data[start:end] != b"ATTRIBUTE2":
            continue
        return struct.unpack_from("<I", data, data_table + _offset)[0]
    return None


artifacts = [{"path": str(p.relative_to(root)), "bytes": p.stat().st_size, "sha256": sha256(p)}
             for p in sorted(output.iterdir()) if p.is_file() and not p.name.endswith("-build.json")]
package_contents = []
for path in sorted(output.glob("*.vpk")):
    with zipfile.ZipFile(path) as package:
        package_contents.append({
            "path": path.relative_to(root).as_posix(),
            "files": [{"path": entry.filename, "bytes": entry.file_size,
                       "sha256": hashlib.sha256(package.read(entry)).hexdigest()}
                      for entry in sorted(package.infolist(), key=lambda entry: entry.filename)
                      if not entry.is_dir()],
        })
tools = {}
for name in ["arm-vita-eabi-gcc", "arm-vita-eabi-strip", "vita-elf-create", "vita-make-fself", "vita-mksfoex", "vita-pack-vpk"]:
    path = Path(subprocess.check_output(["which", name], text=True).strip()).resolve()
    tools[name] = {"path": str(path), "sha256": sha256(path)}
sources = []
for base in (root / 'knife/knife', root / 'knife/knife-render', root / 'knife/knife-core',
             root / 'knife/knife-audio', root / 'knife/knife-render-api', root / 'knife/knife-perf',
             root / 'knife/knife-platform-vita', root / 'knife/vita'):
    sources.extend(p for p in base.rglob('*') if p.is_file()
                   and (p.name == 'CMakeLists.txt' or p.suffix in ('.rs', '.c', '.h', '.toml', '.cg', '.gxp', '.gxpinc', '.sh', '.def', '.inc'))
                   and not any(part in ('target', '.work', '.git') for part in p.relative_to(base).parts))
# Embedded tables and filenames affect the binary identity too.
sources.extend(p for p in (root / "knife/knife/data").rglob("*") if p.is_file())
# PNG/XML files affect the VPK identity even when no Rust or C source changes.
sources.extend(p for p in (root / 'knife/vita/sce_sys').rglob('*') if p.is_file())
sources += [root / 'knife/Cargo.toml', root / 'knife/Cargo.lock',
            root / 'knife/rust-toolchain.toml', root / 'tools/build-vita.py',
            root / 'tools/build-vita.sh', root / 'tools/vita-build-receipt.py', root / 'tools/check-vita-package.py',
            root / 'tools/sanitize-vita-elf.py',
            root / 'tools/generate-runtime-shaders.py']
# Asset regeneration is independent of compiling the checked-in metadata. Its
# tools are recorded when present, never required as hidden packaging inputs.
sources.extend(root / 'tools' / name for name in (
    'offline_vita_shaders.py', 'generate-ps-operation-shaders.py', 'generate-vita-stream-index.py', 'generate-vita-atlas-index.py', 'vita_image_metadata.py',
    'migrate-vita-native-color.py', 'native-color-blocks.c', 'generate_vita_asset_profile_540.py',
    'prepare_vita_ui_ranges.py',
) if (root / 'tools' / name).is_file())
sources.extend(p for p in (root / "knife/vita/vendor").rglob("*") if p.is_file())
source_files = [{"path": p.relative_to(root).as_posix(), "bytes": p.stat().st_size, "sha256": sha256(p)}
                for p in sorted(set(sources))]
cache_abi = int(re.search(r"CACHE_ABI = ([0-9]+)", (root / "knife/vita/runtime_shaders.c").read_text()).group(1))
compiler_policy = int(re.search(r"KV_SHADER_COMPILER_POLICY ([0-9]+)", (root / "knife/vita/shader_extensions.h").read_text()).group(1))
optimization_source = (root / "knife/knife-perf/src/optimizations.rs").read_text()
optimization_bits = {
    name.lower().replace("_", "-"): int(bit)
    for name, bit in re.findall(r"pub const ([A-Z0-9_]+)\s*:\s*u64\s*=\s*1u64\s*<<\s*([0-9]+)", optimization_source)
}
optimization_values = {name.upper().replace("-", "_"): 1 << bit
                       for name, bit in optimization_bits.items()}
# Derive both presets from the Rust definitions instead of maintaining a second
# feature list in the receipt. Accept only these symbolic OR expressions.
for name in ("IMPLEMENTED", "HARDWARE_TEST"):
    expression = re.search(r"pub const " + name + r"\s*:\s*u64\s*=\s*(.*?);",
                           optimization_source, re.DOTALL).group(1)
    value = 0
    for term in re.sub(r"//[^\n]*", "", expression).split("|"):
        value |= optimization_values[term.strip()]
    optimization_values[name] = value
hardware_test_profile = re.search(r'pub const HARDWARE_TEST_PROFILE: &str = "([^"]+)"',
                                 optimization_source).group(1)
features_source = re.search(r"pub const FEATURES:.*?=\s*&\[(.*?)\];", optimization_source, re.DOTALL).group(1)
optimization_features = [(name, optimization_values[symbol]) for name, symbol in
                         re.findall(r'\("([a-z0-9-]+)",\s*([A-Z][A-Z0-9_]+)\)', features_source)]
feature_values = dict(optimization_features)
default_profile = os.environ.get("KNIFE_VITA_OPTIMIZATIONS", hardware_test_profile)
default_mask = 0
for token in filter(None, re.split(r"[\s,]+", default_profile)):
    if token == "baseline":
        default_mask = 0
    elif token == "all":
        default_mask = optimization_values["IMPLEMENTED"]
    elif token == hardware_test_profile:
        default_mask = optimization_values["HARDWARE_TEST"]
    else:
        remove = token.startswith("-")
        name = token[1:] if remove else token
        if name not in feature_values:
            raise ValueError("unknown optimization " + name)
        default_mask = default_mask & ~feature_values[name] if remove else default_mask | feature_values[name]
if default_mask & feature_values["rt-scenes-2"] and default_mask & feature_values["rt-scenes-4"]:
    raise ValueError("rt-scenes-2 and rt-scenes-4 are mutually exclusive")
if default_mask & (feature_values["async-submit"] | feature_values["compact-stream"]) and not default_mask & feature_values["batch-arena"]:
    raise ValueError("async-submit and compact-stream require batch-arena")
if default_mask & (feature_values["dependency-graph"] | feature_values["source-rename"]) and (
        default_mask & (feature_values["batch-arena"] | feature_values["async-submit"]) !=
        feature_values["batch-arena"] | feature_values["async-submit"]):
    raise ValueError("dependency-graph and source-rename require batch-arena and async-submit")
offline_header = (root / "knife/vita/shaders/generated/offline_programs.h").read_text(encoding="utf-8")
offline_programs = [
    {"shader": int(shader), "sha256": digest}
    for shader, digest in re.findall(r"/\* shader ([0-9]+) SHA256 ([a-f0-9]{64}) \*/", offline_header)
]
offline_source = re.search(r'KNIFE_OFFLINE_SOURCE_SHA256 "([a-f0-9]{64})"', offline_header).group(1)
assert offline_source == sha256(root / "knife/vita/shaders/effects.cg"), "stale offline source"
assert set(range(64, 88)).issubset({p["shader"] for p in offline_programs}), "missing offline operation"
receipt = {"built_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
           "target": target, "rust_toolchain": toolchain,
           "build_tag": os.environ.get("KNIFE_BUILD_TAG"),
           "performance_sink": "sceClibPrintf",
           "offline_shaders": {"program_count": len(offline_programs), "all_operation_ids": list(range(64, 88)),
               "source_sha256": offline_source,
               "pc_compiler_sha256": re.search(r'KNIFE_OFFLINE_COMPILER_SHA256 "([a-f0-9]{64})"', offline_header).group(1),
               "embedded_header_sha256": sha256(root / "knife/vita/shaders/generated/offline_programs.h"),
               "programs": offline_programs,
               "load_order": "embedded GXP, validated device cache, runtime compiler; per-entry fallback",
               "shader_capabilities": "two independent u64 banks, program IDs0..127; separate from optimization options",
               "gpu_pixel_equivalence_verified": False, "hardware_performance_verified": False},
           "performance_transport": {"writer": "bounded background worker", "queue_batches": 3,
               "queue_bytes": 196608, "max_batch_bytes": 98304,
               "banks": "changed cumulative; full every6 windows or after observed transport failure",
               "loss": "whole-window drops counted; cumulative counters recover, histograms/hotspots do not"},
           "vita_cache_policy": {"newlib_heap_bytes": 268435456,
               "cpu_source_owner_bytes": 67108864, "cpu_graded_bytes": 33554432,
               "gpu_deferred_bytes": 58720256, "gpu_text_bytes": 12582912,
               "gpu_group_bytes": 12582912, "gpu_movie_frame_bytes": 3145728,
               "gpu_total_with_jpeg_bytes": 109051904,
               "admission": "native owner accounting and kernel free memory; LRU and pressure trimming",
               "text_cache": "exact quad signatures, typing segments then finished-run promotion; native540p bbox, extra RGBA8 rounding may differ slightly",
               "movie_owner": "one persistent native owner; bounded nonblocking main commands, GPU-completed native-frame leases",
               "fixed_language_id": 2 if os.environ.get("KNIFE_VITA_TITLE_ID") == "KNIF00540" else None,
               "gpu_stage_prefix_bytes": 3145728,
               "resource_demand": "STCH poll/PC-preserving; 10s request deadline; legacy waits bounded",
               "glyph_upload": "whole message prewarm to cropped immutable R8 pages, normally256px wide,2MiB native-charge pool/font",
               "cpu_pressure_headroom_bytes": 33554432, "cpu_target_headroom_bytes": 50331648,
               "cpu_hot_ui_soft_priority_bytes": 25165824,
               "extra_ui_sources": "CG/library/thumbnail raw KATL; checked per-KIMG file reads; no whole-atlas preload",
               "extra_ui_teardown": "drop inactive page handles and unpinned source owners; keep active reads and GPU fences",
               "extra_ui_diagnostics": "ui_source_event with build tag, page, status and complete bounded read error",
               "gpu_optional_cache_spare_bytes": 1048576,
               "audio_stream_channels": 8, "audio_stream_queued_blocks": 10,
               "audio_bgm_queued_blocks": 50, "audio_bgm_prefetch_ms": 1000,
               "audio_bgm_pcm_pool_bytes": 399360, "audio_default_pcm_pool_bytes": 92160,
               "audio_stream_pcm_pool_ceiling_bytes": 3194880,
               "audio_stream_pcm_pool_ceiling_scope": "8 BGM-sized pools; actual charge follows per-stream category"},
           "optimization_profile": {"default": default_profile,
               "default_mask": default_mask,
               "default_features": [name for name, bit in optimization_features if default_mask & bit],
               "all_profile": "stable baseline: bits0..28 plus bit30 ps-plain-material; hardware-test is a separate candidate preset",
               "hardware_test": {"name": hardware_test_profile, "mask": optimization_values["HARDWARE_TEST"],
                   "features": [name for name, bit in optimization_features if optimization_values["HARDWARE_TEST"] & bit],
                   "scope": "all plus ps-all-operations, group-bounds, half-color, tiled-targets, blur-weights and rt-scenes-2; hardware acceptance pending"},
               "candidate_all_operations": "bit38 selects64..87 from PC-precompiled authored GXP, plain-first with per-entry native fallback; hardware acceptance pending",
               "candidate_full_material": "explicit bit37 enables full-source OP1/6/13 entries54/55/56, with native capability fallback; excluded from hardware-test as superseded",
               "file": "optimization-profile.txt in external data root; complete override except exact legacy stock values under compiled hardware-test",
               "legacy_stock_migration": {"enabled": default_profile.strip() == hardware_test_profile,
                   "exact_profiles": ["all", "all ps-all-operations", "all ps-full-material"],
                   "trim_outer_whitespace": True, "file_rewritten": False,
                   "custom_profiles_preserved": True, "effective_selection_logged": True},
               "mask_type": "u64", "bits": optimization_bits,
               "default_excludes": [name for name, bit in optimization_features if not default_mask & bit],
               "render_target_modes": {"rt-scenes-2": 2, "rt-scenes-4": 4, "mutually_exclusive": True, "fallback": 1},
               "features": [name for name, _ in optimization_features],
               "batch_dependencies": ["async-submit", "compact-stream"],
               "graph_and_rename_dependencies": ["batch-arena", "async-submit"]},
           "runtime_shader_cache": {"root": "ux0:data/WitchontheHolyNight/shader-cache", "abi": cache_abi, "entry_abi": 2,
               "color_contract": "encoded-unorm8-uchar4", "asset_gxt_codec": 9,
               "compiler_policy": compiler_policy, "extensions_archive_sha256": sha256(root / "knife/vita/vendor/shacccg-ext/libSceShaccCgExt.a"),
               "source_sha256": re.search(r'SOURCE_SHA256 "([a-f0-9]{64})"',
                   (root / "knife/vita/shaders/generated/runtime_source.h").read_text()).group(1),
               "compiler_module_bundled": mode == "game",
               "compiler_module": ({"source": str(Path(os.environ.get("KNIFE_VITA_SHADER_COMPILER", str(root / "knife/vita/vendor/shacccg/libshacccg.suprx"))).relative_to(root))
                                     if Path(os.environ.get("KNIFE_VITA_SHADER_COMPILER", str(root / "knife/vita/vendor/shacccg/libshacccg.suprx"))).is_relative_to(root)
                                     else os.environ["KNIFE_VITA_SHADER_COMPILER"],
                   "path_in_vpk": "sce_module/libshacccg.suprx", "bytes": 1776523,
                   "sha256": "0c58b5d3e1cc47d9837b8c59ab6f3c299bfe7adf55a83f547d553f9fbb776552"} if mode == "game" else None),
               "capability_validation_required": True},
           "performance_console_default": os.environ.get("KNIFE_PERF_CONSOLE") == "1",
           "quiet_build": os.environ.get("KNIFE_VITA_QUIET") == "1",
           "performance_console_forced_off": os.environ.get("KNIFE_VITA_QUIET") == "1",
           "build_kind": "release" if release else "dev",
           "diagnostics_enabled": os.environ.get("KNIFE_VITA_DEV_DIAGNOSTICS") == "1",
           "rustc": subprocess.check_output(["rustc", "+" + toolchain, "-vV"], text=True),
           "scope": ("Unbounded actual Game/Ui entry, direct GXM, code-fixed native 540p scene/composite; 960x544 scanout; hardware testing pending"
                     if release else "DEV actual Game/Ui native entry with a finite frame limit; not a final release"
                     if mode == "game" else f"DEV {mode} probe; runtime checks require actual Vita execution"),
           **({"feature": "vita-gxm", "frame_limit": os.environ.get("KNIFE_VITA_DEV_MAX_FRAMES"),
               "assets": ("external ux0:data/WitchontheHolyNight/assets; no game assets in VPK"
                          if os.environ.get("KNIFE_VITA_TITLE_ID") == "KNIF00540"
                          else "external ux0:data/knife-woh/assets; no game assets in VPK"),
               "save_namespace": os.environ.get("KNIFE_VITA_SAVE_NAMESPACE", "development")}
              if mode == "game" else {}),
           **({"manifest": "knife/probes/vita-movie/Cargo.toml",
               "package": "knife-vita-movie-probe", "binary": "vita-movie-probe",
               "title_id": "KNIF00011",
               "target_cache": os.environ.get("CARGO_TARGET_DIR"),
               "assets": "external ux0:data/knife-woh/assets; exactly three existing MP4s; no game assets in VPK",
               "input_authority": "doc/vita-movie-probe-verification.json",
               "runtime_timeout_seconds": 60,
               "runtime_requirements": "strict single RESULT PASS, actual three active-stage captures, native teardown and requested renderer identity"}
              if mode == "movie" else {}),
           **({"title_id": os.environ.get("KNIFE_VITA_TITLE_ID", "KNIF00540"),
               "physical_canvas": [960, 540],
               "fixed_render_resolution": {"source": "code", "configuration_read": False,
                                           "scene": [960, 540],
                                           "composite": [960, 540]},
               "scene_pixels": [int(v) for v in os.environ.get("KNIFE_VITA_SCENE_PIXELS", "960,540").split(",")],
               "composite_pixels": [960, 540], "display": [960, 540],
               "script_coordinates": [1920, 1080],
               **({"param_sfo": {"path": str((output / "param.sfo").relative_to(root)),
                                 "sha256": sha256(output / "param.sfo"),
                                 "attribute2": sfo_attribute2(output / "param.sfo")}}
                  if (output / "param.sfo").is_file() else {})} if release else {}),
           "entrypoint": "tools/build-vita.sh", "tools": tools, "artifacts": artifacts,
           "package_contents": package_contents,
           "source_files": source_files,
           "runtime_verified": False, "hardware_verified": False}
variant_suffix = "-540p" if os.environ.get("KNIFE_VITA_TITLE_ID") == "KNIF00540" else ""
path = output / f"vita-{mode}{'-release' if release else ''}{variant_suffix}-build.json"
path.write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print("Build receipt:", path)
