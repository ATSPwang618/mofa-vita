# krkr-rs

Rust 实现的 Kirikiri/TJS2 引擎，包含语言前端与虚拟机、KAG 解析器、引擎脚本类、静态插件兼容实现、桌面宿主及独立资源转换工具。

桌面路径使用 winit、wgpu、cpal 和 FFmpeg。PSV 宿主使用 PVR/GLES2、SceAudioOut 和 AvPlayer，已通过真机安装并能显示游戏 UI 和剧情；当前实测帧率仍低，性能与游戏兼容性仍在改进，详见 [PSV 文档](docs/PSV.md)。插件名称可加载也不代表整个游戏已经兼容。

## 文档

- [修改历史](CHANGELOG.md)：版本变化与性能优化记录。
- [PSV](docs/PSV.md)：PSV 构建、VPK 打包内容与安装说明。
- [软件设计与架构](docs/软件设计与架构.md)：当前代码的模块职责、执行流程、资源寿命和实现边界。
- [增量 GC](docs/增量GC.md)：回收阶段、自动策略、原生共享状态约束和性能测量。
- [插件兼容清单](docs/插件兼容清单.md)：注册名称、实现入口、已知差异与未接入目标。
- [兼容补丁](docs/兼容补丁.md)：patch 加载、插件替代与扩展、脚本回调、G 弦示例和常见错误。
- [插件反编译经验](docs/插件反编译经验.md)：无源码插件的静态分析方法与常见误判。

## 构建与运行

使用 Rust 1.98.1 或更新的 stable 工具链，依赖版本以 [Cargo.lock](Cargo.lock) 为准。桌面宿主通过 `ffmpeg-next` 动态链接 FFmpeg；Windows 构建需要 MSVC 工具链、包含 `include/lib/bin` 的 FFmpeg SDK，以及 libclang。设置 `FFMPEG_DIR` 和 `LIBCLANG_PATH`，运行时将 SDK 的 `bin` 加入 `PATH` 或提供匹配的 DLL。SDK 不随仓库分发；其他平台也需要对应的原生开发库。

```text
cargo build -p krkr-rs --release
cargo run -p krkr-rs -- tjs eval "1 + 2;"
cargo run -p krkr-rs -- tjs disasm --expr "var x = 2; x * 3;"
cargo run -p krkr-rs -- engine eval "Debug.message(System.getTickCount());"
cargo run -p krkr-rs -- play <游戏目录>
cargo run -p krkr-rs -- play <游戏目录> --debug
cargo run -p krkr-rs -- play <游戏目录> --show-stats
```

`tjs` 只运行语言与标准库。`engine` 安装引擎类和桌面服务，默认 `--host headless`，可选择 `desktop`。`play` 默认启动桌面窗口并执行 `startup.tjs`，按顺序安装游戏根目录的 `xp3filter.tjs`、执行 `patch.tjs`、执行入口；支持 `--entry`、`--encoding`、`--data-dir`、`--no-filter` 和 `--no-patch`。

离线资源操作使用单独的工具；以下路径是调用参数占位符：

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

`helper` 使用 inquire 交互引导，提供 PSV 游戏资源生成、保持尺寸的资源归一化与 link、XP3 解包和封包。选择游戏目录后，自动处理松散文件和 XP3，发现 `xp3filter.tjs` 时可选择应用，并预填脚本编码；PSV 画布从 `Config.tjs` 中的 `scWidth/scHeight` 常量赋值识别，表达式或冲突配置需手动填写。目标固定适配 960×544，视频自动使用 H.264 Main / AAC-LC，AMV 保留专用解码。扫描后可选择全部或指定路径的音频转 AT9、自动筛选图片并转换 BC1/BC3 压缩纹理；也可分别保留常规音频和无损图片策略。纹理编码通过 crates.io 的 rgbcx/bc-crunch 库内置；AT9 编码仍需将自行提供的 `at9tool.exe` 放在 `krkr-convert.exe` 同目录，与终端当前目录无关。项目不嵌入或分发 SDK 编码器。

PSV 图片步骤默认提供“自动筛选并压缩图片（含透明纹理）”，无需填写资源路径。不透明候选使用 BC1 RGB（4bpp），透明候选使用 BC3 RGBA（8bpp），保持直通 Alpha 和输入色彩空间。CLI 默认 `--texture-storage bc-crunch`，helper 推荐“BC＋无损封装”：先由 [rgbcx](https://crates.io/crates/rgbcx) 编码 BC，再由 [bc-crunch](https://crates.io/crates/bc-crunch) 无损压缩块数据。运行时解包并排列成 Vita 原生布局，不展开 RGBA。封装不再引入画质损失，也不额外减少 GPU 字节数；无法缩小的分块直接存储原生 BC。`--texture-storage bc` 省去运行时解包。无需纹理编码器 exe 或中间 TGA/DDS 文件。

`psv --texture-auto --texture-quality balanced` 及 helper 提供 `fast`、默认 `balanced`、`high` 三档。三个档位都检查实际解码的颜色、Alpha 和黑白背景合成误差，不合格时保留无损 PNG，不放宽验收门槛。BC3 保留完全透明/不透明端点。平衡/高质量档对颜色失败的图片最多尝试两次单轴加密，且 GPU 压缩负载须不超过正常显示密度 RGBA 的 75%，脚本逻辑尺寸保持不变。遮罩、省份图和标签的保护规则不变。

BC 编码默认启用离线抖动，减轻天空等平滑渐变的色带；压缩格式和纹理载荷大小不变，也不增加运行时处理。抖动会引入少量细颗粒噪声，BC 仍然是有损格式；平均颜色误差不能独自保证渐变观感。

纹理基础库使用 crates.io 发布的 `rgbcx 0.1.2` 和 `bc-crunch 0.1.1`，实现为安全 Rust，许可证及上游鸣谢见各自仓库。转换器按文件并行编码，不为每张图片再建立线程池。可选的额外 GPU 端点搜索由 rgbcx 库提供，尚未接入转换器的 BC 路径。AT9 工具由使用者自行提供，项目不分发 SDK 编码器。

`psv --texture-glob "bg/*.png"` 可按相对路径选择图片，可重复传入，默认不启用。按实际像素选择 BC1/BC3；规则图、色键、遮罩和省份图不能仅凭文件名安全转有损格式。选择使用真实资源名，旧脚本名通过 link 保留。存储轴长不超过 1024 时向上取 2 次幂（最小 8），超过时向上取 1024 的倍数，再拆成独立纹理；逻辑尺寸通过 `.krkr-scale` 保留。960×544 背景会存为 1024×1024，BC1 载荷 512 KiB，BC3 1 MiB；原尺寸 RGBA8 约 2040 KiB。应比较实际尺寸后的占用，不能直接把整个游戏显存除以 8 或 4。

转换结果沿用带标签/分块元数据的 KTX 容器，但 BC 块在离线阶段排成 Vita 原生 Morton 顺序，并使用明确的私有格式编号；不是把原生排列冒充标准 S3TC KTX。新版驱动通过 `GL_KRKR_texture_compression_bc` 直接采样，上传仍有压缩字节搬运，不在运行时解压 RGBA 或重新 swizzle。首次修改只将受影响的块转为可写 RGBA，CPU 像素操作按需展开，预算按实际格式计费。新资源需要配套的新引擎和重编驱动；原驱动不能采样这些私有格式。ETC1/PVRTC 的普通图片读取和驱动能力仍保留，转换器不再生成。压缩不保证 XP3 小于原 PNG/JPEG，也不会压缩特效的 RGBA 工作画布。

PSV helper 要求输入已归一化资源：先运行归一化，再选择其输出目录生成 PSV 资源。PSV 步骤检查格式一致性后直接执行硬件资源转换，不重复归一化或重建已有资源 link；发现格式不一致时会停止并提示先归一化。

BC＋无损封装输出 `.kbct`（KBCT v1）引擎容器，保存拼接尺寸、标签及分块。编码标记 0 表示原生 BC，1 表示 bc-crunch range stream。解包另用一个分块缓冲，固定模型工作区按 64 KiB 计费，均计入图像预算。原生 BC 模式输出分块 KTX，使用 KTX1 数组元素及 `krkr.canvas`/`krkr.tags` 元数据；运行时均创建独立 2D 纹理，不要求 GLES2 支持纹理数组。资源和展开后的 BC 块分别限制在约 32 MiB 内，标签最多 1024 项、总元数据不超过 64 KiB；超限时自动模式保留无损资源。摘要报告封装/原生 BC 分块数量、文件字节数和 GPU 块字节数，后者不包含动态画布、FBO 和运行时临时内存。旧 CRN 流程已移除，原 `.kcrn` 资源须重新转换。

封包时需要一起保留附属文件。这是资源缩放和引擎逻辑尺寸适配，尚无真机性能结果。图片读回、某些绘制操作和特效视频可能临时恢复逻辑尺寸；PSB/动态立绘容器、图集边缘、色键与游戏私有格式仍需真实游戏验证。

控制台默认只输出警告和错误。`--log-level off|error|warn|info|debug|trace` 设置引擎日志级别；`--debug` 开启调试信息及脚本 Debug 输出。逐次慢调用计时只在 `trace` 下打印，普通调试不会逐帧刷屏。PSV 的引擎诊断与游戏脚本日志分别在游戏设置中开启。致命错误仍会显示，脚本注册的日志回调不受这些开关影响。

性能分析使用 [krkr-profiler](tools/krkr-profiler/README.md)：录制 VM、GC、图片加载、渲染与内存数据，生成热点报告和时间线。采集不依赖控制台日志级别。`--stats` 则输出一次执行的汇总统计。

`--show-stats` 独立开启左上角的帧率与内存面板，PSV 在游戏设置中开启“性能显示”。默认关闭，不创建面板资源或采样；开启后每秒更新一次小纹理，随画面叠加，不进行 GPU 读回或逐帧日志。FPS 统计实际游戏画面更新，静止画面为 0，面板自身刷新不计入。PC 的 RAM 是进程驻留内存；PSV 的 `RAM FREE` 是系统当前可分配的普通内存，不包含已预留堆内部的空闲块。两端的 GPU 均为渲染器已分配纹理与临时资源字节数，并非 GPU 忙碌百分比。

## PSV

PSV 宿主通过 WSL2 和 `cargo vita` 构建。完整环境准备、构建命令、产物路径及安装方式见 [PSV 文档](docs/PSV.md)。VPK 自带 GPU 模块、LiveArea 资源与内嵌字体，无需额外复制。

游戏放在 `ux0:/data/KRKR/<游戏目录>/`。Nivora 启动器提供游戏库、每游戏启动设置、文件浏览、中英日语言和深浅主题，支持手柄与触摸；按住 START + SELECT 一秒返回列表。游戏内长按 SELECT 呼出虚拟键盘和按键映射，短按仍为 Enter；右摇杆或双指上下滑动发送滚轮，单指用于点击和拖动滑块。详情见 [PSV 文档](docs/PSV.md)。

PSV 与 PC 的 `play` 都会在游戏入口之前自动执行游戏根目录的 `patch.tjs`，文件不存在时跳过。补丁接口、示例和加载规则见[兼容补丁](docs/兼容补丁.md)。

## 开发检查

测试按职责放在各 crate 的 `tests/` 或源码测试模块中。字体夹具及生成脚本位于 `crates/krkr-render/tests/fixtures/fonts/`，引擎与 GPU 测试共用；图像和光标夹具位于 `crates/krkr-engine/tests/fixtures/`。测试不依赖根目录示例或本地参考仓库。

```text
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p tjs-core -p tjs-front -p tjs-bind -p tjs-runtime
cargo test -p krkr-engine -p krkr-render
```

Windows 可使用 ANGLE 运行已适配的 GLES 像素与显存预算测试。将 `libEGL.dll`、`libGLESv2.dll` 及其依赖所在目录加入 `PATH` 后执行：

```text
cargo test -p krkr-render-gles2 -p krkr-host-vita --features krkr-host-vita/windows-gles-tests
```

该开关不启用仅支持 Linux 的测试，也不代替 PSV 实机验证。

按改动范围选择检查；GPU 和媒体测试分别需要可用的图形设备及媒体环境。发行时一并保留[内置字体许可](crates/krkr-render/fonts/QiushuiShotai-LICENSE.txt)及所用组件的许可文件。
