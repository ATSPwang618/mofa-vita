# krkr-rs

用 Rust 重新实现的 Kirikiri / TJS2 引擎：语言前端与虚拟机、KAG 解析器、引擎脚本类、静态插件兼容层、桌面宿主、PS Vita 宿主，以及配套的离线资源转换与分析工具。

同一份代码提供两条运行路径：

- **桌面**：winit + wgpu + cpal + FFmpeg，在 PC 上直接跑游戏，用于调试、资源验证和日常预览。
- **PS Vita**：PVR/GLES2 + SceAudioOut + AvPlayer，在真机上运行，已能安装启动、显示界面并跑通剧情。

插件按注册名映射到静态 Rust 实现，不加载旧 DLL 机器码。**插件名称能被加载不等于整款游戏已经兼容**，请以自己实际游戏的表现判断，已知差异见[插件兼容清单](docs/插件兼容清单.md)。

当前边界，先看这里再看后面的步骤：

- 真机帧率仍明显低于目标（有的游戏只有个位数 FPS），性能与游戏兼容性都在继续改进。
- 需要 PS Vita 能运行自制程序（HENkaku / Enso 等），并已安装 VitaShell。
- 加密或非标准封装的游戏必须先**在 PC 上**用转换工具处理，主机端不做解密。
- 仓库不附带 PlayStation 官方 SDK 的 GPU 运行模块，打包前需要自己准备，见[第 2 步](#2-打包-vpk)。

## 在 PS Vita 上玩 KRKR 游戏

四步，缺一不可：

| 步骤 | 在哪做 | 做什么 |
| --- | --- | --- |
| 1 | PC | 用 `krkr-convert` 把游戏资源解密、归一化、缩放、转码成 PSV 可用资源 |
| 2 | PC（WSL2） | 用 `cargo vita` 把宿主打包成 `krkr-vita.vpk` |
| 3 | PS Vita | 用 VitaShell 安装 VPK |
| 4 | PS Vita | 把转换后的游戏目录放进 `ux0:/data/KRKR/`，用启动器开局 |

### 1. 在 PC 上转换游戏资源

#### 为什么必须在 PC 上先转换

- PS Vita 启动器**不会**安装 `xp3filter.tjs`，加密 XP3 在主机上无法解密。
- 原版资源按 PC 分辨率制作（常见 1280×720、1920×1080），直接进主机超出显存与内存预算。
- 音频和纹理要转成主机硬件能直接用的格式：BC1/BC3 压缩纹理、AT9 硬件解码音频。

#### 构建转换器

桌面 stable 工具链即可，不需要 WSL，也不需要 PSVSDK：

```text
cargo build -p krkr-convert --release
```

Windows 上还需要 MSVC 的 C 编译器（插件集成了 SQLite，构建时需要编译它）。这一步不涉及 FFmpeg SDK 和 libclang，只有桌面宿主 `krkr-rs` 才需要。

运行前提：

| 依赖 | 用途 | 放置位置 |
| --- | --- | --- |
| `ffmpeg`、`ffprobe` | 探测与转码音视频 | 加入 `PATH`，或运行时按提示填写路径 |
| `at9tool.exe` | 音频转 AT9（仅在选择 AT9 时需要） | 与 `krkr-convert.exe` **同一目录** |

BC 纹理编码由内置的 rgbcx / bc-crunch 完成，不需要 texconv 或 PVRTexTool，也不会产生中间 TGA/DDS。`at9tool` 属于 SDK 工具，项目不分发，需要使用者自行提供。

#### 用 helper 交互转换

```text
cargo run -p krkr-convert --release -- helper
```

helper 顶层菜单是「生成 PSV 游戏资源 / 按实际格式归一化并建立资源 link（保持原始尺寸）/ 解包 XP3 / 封包 XP3」。**PSV 资源要求输入已归一化，所以正常流程要走两遍 helper。**

第一遍，归一化：

1. 选「按实际格式归一化并建立资源 link（保持原始尺寸）」。
2. 「游戏或资源目录」填原始游戏目录（松散文件和 XP3 都可以）。
3. 有 `xp3filter.tjs` 时，在「XP3 解包方式」选「使用检测到的 xp3filter.tjs」完成解密；明文资源选「明文 XP3，不使用过滤器」。
4. 输出目录默认是旁边的 `…-normalized`。这一步保持原始尺寸，只统一格式、修正扩展名并给旧脚本名建立 link。

第二遍，生成 PSV 资源：

1. 选「生成 PSV 游戏资源」，输入**第一遍输出的 `…-normalized` 目录**。这一步只做 PSV 转换，不再归一化或修复资源。
2. 「游戏原始画布尺寸」填游戏 PC 端分辨率，例如 `1280x720`。helper 会读取 `Config.tjs` 的 `scWidth/scHeight` 常量赋值并预填，表达式或冲突配置需要手动确认。填错会导致画面拉伸或裁切。
3. 「PSV 音频处理」三选一：全部转 AT9（硬件解码）/ 按路径选择 AT9 音频 / 保留常规音频策略。转 AT9 需要 `at9tool.exe`；未转换的 Opus 在 PSV 上无法播放。
4. 「PSV 图片处理」三选一：自动筛选并压缩图片（含透明纹理）/ 手动指定图片范围 / 保留无损图片策略。自动模式按实际像素选择——不透明图用 BC1，透明图用 BC3，颜色或透明度误差不达标时自动保留无损，原因写入摘要。
5. 需要时再选「纹理编码质量」（平衡 / 快速 / 高质量）和「纹理存储」（BC＋无损封装 / 原生 BC）。
6. 输出目录默认是 `…-psv`。

转换结果注意：

- 输出是**新目录**，原始游戏保持不动；把 `…-psv` 整个目录拷到 PSV。
- 附属文件必须一起保留：`.krkr-scale`（逻辑尺寸）、`.krkr-mp4`（视频标记），以及 link 出来的旧脚本名。缺了它们会出现尺寸错误或找不到资源。
- 已经生成过 PSV 资源的目录不要再拿去归一化。
- 转换器会移除输出根目录的 `xp3filter.tjs`，避免被自动加载。

命令行用法与 `helper` 等价，适合脚本化批量处理：

```text
cargo run -p krkr-convert --release -- normalize <游戏或资源目录>
cargo run -p krkr-convert --release -- psv <已归一化的目录> --canvas 1920x1080 --texture-auto
cargo run -p krkr-convert --release -- psv <已归一化的目录> --canvas 1920x1080 --texture-glob "bg/*.png" --at9-glob "bgm/*.ogg"
cargo run -p krkr-convert -- probe <已解包目录> -o probe.json
cargo run -p krkr-convert -- xp3 unpack encrypted.xp3 --xp3-filter game/xp3filter.tjs
cargo run -p krkr-convert -- xp3 pack <资源目录>
```

### 2. 打包 VPK

#### 2.1 先准备 GPU 运行模块（必须，最容易漏）

`krkr-vita` 启动时会从 `app0:module/` 加载四个模块，缺任何一个都会启动失败：

| 模块 | 用途 |
| --- | --- |
| `libgpu_es4_ext.suprx` | GPU 服务、内存与同步 |
| `libpvrPSP2_WSEGL.suprx` | 显示与交换缓冲 |
| `libIMGEGL.suprx` | EGL 上下文与表面 |
| `libGLESv2.suprx` | OpenGL ES 2.0 |

这四份二进制是官方 PSVSDK 工具链配合 `pvr-psp2-sys` 构建得到的产物，**当前仓库里没有**。打包前请把它们放到：

```text
crates/host-vita/runtime/module/
```

取得方式：在 [pvr-psp2-sys](https://github.com/jhq223/pvr-psp2-sys) 仓库用 Windows 官方 PSVSDK 构建（需要 CMake 3.22+ 与 Ninja）：

```powershell
$env:SCE_PSP2_SDK_DIR = 'C:/SDK/PSVita/sdk'
$env:CMAKE_GENERATOR = 'Ninja'
cargo build --features build-driver
```

构建结束会打印模块目录（形如 `target/debug/build/pvr-psp2-sys-*/out/pvr-driver/module`），把里面的四个 `.suprx` 复制过去。已有同一次驱动构建产出的模块时，直接放进该目录即可。

放好后自检，四个文件都要在：

```powershell
Get-ChildItem crates/host-vita/runtime/module
```

缺模块时 VPK 仍能打包和安装，但一进游戏就会报 `failed to load app0:module/...`。驱动与四个模块必须来自同一次构建，不要混用不同版本。

#### 2.2 准备 WSL2 构建环境

PSV 宿主通过 `cargo vita` 交叉编译，需在 WSL2 的 Bash 中进行。已验证的环境：

| 项目 | 配置 |
| --- | --- |
| 系统 | WSL2 / Ubuntu |
| Rust | `nightly`，已安装 `rust-src` |
| 打包工具 | `cargo-vita 0.2.2` |
| VitaSDK | 安装于 `/usr/local/vitasdk`，`arm-vita-eabi-gcc` 可用 |
| 目标 | `armv7-sony-vita-newlibeabihf` |

新环境先安装 WSL2、Rustup 和 [VitaSDK](https://vitasdk.org/#installing)，然后在 WSL2 中准备工具链：

```sh
export VITASDK=/usr/local/vitasdk
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$VITASDK/bin:$PATH"

rustup toolchain install nightly --profile minimal --component rust-src
cargo +stable install cargo-vita --version 0.2.2 --locked
```

VitaSDK 装在别处就改 `VITASDK` 的值。可以先用下面几条命令确认环境完整：

```sh
cargo vita --version
rustup component list --toolchain nightly --installed
command -v arm-vita-eabi-gcc vita-elf-create vita-make-fself vita-mksfoex vita-pack-vpk
```

#### 2.3 构建 VPK

在仓库根目录（即包含 `crates/host-vita/Cargo.toml` 的目录）执行：

```sh
export VITASDK=/usr/local/vitasdk
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$VITASDK/bin:$PATH"
export RUSTUP_TOOLCHAIN=nightly

cargo vita build vpk --release -p krkr-host-vita --locked
```

要点：

- **必须带 `--release`**，宿主代码会主动拒绝 Vita 的 debug 构建。
- 首次构建会联网下载 Cargo 依赖并编译 Vita 标准库；依赖已缓存时可追加 `--offline`。
- `cargo-vita 0.2.2` 需要 nightly，当前依赖也要求 Rust 不低于 1.98.1；`RUSTUP_TOOLCHAIN` 只影响当前终端，桌面和转换器仍可用 `cargo +stable`。
- 无需先单独生成 ELF，`build vpk` 会完成中间步骤。

默认产物：

```text
target/armv7-sony-vita-newlibeabihf/release/krkr-vita.vpk
```

需要把 PSV 构建缓存放到独立目录时：

```sh
cargo vita build vpk --release -p krkr-host-vita --locked --target-dir target/vita-build
```

产物位置相应变为 `target/vita-build/armv7-sony-vita-newlibeabihf/release/krkr-vita.vpk`，同目录还会生成 `.elf`、`.velf`、`.self` 和 `.sfo`。只验证编译链接、不打包用 `cargo vita build elf --release -p krkr-host-vita --locked`。

#### 2.4 VPK 内容

`crates/host-vita/Cargo.toml` 的 `[package.metadata.vita]` 决定打包配置：应用名 `KRKR`，Title ID `KRKRJHRST`，版本 `0.2.2`（应用信息里的 `00.22`）。`assets = "runtime"` 会把 `crates/host-vita/runtime/` 原样放进 VPK：

```text
eboot.bin                       构建生成
sce_sys/                        param.sfo（构建生成）、icon0.png、pic0.png
  livearea/contents/            template.xml、bg0.png、startup.png
module/                         四个 GPU 模块（需自行放入 runtime/module/）
licenses/                       秋水书体、Nivora、PromptFont 的许可
```

字体通过 `include_bytes!` 内嵌在程序里，VPK 中没有单独的 TTF，PS Vita 上也不需要额外安装字体。`build.rs` 会在构建时校验 LiveArea 图片的尺寸、颜色格式和 420 KiB 大小上限，不符合会直接中止并报出文件名。

### 3. 安装到 PS Vita

1. 用 USB 或 FTP 把 `krkr-vita.vpk` 传到 `ux0:` 任意目录。
2. 打开 VitaShell，选中该 VPK，按 ○ 安装，提示覆盖时确认。
3. 安装完成后主界面出现 `KRKR` 气泡。

更新了 GPU 模块、LiveArea 或 SFO 配置后，请**重装整个 VPK**，不要只替换 `eboot.bin`。

### 4. 放置游戏并启动

游戏目录结构固定为：

```text
ux0:/data/KRKR/
├── launcher.tsv            启动器配置（自动生成）
└── <游戏目录>/             每款游戏一个直接子目录
    ├── startup.tjs         默认入口（或 data.xp3 等归档入口）
    ├── ...                 转换后的资源
    ├── patch.tjs           可选，兼容补丁
    ├── krkr-input.tsv      可选，按键映射（游戏内保存时生成）
    └── savedata/           存档（首次启动自动创建）
```

要点：

- 拷贝的是转换器的 `…-psv` 输出内容，不是原始游戏目录。
- 启动器把**包含 `startup.tjs` 或任意 `.tjs` / `.xp3` 的直接子目录**识别为游戏；首次进入会扫描 `ux0:/data/KRKR/`，之后读 `launcher.tsv` 缓存，新增游戏后在游戏库按 START 刷新。
- 在游戏库选中游戏按 ○ 启动。启动器会在入口脚本之前自动执行游戏根目录的 `patch.tjs`，文件不存在就跳过；补丁执行失败会停止启动并显示错误。
- 游戏入口可以在游戏设置里改，支持松散文件和 `data.xp3>scripts/boot.tjs` 这类归档内路径；「使用默认入口」恢复为 `startup.tjs`。

### 启动器操作

| 操作 | 按键 |
| --- | --- |
| 移动焦点 | 方向键或左摇杆，按住连续移动 |
| 启动所选游戏 / 确认 | ○ |
| 打开游戏设置 | △（游戏库内 □ 也可） |
| 返回 / 在游戏库请求退出 | ×，退出需确认 |
| 刷新游戏库 | START（游戏库内） |
| 游戏库翻页 | L / R |
| 打开界面设置 | SELECT（游戏库内） |

触摸支持点按、列表拖动和滚动条，底部按键提示也可点按。设置分「界面」和「关于」两块，可切换简体中文 / English / 日本語、深浅主题和动效。

游戏设置分「常规」和「诊断」，包含：

| 设置项 | 说明 |
| --- | --- |
| 启动文件 | 散文件或 XP3 内路径，逐级浏览选择 |
| 光标速度 | 左摇杆移动光标的快慢 |
| 特效画质 | 原生 / 均衡 / 低画质，分别以 960 / 720 / 480 像素宽为画布上限；最终画面仍是屏幕分辨率，文字和界面图不跟着缩放 |
| 引擎诊断 | 输出 Debug 级别的启动与资源信息 |
| 游戏脚本日志 | 控制脚本 `Debug` 输出 |
| 性能显示 | 左上角 FPS / 内存 / GPU 面板 |

设置保存在 `ux0:/data/KRKR/launcher.tsv`，三个诊断开关默认关闭并按游戏分别保存。日志统一输出到控制台，由外部日志插件收集，引擎本身不写日志文件。

### 游戏内操作

| 操作 | 按键 |
| --- | --- |
| 鼠标移动 | 左摇杆 |
| 鼠标左键 / 右键 | ○ / × |
| Ctrl / 空格 | △ / □ |
| Esc | START |
| Enter | SELECT（短按） |
| 滚轮 | 右摇杆上下 |
| PageUp / PageDown | L / R |
| 触摸 | 单指点击或拖动滑块，双指上下滑动滚动内容 |
| 虚拟键盘与按键映射 | 长按 SELECT 约 350 ms |
| 返回启动器 | 按住 START + SELECT 一秒 |

按键映射面板里：方向键选择、○ 确认、× 关闭、L / R 切换上下停靠、□ 切换映射页、△ 保存到当前游戏的 `krkr-input.tsv`。面板只是键位映射工具，不是文字输入法。

### 遇到问题先查这些

| 现象 | 原因与处理 |
| --- | --- |
| 进游戏报 `failed to load app0:module/...` | VPK 里缺 GPU 模块，按 [2.1](#21-先准备-gpu-运行模块必须最容易漏) 补齐后重新打包安装 |
| 游戏启动就报资源错误或黑屏 | 资源没在 PC 上归一化/解密；加密 XP3 无法在主机上打开 |
| 没有声音 | 音频仍是 Opus 没有转 AT9，或转换时没提供 `at9tool.exe` |
| 画面拉伸、裁切或错位 | 生成 PSV 资源时画布尺寸填错，重新转换 |
| 卡顿明显 | 打开性能显示确认 FPS；把特效画质调到均衡或低画质；优先压缩纹理和转 AT9 |
| 提示内存不足 | 缩小画布、启用纹理压缩、减少同时加载的资源，或降低特效画质 |
| 修改设置后不生效 | 确认设置页已保存；游戏内设置按游戏分别保存 |
| 想换回原版画面 | 转换输出是新目录，原始游戏未被修改，重新转换即可 |

## 桌面端

桌面路径用于调试、验证资源和预览剧情，需要包含 `include`/`lib`/`bin` 的 FFmpeg SDK 与 libclang：设置 `FFMPEG_DIR` 和 `LIBCLANG_PATH`，并让 SDK 的 `bin` 进入 `PATH`（或提供匹配的 DLL）。SDK 不随仓库分发，其他平台需要对应的原生开发库。

```text
cargo build -p krkr-rs --release
cargo run -p krkr-rs -- tjs eval "1 + 2;"
cargo run -p krkr-rs -- tjs disasm --expr "var x = 2; x * 3;"
cargo run -p krkr-rs -- engine eval "Debug.message(System.getTickCount());"
cargo run -p krkr-rs -- play <游戏目录>
cargo run -p krkr-rs -- play <游戏目录> --debug
cargo run -p krkr-rs -- play <游戏目录> --show-stats
```

`tjs` 只运行语言与标准库；`engine` 额外安装引擎类和桌面服务（`--host headless` 或 `desktop`）；`play` 打开桌面窗口并执行入口脚本，顺序是安装 `xp3filter.tjs`、执行 `patch.tjs`、执行入口，支持 `--entry`、`--encoding`、`--data-dir`、`--no-filter` 和 `--no-patch`。

控制台默认只输出警告和错误。`--log-level off|error|warn|info|debug|trace` 控制级别，`--debug` 打开调试信息与脚本 Debug 输出，`--show-stats` 打开左上角帧率与内存面板。逐次慢调用计时只在 `trace` 下打印。

性能分析使用 [krkr-profiler](tools/krkr-profiler/README.md)，可录制 VM、GC、图片加载、渲染与内存数据并生成热点报告。

## 离线资源工具

除 `helper` 交互流程外，`krkr-convert` 也提供子命令，路径参数为占位符：

```text
cargo run -p krkr-convert --release -- helper
cargo run -p krkr-convert --release -- helper <游戏目录>
cargo run -p krkr-convert -- xp3 decrypt "game/*.xp3"
cargo run -p krkr-convert -- xp3 unpack "game/*.xp3" patch.xp3
cargo run -p krkr-convert -- xp3 unpack encrypted.xp3 --xp3-filter game/xp3filter.tjs
cargo run -p krkr-convert -- xp3 pack <资源目录>
cargo run -p krkr-convert -- xp3 pack <父目录> --each
cargo run -p krkr-convert -- probe <已解包目录> -o probe.json
cargo run -p krkr-convert -- normalize <游戏或资源目录>
cargo run -p krkr-convert -- adjust probe.json -o adjusted.json
cargo run -p krkr-convert -- psv <已归一化的松散资源目录> --canvas 1920x1080
cargo run -p krkr-convert --release -- psv <已归一化的松散资源目录> --canvas 1920x1080 --texture-glob "bg/*.png"
```

资源处理的关键结论：

- PSV 图片默认「自动筛选并压缩」：不透明用 BC1（4bpp）、透明用 BC3（8bpp），保持直通 Alpha 与输入色彩空间。CLI 默认 `--texture-storage bc-crunch`，即先用 rgbcx 编码 BC、再用 bc-crunch 无损压缩块数据；`--texture-storage bc` 省去运行时解包。
- `--texture-auto --texture-quality fast|balanced|high` 三档都检查实际解码后的颜色、Alpha 和黑白背景合成误差，不达标就保留无损 PNG，不放宽验收门槛。BC 编码默认启用离线抖动，减轻渐变色带。
- 存储轴长不超过 1024 时向上取 2 次幂（最小 8），超过时向上取 1024 的倍数再拆分；逻辑尺寸由 `.krkr-scale` 保留。960×544 背景存为 1024×1024，BC1 载荷 512 KiB、BC3 1 MiB，而原尺寸 RGBA8 约 2040 KiB。别把整款游戏的显存直接除以 4 或 8 来估算。
- 规则图、色键、遮罩和省份图不能只凭文件名判断能否转有损格式，自动模式会按实际像素和整款游戏的遮罩关系跳过它们。
- 音频以文件名约定为准：`.ogg/.oga` 会规范为 Ogg/Vorbis，容器内的 Opus 会被识别并转码；带 `.sli` 的循环音频会比较采样率、声道和实际解码采样数，无法保持时拒绝替换，不改写循环点。
- 视频固定转 MP4（H.264 Main / AAC-LC），并生成同名 `.krkr-mp4` 标记；两种附属文件都要随资源一起拷贝。AMV 保留专用解码。
- 就地修正以单文件为事务边界，XP3 解包与 PSV 转换先写暂存目录，全部成功才发布输出目录。

## 文档

| 文档 | 内容 |
| --- | --- |
| [PSV](docs/PSV.md) | PSV 构建细节、打包配置、真机验证、内存与图形策略、线程等待 |
| [软件设计与架构](docs/软件设计与架构.md) | 模块职责、执行流程、资源寿命与实现边界 |
| [插件兼容清单](docs/插件兼容清单.md) | 注册名称、实现入口、已知差异与未接入目标 |
| [兼容补丁](docs/兼容补丁.md) | `patch.tjs` 接口、插件替代与扩展、脚本回调与示例 |
| [增量 GC](docs/增量GC.md) | 回收阶段、自动策略、原生共享状态约束与性能测量 |
| [插件反编译经验](docs/插件反编译经验.md) | 无源码插件的静态分析方法与常见误判 |
| [修改历史](CHANGELOG.md) | 版本变化与性能优化记录 |

## 开发检查

测试按职责放在各 crate 的 `tests/` 或源码测试模块中。字体夹具位于 `crates/krkr-render/tests/fixtures/fonts/`，引擎图像与光标夹具位于 `crates/krkr-engine/tests/fixtures/`，测试不依赖根目录示例或本地参考仓库。

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p tjs-core -p tjs-front -p tjs-bind -p tjs-runtime
cargo test -p krkr-engine -p krkr-render
```

Windows 上可借助 ANGLE 运行已适配的 GLES 像素与显存预算测试：把 `libEGL.dll`、`libGLESv2.dll` 及其依赖所在目录加入 `PATH` 后执行

```text
cargo test -p krkr-render-gles2 -p krkr-host-vita --features krkr-host-vita/windows-gles-tests
```

该开关不启用仅支持 Linux 的测试，也不代替 PSV 实机验证。按改动范围选择检查项；GPU 与媒体测试分别需要可用的图形设备和媒体环境。发行时请一并保留[内置字体许可](crates/krkr-render/fonts/QiushuiShotai-LICENSE.txt)及所用组件的许可文件。
