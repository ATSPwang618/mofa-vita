# knife ·《魔法使之夜》PS Vita 移植

本目录是 **mofa-vita 的 Rust 引擎工作区**，基于 [AuroraWright/knife](https://github.com/AuroraWright/knife) **0.9.4**，面向《魔法使之夜》（Witch on the Holy Night）Switch 版数据进行 PS Vita 适配。

Vita 版本直接调用 **GXM**，使用离线转换的纹理、原生音频输出和影片播放接口。它以引擎重实现的方式读取游戏数据，不需要在设备上运行 Switch 模拟器。本项目仍处于移植和实机验证阶段，复杂演出的性能、特效和长期运行表现需要持续测试，**目前不能保证全程 60 FPS**。

游戏数据、字体和汉化资源由使用者自行准备，不随源码或引擎 VPK 提供。

## 项目范围

| 项目 | 当前实现 |
|---|---|
| Vita 主要适配对象 | 《魔法使之夜》及本项目约定的转换资源包 |
| 语言 | 双语资源配置保留英语和中文（澄空）；汉化内容来自对应资源包 |
| Vita 渲染 | 原生 GXM；不通过 vitaGL，也不依赖 Vulkan 运行时 |
| 桌面渲染 | WGPU，保留桌面运行与开发验证入口 |
| 纹理 | KIMG / KATL 容器；支持 GXT 中的 DXT1 / DXT5，并保留必要的 PNG、JPEG 和遮罩编码 |
| 音频 | BFSAR、Opus / Ogg 流及 Vita 原生 AudioOut 输出 |
| 影片 | Vita `SceAvPlayer` 播放 MP4；当前游戏路径要求可见区域为 960×540、16:9 |
| 资源加载 | 有界后台读取、24 MiB 字节暂存、纹理预读和 GPU 驻留管理 |
| 存档 | 沿用游戏的 `savedata000` 数据结构，Vita 正式版与开发版使用独立存档目录 |

上游还实现了《月姬》的相关逻辑，但本移植的 Vita 资源检查、安装包和验证流程围绕《魔法使之夜》设置，不能据此宣称 Vita 包已支持上游的全部游戏和语言组合。

上游基线及本地改动说明见 [UPSTREAM.txt](UPSTREAM.txt)，引擎机制说明见 [engine-writeup.md](engine-writeup.md)。

## 目录结构

本 README 位于外层项目的 `knife/` 中。**构建、资源转换和打包脚本在上一层 `tools/`，技术文档在上一层 `doc/`。**

```text
mofa-vita/
├─ knife/                       本 README 所在的 Rust 工作区
│  ├─ knife/                    游戏主程序：脚本 VM、舞台、UI、文本、输入
│  ├─ knife-core/               归档、脚本、纹理等格式解析
│  ├─ knife-render/             桌面 WGPU 与 Vita GXM 渲染后端
│  ├─ knife-render-api/         共享渲染数据类型
│  ├─ knife-platform-vita/      Vita 平台接口、资源检查、音视频和存储
│  ├─ knife-audio/              音频资源、解码与混音
│  ├─ kdu/                     资源研究和转换工具
│  ├─ vita/                    GXM 等 C 后端、Cg 源码、预编译着色器
│  └─ probes/                  文本、影片、音频等独立探针
├─ tools/                       构建、转换、打包与验证脚本
├─ doc/                         架构、工具链、性能与资源文档
├─ release/                     本机生成的安装包
├─ dist/                        本机转换资源与中间产物
└─ .work/                       构建缓存、日志与验证记录
```

## 在 PS Vita 上运行

### 安装与资源位置

1. 准备可安装自制应用的 PS Vita，以及与引擎匹配的游戏资源包。
2. 安装 `knife-vita-game-540p.vpk`，其 TITLE ID 为 **`KNIF00540`**。
3. 将转换后的完整 `assets/` 复制到 **`ux0:data/knife-woh-540p/assets/`**。
4. 从 LiveArea 启动。首次无存档时以英语进入，随后可在游戏设置中选择中文（澄空）。

设备目录示意：

```text
ux0:data/knife-woh-540p/
├─ assets/
│  ├─ vita-540p-assets.json
│  ├─ language-profile.txt
│  ├─ ui-aliases.tsv
│  ├─ jallpac.hed / jallpac.nam / jallpac.mrg
│  ├─ allui.hed / allui.nam / allui.mrg
│  ├─ ……其余归档、脚本、字体和语言文件
│  ├─ movie/
│  └─ stream/
├─ saves/
│  └─ witch/
│     └─ savedata000
├─ perf-enable.txt              可选：内容为 1 时开启性能记录
├─ performance.log              开启记录后生成
└─ scene-mode.txt               可选：离屏渲染密度配置
```

这只是布局示意，不是完整资源清单。VPK 只打包引擎、LiveArea 文件和许可证；构建 VPK 不会自动准备游戏数据。原版 NSP、未经处理的 romfs、单独几张 GXT 或另一版本的资源目录，都不能直接代替匹配的 Vita 资源包。

应用启动时会检查内嵌资源 profile：关键文件的大小、内容校验值以及部分大文件的首尾数据必须匹配。出现 `ASSETS REQUIRED`、`WRONG FILE SIZE` 或 `WRONG ASSET VERSION` 时，应核对数据包与 VPK 是否配套，不能只删除清单来绕过检查。

### 操作

Vita 输入由 [vita_input.rs](knife/src/vita_input.rs) 映射到引擎按键。下表列出剧情中的常用操作，菜单和演出状态可能限制部分操作。

| Vita 输入 | 对应操作 |
|---|---|
| × | 确认、推进文字 |
| ○ | 取消、返回；剧情中停止快进或切换消息窗口显示 |
| □ | 回看日志 |
| △ | 系统菜单 |
| L / R | 自动播放 / 快进；菜单中也用于切换项目或页面 |
| START / SELECT | 快速保存 / 快速读取 |
| 方向键、右摇杆方向 | 导航、选择；剧情中的上下方向也用于模式面板 |
| 左摇杆 | 列表滚动、画面移动等界面操作 |
| 前触摸屏 | 点击和拖动操作 |

### 分辨率与画质

`540p` 是这套资源与安装包的档位名称，**不代表默认的每一级离屏目标都按 960×540 绘制**。脚本坐标仍是 1920×1080，场景、UI 画布与最终扫描输出分别处理。

在 `ux0:data/knife-woh-540p/scene-mode.txt` 中写入以下值，并重启应用：

| 文件内容 | 场景链 | UI / 合成画布 | 用途 |
|---|---:|---:|---|
| 不创建文件或留空 | 480×270 | 720×404 | 默认档位 |
| `half` 或 `2` | 480×270 | 960×540 | 提高文字和 UI 画布清晰度，场景仍为半尺寸 |
| `quarter` 或 `4` | 240×135 | 960×540 | 低密度场景对照 |
| `native`、`full` 或 `1` | 960×540 | 960×540 | 场景与画布均使用原生尺寸，开销更大 |

默认画布通常在旧文档中记作 720×405，但 [composite_physical](knife-render/src/vita.rs) 会向下取偶数，当前实际高度为 **404**。当前 `720` 文本也走默认分支，并不会把场景链改为 720×405。

扫描输出为 960×544，游戏内容占 960×540，上下各留 2 像素。调整 `scene-mode.txt` 只改变渲染目标密度，不会重新转换磁盘上的图片，也不能恢复已经丢失的纹理细节。

## 构建 Vita 版本

### 工具链

本工程的两条工具链需要分别准备：

| 用途 | 配置 |
|---|---|
| 桌面默认工具链 | [rust-toolchain.toml](rust-toolchain.toml) 中的 **Rust 1.99.0** |
| Vita 交叉编译 | 构建脚本显式使用 **`nightly-2026-05-28`**，通过 `-Z build-std=std,panic_abort` 编译标准库 |
| Vita 目标 | `armv7-sony-vita-newlibeabihf` |
| 原生工具 | VitaSDK、CMake、Ninja，以及 SDK 目标端的 png、jpeg、z、opusfile、opus、ogg 等静态库 |
| 着色器重编 | 本机已有的 `psp2cgc`；使用已有生成头文件构建时无需重新编译着色器 |

维护脚本按 WSL / Linux shell 环境编写，默认 `VITASDK=/opt/vitasdk`。脚本使用 `--locked --offline`，因此需要提前准备 Cargo 依赖缓存及 nightly 的 `rust-src`；不能把缺少依赖导致的离线失败当作源码编译结果。

### 正式版命令

以下命令在**外层 `mofa-vita/` 项目根目录**执行：

```bash
export VITASDK=/opt/vitasdk
KNIFE_VITA_VARIANT=native540 KNIFE_BUILD_TAG=mofa-local \
  bash tools/build-vita.sh game release
```

`KNIFE_BUILD_TAG` 会编入性能日志，用来识别本次构建。`KNIFE_VITA_VARIANT` 必须显式指定为 `native540`；脚本的缺省变体是旧的 `270p`。

构建成功后，安装包会复制到：

```text
release/knife-vita-game-540p.vpk
```

ELF、VELF、`eboot.bin`、`param.sfo`、链接映射及构建收据位于 `.work/vita/game-release-540p/`；构建日志位于 `.work/logs/vita/game-release-540p-build.log`。保留与实机 VPK 对应的 ELF 和构建收据，便于分析崩溃地址。

540p 正式版默认写入 `ATTRIBUTE2=12`（`0x0C`），请求扩展 MAIN 内存预算；当前平台代码设置 newlib 堆 192 MiB、libc 堆 4 MiB、主线程栈 2 MiB。MAIN、CDRAM 和各缓存是不同的预算，不能把扩展 MAIN 内存理解为显存也增加到 365 MiB。

### 开发探针

例如，只构建 GXM 平台探针：

```bash
bash tools/build-vita.sh gxm dev
```

脚本还支持 `platform`、`image`、`effects`、`audio`、`pipeline`、`text`、`audio-mixer`、`game`、`movie`、`compressed` 和 `ui-color` 模式。**没有 `engine` 模式，`release` 也只接受 `game release`。**

游戏 DEV 包默认运行 360 帧，开发存档写入 `ux0:data/knife-woh-dev/saves/`；正式包持续运行，540p 存档写入 `ux0:data/knife-woh-540p/saves/`。请用正式版 VPK 做日常运行测试，避免把 DEV 包的有限帧退出误认为崩溃。

### 图标与 LiveArea

[vita/sce_sys/](vita/sce_sys/) 保存安装包的系统界面资源，当前图片均为索引色 PNG（PNG color type 3）：

| 相对 `vita/sce_sys/` 的路径 | 尺寸 | 用途 |
|---|---|---|
| `icon0.png` | 128×128 | 应用图标 |
| `livearea/contents/bg.png` | 840×500 | LiveArea 背景 |
| `livearea/contents/startup.png` | 280×158 | 启动按钮图片 |
| `pic0.png` | 960×544 | 系统启动背景 |
| `livearea/contents/template.xml` | — | LiveArea 布局，背景引用 `bg.png`，启动图引用 `startup.png` |

打包脚本会将整个 `sce_sys/` 加入 VPK；`param.sfo` 由构建脚本另行生成。只修改这些系统图片时，重新执行 `game release` 并安装新 VPK 即可，不需要重做游戏的 DXT 资源，也不需要更新 `asset_profile_540p.rs`。这些 PNG 不受 `scene-mode.txt` 的游戏画布档位影响。

2026-10-29 的更新已重新构建，标记为 `mofa-livearea-20261029`，VPK 为 **4,921,694 字节**。本次修正了模板中不存在的 `background.png` 引用；将启动图从 290×164 裁为 280×158，只移除了全透明边框，全部可见像素及原调色板均保留。原图备份、当前 VPK/ELF 哈希及旧包记录见 [构建与工具链](../doc/03-构建与工具链.md)。

构建收据现在同时记录系统图片/XML 的源文件哈希和 VPK 内逐文件哈希。包检查还会拒绝缺失的 XML 图片引用，并检查 `pic0.png` 的尺寸及索引色格式。当前通过的是构建和包内容校验，新版系统界面的实机显示仍需安装后确认。

### 修改着色器

在外层项目根目录的 Windows PowerShell 中执行：

```powershell
.\tools\compile-vita-effects.ps1 -Compiler 'C:\path\to\psp2cgc.exe'
.\tools\compile-vita-shaders.ps1 -Compiler 'C:\path\to\psp2cgc.exe'
```

两个脚本分别更新游戏效果和基础四边形着色器的生成头文件。修改 `.cg` 后必须重新生成头文件，再重建 VPK；只改源码不会更新已经嵌入引擎的 GXP。编译器和 Sony 运行时模块不随项目分发。

## 制作与更新资源包

完整流程见 [从原版数据制作资产](../doc/08-从原版数据制作资产.md)。主要链路是：

```text
自备的 Switch 游戏数据及相应汉化资源
  → 图像提取与 compact 资源准备
  → 按用途转换为 GXT DXT1 / DXT5，保留必要的原编码
  → 生成 vita-540p-assets.json
  → 更新引擎内嵌资源 profile
  → 重建配套 VPK
```

已经有符合本项目布局的 compact 资源时，可在外层项目根目录使用 Windows Python：

```powershell
python tools/make-vita-assets.py `
  --compact 'D:\path\to\compact\assets' `
  --out 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets' `
  --keep tools/graded-images.txt --jobs 8

python tools/generate_vita_asset_profile_540.py `
  --assets 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets'
```

转换需要 Pillow 和本机可用的 `psp2gxt`。当前 `psp2gxt` 路径写在 [pack_dxt_540.py](../tools/pack_dxt_540.py) 和 [vita-game-gxt.py](../tools/vita-game-gxt.py) 的 `SDK` 变量中，换机器时需要核对路径。转换入口接收 compact 资源目录，不是直接接收 NSP；影片、音频和语言布局也应已在前序步骤准备好。

不透明图片优先转换为 DXT1，带透明度的图片优先转换为 DXT5。语言拼接图、需要 CPU 调色或裁切的条目、软裁剪梯度等保留其需要的编码，不能为了统一格式强制全部转成 DXT。`--keep` 用于保留这些特例。

**资源包变化后，需要先更新 `asset_profile_540p.rs`，再构建 VPK。** 只重新生成外部 JSON 清单不会更新 VPK 中的启动检查值。profile 生成器本身也要求符合当前约定的双语清单和文件布局，不是适用于任意资源集的通用工具。

运行时 GXT 路径把 DXT 压缩块交给 GXM 采样，不先展开为完整 RGBA 图。PNG / JPEG 等保留条目仍有独立解码和准备开销。

## 桌面开发与回归测试

### 桌面运行

以下命令从**本 README 所在的 `knife/` 工作区**执行，使用匹配的桌面资源，不要直接混用 Vita 专用 GXT 包：

```bash
cargo +1.99.0 build --release -p knife
cargo +1.99.0 run --release -p knife -- -a /path/to/desktop-assets -l zc
```

桌面影片解码分别使用 macOS VideoToolbox、Windows Media Foundation、Linux GStreamer，并有 `ffmpeg` 回退路径。Linux 需要相应 GStreamer 开发包和运行时解码插件，Windows N 版本可能需要 Media Feature Pack。

桌面常用参数包括 `--assets-dir`、`--language`、`--window-size`、`--save-file`、`--text-websocket`、`--skip-intro` 和 `--skip-movies`；额外调试选项需要 `--features debug`。配置示例见 [knife.ini.example](knife.ini.example)。**Vita 入口不读取桌面 `knife.ini`，也不提供桌面 WebSocket 文本服务。**

### 主机侧测试

在 `knife/` 工作区执行：

```bash
cargo +nightly-2026-05-28 test --locked --offline -p knife-render
cargo +nightly-2026-05-28 test --locked --offline -p knife-core
cargo +nightly-2026-05-28 test --locked --offline -p knife-platform-vita
```

这些命令测试渲染命令、资源格式、队列和平台通用逻辑。渲染测试使用主机侧替身，**通过不代表 GXM 像素、实机帧率或整作流程已经验证**。完整工作区的桌面构建还受系统音视频依赖影响，应区分目标端编译问题和主机环境缺依赖。

## 日志与常见问题

### 开启性能日志

在设备上创建 `ux0:data/knife-woh-540p/perf-enable.txt`，文件内容为：

```text
1
```

重新启动后，日志写入同级 `performance.log`。也支持 `assets/perf-enable.txt`，但数据根目录下的开关优先；如果根目录文件为 `0` 或内容无效，资源目录下的 `1` 不会覆盖它。

日志默认约每 5 秒记录一次，单次上限为 256 KiB，达到上限后停止常规性能记录，游戏继续运行。普通性能日志每次开启都会重建，复制到 PC 后再运行下一次测试。正式版不默认生成 DEV 的 `game-runtime.log`。

在外层项目根目录分析从实机复制的日志：

```bash
python tools/read-vita-performance.py performance.log --environment hardware --out perf-summary.json
```

比较数据时同时记录 VPK 的 `build_tag`、资源版本、`scene-mode.txt` 和设备时钟。当前日志头的 `scene_size` / `composite_size` 仍写死为 960×540，不能拿这两个字段证明实际离屏分辨率；应结合运行配置和源码判断。`gpu_finish_us` 等也是 CPU 在同步调用中观察到的耗时，不是独立的 GPU 计时器。

### 排查入口

| 现象 | 优先检查 |
|---|---|
| `ASSETS REQUIRED` 或资源版本错误 | `assets/` 是否完整，是否混用了 270p / 540p，资源 profile 与 VPK 是否一致 |
| 找不到 `performance.log` | 开关内容是否为 `1`、是否放在本变体的数据根、是否被根目录另一份开关覆盖 |
| 文字或 UI 偏软 | 当前画布密度；可用 `half` 保留半尺寸场景并把画布提高到 960×540 |
| 演出明显掉帧 | 对照同一场景的日志；增加预读和缓存不等于减少多 pass 合成成本 |
| 着色器修改未生效 | 是否重新生成 GXP 头文件并重建、安装了对应 VPK |
| DEV 包运行一段时间自动退出 | 是否到达开发包的帧数上限，改用 `game release` 包验证 |
| Linux 桌面构建缺 `gstreamer-1.0` | 安装桌面后端需要的开发包；不要把这个问题混同为 Vita 后端失败 |
| Vita3K 切换宿主渲染器后表现不同 | 本应用仍然调用 GXM；模拟器的 Vulkan / OpenGL 是其宿主实现，实机表现应独立验证 |

## 技术文档

| 文档 | 内容 |
|---|---|
| [架构总览](../doc/01-架构总览.md) | 脚本、场景链、平台层和资源加载 |
| [GXM 后端实现](../doc/02-GXM后端实现.md) | C 后端、着色器与 pass 生命周期 |
| [构建与工具链](../doc/03-构建与工具链.md) | 构建脚本、产物、收据和故障排查 |
| [性能实测与优化史](../doc/04-性能实测与优化史.md) | 历史实机记录与优化尝试 |
| [资源与画质](../doc/05-资源与画质.md) | 纹理、字体、密度和内存预算 |
| [已知限制与历史尝试](../doc/06-已知限制与已放弃方案.md) | 已记录的画面问题和方案实验 |
| [上游同步与版本](../doc/07-上游同步与版本.md) | 基线与已选取的上游改动 |
| [从原版数据制作资产](../doc/08-从原版数据制作资产.md) | 资源转换与设备部署流程 |

技术文档包含不同构建时期的记录。涉及可执行命令、尺寸和资源契约时，应核对当前脚本、源码和对应构建收据，不能把历史测试数据直接当成当前版本的性能保证。

## 致谢与许可

- [AuroraWright/knife](https://github.com/AuroraWright/knife)：本项目的上游引擎与研究基础。
- [DCNick3/shin](https://github.com/DCNick3/shin)：上游 knife 的灵感来源及部分共享代码。
- 澄空汉化相关制作人员：中文资源的制作与维护；使用者需自行准备相应补丁。

引擎源码沿用 **[Mozilla Public License 2.0](LICENSE)**。游戏本体、字体、图片、影片、音频和汉化资源的权利归各自权利方。发布源码或引擎包时，请保留许可证与上游署名，并将游戏资源与引擎代码分开处理。
