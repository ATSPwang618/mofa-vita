# 从 Switch 源文件重建最终 70% 混合纹理包

2026-11-18 · [English](10-switch-asset-extraction.en.md) · [文档索引](README.md)

## 固定目标与验收

唯一目标为 `Witch-on-the-Holy-Night-Chengkong-540p-Vita-70pct-PVRTC2-4bpp/assets/`。必须 **103个文件的路径、字节数、SHA-256全部一致**，不是仅尺寸相同或重新生成一份自洽清单。

- 总大小：**5,030,762,588字节**，约4.685 GiB。
- 清单SHA：`7c08cff36060b27bf7079917b4f984fe98c5dffe8cd6f9fd9859ee10ef59759d`。
- 不可改写的基准：[59 最终文件锁](evidence/59-final-assets-lock.json)。
- 实际执行状态：[61 复现收据](evidence/61-source-exact-replay.json)。只有 `exact_delivery_match=true` 才表示整包通过。
- 配套程序：`perf-render-20261117-r10-assets70-pvrtc2` / `KNIF00540`。

正式入口是 **`tools/rebuild_final_assets.py`**。它不读取旧100% DXT包、旧70%包、既有最终成品或私人recovery kit。旧候选脚本不是本教程入口。

## 一、必要输入

| 输入 | 内容 |
|---|---|
| BASE NSP | `WITCH ON THE HOLY NIGHT v0 (010012A017F18000) (BASE).nsp`，21,380,558,096字节 |
| 1.02升级NSP | `WITCH ON THE HOLY NIGHT v131072 (010012A017F18800) (UPD).nsp`，19,474,192字节 |
| 澄空MOD | `010012A017F18000/romfs`，包含 `allui.mrg`、`script_text.mrg`、`zc/arshanghaisonggbpro_lt.otf` |
| 定制logo | `logo.mp4`，215,765字节；SHA `6bb49ee83aa8cc0f666db91d08583f5b7c9f656029a2bb4dc376fb844808aba0` |
| 固件字库 | 自备Switch固件registered目录，或包含三份匹配Nintendo bfttf的目录 |
| 提取及编码工具 | 自备prod.keys、nstool.exe、psp2pvrt64.exe、psp2gxt.exe、FFmpeg |
| 本版本源码 | 包括tools/source_replay固定配方、两张credit-intro项目图片及生产数据表 |

**NSP不含澄空汉化、定制logo、项目致谢图和系统共享字体。** “一个命令”不等于“只有NSP就包含全部内容”。脚本可读取本机已有的固件并提取字体，不需要从旧运行包取字体。

源身份按内容检查：

- BASE Program NCA：`c134dd53826646afa027df1833254e0f999e6a6fc1c64bcf470944219f43da86`。
- UPDATE Program NCA：`db83e0cdb33b5eb5a0868f3ddf15204b2173986b4402dad5bf65373731ab5ed4`。
- `nintendo_udsg-r_std_003.bfttf`：`823913c9d80807979381e327846466f561f31ba1842cea8f7fedc5ad7d45d2ad`。
- `nintendo_udsg-r_ext_zh-cn_003.bfttf`：`f5784e7a553fef9a05b3b7dba94e77e79162ace9db8dafc715ace0be1fd9b9ff`。
- `nintendo_udsg-r_org_zh-cn_003.bfttf`：`a04eb490283d8393b2c49c52e315132461d790ac73996cb43ac869ce72003938`。

工具、MOD、配方及项目图片的完整身份见 `tools/source_replay/toolchain-lock.json`。不要改锁来接受不一致输出。

## 二、最短操作步骤（Windows x64）

### 1. 准备环境和路径

验证环境：**Python 3.13.1 x64**、Pillow **11.3.0**、NumPy **2.3.3**、ispc_texcomp **1.0.1**。不要换编码器版本。

在仓库根目录执行：

```powershell
python -m pip install --only-binary=:all: --target .work/replay-python -r tools/requirements-nsp-rebuild.txt
Copy-Item tools/final-assets.example.json .work/final-assets.local.json
```

编辑JSON中的实际输入、工具路径。支持中文和空格。`work` 必须是仓库 `.work` 下的独立空目录；`output` 必须是新的独立目录，不能填现用成品。配置只保存路径，不保存密钥内容。

固件目录可填写：`C:/Users/你的用户名/AppData/Roaming/Ryujinx/bis/system/Contents/registered`。

源码不附带密钥、固件或官方SDK工具。固定配方附带三个自编译宿主辅助程序及其源码/哈希，它们不是游戏EBOOT。

### 2. 快速预检和重建

```powershell
python -X utf8 tools/rebuild_final_assets.py --config .work/final-assets.local.json --check
python -X utf8 tools/rebuild_final_assets.py --config .work/final-assets.local.json
```

预检检查配置、MOD、logo、依赖、工具和配方。完整Program NCA及固件字体在正式提取阶段检查；预检通过不代表整包复现成功。

完成输出：

```text
你的输出目录/
  assets/                      # 固定103个文件
  source-rebuild-receipt.json   # 来源、配方和完整验收结果
```

成功提示必须是 `EXACT FINAL DELIVERY VERIFIED`。失败保留现场，不发布半成品。重跑同一命令复用已校验阶段及缓存。输入/配方改变会拒绝混用。

若异常退出留下 `exact-replay.lock`，先确认其中PID已结束，再删除这一个锁文件续建；不要并行启动两个同目录任务。

### 3. 独立验收

```powershell
python -X utf8 tools/verify_final_delivery.py --assets "你的输出目录/assets"
```

应返回 `physical_files=103`、`physical_bytes=5030762588`、`exact_delivery_match=true`。独立验证只证明文件相同；来源证据看同目录重建收据。

本次正式入口复用了此前已经从声明源文件生成并验证的图像/音频缓存以及RomFS，随后重新校验、组包并执行整包验收；没有为证明“一键”而再次重复全部耗时转换。收据明确记录这种续建范围，不把它写成第二次全冷启动实测。

本流程只生成 **assets**，不会重打EBOOT/VPK或调整存档、画质政策、按键、日志及音频方案。字节一致的资源继续使用既有配套程序。

## 三、实际制作链

1. 读取固定Program NCA、校验SHA，以nstool `--basenca` 合并BASE和1.02升级。更新语音、脚本、shader来自合并RomFS。
2. 用生产MZP解析器处理MOD，选 `blocks[2:6]` 保留英语/澄空，使用MOD字体，按序号生成UI、语言标记和别名。
3. 重放历史图片流程：早期BC7/JPEG缩放、PNG优化、原生颜色迁移、后期画质更新、语言条带及内存预算缩放。一次Lanczos缩放不能代替历史多轮编码。
4. 生成离线调色变体、504像素核心/4像素保护边的大图块、低内存fallback、存档预览。调色/缩放顺序、alpha、重名缩略图序号固定。
5. 执行原70%物理尺寸策略。70%不是JPEG质量值；逻辑坐标、画布、UI、控制图及保护fallback不统一乘0.7。
6. 按固定选择表重编码最终入选的 **11,616个PVRTC条目**，其余保持DXT；共享输入可复用载荷。不重新试验此前已淘汰的材质。
7. 从源Opus转换音频，固定Ogg序列号/CRC，生成voice/bgm bank及M10启动片段。用固定FFmpeg参数处理影片，使用声明的logo。
8. 保留容器压缩级别、扇区填充、去重、分卷、名字表CRLF和UI标题追加更新留下的旧数据区。70%重打包本来就不使用的100%垃圾区无需生成。
9. 全部文件通过不可变交付锁后才发布输出和成功收据。

## 四、历史兼容配方是什么

`tools/source_replay/` 包含生成合同、生产调色LUT、历史Ogg序列号、清单模板、工具锁及**明确标记的历史二进制编码差分**，不只是尺寸表。

旧Intel/AMD近似倒数舍入、PNG封装及少量DXT/影片编码差异会影响SHA。宿主辅助程序重放BC7/JPEG缩放、颜色迁移和ISPC舍入；剩余历史差异由 `historical-corrections.json.gz` 恢复。

当前固定127项兼容差分。两段影片采用固定单线程滤镜和x264编码，再恢复旧容器中的历史编码字节；两段各自已经完成两次独立编码一致性验证。图片仍按3个工作进程并行。

差分必须先匹配**源生成中间结果的SHA**，然后核验补丁SHA和输出SHA。不模糊套用，不从旧运行包补缺图。它含历史编码差分，**不能称为完全不含游戏派生数据的纯元数据，也不代表旧算法已经全部还原**。

复现合同是：**声明的源输入＋本版本固定历史配方＋锁定工具 → 同SHA成品**。其中一张历史预览名称与实际图像不一致，也通过明确源序号及校验保留，不能擅自修正画面却仍声称历史哈希一致。

## 五、时间、磁盘和失败处理

- 默认3个有界工作进程；内容寻址缓存与已验收阶段支持续建，不重跑已完成转换。
- 首次仍需真实编码时间，不承诺几分钟完成。新建任务至少 **50 GiB临时可用空间**，不计已经存放的源文件。
- 提取成功立即删除本次NCA副本。全流程成功默认清理任务提取目录、100%/70%中间包及编码缓存；`--keep-work` 保留它们用于调查。
- 不删除原始NSP、升级档、MOD、logo、固件或既有成品。失败保留现场，不递归清理仓库根目录。
- 换NumPy/GPU编码实现可能改变SHA，不能为了提速悄悄改变复现合同。

| 报错 | 处理 |
|---|---|
| Unsupported/corrupted nsp/update | 检查BASE和1.02 Program NCA，不改锁 |
| Translation input / Custom logo differs | 使用匹配MOD或定制logo |
| Required firmware fonts unavailable | 补充自备固件；它们不在游戏NSP内 |
| Tool / Python dependency differs | 安装锁定版本 |
| Historical encoder intermediate changed | 保留中间结果调查，不强行套补丁 |
| Checkpoint changed / Corrupted cache | 定位被改动文件，不用旧成品填补 |
| FINAL_DELIVERY_MISMATCH | 不是指定成品，不会宣告成功 |

旧80%、70%纯DXT、100%PVRTC及私人kit候选教程已退出当前操作流程；旧收据仅作有日期的调查证据。文件一致不是新增实机帧率或稳定性测试。
