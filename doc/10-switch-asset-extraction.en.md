# Rebuild the exact final 70% mixed-texture pack

2026-11-18 · [中文](10-Switch原版资源提取.md)

Target: `Witch-on-the-Holy-Night-Chengkong-540p-Vita-70pct-PVRTC2-4bpp/assets/` — **103 physical files, 5,030,762,588 bytes**, manifest SHA `7c08cff36060b27bf7079917b4f984fe98c5dffe8cd6f9fd9859ee10ef59759d`.

The immutable oracle is [lock 59](evidence/59-final-assets-lock.json). Actual execution status is in [receipt 61](evidence/61-source-exact-replay.json); require `exact_delivery_match=true`. Format checks or a newly self-consistent manifest are insufficient.

## Inputs

- Retail BASE NSP: Title ID `010012A017F18000`, 21,380,558,096 bytes.
- 1.02 update NSP: v131072, Title ID `010012A017F18800`, 19,474,192 bytes.
- Chengkong MOD `010012A017F18000/romfs`, including allui.mrg, script_text.mrg, and zc/arshanghaisonggbpro_lt.otf.
- Custom logo.mp4, 215,765 bytes, SHA `6bb49ee83aa8cc0f666db91d08583f5b7c9f656029a2bb4dc376fb844808aba0`.
- Your Switch firmware registered directory, or the three matching Nintendo bfttf files. These system fonts are **not in the retail game NSP**.
- Your keys and matching nstool.exe, psp2pvrt64.exe, psp2gxt.exe, and FFmpeg.
- This source revision, including tools/source_replay, the two credit PNGs, and production engine tables.

The supported BASE Program NCA SHA is `c134dd53826646afa027df1833254e0f999e6a6fc1c64bcf470944219f43da86`; UPDATE Program NCA SHA is `db83e0cdb33b5eb5a0868f3ddf15204b2173986b4402dad5bf65373731ab5ed4`. All other tool/MOD/artwork/recipe identities are pinned in tools/source_replay/toolchain-lock.json.

The production builder never reads a prepared 100% pack, 70% pack, final release, or private recovery kit. One command does not mean the NSP contains the translation, custom art, logo, or firmware.

## Quick start — Windows x64

Use **Python 3.13.1 x64**, Pillow **11.3.0**, NumPy **2.3.3**, ispc_texcomp **1.0.1**:

```powershell
python -m pip install --only-binary=:all: --target .work/replay-python -r tools/requirements-nsp-rebuild.txt
Copy-Item tools/final-assets.example.json .work/final-assets.local.json
```

Edit every input and tool path. Set work to a dedicated empty directory under this repository's .work; output must be a separate new directory. Do not point output at your installed assets. The firmware path may be your Ryujinx bis/system/Contents/registered directory. Required SDK tools, keys and firmware are not included; the three included host helper binaries have source and pinned hashes.

```powershell
python -X utf8 tools/rebuild_final_assets.py --config .work/final-assets.local.json --check
python -X utf8 tools/rebuild_final_assets.py --config .work/final-assets.local.json
```

Preflight checks configuration, MOD, logo, dependencies, tools and recipe. Actual extraction additionally verifies Program NCA hashes and firmware fonts. Preflight is not a completed reproduction.

Output contains assets/ and source-rebuild-receipt.json. Require `EXACT FINAL DELIVERY VERIFIED`. Failure retains evidence and does not publish a successful pack. Repeat the command to resume hash-checked stages and caches. Changed inputs/recipes are rejected. After an abnormal exit, inspect the PID in exact-replay.lock and remove only that stale lock after confirming the process ended.

Independent verification:

```powershell
python -X utf8 tools/verify_final_delivery.py --assets "output/assets"
```

Require physical_files=103, physical_bytes=5030762588 and exact_delivery_match=true. The standalone verifier establishes file identity; the adjacent build receipt records source execution. This builds **assets only**, not EBOOT/VPK, and does not modify saves, mapping, rendering or audio policy. Identical assets use the existing matching executable.

## Pipeline

1. Locate/hash the supported Program NCAs and merge the 1.02 update with nstool --basenca. Updated voices, scripts and shaders come from patched RomFS.
2. Parse MOD text with the production MZP parser, retain blocks[2:6] for English/Chengkong, use the MOD font, and rebuild UI and aliases by ordinal.
3. Replay historical image contracts: early BC7/JPEG resize, PNG optimization, native-color migration, later quality refresh, language strips and budget scaling. One resize is not equivalent to that history.
4. Generate graded variants, 504-pixel tile cores with four-pixel guards, fallbacks and previews. Alpha, grading order and duplicate-name indices are fixed.
5. Apply the old70% physical-size policy to eligible story variants; logical coordinates, canvas, UI and protected/control textures are preserved. This is not a JPEG-quality setting.
6. Re-encode the **11,616 previously selected PVRTC entries**; retain DXT elsewhere. Identical inputs share cache payloads. Rejected candidates are not re-tested needlessly.
7. Rebuild Opus/Ogg banks with fixed serials and CRCs, the M10 lead, locked movie conversion and explicit custom logo.
8. Preserve compression levels, padding, deduplication, volumes, name-table CRLF and append-updated UI data. Unreferenced garbage in the 100% intermediate is discarded by the 70% repack.
9. Verify every final path, length and SHA before publishing output.

## Historical compatibility recipe

The fixed recipe includes contracts, production grading tables, historical Ogg serials, manifest templates, tools and **explicit binary encoder corrections**. It is not purely descriptive metadata.

Historical Intel/AMD reciprocal rounding, PNG serialization and a small number of DXT/movie encoding differences otherwise prevent identical bytes. Host helpers reproduce legacy resize, color migration and reciprocal behavior. Remaining corrections require the exact source-generated intermediate SHA, then verify correction and output SHA. Missing input never falls back to reference-pack pixels.

Do not describe this as full recovery of every historical algorithm, or as containing no game-derived data. The contract is **declared sources + this versioned historical recipe + locked tools**. One historically mislabeled preview is retained through an explicit source-index mapping and correction.

## Disk, time and failures

- Three bounded workers by default. Verified content-addressed caches and completed stages are reusable.
- A fresh run requires **50 GiB free scratch space**, excluding existing source files. Real encoding takes time; a few-minute completion is not promised.
- Temporary NCAs are removed after extraction. Owned intermediate packs, extracted files and caches are cleaned after success; --keep-work retains them.
- Original sources and existing releases are never deleted. Failures preserve evidence and never rewrite the acceptance hashes.
- Wrong tools/MOD/logo/fonts, changed intermediate corrections, corrupt checkpoints or final mismatches stop the pipeline. Investigate the named file; do not substitute a release file or change the lock.
- Byte identity is not a new hardware gameplay or performance validation.

Old recovery-candidate commands are historical investigation tools. The sole production entry point is rebuild_final_assets.py.

The recorded execution resumes source-generated, verified caches and the extracted RomFS, then reruns production packing and complete final verification. It is not claimed as a second all-cold conversion. The 127 compatibility records include historical binary differences. Both opening movies use fixed single-thread filter/x264 intermediates, each independently encoded twice with identical hashes before historical correction. Image work remains bounded to three workers.
