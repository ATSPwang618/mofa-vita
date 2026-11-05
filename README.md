# mofa-vita

《魔法使之夜》（Witch on the Holy Night，Switch 版）的 **PS Vita 移植**。

用 Rust 引擎 [`knife`](knife/) 驱动全部游戏逻辑，图形走 Vita 原生 **GXM** C 后端（桌面调试为 WGPU）。项目基于 [AuroraWright/knife](https://github.com/AuroraWright/knife) 0.9.4，并在上游基础上加入 KIMG/KATL 纹理、Ogg Opus、960×540 画布、native 540p MP4、Windows/WSL 构建与质检等本移植特有的支持。

本仓库只包含**引擎源码、构建工具与生成元数据**。游戏本体、字体、音视频和汉化数据由使用者自备，不入库；**编译 VPK 不需要游戏素材**。

2026-11-05 当前本地版本 `stage-effect-r4-sparse-20261105` 先优化 scene38/PC1246 的 IMG0436 星点层：按精确 DXT5 非零 alpha 和过滤邻域生成 68 个互不重叠的矩形，在原 Quad 内提交一次 Draw。该层源域覆盖约减少 73.9%，保留 960×540 输出、原纹理和混合顺序；这是几何覆盖证明，实机 FPS 与画面仍待验证。默认输出带版本号的 VitaDeck 日志，配套素材无需重传；详见 [透明覆盖优化](doc/24-scene38-透明覆盖优化.md)。

2026-11-05 上一版本 `stage-effect-r3-heavy-ps7-20261105` 优先优化约 6 FPS 的 PC1246 重场景：为其中最频繁的 op7 增加完整材质专用 shader29，保留原公式、特效与 960×540 分辨率；不可用时回退原路径。默认开启 VitaDeck 日志和版本号，增加 `draw_ps7`/`ps_fetch7_29` 便于确认实际接管。用户新实机日志确认29已启用，目标全重绘窗口约6.076→7.034 FPS、render约162.65→140.27 ms；有小幅收益但仍卡顿，详见[实机复核](doc/22-op7专用shader实机复核.md)。实现和host验证见 [重场景 op7 专用着色器](doc/21-重场景op7专用着色器.md)。上一轮无目标收益的 r2 前缀、空 draw、present clear 实验继续保持撤回，对照见 [r2 实机复核与回退](doc/20-r2实机复核与回退.md)。

2026-11-04 本地版本 `540p-7a65c2c-auto-only-20261104` 关闭 Vita 玩家触发的剧情快进：R 键不再切换快进，顶部面板快进按钮不可用，其他手动向前加速输入也不生效；L 键保留正常速度自动播放。操作与验证范围见 [关闭手动快进](doc/18-关闭手动快进.md)。已使用上一版 uifix 配套资源的设备只需更新 EBOOT/VPK。

继承 `540p-d734b38-uifix-20261104` 的书库/图录修复：四个图集预先去掉外层 NX 压缩，按成员定位读取，675 张图片的尺寸、编码与字节保持一致，详见 [书库与图录加载修复](doc/17-书库与图录加载修复.md)。从更早资源包升级仍须同步 `allui.hed`、`allui.mrg`、`vita-540p-assets.json`。

继承 `540p-d734b38-bgmbuf-20261103` 的 BGM 一秒 PCM 预读，单路增加 300 KiB MAIN 池载荷；语音沿用原档。此前 29 项音频测试记录见 [BGM 预读缓冲调整](doc/16-BGM预读缓冲调整.md)。

此前 `540p-d734b38-targetfix-20261103` 修复读档/转场时 `old frame` 分配失败直接 panic 的路径：优先借用备用目标，实际分配失败后清理可选缓存并有限重试，持续不足则保留完整画面并暂停脚本。菜单同类捕获与存档缩略图也有失败处理；114 项渲染 host 测试及 ARM 打包通过；后续用户日志已确认该版本在实机运行，并记录了本轮音频断供。详见 [读档与转场目标分配恢复](doc/15-读档与转场目标分配恢复.md)。

剧情素材沿用 2026-11-02 的 prebudget 限额，已提前扫描全库，将 20,181 个潜在 RGBA 大图离线缩小，单张完整 RGBA 按原生行距/页计费不超过 1 MiB。本次仅重排 Extra 图集的外层存储，画布仍为 960×540；继承此前纹理池与帧准备修复。处理范围和逐图验证见 [演出材质离线显存限额](doc/14-演出材质离线显存限额.md)。

本机 VitaDeck 的单连接日志阻塞已通过 [PC 接收补丁](tools/vitadeck/README.md) 修复，日志自动保存到 VitaDeck 的 `logs/vita-console.log`。当前交付包默认开启日志，尚未推送到设备。此前接收验证见 [记录](doc/13-VitaDeck日志接收修复.md)。

## 截图

以下均为实机截图（960×544 直出；左上角是调试用性能 HUD；当前构建默认开启控制台采集）：

| | |
|---|---|
| ![标题画面](doc/screenshots/01-title.jpg) | ![环境设置·语音](doc/screenshots/02-voices.jpg) |
| 标题画面（中文 ID2） | 环境设置：语音音量与试听 |
| ![书库](doc/screenshots/03-archive.jpg) | ![剧情演出](doc/screenshots/04-story.jpg) |
| 书库 / Archive | 剧情演出（960×540 + 上下各 2 px letterbox） |

## 功能

- **原生 960×540 渲染**：`scene`/`composite` 固定 960×540，最终居中输出到 960×544，上下各留 2 px；脚本坐标仍按 1920×1080 的布局单位换算。
- **完整剧情引擎**：演出指令、motion/effects、group pixel-mask 合成、电影播放（MP4）、BGM/语音/SE（Opus）、存档/读档（带缩略图）、回想、正常速度自动播放、文本窗口控制；Vita 关闭玩家手动快进。
- **全部菜单**：标题、环境设置（含逐角色语音音量）、书库 Archive、CG 鉴赏、乐曲鉴赏、系统菜单；固定中文 ID2。
- **开机感谢页**：TYPE-MOON 影像与原版注意画面之间播放两页感谢图（每页约 5 秒，任意键跳过），向参与和支持本项目的朋友致谢；源图与打包脚本见 [tools/credit-intro](tools/credit-intro/README.md)。
- **运行时 shader 编译与缓存**：首次启动编译专用 Cg 程序并写入数据根下的 `shader-cache/`，之后全部命中就不再加载编译器；缓存键包含源码 SHA、编译器 SHA、编译策略、缓存 ABI 与 shader id。
- **跨平台构建**：Windows（自动调用 WSL）/ Linux 上用 `tools/build-vita.py` 一条命令出 VPK；同一套引擎另有桌面 WGPU 后端用于调试。
- **单一 540p 构建档**：`TITLE_ID = KNIF00540`（SFO `ATTRIBUTE2=12`，扩大主内存）。旧的 270p 变体与相关死代码已彻底移除。

## 构建

安装 Python **3.9+**，在项目根目录执行：

```bash
python3 tools/build-vita.py --check      # 只检查环境
python3 tools/build-vita.py game release # 构建正式版 VPK
```

- `--check` 不安装、不编译；正常构建会自动安装缺失的 Rust 工具链与 `rust-src`，也可先跑 `--setup`。
- Windows 用 `py -3` 代替 `python3`，脚本会检测并转换到 WSL 路径。
- 需要可用的 VitaSDK（GCC、newlib、平台 stubs、打包工具）以及 CMake、Ninja、Git、Bash；Vita Rust 固定 `nightly-2026-05-28`。
- png、jpeg、z、ogg、opus、opusfile、taihen 七个静态库、shader 编译器模块与 SceShaccCgExt 已随仓库放在 `knife/vita/vendor/`，无需另行安装。
- 首次安装 Rust/编译需要联网；工具链与缓存齐全时可设 `KNIFE_VITA_OFFLINE=1`。正式版默认输出带版本号的控制台性能日志；显式 `KNIFE_VITA_QUIET=1` 或 `KNIFE_PERF_CONSOLE=0` 才构建静音包。

构建产物：**`release/knife-vita-game-540p.vpk`**。ELF、链接映射与构建收据在 `.work/vita/game-release-540p/`，构建日志在 `.work/logs/vita/`。完整工具链、打包与排错说明见 [构建与工具链](doc/03-构建与工具链.md)。

## 安装

1. 在支持自制应用的 PS Vita 上安装 VPK（或把交付目录里的 `eboot.bin` 推到 `ux0:app/KNIF00540/`）。
   本机已构建的交付包在 `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/`（不入库），含 `eboot.bin`、VPK 与说明文档。
2. 把配套完整 `assets/` 复制到 **`ux0:data/WitchontheHolyNight/assets/`**。
3. 从 LiveArea 启动。Vita 版固定使用中文 ID2；存档在 `saves/` 下。

运行时会使用同一个数据根：

| 路径 | 内容 |
|---|---|
| `ux0:data/WitchontheHolyNight/assets/` | 素材包（`vita-540p-assets.json` 在根） |
| `ux0:data/WitchontheHolyNight/saves/` | 正式版存档（自动创建） |
| `ux0:data/WitchontheHolyNight/shader-cache/` | 运行时编译的 shader 缓存（自动生成；首次启动会慢几秒） |
| `ux0:data/WitchontheHolyNight/perf-enable.txt` 等 | 可选的性能开关，当前默认开启；文件内容 `0` 会关闭，见 [性能采集与分析](doc/06-性能采集与分析.md) |



## 素材兼容性

当前引擎匹配 **native540 codec9 离线限额 + Extra 图集直接读取包（2026-11-04）**，清单 `assets/vita-540p-assets.json` 的 SHA-256 为：

```text
0f161f5c868f109cbcaca0da8db2c57a8a1cce7260ad2d0335ba70952e3ae1a3
```

- 素材需由使用者自备的 Switch 游戏数据（及相应汉化资源）转换得到；原始 NSP、未经转换的 romfs、旧 codec8 包和其他分辨率版本不能直接替代。
- VPK 不含游戏素材。已经安装匹配素材包时，仅重编引擎不需要重新复制素材；素材清单变化时 EBOOT 会做启动校验。
- 本移植的开机感谢页改变了 `parts.hed`、`parts.mrg` 与清单，**升级到带感谢页的版本时这 3 个文件要一起更新**，否则启动会提示 `WRONG FILE SIZE` / `WRONG ASSET VERSION`。
- 从 bgmbuf、targetfix 或 prebudget 升级本次 uifix 时，须同时更新 **`allui.hed`、`allui.mrg`、清单和 EBOOT/VPK**；只换程序会被启动校验拒绝。其余已匹配素材无需重传，详见 [17](doc/17-书库与图录加载修复.md)。
- 当前运行包对高清输入再做全库离线显存限额，提前缩小 **20,181 个条目**，保持逻辑坐标和完整画面。更新本版需一起替换四个剧情归档的 `.hed/.mrg`（`jallpac`、`jallpachd`、`allpacml`、`allpachdml`）、清单和 EBOOT；也可复制完整配套 `assets/`。处理规则和复现见 [离线限额](doc/14-演出材质离线显存限额.md)。
- 当前配套包已同步用户替换的 `movie/logo.mp4`（2026-11-02）。替换视频也会改变素材校验身份，需更新清单、内嵌元数据并重编 EBOOT；只覆盖 MP4 会触发 `WRONG FILE SIZE` 或 `WRONG ASSET VERSION`。
- 启动出现 `ASSETS REQUIRED` 等提示时，先核对素材完整性与清单版本；更新流程见 [更新素材元数据](doc/03-构建与工具链.md#83-更新素材元数据)。

## 操作

本版起采用**日式（亚洲）布局**：○ 确认、× 取消。

| Vita 输入 | 剧情中的常用操作 |
|---|---|
| ○ | 确认、推进文字 |
| × | 取消、返回或切换消息窗口显示 |
| □ | 回看日志（回想） |
| △ | 系统菜单 |
| L | 开启 / 关闭正常速度自动播放 |
| R | 剧情中不启用快进；菜单中的原有导航作用保留 |
| START / SELECT | 快速保存 / 快速读取 |
| 方向键、右摇杆方向 | 导航、选择 |
| 左摇杆 | 列表滚动、画面移动等界面操作 |
| 前触摸屏 | 点击、拖动 |

> 游戏内的按键提示沿用 Switch 版字形（Ⓐ Ⓑ Ⓧ Ⓨ 等），与实际按键的对应关系是：**Ⓐ=○、Ⓑ=×、Ⓧ=□、Ⓨ=△**。映射源码见 [vita_input.rs](knife/knife/src/vita_input.rs)。

## 项目结构

| 路径 | 内容 |
|---|---|
| [knife/](knife/) | Rust 工作区：游戏逻辑、格式解析、渲染抽象、音频与 Vita 平台层 |
| [knife/vita/](knife/vita/) | GXM C 后端、Cg 源码、生成的运行时着色器与系统图片 |
| [tools/](tools/README.md) | 本地构建、打包、素材处理、开机感谢页与质检脚本 |
| [doc/](doc/README.md) | 技术文档索引：架构、渲染、资源、优化、构建、性能、限制、素材 |
| [doc/screenshots/](doc/screenshots/) | README 使用的实机截图 |
| `release/`、`.work/`、`dist/` | 本机产物、缓存与自备素材，不入库 |

## 文档

| 文档 | 内容 |
|---|---|
| [01-架构总览](doc/01-架构总览.md) | 项目定位、crate 结构、Vita 帧循环与线程模型、坐标与渲染目标 |
| [02-渲染与GPU后端](doc/02-渲染与GPU后端.md) | GXM 后端结构、批次/arena、纹理池、运行时 shader 编译与缓存 |
| [03-构建与工具链](doc/03-构建与工具链.md) | 构建脚本、SDK/WSL 探测、VPK 打包、素材元数据更新、排错 |
| [04-资源调度与缓存](doc/04-资源调度与缓存.md) | 资源生命周期、atlas 共享、GXT Ready、图像 worker、音频通道 |
| [05-优化记录与实测数据](doc/05-优化记录与实测数据.md) | 28 项优化开关、历史真机对照与验证状态 |
| [06-性能采集与分析](doc/06-性能采集与分析.md) | 优化开关/profile、采集框架、日志格式与分析工具 |
| [07-已知限制与未验证项](doc/07-已知限制与未验证项.md) | 尚未验证的能力、平台/构建/许可限制 |
| [08-素材与安装](doc/08-素材与安装.md) | 素材清单、从原始数据制作、安装路径与更新流程 |
| [09-仓库维护与清理](doc/09-仓库维护与清理.md) | 唯一资源目录、产物保留、可清理项与验证规则 |
| [10-资源加载失败修复](doc/10-资源加载失败修复.md) | 第一轮原生分配失败恢复与错误传播 |
| [11-高清纹理显存与崩溃修复](doc/11-高清纹理显存与崩溃修复.md) | 最新 core、长立绘占用、纹理页隔离、帧准备回退与复测 |
| [12-透明调色板纹理排查](doc/12-透明调色板纹理排查.md) | 全库 alpha/RGBA 颜色统计、P4/P8 能力与实际收益边界 |
| [13-VitaDeck日志接收修复](doc/13-VitaDeck日志接收修复.md) | 日志开关、PC 多连接接收、实机恢复与自动保存 |

文档入口见 [doc/README.md](doc/README.md)；机器可读证据保留在 [doc/evidence/](doc/evidence/)。

本地资源只保留两份：`dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/`（运行包，约 2.11 GiB）和 `dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-image-source/`（可编辑图片输入，约 5.64 GiB）。运行包已经使用离线缩小后的实际纹理；PNG 工程保留一次较高分辨率编辑输入，重新打包必须应用同一限额。历史素材、转换中间图、回滚资源和额外素材压缩包不长期保留。最新 ELF/map 留在 `.work/vita/game-release-540p/` 用于崩溃定位。

## 已知限制

- 当前离线限额包已完成资源/解码与 ARM 构建验证，尚待实机画面、帧时间与显存峰值回归。素材选择已覆盖全库，不依赖实机逐段发现大图；设备验证仍用来确认整体渲染和帧时间。历史捕获不能代替新包的测量，见 [07](doc/07-已知限制与未验证项.md)、[14](doc/14-演出材质离线显存限额.md)。
- 游戏素材、字体与汉化资源需自备；仓库与 VPK 都不包含。
- 部分菜单/提示字形沿用 Switch 版贴图（对应关系见「操作」），不随本移植改为 Vita 符号。
- 存档、设置与真机行为的完整兼容矩阵以 [07](doc/07-已知限制与未验证项.md) 与代码为准。

## 致谢



此外感谢上游引擎 [AuroraWright/knife](https://github.com/AuroraWright/knife) 及其逆向与文档工作，以及 SceShaccCgExt、VitaSDK 与所有第三方依赖的作者。

## 许可与声明

- 引擎沿用上游 [Mozilla Public License 2.0](knife/LICENSE)。
- 第三方 SceShaccCgExt 的来源与 GPLv3 许可见 [NOTICE](knife/vita/vendor/shacccg-ext/NOTICE) 与 [LICENSE](knife/vita/vendor/shacccg-ext/LICENSE)，发布时须保留随包声明。
- 《魔法使之夜》游戏本体、图像、音频、字体与汉化资源的权利归各自权利方；本仓库不包含游戏数据。README 截图仅用于展示移植运行效果。
