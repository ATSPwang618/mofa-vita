# mofa-vita · 魔法使之夜

**PS Vita 原生 GXM 引擎 · 960×540 演出与合成**

[English](README.en.md) · [项目官网](https://atspwang618.github.io/myvita.github.io/mofa-vita/) · [实机演示](https://www.bilibili.com/video/BV1vUxn6nE59) · [文档](doc/README.md)

基于 [AuroraWright/knife](https://github.com/AuroraWright/knife) 0.9.4。
引擎以 Rust 读取 Switch 游戏数据，在 Vita 的 960×544 屏幕上居中显示 960×540 画面，
桌面 WGPU 后端保留用于开发。游戏数据、汉化、密钥和系统字体由使用者自行提供。

## 安装

1. 用 VitaShell 安装配套的 `dist/mofa-vita.vpk`。
2. 关闭游戏，将 `dist/ux0/` 内的内容按同名目录复制到设备 `ux0:`。
   游戏数据位置为 `ux0:data/WitchontheHolyNight/assets/`。
3. 完整替换 `assets`，保留上级目录中的存档和设置。程序与资源须来自同一次交付。

当前资源采用 **70% 变体缩放、PVRTC2 4bpp / DXT 混合纹理**，
音频存放在 `voice.mrg` 和 `bgm.mrg`，没有散装 `stream/`。
清单和编译索引已经绑定，后续构建会检查一致性。
具体操作见[安装与设置](doc/08-素材与安装.md)。

## 构建与制作资源

- [制作完整资源](doc/10-Switch原版资源提取.md)：入口 `prepare-mofa-vita.py`，
  合成本体与更新，并优先使用澄空 MOD 的同名文件。
- [构建 VPK](doc/03-构建与工具链.md)：使用 VitaSDK、固定 Rust 工具链及同步后的资源元数据。
- [诊断与性能日志](doc/06-性能采集与分析.md)：核对实际运行版本、配置、资源失败及设备日志。

资源转换仍需要外部密钥、固件字体、锁定的 FFmpeg 和 Vita SDK 编码工具。
脚本支持断点续跑；清空全部转换缓存后的完整重建尚未单独验收。
本次交付已完成主机侧资源验收和 VPK 构建检查，最新包的实机画面、性能与长期稳定性待验证。

## 实机截图

| 标题 | 语音设置 |
|---|---|
| ![标题画面](doc/screenshots/01-title.jpg) | ![语音设置](doc/screenshots/02-voices.jpg) |
| ![书库界面](doc/screenshots/03-archive.jpg) | ![剧情画面](doc/screenshots/04-story.jpg) |

截图来自历史实机运行，不作为最新交付包的验收记录。

## 仓库与许可

| 路径 | 用途 |
|---|---|
| [knife/](knife/README.md) | Rust 引擎和原生 GXM 后端 |
| [vita_asset_pipeline/](vita_asset_pipeline/README.md) | 当前资源制作、验收、元数据同步与配套打包 |
| [tools/](tools/README.md) | 引擎构建、诊断及专项开发工具 |
| [doc/](doc/README.md) | 使用与技术说明，历史证据集中在 `doc/evidence/` |
| `dist/`、`release/`、`.work/` | 本地交付物、构建符号和转换缓存，不入 Git |

引擎沿用 [MPL 2.0](knife/LICENSE)，来源见 [UPSTREAM.txt](knife/UPSTREAM.txt)。
第三方依赖保留各自许可。历史资源配方包含游戏派生编码差分，范围见
[配方与许可说明](vita_asset_pipeline/production/tools/source_replay/README.md)。

特别感谢 [SpriteLisen](https://github.com/SpriteLisen) 和 [DeQxJ00](https://github.com/DeQxJ00) 两位作者提供的技术支持，为本项目的实现提供了重要帮助。
同时感谢 @今何求 的技术支持。
