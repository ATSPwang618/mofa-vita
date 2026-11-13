# mofa-vita · 魔法使之夜

**PS Vita 原生 GXM 移植引擎 · 960×540**

[![English](doc/language-en.svg)](README.en.md)

[项目官网](https://atspwang618.github.io/myvita.github.io/mofa-vita/) · [实机演示](https://www.bilibili.com/video/BV1vUxn6nE59) · [技术文档](doc/README.md) · [优化清单](doc/05-优化记录与实测数据.md)

基于 [AuroraWright/knife](https://github.com/AuroraWright/knife) 0.9.4，以 Rust 引擎运行用户自行准备的《魔法使之夜》Switch 数据。Vita 使用原生 **GXM**，场景与合成画布为 **960×540**，居中输出至 960×544 屏幕；保留桌面 WGPU 后端供开发对照。

仓库提供引擎源码、构建工具、离线 Shader 和资源元数据。游戏本体、字体、音视频与汉化数据需自行准备，**编译引擎无需整套游戏素材**。详细提取步骤见 [Switch 原版资源提取指南](doc/10-Switch原版资源提取.md)。

## 实机截图

| 标题 | 语音设置 |
|---|---|
| ![标题画面](doc/screenshots/01-title.jpg) | ![语音设置](doc/screenshots/02-voices.jpg) |
| **书库** | **剧情** |
| ![书库界面](doc/screenshots/03-archive.jpg) | ![剧情画面](doc/screenshots/04-story.jpg) |

历史实机截图，用于展示游戏界面，不代表最新候选版本的性能验收。

## 当前版本

本地候选标识为 `perf-render-20261109-213051-210a811-story-font-local`，应用 ID 为 `KNIF00540`。标识中的时间来自本地 `perf/render` 基准提交；`local` 表示包含未提交修改，具体构建用收据及 SHA256 区分。当前构建身份与验证结果见 [安装说明](doc/08-素材与安装.md)。

- **全游戏材质选路**：85 个离线 GXP，覆盖 24 种 PS 混合及有限精简材质组合；不符合条件时回退完整路径。
- **降低 GPU 与资源开销**：原生 DXT1/DXT5 上传、目标色读取、Group 合成与保守裁剪、按需读取、共享缓存及安全退休。
- **稳定性保护**：大 UI 图集按成员读取、纹理压力下有界回收重试、最近有效进度保护、存读档焦点修复、BGM 约一秒 PCM 预读。
- **[阅读与交互设置](doc/11-可选交互更新.md)**：L+□ 打开菜单，剧情字号 50%–150%、每档 5%，○ 保存、× 取消；放大长段落跟随显示进度滚动。背板模拟 L2/R2 默认关闭；PS 按键提示随包提供。剧情回退、快进与场景跳转仍禁用，自动播放保留。
- **本地新增构建流程**：GitHub Actions 配置已加入；尚未在云端运行。显存余量读数误拒绝修复也已接入，见 [更新范围](doc/11-可选交互更新.md)。

完整的开关、历史收益与未验证项见 [优化清单](doc/05-优化记录与实测数据.md)。启用实验不等于已证明收益，当前不保证全游戏 20 FPS 或永不发生资源失败。

## 构建与安装

在 Linux/WSL 的仓库根目录执行，使用 VitaSDK 与固定的 Rust `nightly-2026-05-28`：

```bash
python3 tools/build-vita.py --check
KNIFE_BUILD_TAG=<本次版本号> python3 tools/build-vita.py game release
```

Windows 环境及详细依赖见 [构建文档](doc/03-构建与工具链.md)。成功构建后固定输出：

| 本地文件 | 用途 |
|---|---|
| `release/mofa-vita.vpk` | 安装包，不含整套游戏资源 |
| `release/eboot.bin` | 关闭游戏后，通过 VitaDeck 更新 `ux0:app/KNIF00540/eboot.bin` |
| `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets/` | 配套资源，复制到 `ux0:data/WitchontheHolyNight/assets/` |

已安装当前 r11 配套资源时，本次只需更新程序。Vita 界面固定澄空中文 ID2，资源保留英语/中文。清单校验、旧包升级与故障恢复见 [安装说明](doc/08-素材与安装.md)。

VitaDeck 日志默认开启；数据根目录下的 `perf-enable.txt` 显式 `0` 会关闭日志，`1` 会开启。启动核对 `build_tag`，网络变更时核对接收端配置，见 [日志说明](doc/06-性能采集与分析.md)。

## 仓库导航

[knife 引擎](knife/README.md) · [tools 工具](tools/README.md) · [doc 技术文档](doc/README.md) · [维护规则](doc/09-仓库维护与清理.md)

`release/` 保存当前程序；`dist/` 只保留一份运行资源与一份可编辑 PNG 工程；`.work/` 保存构建暂存、符号及验证记录，均不入 Git。修改默认仅留本地，不自动推送。

## 来源与许可

引擎基于 [AuroraWright/knife](https://github.com/AuroraWright/knife)，沿用 [MPL 2.0](knife/LICENSE)，详见 [上游来源](knife/UPSTREAM.txt)。第三方依赖保留 `knife/vita/vendor/` 下的许可证与说明。游戏内容归各权利人所有，不随源码提供。特别感谢 @SpriteLisenLisen @DeQxJ00 为项目提供的技术支持。
