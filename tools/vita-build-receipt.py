"""Record actual built artifacts and selected tool identities (never runtime proof)."""
import datetime
import hashlib
import json
import os
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
    return hashlib.file_digest(path.open("rb"), "sha256").hexdigest()


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
for name in ["arm-vita-eabi-gcc", "vita-elf-create", "vita-make-fself", "vita-mksfoex", "vita-pack-vpk"]:
    path = Path(subprocess.check_output(["which", name], text=True).strip()).resolve()
    tools[name] = {"path": str(path), "sha256": sha256(path)}
sources = []
for base in (root / 'knife/knife', root / 'knife/knife-render', root / 'knife/knife-core',
             root / 'knife/knife-audio', root / 'knife/knife-render-api',
             root / 'knife/knife-platform-vita', root / 'knife/vita'):
    sources.extend(p for p in base.rglob('*') if p.is_file()
                   and p.suffix in ('.rs', '.c', '.h', '.toml', '.cg', '.gxp', '.gxpinc', '.sh')
                   and not any(part in ('target', '.work', '.git') for part in p.relative_to(base).parts))
# PNG/XML files affect the VPK identity even when no Rust or C source changes.
sources.extend(p for p in (root / 'knife/vita/sce_sys').rglob('*') if p.is_file())
sources += [root / 'knife/Cargo.toml', root / 'knife/Cargo.lock',
            root / 'tools/build-vita.sh', root / 'tools/vita-build-receipt.py', root / 'tools/check-vita-package.py']
source_files = [{"path": p.relative_to(root).as_posix(), "bytes": p.stat().st_size, "sha256": sha256(p)}
                for p in sorted(set(sources))]
receipt = {"built_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
           "target": target, "rust_toolchain": toolchain,
           "build_tag": os.environ.get("KNIFE_BUILD_TAG"),
           "build_kind": "release" if release else "dev",
           "diagnostics_enabled": os.environ.get("KNIFE_VITA_DEV_DIAGNOSTICS") == "1",
           "rustc": subprocess.check_output(["rustc", "+" + toolchain, "-vV"], text=True),
           "scope": ("Unbounded actual Game/Ui entry, direct GXM, native 960x540 scene and text/UI composite inside the 960x544 scanout; hardware testing pending"
                     if release else "DEV actual Game/Ui native entry with a finite frame limit; not a final release"
                     if mode == "game" else f"DEV {mode} probe; runtime checks require actual Vita execution"),
           **({"feature": "vita-gxm", "frame_limit": os.environ.get("KNIFE_VITA_DEV_MAX_FRAMES"),
               "assets": ("external ux0:data/knife-woh-540p/assets; no game assets in VPK"
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
           **({"title_id": os.environ.get("KNIFE_VITA_TITLE_ID", "KNIF00270"),
               "physical_canvas": [960, 540],
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
path = root / f"doc/vita-{mode}{'-release' if release else ''}{variant_suffix}-build.json"
path.write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print("Build receipt:", path)
