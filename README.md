# mofa-vita

《魔法使之夜》（Witch on the Holy Night）Switch 版引擎的 **PS Vita 移植**源码工程。

以 `knife` 0.9.4（上游 `AuroraWright/knife`，Switch 引擎的 Rust 重实现）为基线，
为 Vita 实现了原生 **GXM 渲染后端**，替代桌面端 OpenGL；输出固定 960×540（上下各 2 px 黑边）。
本仓库是**源码工程**：游戏资源、构建产物与存档都不入库。`dist/` 是本机用自备游戏数据生成的
资产包与各档中间产物（已被 `.gitignore` 排除），只用于实机测试。

## 状态

| 项目 | 数值 |
|---|---|
| 画面 | 场景链 480×270、画布 720×405、扫描 960×544 |
| 帧率（实机 444/222/222） | 对白 **22–45 FPS**、一般场景 17–25 FPS、重场景 **约 5 FPS**、全程平均 **19–21 FPS** |
| 稳定性 | 启动/运行崩溃已修（堆、字体图集、音频解码、GXM 对象生命周期） |
| 加载 | 纹理准备 670 ms/条 → **6 ms/条**（后台读盘 + 24 MiB 环形缓冲） |
| 纹理 | 预烘焙 GXT（DXT1/DXT5）直通 GXM |

重场景（逐帧动画，约占 5% 时间）卡在约 190 ms：39 个渲染 pass，每个 pass 约 2 ms 的
`sceGxmFinish` 驱动往返，属于当前渲染结构的固有成本，详见 `doc/06-已知限制与已放弃方案.md`。

## 目录

```
knife/                    引擎工作区（Rust）
  knife/                  主程序：脚本 VM、舞台、UI、文本、输入
  knife-render/           渲染抽象；src/vita.rs 为 Vita 后端
  knife-platform-vita/    GXM FFI 与平台服务
  knife-core/             归档、纹理、脚本等格式解析
  knife-audio/            BFSAR/Opus 音频
  vita/                   GXM 后端 C 源码与着色器（被编入静态库）
  probes/                 后端探针程序
tools/                    构建、打包、着色器编译脚本
doc/                      技术文档（本文档的详细版）
release/                  每次成功构建后自动发布的 VPK
dist/                     本机资产包与中间产物（不入库，见 doc/08）
实机开发测试工具/         VitaDeck 与设备端插件（不参与构建）
```

## 构建

每次构建成功后，脚本会把 VPK **复制到项目根目录的 `release/`**：

```
release/knife-vita-game-540p.vpk
```

并在构建日志末尾打印文件大小与 SHA-256。中间产物（elf/velf/eboot/param.sfo/收据）留在
`.work/vita/game-release-540p/`；`release/` 只放可直接安装的包。

2026-10-29 已将更新的图标、LiveArea 背景、启动按钮及 `pic0.png` 重新打包。当前发布包
`build_tag=mofa-livearea-20261029`，大小 **4,921,694 字节（约 4.69 MiB）**，SHA-256 为：

```text
c40dcc5922a0d7ae266cab528f2c42772f137698054574887fd4fd03bf81121b
```

图片源文件位于 `knife/vita/sce_sys/`；尺寸、模板引用、原图备份和历史包身份见
[构建与工具链](doc/03-构建与工具链.md)。本次已验证构建和包内容，尚未验证新版 LiveArea 的实机显示。

依赖：WSL2 Ubuntu + vitasdk（`VITASDK`）；着色器重编需 Windows 侧 `psp2cgc`。

```bash
# GXM 开发探针（不需要游戏数据）
bash tools/build-vita.sh gxm dev

# 540p 正式引擎包（游戏数据另行准备）
KNIFE_VITA_VARIANT=native540 KNIFE_BUILD_TAG=<tag> bash tools/build-vita.sh game release
```

着色器改动后必须重新生成头文件：

```powershell
.\tools\compile-vita-effects.ps1     # knife/vita/shaders/effects.cg -> generated/effects.h
.\tools\compile-vita-shaders.ps1
```

主机侧测试：`cargo test -p knife-render --offline`（68 项）。
`cargo check` 在主机上会因 gstreamer 依赖失败，以 Vita 构建结果为准。

## 运行（设备侧）

1. 安装构建出的 VPK（TITLE ID `KNIF00540`）。
2. 把游戏资源放到 `ux0:data/knife-woh-540p/assets/`——资源需用你自己的游戏数据转换，
   见 [从原版数据制作资产](doc/08-从原版数据制作资产.md)（`tools/make-vita-assets.py`）。
3. 数据根、存档根、日志根均为 `ux0:data/knife-woh-540p`。

渲染密度可通过 `ux0:data/knife-woh-540p/scene-mode.txt` 切换：
不存在 = 480×270 场景 + 720×405 画布（默认）；`full` = 原生文字；`720` / `quarter` / `native` 为对照档。

## 实机开发测试工具

`实机开发测试工具/` 是从开发期沿用的设备侧工具集，**不参与构建**，装不装都能运行游戏：

| 路径 | 用途 |
|---|---|
| `VitaDeck/VitaDeck.exe` | PC 端 Vita 调试工具（截图、文件传输、插件注入） |
| `VitaDeck.zip` | 同上的分发包（与原目录内容重复，占 52 MB） |
| `实机开发测试插件/vitacompanion.suprx` | taiHEN 用户插件：配合 VitaDeck 做进程/内存操作 |
| `实机开发测试插件/catlog.skprx` / `.suprx` | 日志转发到 PC（`catlog`） |
| `实机开发测试插件/net_keepalive.skprx` | 保持网络连接，避免长时间挂机掉线 |
| `实机开发测试插件/NoPowerLimits.skprx` / `nosleep.skprx` | 解除降频/休眠，测性能时避免频率波动 |
| `实机开发测试插件/InfiniteNet.suprx` | 网络相关调试插件 |
| `实机开发测试插件/config.txt` | taiHEN 配置示例，已包含上述插件的加载行 |
| `实机开发测试插件/performance.log` | 一份实机性能日志样例，字段含义见 `doc/04` |

**测性能请装 `NoPowerLimits` + `nosleep`**：否则系统可能在测量中途降频或休眠，日志里的帧时间会出现
无法解释的台阶。装插件后需要重启，日志头里的 `arm_mhz` 才代表稳定状态。

## 文档

| 文档 | 内容 |
|---|---|
| [架构总览](doc/01-架构总览.md) | 帧链、渲染目标、线程与资源加载 |
| [GXM 后端实现](doc/02-GXM后端实现.md) | C 后端结构、pass 模型、着色器与顶点格式 |
| [构建与工具链](doc/03-构建与工具链.md) | 构建脚本、LiveArea 图片、发布包哈希、着色器编译 |
| [性能实测与优化史](doc/04-性能实测与优化史.md) | 全部实测数据、有效优化与量化结论 |
| [资源与画质](doc/05-资源与画质.md) | GXT/DXT 管线、分辨率档位、文字清晰度 |
| [已知限制与已放弃方案](doc/06-已知限制与已放弃方案.md) | 结构性瓶颈与不要重复尝试的路线 |
| [上游同步与版本](doc/07-上游同步与版本.md) | 与上游 0.9.4→0.9.9 的差异与移植判断 |
| **[从原版数据制作资产](doc/08-从原版数据制作资产.md)** | 从自备的 Switch 数据生成设备端 assets 的完整链路 |
| [历史记录](doc/archive/) | 移植期的调查记录：GXM 移植计划、DXT 实施、三次崩溃取证、OpenGL 黑屏调查等 |

## 授权

引擎部分沿用上游 `knife` 的许可（见 `knife/LICENSE`）。游戏数据、字体与图像资源版权归原权利方，
不随本仓库分发。
