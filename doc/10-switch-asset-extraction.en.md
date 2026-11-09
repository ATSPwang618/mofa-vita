# Switch asset extraction guide

[Back to project README](../README.en.md) · [简体中文](10-Switch原版资源提取.md)

**Extract a complete RomFS with the base game and update 1.02 merged, add the shared fonts and translation, then convert it into the Vita asset format.** `prepare_game.py` produces original game data, not an asset pack ready for the current EBOOT. Neither this repository nor the VPK includes game data, keys, firmware or translation files; supply these from your own game/device and separately obtained translation.

## 1. Inputs and tools

These are the versions used by this project. Filenames may differ, but the game and update must match.

| Input | Requirement |
|---|---|
| Base NSP | *WITCH ON THE HOLY NIGHT*, title ID `010012A017F18000`, v0 / BASE |
| Update NSP | **1.02**, update title ID `010012A017F18800`, package version `v131072`; `--update` takes the actual `.nsp` file, not its containing folder |
| `prod.keys` | Your own Switch keys able to decrypt the game and firmware; the script reads the original file and does not copy it into assets |
| Firmware directory | Unpacked Switch firmware NCAs, either directly inside the directory or as `<subdirectory>/00` registered-content entries. A firmware ZIP is not accepted directly |
| hactool | Obtain the Windows executable from [SciresM/hactool](https://github.com/SciresM/hactool) and place it at `.tools/hactool/hactool.exe` in the repository. The script currently uses this fixed location; the binary is not bundled |
| Python | Python 3.11+ recommended; extraction uses only the standard library, without image-conversion packages |
| Chengkong translation | To reproduce the current Chinese variant, supply the matching MOD's inner `romfs` directory as described in step 5 |

**The extraction script has a specific input contract:** it parses PFS0 NSPs, selects the largest NCA candidate and verifies `Content Type: Program`. It targets inputs that need no additional ticket/title-key handling and does not import tickets. XCI, NSZ, NSPs requiring additional rights data, and packages whose largest NCA is not the Program are outside this automated path. For those inputs, first export a complete merged RomFS using a tool suitable for your own backup format, then continue with fonts and validation.

Older Windows hactool builds have limited Unicode path support. Prefer an ASCII checkout such as `D:/mofa-vita`, with ASCII paths for keys and firmware, and put `--out` under that checkout's `.work` directory. The script passes relative output paths to hactool, so **output on a different drive is unsupported**. Python reads the original NSP paths, which may contain spaces or Unicode. Allow tens of GiB for extracted NCAs, original RomFS and conversion intermediates; the final runtime pack's size is not the required workspace size.

## 2. Merge the base and update, then extract fonts

The following example uses **Windows PowerShell** from the repository root. Replace all input paths. If the `py` launcher is unavailable, substitute your Python executable for `py -3`.

```powershell
Set-Location 'D:\mofa-vita'
$baseNsp = 'D:\mahoyo-input\WITCH ON THE HOLY NIGHT v0 (010012A017F18000) (BASE).nsp'
$updateNsp = 'D:\mahoyo-input\WITCH ON THE HOLY NIGHT v131072 (010012A017F18800) (UPD).nsp'
$keysFile = 'D:\mahoyo-input\prod.keys'
$firmwareDir = 'D:\mahoyo-input\firmware-nca'
$romfsDir = Join-Path (Get-Location).Path '.work\switch-romfs-1.02'

py -3 tools/prepare_game.py --base "$baseNsp" --update "$updateNsp" --keys "$keysFile" --firmware "$firmwareDir" --out "$romfsDir"
if ($LASTEXITCODE -ne 0) { throw 'Extraction failed; resolve the error before converting.' }
```

This command:

1. Extracts NCAs into `.work/nca/base/` and `.work/nca/update-1.02/`, recording source paths, sizes and SHA-256 identities.
2. Checks the candidate Program NCAs, then uses hactool's `--basenca` to supply the base for the update's BKTR partition and export the **complete merged** RomFS. Extracting only changed files or overwriting encrypted NCA files is not equivalent.
3. Scans the firmware for the four shared `.bfttf` fonts below and extracts them into the same output directory. These fonts may not be inside the game NSP.
4. Prints an `Assets ready` summary. Local provenance is recorded in `doc/assets-provenance.json` and `doc/fonts-provenance.json`; extraction logs are under `doc/logs/`. These generated records contain local source paths and should not be uploaded as public game data.

| Firmware content title ID | Extracted fonts |
|---|---|
| `0100000000000811` | `nintendo_udsg-r_std_003.bfttf` |
| `0100000000000814` | `nintendo_udsg-r_org_zh-cn_003.bfttf`, `nintendo_udsg-r_ext_zh-cn_003.bfttf` |
| `0100000000000813` | `nintendo_udjxh-db_zh-tw_003.bfttf` |

The extraction stage requires all four fonts; the later English/Chengkong pack retains only those it needs. Keep the original `.bfttf` filenames rather than renaming them to `.ttf`.

## 3. Validate the extracted directory

Resources must be directly inside `$romfsDir`, without an extra `romfs/` layer:

```text
switch-romfs-1.02/
  allscr.mrg                  # story scripts
  script_text.mrg             # text
  jallpac.{hed,mrg,nam}       # standard story art; three separate files
  jallpachd.{hed,mrg,nam}     # HD input for offline conversion
  allpacml.{hed,mrg,nam}      # multilingual images
  allpachdml.{hed,mrg,nam}
  allui.{hed,mrg,nam}         # UI atlases
  parts.hed / parts.mrg
  witch.bfsar
  exfont_nx01.bffnt
  nintendo_*.bfttf            # shared fonts added above
  stream/                    # original sound streams
  movie/
  en/  zc/  ...              # retain other original directories at this stage
```

Check for missing files first. This detects omissions, not correct decryption or version compatibility:

```powershell
$requiredFiles = @('allscr.mrg', 'script_text.mrg', 'witch.bfsar', 'exfont_nx01.bffnt',
  'nintendo_udsg-r_std_003.bfttf', 'nintendo_udsg-r_org_zh-cn_003.bfttf',
  'nintendo_udsg-r_ext_zh-cn_003.bfttf', 'nintendo_udjxh-db_zh-tw_003.bfttf')
foreach ($archive in @('jallpac', 'jallpachd', 'allpacml', 'allpachdml', 'allui')) {
  foreach ($extension in @('hed', 'mrg', 'nam')) { $requiredFiles += "$archive.$extension" }
}
$missingFiles = @($requiredFiles | Where-Object { -not (Test-Path -LiteralPath (Join-Path $romfsDir $_) -PathType Leaf) })
if ($missingFiles.Count) { throw "Missing files: $($missingFiles -join ', ')" }
```

With a host Rust toolchain installed, build `kdu` to inspect the archive index and parse all scripts. This is a PC utility, not a Vita executable:

```powershell
cargo +1.99.0 build --manifest-path knife/Cargo.toml --locked --release -p kdu
if ($LASTEXITCODE -ne 0) { throw 'kdu build failed.' }
# Default Cargo output; adjust if CARGO_TARGET_DIR is set.
$kduExe = '.\knife\target\release\kdu.exe'
& $kduExe --romfs "$romfsDir" list jallpac
if ($LASTEXITCODE -ne 0) { throw 'Story archive read failed.' }
& $kduExe --romfs "$romfsDir" parse-all
if ($LASTEXITCODE -ne 0) { throw 'Script parsing failed.' }
```

`kdu` also supports `extract`, `export-bntx` and `scenes`; consult `--help`. Asset extraction does not require exporting ExeFS or replacing the game tables already present in the repository.

## 4. An existing merged RomFS, or a font-only retry

If you already exported a **complete RomFS with update 1.02 applied**, point `$romfsDir` at that directory and add fonts:

```powershell
py -3 tools/prepare_game.py --base "$baseNsp" --update "$updateNsp" --keys "$keysFile" --firmware "$firmwareDir" --out "$romfsDir" --fonts-only
if ($LASTEXITCODE -ne 0) { throw 'Font extraction failed.' }
```

`--fonts-only` skips NSP and game RomFS extraction, although the current argument parser still requires `--base` and `--update`; their files are not read in this mode. A directory containing only the update delta is insufficient. When switching input dumps, do not reuse old `.work/nca/` caches: reuse currently checks filenames and lengths rather than rehashing every cached NCA. Use a separate checkout or move the previous cache and output aside first.

## 5. Chengkong translation and conversion input

Extraction does not automatically install the translation. The historical `tools/create_540p.py` entry point expects `--mod` to name the MOD's **inner `romfs` directory**, containing exactly these four files, not the outer `atmosphere/contents/<TitleID>/` tree:

```text
romfs/
  allui.mrg
  script_text.mrg
  zc/arshanghaisonggbpro_lt.otf
  zt/SungASC-Medium.TTF
```

The script makes private copies of translation replacements and preserves the original extraction for inspection and art rebuilding. A different MOD file set raises `Unexpected MOD file set`; check its version rather than deleting files just to pass the check. Follow the translation author's terms for use and distribution.

To create the historical 540p intermediate, copy the `kdu.exe` built above to the locations expected by the scripts:

```powershell
Copy-Item -LiteralPath $kduExe -Destination tools/kdu.exe
Copy-Item -LiteralPath $kduExe -Destination tools/kdu-compact.exe
$modRomfs = 'D:\mahoyo-input\chengkong\romfs'
py -3 tools/create_540p.py --source "$romfsDir" --mod "$modRomfs" --out '.work/chengkong-540p-source' --jobs 4
if ($LASTEXITCODE -ne 0) { throw 'Translation/540p intermediate conversion failed.' }
```

The result is `.work/chengkong-540p-source/assets/`, **still an intermediate**. Unchanged files may be NTFS hard links: do not modify them in place, which could alter the original extraction. Write separate outputs or break the links first.

## 6. Conversion into the current Vita asset contract

**There is currently no validated one-command pipeline from a clean checkout plus NSPs to an asset pack identical to the r14 delivery.** Extraction and the intermediate entry point above can be checked against the scripts; subsequent tools retain some fixed historical paths, receipts and host-tool dependencies. This table describes the stages and their contracts, not an unconditional copy-and-run pipeline:

| Stage | Tool and requirements |
|---|---|
| PNG/JPEG, Ogg Opus and H.264 MP4 intermediate | `create_compact.py --source <previous assets> --out <new directory> --ffmpeg <ffmpeg.exe> --jobs 4`; requires `tools/kdu-compact.exe`, Pillow from `tools/requirements-compact.txt`, and `ffprobe.exe` next to FFmpeg. Ogg here contains Opus |
| English/Chengkong reduction | `slim_languages.py` updates strips, text and mappings while retaining original language IDs 1/2. It still expects fixed Compact paths and historical receipts from `verify_compact_assets.py`; deleting `ja/zt` directories alone is not equivalent |
| Vita DXT and color contract | Historical `make-vita-assets.py` / `pack_dxt_540.py` outputs may contain **prebaked codec8** and require the `psp2gxt` host tool (`KNIFE_VITA_GXT`). They cannot be deployed directly. `migrate-vita-native-color.py` also assumes known input counts/receipts. The current target is **codec9, encoded UNORM** |
| Story art rebuilt from originals | [quality540](../tools/quality540/README.md): `rebuild_all_art.py` consumes the original `$romfsDir` and an existing matching Vita base pack. It rebuilds the two story archives, not an entire pack from scratch. Requires Pillow, NumPy and `ispc_texcomp==1.0.1` |
| Offline allocation limits | `quality540/limit_runtime_art.py` takes a matching runtime pack and editable project, staging limited archives; `verify_runtime_art.py` checks them. The limit accounts for possible RGBA expansion and must not be skipped |
| r11 UI member reads | `prepare_vita_ui_ranges.py --assets <complete Vita assets> --output <new staging directory>`; verify and merge its archives, indexes and manifest. Its output alone is not a complete runtime pack |
| Final identity and matching program | Update `vita-540p-assets.json`, startup profile and atlas/stream indexes, then rebuild EBOOT. Independently converted assets need not match the checks embedded in a prebuilt EBOOT |

The final manifest must use `schema=knife-540p-assets-v1`, `method=native-540p-encoded-unorm-v2`, `color_contract=encoded-unorm8-v1` and languages `en/zc`. The current startup-profile generator also expects 9,470 stream files. Do not deploy original HD archives directly or relabel an old codec8 manifest as the new format.

**Only after the complete final pack and its SHA/size manifest exist, with all staged changes correctly merged**, place it in the standard directory and regenerate the matching source metadata before following the [build guide](03-构建与工具链.md):

```powershell
$vitaAssets = 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets'
py -3 tools/generate_vita_asset_profile_540.py --assets "$vitaAssets"
if ($LASTEXITCODE -ne 0) { throw 'Asset contract/manifest mismatch.' }
py -3 tools/generate-vita-atlas-index.py "$vitaAssets" knife/knife/data/vita_atlas_index_540p.rs
if ($LASTEXITCODE -ne 0) { throw 'Atlas index generation failed.' }
py -3 tools/generate-vita-stream-index.py "$vitaAssets/vita-540p-assets.json" knife/knife/data/vita_stream_index_540p.txt
if ($LASTEXITCODE -ne 0) { throw 'Stream index generation failed.' }
py -3 tools/generate_vita_asset_profile_540.py --assets "$vitaAssets" --check
if ($LASTEXITCODE -ne 0) { throw 'Startup profile validation failed.' }
py -3 tools/prepare_vita_ui_ranges.py --assets "$vitaAssets" --check
if ($LASTEXITCODE -ne 0) { throw 'UI member-layout validation failed.' }
```

`generate_vita_asset_profile_540.py` **validates an existing manifest and generates the Rust profile; it does not convert original assets or create a missing manifest for you**. Deploy the complete pack so the device can find `ux0:data/WitchontheHolyNight/assets/allscr.mrg`, without a duplicated `assets/assets/` directory. See [assets and installation](08-素材与安装.md) for package/version matching.

## Extraction troubleshooting

| Symptom | Check |
|---|---|
| hactool missing / `WinError 2` | Check `.tools/hactool/hactool.exe`; it is not included in Git or supplied by VitaSDK |
| Decryption, key-generation or Rights ID errors | Check your own keys and whether the input is supported by this script. Do not publish keys or put them in the asset pack |
| BKTR/base NCA errors or many missing original files | Use the matching base plus 1.02 update and export the complete filesystem, not only changed files |
| `Shared fonts not found` | Unpack firmware first; check the directory contains NCAs or registered-content subdirectories with `00`, and the three font content IDs are present |
| `Unexpected MOD file set` | Select the inner translation `romfs`, check its four files and version |
| `WRONG FILE SIZE` / startup asset validation failure | New assets paired with an old EBOOT/profile, or incomplete copying; regenerate metadata and build a matching program |

Commands were checked against repository scripts and [hactool's upstream options](https://github.com/SciresM/hactool#usage). This documentation update did not re-extract the complete NSP or validate a fresh end-to-end rebuild of the current runtime pack.

