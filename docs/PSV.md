# PSV

## 构建与打包

维护原生 PVR 驱动时，使用独立仓库 [`pvr-psp2-sys` 的驱动构建入口](https://github.com/jhq223/pvr-psp2-sys)：Windows Cargo 调用 CMake 和官方 PSVSDK，生成四个 `.suprx`。下文的 WSL/VitaSDK 流程负责 Rust 引擎；普通引擎构建不会重新编译驱动。

PSV 宿主为 `krkr-host-vita`，程序名为 `krkr-vita`。以下命令在 WSL2 的 Bash 中执行，工作目录为包含 `crates/host-vita/Cargo.toml` 的仓库根目录。

### 构建环境

已验证的环境：

| 项目 | 配置 |
| --- | --- |
| 系统 | WSL2 / Ubuntu |
| Rust | `nightly`，安装 `rust-src` |
| 打包工具 | `cargo-vita 0.2.2` |
| VitaSDK | 安装于 `/usr/local/vitasdk`，`arm-vita-eabi-gcc` 10.3.0 |
| 目标 | `armv7-sony-vita-newlibeabihf` |

已有 VitaSDK 构建环境可以直接复用。新环境先安装 WSL2、Rustup 和 [VitaSDK](https://vitasdk.org/#installing)，然后在 WSL2 中准备 Rust 工具链与打包工具：

```sh
export VITASDK=/usr/local/vitasdk
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$VITASDK/bin:$PATH"

rustup toolchain install nightly --profile minimal --component rust-src
cargo +stable install cargo-vita --version 0.2.2 --locked
```

若 VitaSDK 安装在其他位置，将 `VITASDK` 改为实际路径。已有对应版本时无需重复安装。可用下面的命令检查工具是否可用：

```sh
cargo vita --version
rustup component list --toolchain nightly --installed
command -v arm-vita-eabi-gcc vita-elf-create vita-make-fself vita-mksfoex vita-pack-vpk
```

`cargo vita` 会为 Vita 目标构建标准库并配置原生编译、链接及打包流程。PSV 宿主不链接桌面的 wgpu、CPAL 或 FFmpeg；离线资源转换工具所需的 FFmpeg 环境单独准备。

### 生成 VPK

在仓库根目录执行：

```sh
export VITASDK=/usr/local/vitasdk
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$VITASDK/bin:$PATH"
export RUSTUP_TOOLCHAIN=nightly

cargo vita build vpk --release -p krkr-host-vita --locked
```

必须带 `--release`，宿主代码会拒绝 Vita 的 debug 构建。`cargo-vita 0.2.2` 要求 nightly，当前依赖也要求 Rust 版本不低于 1.98.1；旧的 `nightly-2026-06-15` 不再满足要求。`RUSTUP_TOOLCHAIN` 只设置当前终端使用的工具链；桌面和转换器仍可通过 `cargo +stable ...` 构建。

无需先单独生成 ELF，`build vpk` 会完成中间步骤。默认安装包位置为：

```text
target/armv7-sony-vita-newlibeabihf/release/krkr-vita.vpk
```

需要将 PSV 构建缓存放在独立目录时，使用：

```sh
cargo vita build vpk --release -p krkr-host-vita --locked --target-dir target/vita-build
```

对应产物位置为 `target/vita-build/armv7-sony-vita-newlibeabihf/release/krkr-vita.vpk`。同一输出目录下还会生成 `.elf`、`.velf`、`.self` 和 `.sfo`。只检查编译、链接而不打包时，可执行：

```sh
cargo vita build elf --release -p krkr-host-vita --locked
```

依赖已缓存时可在构建命令末尾追加 `--offline`。首次构建需要获取 Cargo 依赖及标准库构建所需依赖。

SGX 着色器预编译使用独立子进程并行执行，进程数受可用 CPU 数、Cargo 任务上限和当前空闲任务额度限制。可通过 `CARGO_BUILD_JOBS` 调整 Cargo 上限；设为 `1` 时串行执行。原生编译器有进程级全局状态，同一进程内的多线程编译仍会串行。每个子进程持有独立编译器上下文，最终着色器索引按固定顺序生成，构建日志会显示编译进程数和着色器编译耗时。

### 打包配置与自带资源

打包配置位于 [host-vita/Cargo.toml](../crates/host-vita/Cargo.toml) 的 `[package.metadata.vita]`，应用标题为 `KRKR`，Title ID 为 `KRKRJHRST`。启动器显示版本 `0.2.1`，Vita 应用信息显示 `00.21`。`assets = "runtime"` 会将 [runtime](../crates/host-vita/runtime) 中的资源直接加入 VPK：

```text
eboot.bin
sce_sys/
├── param.sfo
├── icon0.png
├── pic0.png
└── livearea/contents/
    ├── template.xml
    ├── bg0.png
    └── startup.png
module/
├── libGLESv2.suprx
├── libgpu_es4_ext.suprx
├── libIMGEGL.suprx
└── libpvrPSP2_WSEGL.suprx
licenses/
├── QiushuiShotai.txt
├── Nivora-MPL-2.0.txt
└── PromptFont-OFL.txt
```

`eboot.bin` 和 `param.sfo` 由构建工具生成，其余文件来自仓库。LiveArea 使用已有背景和原创美少女图标；模板、气泡图标和启动画面均已准备好。PVR 链接桩与运行模块也已纳入仓库，构建无需读取 `ref/` 或再次从 ons-rs 复制文件。

Vita 打包沿用 ons-rs 的 `ATTRIBUTE2=12` 内存配置和 `0x2F00000000000001` SELF 权限，同时在 `[package.metadata.vita.profile.release]` 中设置 `strip_symbols = false`，保留 ELF 转换和 PVR 初始化所需符号。这是 cargo-vita 的打包选项，不是 Cargo 的 `[profile.release]`；正常打包日志不应出现 `Stripping symbols from elf`。

秋水书体通过 `include_bytes!` 内嵌在程序中，启动器与游戏使用同一份字体资源；VPK 中没有单独的 TTF，也无需在 PSV 上另外安装字体。启动器使用 Nivora 的 ab_glyph 后端，启动游戏前释放 UI 的字体副本和字形图集。秋水书体、PromptFont 与 Nivora 的许可随包放入 `licenses/`。

正常构建直接使用 `runtime/` 内已提交的最终 PNG，不需要 Node.js、图片处理工具或额外的素材生成脚本。

LiveArea 图片按 [livearea-specs](https://github.com/hammerill/livearea-specs) 及其链接的[更新说明](https://gist.github.com/hammerill/64411eebf071b93396b7d310ba8d6776) 检查：四张图片采用 8-bit 索引色、不交错；`icon0.png`、`pic0.png`、`bg0.png` 不允许 `tRNS` 透明信息，即使透明调色板项没有被像素使用；只有 `startup.png` 可以透明。`pic0.png` 需要完整的 256 项调色板。`build.rs` 会在构建时校验尺寸、上述 PNG 条件及 420 KiB 文件大小上限，不符合时直接报告文件名并中止。

`template.xml` 使用 `a1` 布局，引用同目录的 `bg0.png` 和 `startup.png`，保存为无 BOM 的 UTF-8、CRLF 换行。修改图片时保留其色彩；指导中的 `-pix_fmt ya8` 会将图片转成灰度，不应直接用于彩色图标。

### 安装

将生成的 VPK 传到 PSV，通过 VitaShell 安装。更新 GPU 模块、LiveArea 或 SFO 配置后，请安装完整 VPK。

### 实机验证

PSV 的画面、驱动兼容性、音视频、存档和性能均在实机上验证。普通构建、格式与 Clippy 检查在 Windows 上运行；VPK 构建使用上文的 WSL2 环境。

比较优化前后的表现时，使用相同的游戏资源、存档、场景和系统设置，并分别记录首次启动与同一进程内重复进入的结果。日志可通过 vdb 收集，帧率与内存可通过游戏设置中的性能显示查看。逐阶段录制与离线分析见 [krkr-profiler](../tools/krkr-profiler/README.md)。

### 运行与诊断

游戏内长按 SELECT 350 ms 呼出虚拟键盘和按键映射，短按仍为 Enter。方向键选择、○ 按键、× 关闭，Ctrl/Alt/Shift 点击锁定，L/R 改变上下停靠，□ 切换映射页。映射页可录制手柄组合键，绑定键盘组合键、鼠标键或滚轮，△ 保存到当前游戏的 `krkr-input.tsv`。START+SELECT 保留为返回启动器。界面仅打开时创建纹理，选择变化仅更新变化区域，关闭后释放；该面板用于游戏操作，不作为文字输入法。

默认左摇杆移动鼠标，○/× 为左右鼠标键，△ 为 Ctrl，□ 为空格，START 为 Esc，L/R 为 PageUp/PageDown。右摇杆上下发送鼠标滚轮；触屏单指点击或拖动滑块，双指上下滑动滚动内容。双指手势会释放已有鼠标拖动，所有手指离开后才恢复单指点击。滚动行为由游戏的 `onMouseWheel` 决定。

纹理转换会解码检查实际编码结果：大面积不透明像素变透明、Alpha 误差过大时，自动退回 PNG；自动模式还限制可见颜色误差。回退保留标签、资源 link 和逻辑尺寸，存储按显示密度缩放，无需补齐为 POT。已有有损纹理无法恢复丢弃的色彩或透明度，需从原素材重新转换。压缩纹理主要减少资源读取、上传和只读纹理占用，后续编辑与多层合成仍有 RGBA 工作纹理开销，不能据此直接推算帧率倍数。

预处理后的每个游戏放在 `ux0:/data/KRKR/<游戏目录>/`。启动器使用 Nivora，以 960×544 原生尺寸显示游戏库、游戏详情、界面设置和启动文件浏览器。

| 操作 | 按键 |
| --- | --- |
| 移动焦点 | 方向键或左摇杆；按住可连续移动 |
| 启动所选游戏 / 确认 | ○ |
| 游戏设置 | △（游戏库）；□ 也可打开 |
| 返回 / 在游戏库请求退出 | ×；退出需要确认 |
| 刷新游戏库 | START（游戏库） |
| 游戏库翻页 | L / R |
| 界面设置 | SELECT（游戏库） |

触摸支持点按、列表拖动和滚动条；底部带动作的按键提示也可点按。设置分为“界面”和“关于”，提供简体中文、英文、日文、深浅主题及动效开关。游戏设置分为“常规”和“诊断”，侧栏可直接启动当前游戏。游戏库采用虚拟列表，目录扫描与 XP3 索引读取在后台执行，等待期间显示转圈和状态文字，不显示加载图片。

游戏详情中可指定启动文件、调整光标速度，并独立控制引擎诊断、游戏脚本日志和性能显示。文件浏览器支持散文件和 XP3 内部路径，返回键逐级返回，“使用默认入口”恢复 `startup.tjs`。设置保存在 `ux0:/data/KRKR/launcher.tsv`，兼容已有配置，并记住上次选择的游戏。三个开关默认关闭，按游戏保存。引擎诊断开启 Debug 级别的启动和资源信息，不输出逐帧慢调用。警告和错误默认保留，关闭的日志跳过参数求值。游戏脚本日志控制脚本 Debug 输出，不改变脚本注册的日志回调。致命错误和 panic 始终输出。选择游戏后显示静态加载提示，保留到首个可见游戏画面就绪；等待时不重复绘制。脚本日志、引擎诊断和完整启动、运行错误统一输出控制台，由外部日志插件收集；引擎不创建专用日志文件。

启动器会在选定的游戏入口之前自动执行根目录 `patch.tjs`，默认入口、散文件和 XP3 入口行为一致；补丁不存在时跳过，失败时停止启动。无需增加引导脚本。完整示例与接口说明见[兼容补丁](兼容补丁.md)。

音频预处理以游戏文件名的使用约定为准：转换器将 `.ogg/.oga` 规范为 Ogg/Vorbis，识别并转换容器内的 Opus。PSV 的扩展音频后端接入 `sceAudiodec` ATRAC9；未转换的 Opus 不能直接在 PSV 播放，PC 的 Opus 路径依赖 `wuopus/wuffmpeg` 插件启用。

已有 PSV 资源中的 `.ogg` 若仍是 Opus，可以用新版 `krkr-convert helper` 选择保持尺寸的媒体格式修正，输出新的规范化游戏目录，无需再次缩放。带 `.sli` 的音频会比较采样率、声道与实际解码采样数；编码器不能保留采样数时拒绝替换，不改写循环点。

`krkr-convert helper` 的 PSV 流程可选择全部音频转 AT9 和自动筛选压缩图片。纹理编码使用内置 rgbcx，不需要 texconv 或 PVRTexTool，也不产生中间 TGA/DDS；AT9 仍需自行把 `at9tool.exe` 放在转换器同目录。CLI 的 `--texture-quality fast|balanced|high` 和 helper 均可选择编码档位，默认平衡档按块精修，所有档位保持相同验收门槛。不透明图片生成 BC1 RGB 4bpp，透明图片使用 BC3 RGBA 8bpp，保留完全透明/不透明的 Alpha 端点，离线转换成 Vita 原生块顺序，由配套的新驱动采样；编码后检查可见颜色、Alpha 及黑白背景合成误差。带显存节省约束的密度重试、自动筛选及无损回退说明见 [转换器说明](../README.md)。

图片、音频转换共用事务和原始名称映射，适用于松散资源和多 XP3 游戏。摘要显示实际 AT9、BC1、BC3 数量与保留无损的原因。手动资源匹配是可选覆盖方式。脚本化的 `psv` 命令通过重复的 `--at9-glob` 选择音频；纹理使用 `--texture-auto`。示例：

```powershell
krkr-convert psv "D:\game-unpacked" --canvas 1920x1080 --at9-glob "bgm/*.ogg" --at9-glob "voice/*.ogg"
```

支持单轨、单声道或双声道、8–96 kHz 输入。离线解码为 16-bit PCM 后，使用 `asetrate=48000` 只改变编码器输入的采样率标记，不增删采样，再编码 AT9。RIFF 数据前的 `krSR` chunk（8 字节：little-endian `version=1, source_rate`）记录原采样率；运行时仍按原采样率混音。因此 44.1 kHz 的 `.sli` 循环、标签和脚本采样位置保持原索引，尾部填充及编码延迟不进入游戏时间轴。该 AT9 在忽略 `krSR` 的普通播放器中会按 48 kHz 播放，应由本引擎读取。转换是有损的，输入文件保留；导出目录使用真实 `.at9` 后缀与逻辑名称链接。

发布转换结果前校验源 PCM 采样数、AT9 `fact` 时间轴以及外部工具单次解码后的采样数。PSV 通过每路 256 字节对齐的输入/输出缓冲区，逐个 superframe 调用 `sceAudiodecDecodeNFrames`；普通混音、变速和滤镜仍在 CPU。压缩音频由独立线程按 16 KiB 补充预读缓冲区，仅文件尾块可更小；每路容量按码率和资源长度限制在 16–128 KiB，预算不足时回退到直接读取。加载音频时解析头并启动预读，首次播放才申请硬解句柄，解码结束或重定位后释放。库配置最多 16 个同时解码的源声道；并发超过硬件额度会返回错误，不代表 64 路混音都能同时硬解。桌面 FFmpeg 后端应用相同延迟裁剪及源采样率。自动测试覆盖模拟 PCM 解码的重定位、实际 AT9 参考解码、VFS/XP3 名称与循环点，驱动性能和多路并发仍需实机验收。

性能面板每秒更新一次：`FPS` 是实际游戏画面更新次数（静止画面为 0，面板刷新不计帧）；`RAM FREE` 是内核报告的可分配普通内存，不含已预留 newlib 堆内部的空闲块；`GPU` 是渲染器已分配的纹理与临时资源，单位 MiB。关闭时不查询内存、不创建面板纹理。开启后复用游戏画布，不触发 VM 计时器，也不使用 GPU 读回、堆遍历或逐帧日志；实际开销仍以真机为准。PC 使用 `--show-stats`，RAM 显示进程驻留内存。

启动器按需缓存当前页面的字形；使用静态焦点描边，界面和动画均无变化时停止提交画面。`System.createAppLock` 由 Vita 系统宿主提供，同名锁不能重复获取，退出游戏后随引擎释放；Vita 上同一应用仅运行一个实例，因此锁在进程内管理。

### 线程等待

当前 VitaSDK 的 `pthread_cond_init` 未采用时钟属性；定时等待将截止时间交给 `pte_relmillisecs`，后者使用 `ftime` 的墙上时钟。Rust 标准库传入的却是 `CLOCK_MONOTONIC` 截止时间，两者混用会使等待提前超时并反复轮询。

Vita 的 VM、视频解码和音频预读通过内核事件标志等待，使用相对微秒超时。音视频命令回复、SQLite 和性能记录的限时接收通过 `try_recv` 与相对睡眠实现。无超时条件变量仍用于音频预读队列；桌面平台继续使用标准库等待。

参考：[条件变量初始化](https://github.com/vitasdk/pthread-embedded/blob/master/pthread_cond_init.c)、[等待实现](https://github.com/vitasdk/pthread-embedded/blob/master/pthread_cond_wait.c)、[超时换算](https://github.com/vitasdk/pthread-embedded/blob/master/pte_relmillisecs.c)。

### 图形与内存

PSV 按 960×544 屏幕尺寸合成画面。普通画布可缩小存储，文字保留原始采样密度，在最终合成时缩放。已转换的 BC1/BC3 图片直接使用压缩纹理；编辑图片和多层合成仍需要 RGBA 存储。

每个游戏可在启动设置中选择“特效画质”：原生、均衡或低画质，分别以 960、720、480 像素宽为画布上限。最终画面保持屏幕分辨率，文字和直接显示的界面图片不随画布一起缩小；绘入特效画布的内容会变软。均衡和低画质还将魔夜的 ActionManager 动画更新降至约 30 次/秒，动画时长不变。默认原生。

反复绘制的画布可直接作为纹理 FBO 使用，减少中间表面的复制。缓存最多 8 个目标，附件图像合计不超过 8 MiB，其中一半留给合成与特效中间画布。槽位随纹理存储释放。缓存满时，以及需要读取目标旧像素的混合和文字批次，使用独立工作表面。

PVR 驱动会自动回收闲置的底层渲染表面和传输同步对象，保留纹理像素。驱动的表面缓存目标为 16 个，详见[离屏渲染](https://github.com/jhq223/pvr-psp2-sys/blob/main/docs/driver.md#离屏渲染)。

内存配置位于 `crates/host-vita/src/memory.rs`：

| 用途 | 上限 |
| --- | ---: |
| newlib 堆 | 160 MiB |
| sceLibc 堆 | 16 MiB |
| 图形资源总额 | 192 MiB |
| 图形临时资源 | 48 MiB，包含在图形总额内 |
| CPU 暂存 | 32 MiB |
| CPU 位图 | 64 MiB |
| 字体缓存 | 8 MiB |

应用开启扩展内存预算。newlib 堆固定预留 MAIN，图形资源按需分配，驱动先使用 CDRAM，再使用可供 GPU 访问的 MAIN；192 MiB 是两者共用的资源上限，不是预留显存。CPU 暂存、位图和字体分配位于 newlib 堆内。

这些预算统计引擎持有的数据，驱动的对齐、传输缓冲和内部对象还会占用额外内存。内存不足时，引擎回收缓存，并尝试将暂时不用的 RGBA 画布无损压缩到暂存池；恢复画布会增加开销。

图形错误包含失败命令、引擎预算和内核剩余内存。`kernel_free` 不包含 newlib 和驱动已预留内存块内部的空闲空间，不能与引擎预算直接相加。
