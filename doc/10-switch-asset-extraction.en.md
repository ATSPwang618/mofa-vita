# Build Vita assets from Switch inputs

Use `prepare-mofa-vita.py` at the repository root.
The implementation lives in [vita_asset_pipeline/](../vita_asset_pipeline/README.md);
the default delivery is `dist/ux0/data/WitchontheHolyNight/assets/`.
The old `tools/rebuild_final_assets.py` targets the historical package and is retained for investigation.

## Requirements

Supply the Switch BASE NSP, update 1.02 NSP and Chengkong MOD RomFS.
The pipeline merges BASE and update, then overlays matching MOD paths without modifying the inputs.
Defaults are defined in [switch.py](../vita_asset_pipeline/switch.py);
override them with `--base`, `--update`, `--mod`, `--keys` and `--firmware`.

External inputs also include `prod.keys` and three matching shared BFTTF fonts extracted from firmware.
The default firmware/keys locations are under `%APPDATA%/Ryujinx/`.
The merged RomFS movie supplies the logo; the old custom desktop logo is not required.

Use Windows x64 Python 3.13.1, Pillow 11.3.0, NumPy 2.3.3 and ispc_texcomp 1.0.1.
Missing Python packages are installed in the local `.runtime/`.
Hactool has a verified download path. Supply the SHA-locked FFmpeg 8.0 build
(`--ffmpeg` can select it) and Vita SDK `psp2pvrt64.exe`/`psp2gxt.exe`.
The latter are initially copied from `D:/unity/PSVITA/sdk/host_tools/bin/` and hash-checked.

The game-data folder alone is insufficient on an otherwise empty machine.
Conversion checkpoints support resuming; a full run after deleting every conversion cache has
not been independently accepted. The delivered package was built and checked in stages.

## Run

```powershell
python prepare-mofa-vita.py --setup
python prepare-mofa-vita.py --work-dir .work/mofa-asset-build --verify-samples
python prepare-mofa-vita.py --work-dir .work/mofa-asset-build --gate .work/mofa-asset-build/samples-main/verification.json --jobs 8
python prepare-mofa-vita.py --check
```

The sample gate binds source identity and conversion code. Re-run it after either changes.
Resume using the same work directory; do not run concurrent builds there.
Existing complete outputs are preserved; choose `--output <new-directory>` for another delivery.
Extracted data and intermediate archives need substantially more space than the final 4.69 GiB pack.

For NVIDIA acceleration, run `--setup-cuda`, then add `--image-backend cuda` to both sample
and complete-build commands. CUDA handles eligible resize, grading and BC3 decode operations.
The 70% RGBA8 path matches Pillow's integer filtering. DXT encoding stays with locked ISPC;
PVRTC uses the original SDK encoder with up to eight bounded workers. SHA acceptance is unchanged.

## Acceptance and VPK binding

Keep the existing 70% variant policy, protected textures, fallback, logical coordinates,
archive names/volumes and fixed 11,616-entry PVRTC selection.
Audio uses two banks, `voice.mrg` and `bgm.mrg`, containing 9,471 logical members.

The current delivery has 103 physical files and 5,030,884,638 bytes.
101 files match the historical lock exactly. The requested-source logo and actual manifest
are independently validated, rather than forced to match the old custom-logo package.

Full validation checks the physical inventory, manifest, all audio members, archive hashes
and texture budget. Publication writes validation/delivery receipts and automatically synchronizes
the manifest, startup profile, atlas index, stream index and binding receipt into the project.

For an existing complete delivery, `--sync-project` validates and resynchronizes it.
Check with `python vita_asset_pipeline/build_binding.py --check`, then
[build the matching VPK](03-构建与工具链.md). Host checks do not establish Vita gameplay validation.

The pinned production recipe includes 127 historical binary encoding deltas and host helpers.
It is not pure metadata or a complete reconstruction of every historical encoder.
Keep its [source provenance and licenses](../vita_asset_pipeline/production/tools/source_replay/README.md).
See the [Chinese guide](10-Switch原版资源提取.md) for input locations and historical image tools.
