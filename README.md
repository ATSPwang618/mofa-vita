# mofa-vita

《魔法使之夜》（Witch on the Holy Night）Switch 版引擎的 PS Vita 移植，基于 [AuroraWright/knife](https://github.com/AuroraWright/knife) 0.9.4。Vita 使用原生 GXM；桌面后端使用 WGPU。

本仓库提供引擎源码、构建工具、生成元数据、运行时编译器及所需原生依赖。游戏本体、字体、音视频和汉化数据由使用者自行准备，不入库。**编译 VPK 不需要游戏素材。**

## 构建和安装

安装 Python **3.9 或更新版本**后，在项目根目录执行：

```bash
python3 tools/build-vita.py --check
python3 tools/build-vita.py game release
```

`--check` 只检查环境，不安装或编译。正常构建会自动安装缺失的 Rust 工具链及 `rust-src`；也可先执行 `python3 tools/build-vita.py --setup`。Windows 可使用 `py -3` 代替 `python3`，工具会检测 WSL 并转换项目路径。通过 `VITASDK` 或 `--sdk` 指定 SDK，其他选项见 [构建与工具链](doc/03-构建与工具链.md)。

本机仍需能运行的 VitaSDK 核心工具链（GCC、newlib、平台 stubs 和打包工具），以及 CMake、Ninja、Git 和 Bash。png、jpeg、z、ogg、opus、opusfile、taihen 七个静态库及头文件随仓库放在 `knife/vita/vendor/dependencies/`，无需向 SDK 另行安装。shader 编译器模块与 SceShaccCgExt 静态库也保存在 `knife/vita/vendor/`，无需指定本机插件路径。

Vita Rust 固定为 `nightly-2026-05-28`，无需额外安装桌面默认的 Rust 1.99.0。首次 Rust 安装和 Cargo 构建需要联网；已有完整工具链与缓存时可设置 `KNIFE_VITA_OFFLINE=1`。正式版默认 `KNIFE_VITA_QUIET=1`，不启用性能采集输出。

构建成功后，安装包位于 **`release/knife-vita-game-540p.vpk`**，TITLE ID 为 `KNIF00540`。ELF、链接映射和构建收据保存在 `.work/vita/game-release-540p/`，构建日志在 `.work/logs/vita/`。

1. 在支持安装自制应用的 PS Vita 上安装 VPK。
2. 将配套完整 `assets/` 目录复制到 **`ux0:data/knife-woh-540p/assets/`**。
3. 从 LiveArea 启动。当前 Vita 版本固定使用中文 ID2；存档位于 `ux0:data/knife-woh-540p/saves/`。

## 素材与画面

当前引擎匹配 **r7 native540 codec9** 素材包，清单 `assets/vita-540p-assets.json` 的 SHA-256 为：

```text
a66a1318a147a901c7f76e9868e04f0551131f1128bb8dc591c242402de79d41
```

素材需由使用者自备的 Switch 游戏数据及相应汉化资源转换得到。原始 NSP、未经转换的 romfs、旧 codec8 包和其他分辨率版本不能直接替代。VPK 不含游戏素材；已经安装匹配素材包时，无需因重新编译引擎而重复复制素材。

场景、组 pixel/mask 和 UI 合成固定 **960×540**，最终居中输出到 960×544 屏幕，上下各留 2 像素。脚本保留 1920×1080 坐标，局部图片和图集使用各自尺寸。程序不读取 `scene-mode.txt`。主 newlib 堆上限为 256 MiB，各缓存按需使用；这不等于显存预算。

启动出现 `ASSETS REQUIRED`、`WRONG FILE SIZE` 或 `WRONG ASSET VERSION` 时，核对素材完整性与清单版本。更新素材还需同步引擎内嵌 profile、图集索引及音频文件索引，步骤见 [更新素材元数据](doc/03-构建与工具链.md#更新素材元数据)。

## 操作

| Vita 输入 | 剧情中的常用操作 |
|---|---|
| × | 确认、推进文字 |
| ○ | 取消、返回；停止快进或切换消息窗口显示 |
| □ | 回看日志 |
| △ | 系统菜单 |
| L / R | 自动播放 / 快进 |
| START / SELECT | 快速保存 / 快速读取 |
| 方向键、右摇杆方向 | 导航、选择 |
| 左摇杆 | 列表滚动、画面移动等界面操作 |
| 前触摸屏 | 点击、拖动 |

具体操作随菜单和演出状态变化，输入映射见 [vita_input.rs](knife/knife/src/vita_input.rs)。

## 源码结构

| 路径 | 内容 |
|---|---|
| [knife/](knife/) | Rust 工作区：游戏、格式解析、渲染、音频与 Vita 平台层 |
| [knife/vita/](knife/vita/) | GXM C 后端、Cg 源码、生成着色器和系统图片 |
| [tools/](tools/) | 本地构建、打包与素材处理脚本 |
| [构建与工具链](doc/03-构建与工具链.md) | 跨电脑构建、依赖与维护步骤 |
| `release/`、`.work/`、`dist/` | 本机产物、缓存和自备素材，不入库 |

历次逆向分析、实机性能基线与修复依据见[历史文档归档](doc/archive/README.md)。

## 许可

引擎沿用上游 [Mozilla Public License 2.0](knife/LICENSE)。第三方 SceShaccCgExt 的来源和 GPLv3 许可见 [NOTICE](knife/vita/vendor/shacccg-ext/NOTICE) 与 [LICENSE](knife/vita/vendor/shacccg-ext/LICENSE)，发布时须保留随包声明。游戏数据及汉化资源的权利归各自权利方。
