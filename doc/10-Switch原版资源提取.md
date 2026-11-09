# Switch 原版资源提取指南

[返回项目首页](../README.md) · [English](10-switch-asset-extraction.en.md)

**先提取本体 + 1.02 更新的完整 RomFS，再准备字体和汉化，最后转换为 Vita 专用资源。** `prepare_game.py` 的输出是原版资源，不是能直接搭配当前 EBOOT 的 Vita 包。源码仓库与 VPK 都不包含游戏数据、密钥、固件或汉化文件，以下输入由使用者自行从自己持有的游戏/设备准备。

## 1. 准备输入与工具

本项目已使用的输入版本如下；文件名可以不同，但游戏与更新版本必须匹配。

| 输入 | 要求 |
|---|---|
| 本体 NSP | 《WITCH ON THE HOLY NIGHT》，Title ID `010012A017F18000`，v0 / BASE |
| 更新 NSP | 同一游戏的 **1.02**，更新 Title ID `010012A017F18800`，包版本 `v131072`；`--update` 指向 `.nsp` 文件，不是补丁目录 |
| `prod.keys` | 使用者自己的 Switch 密钥文件，能够解密上述游戏与固件；脚本读取其原位置，不复制到 assets |
| 固件目录 | 已解包的 Switch 固件 NCA 所在目录；支持目录下直接放 NCA，或注册内容目录中的 `<子目录>/00`。不能直接传固件 ZIP |
| hactool | 自行取得 [SciresM/hactool](https://github.com/SciresM/hactool) 的 Windows 可执行文件，放在仓库的 `.tools/hactool/hactool.exe`；当前脚本固定读取此位置，仓库不附带该程序 |
| Python | 建议 Python 3.11+；提取脚本仅使用标准库，不需要先安装图像转换依赖 |
| 澄空汉化 | 若制作当前中文版本，另备与上述版本配套的汉化 MOD 的 `romfs` 目录，见第 5 步 |

**当前提取脚本有明确适用范围：**只解析 PFS0 格式 NSP，按最大 NCA 选择候选并验证其 `Content Type: Program`；针对无需额外 ticket/title-key 处理的输入，不导入 ticket。XCI、NSZ、需要额外授权数据的 NSP，以及“最大 NCA 不是 Program”的其他打包布局，不在这条自动流程的支持范围。此时应先用适合自己备份格式的工具导出完整合并 RomFS，再从字体/资源检查步骤继续。

Windows 旧版 hactool 对 Unicode 路径支持有限。建议在纯英文路径检出源码，例如 `D:/mofa-vita`，密钥和固件路径也使用英文；`--out` 放在同一仓库的 `.work` 下。脚本向 hactool 传相对输出路径，**跨盘符输出不受支持**。原始 NSP 由 Python 读取，可使用带空格或中文的原路径。为 NCA、原始 RomFS 和转换中间产物预留数十 GiB 工作空间，不要按最终约数 GiB 的运行包估算。

## 2. 合并本体与 1.02 更新，提取共享字体

以下为 **Windows PowerShell** 示例。从项目根目录执行；路径均为示例，请替换为自己的文件。若没有 `py` 启动器，用对应 Python 可执行文件替代 `py -3`。

```powershell
Set-Location 'D:\mofa-vita'
$baseNsp = 'D:\mahoyo-input\WITCH ON THE HOLY NIGHT v0 (010012A017F18000) (BASE).nsp'
$updateNsp = 'D:\mahoyo-input\WITCH ON THE HOLY NIGHT v131072 (010012A017F18800) (UPD).nsp'
$keysFile = 'D:\mahoyo-input\prod.keys'
$firmwareDir = 'D:\mahoyo-input\firmware-nca'
$romfsDir = Join-Path (Get-Location).Path '.work\switch-romfs-1.02'

py -3 tools/prepare_game.py --base "$baseNsp" --update "$updateNsp" --keys "$keysFile" --firmware "$firmwareDir" --out "$romfsDir"
if ($LASTEXITCODE -ne 0) { throw '提取失败，请先处理错误，不要继续转换。' }
```

这个命令按顺序完成：

1. 将两份 NSP 中的 NCA 解包到 `.work/nca/base/` 和 `.work/nca/update-1.02/`，记录来源、大小与 SHA-256。
2. 检查候选 Program NCA。使用 hactool 的 `--basenca` 将本体作为更新 BKTR 分区的基础，导出**完整合并后的** RomFS。不能只提取更新文件，也不能把两组加密 NCA 文件直接覆盖当作合并。
3. 扫描固件中的字体内容，提取下列四个 `.bfttf` 到同一个输出目录。字体不一定在游戏 NSP 内，所以只解包游戏会缺字体。
4. 输出 `Assets ready` 摘要；详细记录位于 `doc/assets-provenance.json`、`doc/fonts-provenance.json`，提取日志位于 `doc/logs/`。这些是本机生成记录，包含本地来源路径，不应作为公开游戏资源上传。

| 字体内容 Title ID | 提取文件 |
|---|---|
| `0100000000000811` | `nintendo_udsg-r_std_003.bfttf` |
| `0100000000000814` | `nintendo_udsg-r_org_zh-cn_003.bfttf`、`nintendo_udsg-r_ext_zh-cn_003.bfttf` |
| `0100000000000813` | `nintendo_udjxh-db_zh-tw_003.bfttf` |

提取阶段要求四个字体齐全；后续英语/澄空双语运行包只保留所需字体。`.bfttf` 按原名保留，不需要手动改名为 `.ttf`。

## 3. 检查提取结果

`$romfsDir` 应当直接包含资源文件，不能再多套一层 `romfs/`：

```text
switch-romfs-1.02/
  allscr.mrg                  # 剧情脚本
  script_text.mrg             # 文本
  jallpac.{hed,mrg,nam}       # 普通剧情图库，三个独立文件
  jallpachd.{hed,mrg,nam}     # 高清图库，离线转换输入
  allpacml.{hed,mrg,nam}      # 多语言图片
  allpachdml.{hed,mrg,nam}
  allui.{hed,mrg,nam}         # 界面图集
  parts.hed / parts.mrg
  witch.bfsar
  exfont_nx01.bffnt
  nintendo_*.bfttf            # 上一步补入的共享字体
  stream/                    # 原版声音流
  movie/
  en/  zc/  ...              # 其他语言/资源目录也先保留
```

先做文件存在性检查；它只能发现缺失，不能证明内容解密和版本正确：

```powershell
$requiredFiles = @('allscr.mrg', 'script_text.mrg', 'witch.bfsar', 'exfont_nx01.bffnt',
  'nintendo_udsg-r_std_003.bfttf', 'nintendo_udsg-r_org_zh-cn_003.bfttf',
  'nintendo_udsg-r_ext_zh-cn_003.bfttf', 'nintendo_udjxh-db_zh-tw_003.bfttf')
foreach ($archive in @('jallpac', 'jallpachd', 'allpacml', 'allpachdml', 'allui')) {
  foreach ($extension in @('hed', 'mrg', 'nam')) { $requiredFiles += "$archive.$extension" }
}
$missingFiles = @($requiredFiles | Where-Object { -not (Test-Path -LiteralPath (Join-Path $romfsDir $_) -PathType Leaf) })
if ($missingFiles.Count) { throw "缺少文件：$($missingFiles -join ', ')" }
```

有宿主 Rust 工具链时，可使用仓库中的 `kdu` 再检查归档索引和全部脚本；这是 PC 工具，不是 Vita EBOOT：

```powershell
cargo +1.99.0 build --manifest-path knife/Cargo.toml --locked --release -p kdu
if ($LASTEXITCODE -ne 0) { throw 'kdu 构建失败。' }
# 默认 Cargo 输出目录；若设置了 CARGO_TARGET_DIR，请改用实际路径。
$kduExe = '.\knife\target\release\kdu.exe'
& $kduExe --romfs "$romfsDir" list jallpac
if ($LASTEXITCODE -ne 0) { throw '剧情归档读取失败。' }
& $kduExe --romfs "$romfsDir" parse-all
if ($LASTEXITCODE -ne 0) { throw '脚本解析失败。' }
```

`kdu` 还支持 `extract`、`export-bntx` 和 `scenes`，可用 `--help` 查看。提取素材不要求另外导出 ExeFS 或替换仓库已有的游戏表。

## 4. 已有完整 RomFS，或只有字体提取失败

如果已经从自己的工具导出了**应用 1.02 更新后的完整 RomFS**，将 `$romfsDir` 指向该目录，补字体即可：

```powershell
py -3 tools/prepare_game.py --base "$baseNsp" --update "$updateNsp" --keys "$keysFile" --firmware "$firmwareDir" --out "$romfsDir" --fonts-only
if ($LASTEXITCODE -ne 0) { throw '字体提取失败。' }
```

`--fonts-only` 跳过 NSP 解包与游戏 RomFS 提取，但当前参数解析仍要求填写 `--base`、`--update`；该模式不会读取两者。不要把只含更新差异的目录当作完整 RomFS。更换游戏/更新输入时不要复用旧 `.work/nca/` 缓存：目前复用检查依赖文件名和长度，并非每次重算内容哈希；使用独立检出或先移走旧缓存和输出目录。

## 5. 澄空汉化与转换输入

本体提取不会自动加入澄空汉化。历史初制脚本 `tools/create_540p.py` 要求 `--mod` 指向 MOD 的 **`romfs` 内层目录**，且恰好包含以下四个文件；不要传包含 `atmosphere/contents/<TitleID>/` 的外层目录：

```text
romfs/
  allui.mrg
  script_text.mrg
  zc/arshanghaisonggbpro_lt.otf
  zt/SungASC-Medium.TTF
```

该脚本私有复制汉化文件，并保留原版提取目录供检查和从原图重建。其他版本 MOD 若文件集合不同，会触发 `Unexpected MOD file set`，需要先核对版本，不能删文件凑数。汉化的使用/分发条件以其作者说明为准。

制作历史 540p 中间包时，先将本节构建的 `kdu.exe` 复制到脚本要求的位置，再运行：

```powershell
Copy-Item -LiteralPath $kduExe -Destination tools/kdu.exe
Copy-Item -LiteralPath $kduExe -Destination tools/kdu-compact.exe
$modRomfs = 'D:\mahoyo-input\chengkong\romfs'
py -3 tools/create_540p.py --source "$romfsDir" --mod "$modRomfs" --out '.work/chengkong-540p-source' --jobs 4
if ($LASTEXITCODE -ne 0) { throw '汉化/540p 中间包生成失败。' }
```

输出为 `.work/chengkong-540p-source/assets/`，**仍是转换中间包**。该脚本可能为未改动文件创建 NTFS 硬链接；不要在中间目录中原位修改这些文件，否则可能改到原版输入。另存转换结果或先断开链接。

## 6. 从原版输入到当前 Vita 包的边界

**当前仓库没有一条已验证的“干净检出 + NSP → r14 同身份 assets”一键命令。** 原始提取流程与上述中间包入口可以按源码核对；后续工具中仍有固定历史目录、旧收据和宿主工具依赖。下表说明转换顺序与各自合同，不应把各行无条件串成一键脚本：

| 阶段 | 工具与要求 |
|---|---|
| PNG/JPEG、Ogg Opus、H.264 MP4 中间资源 | `create_compact.py --source <上一步 assets> --out <新目录> --ffmpeg <ffmpeg.exe> --jobs 4`；需要 `tools/kdu-compact.exe`、`tools/requirements-compact.txt` 中的 Pillow，且 `ffprobe.exe` 与 FFmpeg 同目录。Ogg 是容器，此处声音是 Opus |
| 英语/澄空精简 | `slim_languages.py` 处理语言条带、文字和映射，保留原语言 ID1/2；仍依赖固定 Compact 目录及 `verify_compact_assets.py` 生成的历史收据，不能只手动删除 `ja/zt` 文件夹 |
| Vita DXT/颜色合同 | `make-vita-assets.py` / `pack_dxt_540.py` 的旧输出可能含 **codec8 预烘焙**，还依赖 `psp2gxt` 宿主工具（`KNIFE_VITA_GXT`）。不能直接部署；旧包迁移工具 `migrate-vita-native-color.py` 另有已知输入数量/收据要求。当前目标是 **codec9、encoded UNORM** |
| 从原图提升剧情图库质量 | [quality540](../tools/quality540/README.md)：`rebuild_all_art.py` 使用原始 `$romfsDir` 和已有匹配 Vita 基包，只重建两大剧情图库；不是从零生成全包。依赖 Pillow、NumPy、`ispc_texcomp==1.0.1` |
| 离线显存限额 | `quality540/limit_runtime_art.py` 使用匹配的运行包与可编辑工程，另存受限归档；`verify_runtime_art.py` 校验。限制的是潜在 RGBA 展开后的单图分配，不能省略 |
| r11 UI 按成员读取 | `prepare_vita_ui_ranges.py --assets <完整 Vita assets> --output <新的暂存目录>`；校验后合入所输出的归档、索引和清单。暂存目录不是完整运行包 |
| 最终身份与程序配套 | 更新 `vita-540p-assets.json`、启动 profile、atlas/stream 索引，然后重编 EBOOT；独立生成的素材不一定与现成 EBOOT 的校验值一致 |

最终包的清单应满足 `schema=knife-540p-assets-v1`、`method=native-540p-encoded-unorm-v2`、`color_contract=encoded-unorm8-v1`、语言 `en/zc`；当前启动生成器还要求 9,470 个 stream 文件。原始高清归档不直接部署；也不要仅修改清单标签使旧 codec8 包冒充新格式。

**仅在完整最终包及其 SHA/尺寸清单已生成、所有暂存改动已正确合入后**，放入标准目录并生成配套源码，再按[构建文档](03-构建与工具链.md)编译：

```powershell
$vitaAssets = 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets'
py -3 tools/generate_vita_asset_profile_540.py --assets "$vitaAssets"
if ($LASTEXITCODE -ne 0) { throw '素材合同/清单不匹配。' }
py -3 tools/generate-vita-atlas-index.py "$vitaAssets" knife/knife/data/vita_atlas_index_540p.rs
if ($LASTEXITCODE -ne 0) { throw '图集索引生成失败。' }
py -3 tools/generate-vita-stream-index.py "$vitaAssets/vita-540p-assets.json" knife/knife/data/vita_stream_index_540p.txt
if ($LASTEXITCODE -ne 0) { throw '声音索引生成失败。' }
py -3 tools/generate_vita_asset_profile_540.py --assets "$vitaAssets" --check
if ($LASTEXITCODE -ne 0) { throw '启动 profile 校验失败。' }
py -3 tools/prepare_vita_ui_ranges.py --assets "$vitaAssets" --check
if ($LASTEXITCODE -ne 0) { throw 'UI 成员布局校验失败。' }
```

`generate_vita_asset_profile_540.py` **验证已有清单并生成 Rust profile，不会将原版文件自动转换或替你重建缺失的清单**。最终复制的是完整包的 `assets/` 内容，设备应能直接找到 `ux0:data/WitchontheHolyNight/assets/allscr.mrg`，而不是 `assets/assets/allscr.mrg`。部署路径和版本对应关系见[素材与安装](08-素材与安装.md)。

## 常见提取问题

| 现象 | 检查项 |
|---|---|
| 找不到 hactool / `WinError 2` | `.tools/hactool/hactool.exe` 是否存在；它不随 Git 提供，也不是 VitaSDK 工具 |
| 解密、key generation、Rights ID 错误 | 自有密钥是否匹配输入、该 NSP 是否超出自动脚本支持范围；不要公开密钥或将其放入资源包 |
| BKTR/base NCA 错误或缺大量原版文件 | 必须配套本体 + 1.02，完整导出，而不是仅更新差异 |
| `Shared fonts not found` | 固件是否先解包，路径是否直接包含 NCA 或注册内容子目录的 `00`，是否存在三个字体 Title ID |
| `Unexpected MOD file set` | 指向正确的汉化 `romfs` 内层目录，核对四个文件与 MOD 版本 |
| `WRONG FILE SIZE` / 启动素材校验失败 | 新素材与旧 EBOOT/profile 混用，或复制不完整；按最终包重新生成元数据并构建配套程序 |

本节命令依据仓库脚本及 [hactool 上游选项](https://github.com/SciresM/hactool#usage)核对。此次文档更新未重新解包整份 NSP，也未声称完成从干净环境重制当前运行包的端到端验证。

