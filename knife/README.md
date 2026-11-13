# knife · mofa-vita 引擎工作区

本目录是《魔法使之夜》PS Vita 移植的 Rust 工作区，基于 [AuroraWright/knife](https://github.com/AuroraWright/knife) 0.9.4。**当前构建、安装、素材版本与操作说明统一见 [项目根 README](../README.md)。** 构建工具在上一层 `tools/`，须从外层项目根目录执行。

| 目录 | 职责 |
|---|---|
| `knife/` | 游戏脚本、舞台、UI、文字与输入 |
| `knife-core/` | 归档、脚本、图片和字体等格式解析 |
| `knife-render/`、`knife-render-api/` | 桌面 WGPU、Vita GXM 后端与共享渲染类型 |
| `knife-platform-vita/` | Vita 平台、资源检查、音视频与存储接口 |
| `knife-audio/` | 音频资源、解码与混音 |
| `knife-perf/` | 运行时计时、缓存预算与计数接口 |
| `vita/` | 原生 C 后端、着色器、原生依赖与系统界面资源 |
| `kdu/`、`probes/` | 格式转换工具与开发探针 |

Vita 包固定中文 ID2，使用 960×540 场景和合成画布。编译引擎无需游戏素材；素材由使用者自行准备。换电脑构建时使用 `python3 tools/build-vita.py --check` 检查本地环境，完整步骤见 [构建与工具链](../doc/03-构建与工具链.md)。

当前配套素材为 native540 离线显存限额 + r11 raw UI成员读取包。剧情图保留逻辑坐标与完整长图内容，物理采样尺寸按离线预算限制；PNG编辑工程不能直接代替运行归档。当前交互和显存准入更新沿用r11资源身份；程序固定输出至外层 `release/mofa-vita.vpk` 与 `release/eboot.bin`。清单和安装说明见[素材与安装](../doc/08-素材与安装.md)，制作链见[quality540](../tools/quality540/README.md)。

本地资源与临时产物的保留规则见 [仓库维护与清理](../doc/09-仓库维护与清理.md)；脚本分类见 [tools](../tools/README.md)。

Vita阅读设置由 `story_font.rs`、`vita_controls.rs` 与 `vita_input.rs` 配合 `Game` 实现：L+□调整剧情字号50%～150%，○保存、×取消；配置独立于故事存档。模块职责、状态保护及测试边界见[阅读与交互设置](../doc/11-可选交互更新.md)。

## 桌面开发

以下命令在本目录执行，使用匹配的桌面资源：

```bash
cargo +1.99.0 build --locked --release -p knife
cargo +1.99.0 run --locked --release -p knife -- -a /path/to/desktop-assets -l zc
```

桌面资源与 Vita 专用 GXT 包不能直接混用。Linux 桌面构建需要 GStreamer 开发包和运行时解码插件；macOS 影片使用 VideoToolbox，Windows 使用 Media Foundation。桌面配置示例见 [knife.ini.example](knife.ini.example)，Vita 入口不读取此配置。

上游来源见 [UPSTREAM.txt](UPSTREAM.txt)，引擎机制研究见 [engine-writeup.md](engine-writeup.md)。引擎沿用 [MPL 2.0](LICENSE)；Vita 第三方依赖的来源和许可证见 [vendor/shacccg-ext/NOTICE](vita/vendor/shacccg-ext/NOTICE) 与同目录 `LICENSE`。游戏本体、字体、图片、音视频及汉化资源由使用者自行准备。

## English

This directory is the Rust engine workspace for the PS Vita port. Build from the outer repository root using `python3 tools/build-vita.py game release`. The native backend is GXM at 960×540; the desktop WGPU backend remains for development. The Vita UI currently uses Chengkong Chinese. See the [English project README](../README.en.md) for installation, logging, optimizations and release status. Full game assets are supplied separately.
