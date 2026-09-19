# mofa-vita-krkr

**《魔法使いの夜》（魔法使之夜 krkr 版）的 PS Vita 启动器。**

引擎侧是 [Yuri](https://github.com/YuriSizuku/Kirikiroid2Yuri)（Android 版 KiriKiri2 /
Kirikiroid2 一脉）加 VitaGL 呈现层。游戏保持原样：继续使用它自己的脚本、XP3 数据包、
存档与插件，不做任何数据上传，也不修改游戏数据目录。

| 项目 | 值 |
| --- | --- |
| 平台 | PS Vita / PS TV（需 HENkaku 或 enso），也可用 Vita3K 验证 |
| 标题 ID / 版本 | `MOFA00001` / 01.00，`ATTRIBUTE2=12`（扩大应用内存配额） |
| 数据根目录 | `ux0:data/mofa-vita/`（游戏固定在 `game/`，补丁在 `patch/`） |
| 实机依赖 | `ur0:/data/libshacccg.suprx`（真机导出的着色器编译器） |
| 性能目标 | 实机稳定 40fps、无 >33ms 尖峰、不出现位图分配失败 |
| 结论性文档 | [docs/性能适配与优化总结.txt](docs/性能适配与优化总结.txt) |

---

## 目录

- [1. 这是什么](#1-这是什么)
- [2. 快速开始](#2-快速开始)
- [3. 架构](#3-架构)
- [4. 引擎覆盖机制（改引擎行为的唯一入口）](#4-引擎覆盖机制改引擎行为的唯一入口)
- [5. 性能与测量](#5-性能与测量)
- [6. 诊断与排错](#6-诊断与排错)
- [7. 开发指南](#7-开发指南)
- [8. 插件支持清单](#8-插件支持清单)
- [9. 附录](#9-附录)

---

## 1. 这是什么

### 1.1 定位与边界

**做的**：把一部 Windows 上的 KiriKiri2 商业作品（含中文补丁）原样跑在 PS Vita 上——
原脚本、原 XP3、原存档、原插件名；引擎行为按 Vita 的 CPU/GPU/内存实力做适配。

**不做的**：

- 不修改游戏本体数据（`ux0:data/mofa-vita/` 下的文件只读；启动器只往那里写自己的日志）。
- 不加载 Windows DLL：插件的"名字"由静态注册表接管，"功能"要么真实现、要么明确标注为缺失。
- 不把主机路径、调试开关或本机绝对路径写进源码（仓库里搜不到 `C:\Users`、`D:\PSV`、`/mnt/d/`）。

### 1.2 当前状态

| | |
| --- | --- |
| Vita3K | 已实测：进标题、序章（含转场与视频解码）一路推进到 `1-0.ks` page21+ |
| 真机 PS Vita | 构建通过全部契约检查，**尚未实机验证**；真机预算推算见第 5 节 |
| 已知缺口 | 见第 8 节的分级表：`stringUtil.dll` 的 `isNewNumber/parseKeyFrame/initSpline` 等仍是占位 |
| 已修 | 位图 OOM（原因与修复见 5.5）、合成/上传/场景加载三处被测量后确认不是瓶颈 |

### 1.3 术语表（读代码前先扫一遍）

| 词 | 含义 |
| --- | --- |
| Yuri | 上游引擎（Kirikiroid2 的 C++ 移植），本仓库把它当作**只读**依赖，按 pin 的提交拉取 |
| TVP / TvPgl | Kirikiri 的图形层与像素内核表（`TVPAlphaBlend` 等函数指针） |
| KAG | 脚本层（`.ks` 文本脚本 + `Conductor` 驱动剧情/关键帧/文字显示） |
| XP3 | Kirikiri 的资源容器；本作正文在 XP3 里是加密的，需要过滤器 |
| xp3filter.tjs | 解密过滤器脚本，用独立 TJS 引擎执行（每个解包线程一个 VM） |
| TLG5/TLG6、PVR、PSB、PSD | 游戏用到的图像容器/格式；PVR 是移动端纹理缓存，本作不使用 |
| PVF | Vita 系统字体格式（`yuri_pvf_font_rasterizer.cpp` 用它做系统字体回退） |
| memblock | `sceKernelAllocMemBlock` 分配的可回收内存块；位图优先用它 |
| USER_RW / CDRAM | Vita 的两类内存池：前者给应用与位图，后者主要是 GPU 纹理（VitaGL 独占） |
| 生成源码 (`generated/yuri/`) | 由 `cmake/YuriBackend.cmake` 打补丁后生成的引擎源文件，不手工编辑 |
| 契约 (`cmake/Verify*.cmake`) | fail-closed 的构建断言：生成结果/我们源码里必须出现某些字符串，否则构建失败 |

---

## 2. 快速开始

### 2.1 实机安装

1. 用 VitaShell 安装 `mofa-vita-krkr.vpk`。
2. 准备数据目录（**目录名不能改**）：

   ```text
   ux0:data/mofa-vita/
   ├── game/                     复制整个游戏目录的内容
   │   ├── data.xp3              游戏启动档案
   │   ├── bg.xp3  bgm.xp3  fg.xp3  image.xp3  rule.xp3  sound.xp3
   │   ├── mahoyo.xp3  mahoyo.dll  *.exe
   │   ├── plugin/               游戏自带插件（原样保留，启动器会登记需要的那几个）
   │   └── savedata/             存档（可留空，首次运行自动建）
   ├── patch/
   │   ├── xp3filter.tjs         XP3 解密过滤器（必需，否则读不出正文）
   │   └── patch.tjs             Kirikiroid2 兼容补丁（必需，插件入口靠它接管）
   ├── msgothic.ttc              字体（用户自备，永不入库/入包）
   ├── mofa.ini                  可选：按键映射（见 2.3）
   └── boot-status.txt / error.txt / engine.log   运行日志（启动器写）
   ```

3. 启动游戏。

**过滤器与补丁的解析顺序**（`src/engine/vita/vita_launch.cpp`）：

1. `patch/xp3filter.tjs`（用户放置）；
2. 游戏目录自带的 `xp3filter.tjs`；
3. **Phase 1 归档推断**：采样真实 XP3 数据合成规则，**用归档数据自校验通过**才采用；
4. VPK 内嵌的默认过滤器 `app0:mofa-vita/default-xp3filter.tjs`；
5. 都不成立时带着确切原因停下，而不是猜一个会把数据解坏的过滤器。

推断结果按归档指纹缓存在 `ux0:data/mofa-vita/heuristics/<指纹>/`；VPK 内嵌的
Kirikiroid2 补丁集（`mofa-vita/patches/patches.zip` + `alldata.js`）按 manifest 解到
`ux0:data/mofa-vita/patch-cache/`，只解出 alldata 指定的那一个修订。

### 2.2 在 Vita3K 上验证

```powershell
& '<vita3k>\Vita3K.exe' '<仓库>\build-release-vita\mofa-vita-krkr.vpk'
# 已安装过时也可以按标题 ID 启动
& '<vita3k>\Vita3K.exe' -r MOFA00001
```

两个必须知道的坑：

- **重新安装确认框**：同一版本再次安装时 Vita3K 会弹「是否重新安装该内容？」并一直等待
  ——窗口标题停在应用名、`vita3k.log` 是 0 字节、游戏不启动，直到点 OK。自动化运行时
  要么点掉它，要么用 `-r MOFA00001` 启动已安装的副本。
- **着色器编译器**：把真机导出的 `libshacccg.suprx`（约 3 MB，`SCE\0` 开头）放到
  `ur0\data\libshacccg.suprx`。**0 字节占位文件会在现场编译着色器时把模拟器打成崩溃**；
  本构建会拒绝占位文件并给出中文提示。

数据目录与真机一致：`<存储卡>\ux0\data\mofa-vita\`。

### 2.3 按键映射（可选）

放在 `ux0:data/mofa-vita/mofa.ini`，只有输入部分会被读取：

```ini
input_mapping_version=4
analog_deadzone=0.18
cursor_speed=780
touch_enabled=true
bind.circle=key_enter
bind.cross=mouse_right
bind.square=disabled
bind.triangle=mouse_wheel_up
bind.ltrigger=mouse_left
bind.rtrigger=key_control
bind.dpad_up=key_up
bind.dpad_down=key_down
bind.dpad_left=key_left
bind.dpad_right=key_right
bind.start=disabled
bind.select=disabled
bind.left_stick=mouse_cursor
bind.front_touch=mouse_absolute
```

每个字段都有默认值，删掉某项就回到默认；改完重启应用生效。

可绑定**来源**：`cross circle square triangle ltrigger rtrigger dpad_up dpad_down
dpad_left dpad_right start select left_stick front_touch`。
可绑定**动作**：`mouse_left mouse_right mouse_wheel_up mouse_wheel_down mouse_cursor
mouse_absolute`、按键 `key_space key_enter key_escape key_pageup key_pagedown
key_up key_down key_left key_right key_control key_shift key_tab key_backspace`、
`menu`（等同 Escape）以及 `disabled`。

默认：左摇杆移动指针，前触摸屏直接定位，L = 鼠标左键，× = 鼠标右键，○ = 回车，
△ = 滚轮上滚，R = 按住 Ctrl（快进），方向键 = 方向键。

---

## 3. 架构

### 3.1 分层与数据流

```text
                ┌─────────────────────────── Vita 侧（本仓库） ───────────────────────────┐
启动链：        early_boot_trace.c → yuri_main.cpp → vita_launch.cpp → vitagl_presenter.cpp
                │        (preinit 日志)     (堆/栈/线程/循环)   (工程·过滤器·补丁选择)  (VitaGL 池·呈现)
存储与兼容：    src/common/*  →  src/yuri/*  →  src/engine/retail/*（插件实现）
                │  XP3/松装统一命名空间、过滤器 VM、补丁仓库   每个 DLL 名一个模块
引擎：          Yuri（只读依赖，按 pin 提交拉取）+ cmake/YuriBackend.cmake 的补丁
呈现：          软件合成器 → LayerManager 完成 → yuri_window_layer.cpp → vitagl_presenter.cpp → vitaGL
测量：          [mofa-meter] / [mofa-stage] / [mofa-pixels] / [mofa-perf] / [mofa-mem]
```

一句话概括：**KAG/TJS 在 CPU 上跑，图层是 CPU 位图，最后合成成一张 surface 交给 GPU 贴出来**；
GPU 目前只负责最后一层呈现（缩放/叠加），这也是"每层纹理化"那条路线的切入点（见 5.6）。

### 3.2 启动链（六段，各自职责）

| # | 位置 | 做什么 |
| --- | --- | --- |
| 1 | `src/engine/vita/early_boot_trace.c` | 挂在 `.preinit_array`，在任何 C++ 全局构造之前就能写 `boot-status.txt`/`error.txt`；所有日志格式与中英对照表都在这里 |
| 2 | `src/platform/vita/yuri_main.cpp` | `main`：newlib 堆 128 MiB、主线程栈 2 MiB（写在段属性里）→ 线程调度策略 → 线程自检 → `mofa_resolve_launch` |
| 3 | `src/engine/vita/vita_launch.cpp` | 决定工程目录、xp3filter、启动补丁（含 Phase 1 推断与内嵌补丁包的按需解包） |
| 4 | `src/engine/vita/vitagl_presenter.cpp` | 着色器编译器检查 → VitaGL 池与阈值 → 5 张轮转呈现纹理 → 脏区上传 → 视频叠加 → 光标 → `[mofa-perf]` |
| 5 | Yuri `Application->StartApplication` | 脚本引擎/字体/基础系统初始化；执行 `patch.tjs` 与 `store/`；`resources/vita/retail-after-startup.tjs` 在游戏启动脚本之后做第二段 Vita 专属适配 |
| 6 | 事件循环（`yuri_main.cpp`） | `input_pump → Application->Run() → present → RecycleProcess() → 阶段计时 → 60Hz 节流` |

### 3.3 目录结构与源码地图

```text
CMakeLists.txt           主机端工具/测试，以及 Vita 构建入口与测试注册
cmake/Dependencies.cmake 固定的 Yuri / Oniguruma / KrKr2-Next / SQLite 版本（FetchContent）
cmake/YuriBackend.cmake  引擎补丁与生成（唯一允许改引擎行为的地方）
cmake/Verify*.cmake      fail-closed 构建契约、包内容与追踪校验
docs/                    性能适配与优化总结（非构建输入）
include/mofa/            71 个"策略 + 常量 + 契约注释"头文件
resources/vita/          气泡资源、默认过滤器、启动后脚本
scripts/                 构建、兼容性审计、真机/A9 板卡与试验台驱动
src/common/              与平台无关的零售侧逻辑
src/engine/retail/       每个零售插件一个模块文件
src/engine/vita/         Vita 专属：早期追踪、启动解析、VitaGL 呈现
src/platform/vita/       平台桥：事件循环、存储、输入、音频、字体、内存、计量
src/yuri/                TJS 平台适配、过滤器虚拟机、上游第三方薄头 compat/
tests/                   主机测试套件（ctest）+ 脚本驱动的硬件试验台
vita/booter/             气泡/引导模板（由 scripts/package-bubble.sh 使用）
```

| 目录 | 内容要点 |
| --- | --- |
| `src/common/` | `xp3_archive`（索引/分段/过滤读）、`storage`（松装 + XP3 统一命名空间）、`text_codec`（UTF-8/16、FE FE、zlib）、`filter_heuristic`（约 1.8k 行的过滤器推断引擎）、`retail_filter`/`phase1_filter`、`patch_repository`/`patch_manifest`、`native_plugin_inventory`/`pe_resources`（清点游戏自带 Windows 插件）、`sfo`/`sha256`/`png`/`ajpm`/`profile`/`game`/`bubble` |
| `src/engine/retail/` | `NCB_MODULE_NAME` 决定顶替哪个 DLL 名；薄文件（20-30 行）只把上游实现登记进静态注册表，厚文件是真实现：`sqlite3`(1078 行)、`shrinkCopy`(533)、`psb`(473)、`fstat`(259)、`rsa_pss_signature`(253)、`psbfile`(236)、`sigcheck`(195)、`gfxEffect`(178)、`extNagano`(131) |
| `src/engine/vita/` | `early_boot_trace.c`、`vita_launch.cpp`、`vitagl_presenter.cpp`、`tvpgl_kernel_policy.cpp`（`tvpgl-scalar` 覆盖标记） |
| `src/platform/vita/` | `yuri_main.cpp`、`yuri_window_layer.cpp`、`yuri_storage_preflight.cpp`（松装自动路径）、`vita_bitmap_allocator.cpp`（三层分配）、`yuri_tvpgl_benchmark.cpp`/`yuri_tvpgl_meter.cpp`、`yuri_stage_profile.cpp`、`yuri_video_overlay.cpp`（FFmpeg）、`yuri_openal_mixer.cpp`、`yuri_pvf_font_rasterizer.cpp`、`yuri_input.cpp`、`yuri_config.cpp`、`yuri_7z_libarchive.cpp`、`yuri_ms_gothic_stream.cpp`、`yuri_thread_policy.cpp`、`yuri_threading_self_test.cpp` |
| `src/yuri/` | TJS 平台适配、过滤器 VM、`compat/` 下替换上游的第三方薄头（libarchive / oniguruma / opencv / freetype / lz4 / xxhash / unzip） |
| `include/mofa/` | 行为策略的唯一落点（内存预算、脏区、呈现事务、插件分级、像素家族、文本前缀宽度……）。**每个文件开头的注释就是它存在的理由**，改行为前先读它 |
| `tests/` | 21 个 ctest 用例（`test_main.cpp` 是 3k 行主套件）+ 4 个**不在 ctest**的硬件试验台（ARM alpha / 纹理别名 / 图层合成 / AJPM 帧预算），由 `scripts/run-*.sh` 驱动 |

### 3.4 VPK 里有什么

| 路径 | 内容 |
| --- | --- |
| `eboot.bin` | `mofa-yuri.self`（`mofa-yuri` 可执行体，已转 SELF） |
| `sce_sys/*` | 图标、启动图、livearea（`resources/vita/sce_sys/`） |
| `mofa-vita/default-xp3filter.tjs` | 内嵌默认过滤器（最后兜底） |
| `mofa-vita/retail-after-startup.tjs` | 游戏启动脚本之后执行的 Vita 专属适配 |
| `mofa-vita/patches/alldata.js` + `patches/patches.zip` | 内嵌的 Kirikiroid2 补丁集与 manifest，运行时按需解到 `ux0:data/mofa-vita/patch-cache/` |
| `param.sfo` | TITLE_ID `MOFA00001`、`ATTRIBUTE2=12`（大内存配额） |

构建后还会跑 `cmake/VerifyVitaVpk.cmake` 检查包身份、ATTRIBUTE2、资源与补丁内容，
以及 `cmake/VerifyVitaElf.cmake` 检查 ELF 里该有的符号与不该有的前端源码。

### 3.5 `boot-status.txt` 里的里程碑

```text
preinit-entered → main-entered → vita-threading-self-test-passed
→ retail-xp3filter-selected → retail-launch-resolved
→ vitagl-initialized → yuri-platform-ready
→ yuri-storage-preflight-complete → yuri-startup-script-entered
→ retail-layereximage-ready（插件从这里开始注册）
→ yuri-pixel-meter-installed（性能日志从这行之后开始）
→ yuri-startup-script-complete → vitagl-first-game-frame-presented
→ retail-runtime-5s/30s-stable-with-video
```

每个英文标记的**下一行**是以 `#` 开头的中文说明；英文标记本身保留原样，因为
`cmake/VerifyHardwareBootTrace.cmake` 按整行精确匹配它们。

---

## 4. 引擎覆盖机制（改引擎行为的唯一入口）

上游 Yuri 是**只读**的（FetchContent 按 commit 拉取）。所有引擎侧改动都在
`cmake/YuriBackend.cmake` 里用"读取源码 → `string(REPLACE)` → 生成到
`build-release-vita/generated/yuri/` → 替换原文件"完成。

这套机制是 **fail-closed** 的：每个补丁都要求锚点文本存在、替换后内容确实变了，
否则配置阶段直接 `FATAL_ERROR`。上游一升级、锚点漂移，构建就会停下，不会静默生成
一份语义不明的引擎。

### 4.1 新增一个引擎补丁的步骤

1. 在 `cmake/YuriBackend.cmake` 里 `file(READ ...)` 读原文件，`string(REPLACE)` 打补丁，
   并检查"替换前存在、替换后已变"（两个分支都要 `message(FATAL_ERROR ...)`）。
2. `file(CONFIGURE OUTPUT "${yuri_generated_dir}/X.cpp" ...)` 生成，并把生成文件替换进
   对应目标的源码列表。
3. 在 `cmake/VerifyYuriBuild.cmake` 里加 `require_text("<生成文件>", "<必须出现的字符串>",
   "<为什么必须在>")`，让"删掉这条接缝"变成构建失败而不是靠 review。
4. 跑一次 Vita 构建，看到 `Yuri generated-source and backend-boundary contracts passed`。

> 经验：锚点尽量用**语义稳定**的短句（函数签名、常量声明），不要用大段连续代码；
> 生成的源码里不要留"调试用"的东西（契约里还有若干 `forbid_text`，比如不允许
> 事件循环里出现性能计数器）。

### 4.2 命名与分层约定

| 前缀 | 位置 | 说明 |
| --- | --- | --- |
| `mofa/xxx.hpp` | `include/mofa/` | 可移植策略/常量/契约；不放平台实现 |
| `yuri_xxx.cpp` | `src/platform/vita/` | 平台桥（引擎与 Vita 之间的适配层） |
| `retail_xxx` / `xxx_module.cpp` | `src/engine/retail/` | 零售插件与过滤器实现 |
| `mofa_yuri_*`（`extern "C"`） | 引擎补丁调用的接缝 | 例如 `mofa_yuri_begin_frame_damage`、`mofa_yuri_stage_bucket` |

---

## 5. 性能与测量

这一节的数字都来自 Vita3K 实测（标量内核）。真机是 444 MHz Cortex-A9 + NEON，
**不能直接套用主机的毫秒数**——能搬的是占比与工作量，见 5.4 的换算。

当前合成面：**1024×576 = 589,824 像素 = 全量 2.36 MB/帧**（游戏自己的 KAG 窗口尺寸，
呈现时缩放到 960×544）。

### 5.1 五类日志（都在 `ux0:data/mofa-vita/boot-status.txt`）

除注明外都是**每帧**单位，每秒一行（60 帧一个窗口）。

```text
[mofa-meter]  ns/px blend=.. adddest=.. add=.. stretch=.. sadd=.. affine=.. copyfill=.. cmap=..
```

启动时一行。由**设备上真正装着的那个内核**微基准测出（1024/512 像素 × 64 次 × 3 轮取最小）。
真机是 NEON、模拟器可能是标量，两边必须各测各的，不要拿主机数字外推真机。

```text
[mofa-stage]  frames=60 loop=.. busy=.. engine=.. script=..(events=.. timer=.. kag=..
              kload=.. klabels=.. khooks=.. cont=.. tags=.. rest=..)
              composite=..(%,calls) present=.. input=.. recycle=.. idle=..
```

| 字段 | 接缝 | 含义 |
| --- | --- | --- |
| `loop` / `busy` / `idle` | 事件循环 | 整帧节奏 / CPU 真忙 / 等 60 Hz 的剩余时间 |
| `engine` | `Application::Run()` | 引擎一整帧；`script = engine − composite` |
| `events` | `Application::ProcessMessages` 的消息循环 | 输入 / 窗口更新事件投递 |
| `timer` | `TVPTimer::ProgressAllTimer` | TJS 定时器 |
| `kag` | `KAGParser::GetNextTag` | 原生 KAG 标签解析（`tags` 是解析次数） |
| `kload` / `klabels` / `khooks` | `LoadScenario` / `EnsureLabelCache` / 游戏的 `onScenarioLoad(ed)` | 场景读取+切行 / 标签缓存构建 / 游戏自己的回调 |
| `cont` | `TVPDeliverAllEvents` | **KAG Conductor 在这里**：标签派发、关键帧求值、图层属性写入 |
| `composite` | `LayerManager::UpdateToDrawDevice` | 软件合成（图层树 → DrawBuffer），括号里是完成次数 |
| `present` / `input` / `recycle` | 呈现器 / 输入泵 / 纹理回收 | 上传+绘制+交换等 |

```text
[mofa-pixels] frames=60 blend=..k stretch=..k add=..k adddest=..k sadd=..k affine=..k
              copy=..k cmap=..k calls=.. est=..ms
```

100 个热点 TvPgl 内核上装了"只做整数累加"的转发壳，按 8 个家族统计目标像素数。
计数用 relaxed 原子加法（行分割混合会跑在两个渲染 worker 上），每次扫描线一次加法，
代价远小于它描述的那次混合。`est` 用 `[mofa-meter]` 的 ns/像素把像素折算成毫秒。

> **踩过的坑**：`LayerBitmapIntf` 是按 `basename / _o / _HDA / _HDA_o` 逐扫描线选内核的，
> 而其中 `hda = true if destination has alpha`——**往普通 alpha 图层里画走的是 `_HDA`**。
> 漏掉 `_HDA` 会让计量结果比合成小两个数量级（本项目确实踩过，补齐后混合像素量从
> 8k/帧跳到 340k/帧）。仍未覆盖：Photoshop 混合模式族 `TVPPs*`（本作脚本不使用
> `blendMode`）与一次性格式转换 `TVPReverseRGB` / `TVPRedBlueSwap*`。

```text
[mofa-perf]   frames=60 frame=..ms composite+present=..ms upload=..ms draw+swap=..ms
              full=.. partial=.. px/frame=.. video_up=.. total_up=..MB
[mofa-mem]    <tag> free_user=..MB live_memblock=..MB live_malloc=..MB
```

`[mofa-perf]` 描述上传与脏区行为（`px/frame` 占合成面的百分比是关键指标）；
`[mofa-mem]` 只在内存压力/回收路径打点，用来判断"是策略拒绝还是真的没内存"。

**读日志的顺序**：先看 `[mofa-stage]` 决定钱花在哪一段 → 再用 `[mofa-pixels]` 判断
那一段里有多少是像素活 → 最后用 `[mofa-perf]` 看上传是否退化，用 `[mofa-mem]` 看内存。

### 5.2 内核的自适应选择

Yuri 的 ARM 后端只要看到 CPU 特性位就装一整套手写 NEON 内核。真机上那是对的，但
模拟器的 JIT 可能把 NEON 退回解释器（结果正确、慢很多）。现在启动时对 8 个吃像素的
内核各测一小段（单轮封顶 25ms），**NEON 必须快 15% 以上才保留**，测不出来就用标量版。
日志是 `[mofa-tvpgl] <内核> scalar=..us neon=..us pick=neon|scalar`，末尾一行
`yuri-tvpgl-kernels-measured-on-device`。

`ux0:data/mofa-vita/tvpgl-scalar` 仍然有效，语义是「**永远不许装 NEON**」的硬覆盖
（命中时跳过测量并打 `[mofa-tvpgl] scalar-requested-skip-benchmark`）。
**真机数据目录里不要放这个标记。**

### 5.3 内存模型

| 池 | 大小 | 说明 |
| --- | --- | --- |
| newlib 堆 | 128 MiB | 启动时固定，`free()` 无法归还内核；脚本 / TJS 对象 / SQLite / FreeType / 小分配 |
| VitaGL 池 | 32 MiB | `vglInitExtended` 的阈值按「实际空闲 USER_RW − 32 MiB」推导（实测日志 `vitagl-user-ram-free-237m-threshold-205m`）。软件合成下呈现只需约 20 MiB（5 张 1024×576 轮转纹理 + 覆盖层 + 交换链） |
| 位图 memblock | ≥1 MiB 的位图 | 三层：先直接申请 memblock（内核权威）→ 12 MiB 保留线 → 4 MiB 紧急下限 → 最后才退回固定堆 |
| CDRAM / GPU 纹理 | VitaGL 独占 | 与 USER_RW 分开，呈现纹理首选这里 |

预算常量集中在 `include/mofa/vita_memory_budget.hpp`，分配器与策略在
`include/mofa/vita_bitmap_allocator.hpp` + `src/platform/vita/vita_bitmap_allocator.cpp`，
两者都有构建契约保护，改动要同步更新 `cmake/VerifyYuriBuild.cmake`。

### 5.4 实测摘要

**阶段（连续推进正文的 73 个窗口，毫秒/帧）**

| 字段 | 平均 | 中位 | 峰值 |
| --- | --- | --- | --- |
| loop | 23.71 | 17.76 | 97.29 |
| engine | 8.94 | 2.97 | 84.23 |
| script | 7.10 | 1.09 | 82.10 |
| ├ kag（解析） | 0.69 | 0.06 | 45.00 |
| ├ kload / klabels / khooks | 0.02 / 0.00 / 0.00 | — | 0.85 / 0.02 / 0.00 |
| ├ cont（KAG 派发+关键帧+图层写入） | 6.42 | 1.01 | 85.11 |
| └ rest | 0.67 | 0.01 | 43.99 |
| composite | 1.83 | 1.76 | 8.89 |
| present | 0.30 | 0.24 | 1.05 |
| idle | 14.44 | 14.62 | 17.60 |
| tags/帧 | 84.85 | 96.00 | 144.00 |

**像素（P1a 补齐 100 个内核后的 100 个活跃窗口，每帧）**

| 家族 | 平均 | 中位 | 峰值 | ns/像素（Vita3K 标量） |
| --- | --- | --- | --- | --- |
| blend | 385k | 340k | 1,586k | 5.9–6.2 |
| copy | 193k | 147k | 827k | 2.0 |
| adddest（往 ltAddAlpha 里画） | 0 | 0 | 0 | 13.5（最贵，但本作稳态不调用） |
| stretch / add / sadd / affine / cmap | 0 | 0 | 0 | 2.4–7.1 |
| 折算 est | 2.67ms | 2.28ms | 10.55ms | — |
| 对照 composite | 2.16ms | 2.01ms | 9.11ms | **est/composite ≈ 1.24** |

**真机换算**：主机单核大致是真机的 8–20 倍，按此推算稳态 `engine` 2.97ms + `present`
0.24ms → 真机 25–60ms/帧（即 25–30fps），而混合像素在 NEON 生效时约 0.5–1.5ms/帧；
也就是说**真机 40fps 的未知数在 `cont`（脚本侧），不在合成与上传**。

### 5.5 位图 OOM：成因与修复（示例：一次完整的"证据 → 修复"）

症状：游戏在 `1-0.ks:81`（`fgact`）抛
`Cannot allocate memory for Bitmap : at TVPAllocBitmapBits (size=3010600(1120x672))`
——3 MB 解码位图分配失败，是功能中断而不是卡顿。

关键证据（`[mofa-mem]` 打点）：

```text
[mofa-mem] before-reclaim free_user=32MB live_memblock=28MB live_malloc=58MB
```

内核报告的"空闲 USER_RW"恰好停在 32 MiB 保留线上，而位图 memblock 只占 28 MB：
保留线把所有 ≥1 MiB 的申请挡掉 → 这些多兆位图被挤进**启动时就一次性预留好的**
128 MiB 堆（堆里最多堆到 59 MB 位图）→ 堆满 → 全量压缩回收 → 仍然失败。
而这个后端里除位图分配器外**没有任何模块申请 USER_RW memblock**（脚本 / FreeType /
SQLite / 音频 / 视频都在那个堆里，VitaGL 的池在初始化时一次性认领），所以那条大保留线
保护不了谁，只是把内存从游戏手里拿走。

修复三处：① 分配改为"内核权威"（≥1 MiB 先申请 memblock，内核拒绝才回退）；
② 保留线 32→12 MiB、紧急下限 8→4 MiB；③ VitaGL 池 48→32 MiB，把 16 MiB 让给位图。

验证：同一失败点（page6 / line 81）顺利通过并推进到 page13+，
**0 条内存压力标记、0 次 OOM 回收、无新 `error.txt`**。

### 5.6 被数据否掉的假设与仍开放的问题

已被测量否掉（不要重做）：

1. **"给加法 alpha 写精确 NEON 内核"** —— adddest 家族在稳态调用量为 0。
2. **"合成的时间花在调用/几何开销上"** —— 补齐 `_HDA` 后 `est/composite ≈ 1.24`，
   成本就是像素本身。
3. **"场景加载/解析要预解析或建索引"** —— `kload`/`klabels`/`khooks` 全为 0–0.85ms，
   45ms 全在 `GetNextTag` 内部（本作序章那个大 `[iscript]` 块），随后 85ms 是游戏自身
   脚本执行；属于"进章节一次性 0.7–1.7 秒"，不是每帧成本。

仍开放：

- **真机实测**：`cont`（脚本侧）到底落在 13ms 还是 33ms/帧，决定 40fps 能否达成。
- **40fps 节流 + 分辨率档位**：`include/mofa/engine_tick_pacer.hpp` 的
  `kYuriEngineTickUs` 16667 → 25000 即可切 40fps；分辨率档位在软件合成下不是"白赚"
  （图层是 CPU 位图，目标面缩小就要把 blit 换成拉伸采样），只有 GPU 缩放或
  "解码尺寸上限"才是净收益。
- **GPU 合成（每层一张纹理）**：仓库里那条整体 OGL 合成器默认关闭且注释写明
  "对商业作品不稳"；前置是 ltAddAlpha 的 GPU 原型（SGX543 无 dual-source blending）
  与脚本回读像素（`layerExSave`/`layerExAreaAverage`/`getMainImage`）的降级通路。
- **数据侧优化**：大图离线降采样（省内存/解码）、片头视频重编码（省每帧 `sws_scale`）、
  必要时把 KAG 窗口对齐到 960×544（−11.5% 像素，但会动布局，需实测）。

### 5.7 性能工作的流程（约定）

1. **先插日志点，再改行为**：能用现成的五类日志衡量的，就不要再加新的；
   必须新加的接缝，同时补 `cmake/VerifyYuriBuild.cmake` 契约。
2. **一次只改一处**，然后跑一轮对照（同一场景、同一时长），出报告时区分新旧日志：
   `boot-status.txt` 每次运行重写可直接当本次；`engine.log` 是追加的，用最后一次
   `Loading startup script` 或时间戳切分。
3. **采证**：把 `boot-status.txt` 复制到 `build-release-vita/perf-evidence/` 并标明
   构建版本（该目录已被 `.gitignore` 忽略，不会进仓库）。
4. **验收线**：60 秒脚本化场景内 0 个 >33ms 的帧、主线程 CPU < 20ms/帧、
   连续 10 分钟不出现位图分配失败。

---

## 6. 诊断与排错

### 6.1 诊断文件

| 文件 | 内容 |
| --- | --- |
| `ux0:data/mofa-vita/error.txt` | 本次启动失败的原因：先中文说明与处理方向，再附引擎/脚本原文；**每次运行重写** |
| `ux0:data/mofa-vita/boot-status.txt` | 启动追踪 + 五类性能日志；每个英文标记的下一行是中文说明 |
| `ux0:data/mofa-vita/engine.log` | 引擎日志：过滤器、补丁、插件与存储的决定（追加写入） |
| `game/savedata/krkr.console.log` | KiriKiri 控制台输出，脚本异常在这里 |
| `ux0:data/mofa-vita/heuristics/<指纹>/` | Phase 1 推断出的过滤器缓存 |
| `ux0:data/mofa-vita/patch-cache/` | 从 VPK 内嵌补丁包解出的补丁修订 |

### 6.2 「画面正常但某个特效没有」怎么判断

**链接 ≠ 有实现。** 插件名被登记只保证 `Plugins.link()` 不中断启动。遇到特效缺失：
先看 `game/savedata/krkr.console.log` 是不是某个空实现被调到了，再对照第 8 节的分级
（`control_flow_fallback` 与 `link_only` 就是"名字在、行为不在"），而不是当成启动失败。
本作大多数这类入口由 `patch.tjs` 用 TJS 接管，所以正常游玩不受影响。

### 6.3 常见错误对照

| `error.txt` 里的报错 | 原因 | 处理 |
| --- | --- | --- |
| `没有找到精确匹配的 xp3filter.tjs…` | `patch/` 里没有过滤器，且只靠归档数据解不出来 | 把本作的 `xp3filter.tjs` 放进 `ux0:data/mofa-vita/patch/` |
| `vitaGL 需要真实的 ur0:/data/libshacccg.suprx 着色器编译器` | 没装着色器编译器，或只放了 0 字节占位文件 | 安装/复制真的 `libshacccg.suprx`（Vita3K 见 2.2） |
| `Member "isExistentDirectory" does not exist` | 用的是补齐吉里吉里Z API 之前的旧构建 | 用本仓库重新编译 |
| `Cannot load Plugin <插件名>.dll` | 作品链接了尚未登记的 KAGEX 插件 | 按 8.6 的流程登记并实现（至少登记为 link-only） |
| `找不到游戏目录：ux0:data/mofa-vita/game` | 游戏没放到固定目录 | 放进 `ux0:data/mofa-vita/game/` |
| `Yuri 没有跑完游戏的启动脚本（启动脚本中途中止）` | 游戏自己的启动脚本抛异常 | 看 `game/savedata/krkr.console.log` |
| `Vita 的 pthread/libstdc++ 同步自检失败` | 静态链接缺 libpthread 或调度异常（Vita3K 偶发） | 构建已 whole-archive libpthread；模拟器上重跑一次即可 |
| `Cannot allocate memory for Bitmap ...` | 位图内存耗尽（成因与修复见 5.5） | 已是已知问题；若复现请附 `[mofa-mem]` 行 |

### 6.4 Vita3K 上偏慢 / 日志暴涨

Dynarmic 不是每条 ARM NEON 指令都能翻译，混合内核里的 `vsubhn` 序列会退回解释器，
日志里刷 `InterpreterFallback` / `Undefined instruction`。**本构建会自己测量并留在
标量内核**（`[mofa-tvpgl] <内核> ... pick=scalar`），一般不需要手动干预；确实要强制时
再放 `ux0:data/mofa-vita/tvpgl-scalar` 标记（真机不要放）。

---

## 7. 开发指南

### 7.1 构建

完整流水线（WSL2，含主机测试 → Vita 版 FFmpeg → 后端 → VPK）：

```bash
wsl -d <发行版> -u root bash /mnt/<盘符>/<路径>/mofa-vita/scripts/build-vita-wsl.sh
```

脚本会自己导出 `VITASDK`、检查依赖并在缺东西时打印确切安装命令：

- 主机包：`libcurl4-openssl-dev zlib1g-dev libpng-dev libsqlite3-dev libssl-dev libfreetype-dev nasm`
- VitaSDK 包：`vdpm install boost libarchive vitaGL openal-soft opusfile`

首次构建要下载编译 FFmpeg（约半小时，缓存在 `.cache/`）。日常增量：

```bash
# 只编译 VPK（改过 src/ include/ cmake/ 后；会自动重新 configure）
wsl -d <发行版> -u root bash -lc 'export VITASDK=/opt/vitasdk; \
  cd /mnt/<盘符>/<路径>/mofa-vita && \
  cmake --build build-release-vita --parallel 12 --target mofa-vita-krkr.vpk-vpk'
```

主机端（不需要 VitaSDK，用来跑回归）：

```bash
cmake -S . -B build-release-host -DCMAKE_BUILD_TYPE=Release -DMOFA_BUILD_TESTS=ON
cmake --build build-release-host --parallel
ctest --test-dir build-release-host --output-on-failure                    # 全部
ctest --test-dir build-release-host -R "mofa-tvpgl" --output-on-failure    # 子集
```

### 7.2 构建目标一览

| 目标 | 作用 |
| --- | --- |
| `mofa-common` | 可移植的零售侧逻辑（XP3/过滤器/补丁/SFO/SHA256/文本……） |
| `mofa-sqlite` | 官方 SQLite amalgamation（主机与 Vita 用同一个版本） |
| `mofa-retail-resolver` | VPK 内的补丁/过滤器解析（`src/engine/vita/vita_launch.cpp`） |
| `mofa-yuri-{tjs,base,utils,extension,visual,sound,environ,plugins}` | 上游 Yuri 按子系统切分的静态库 + 我们的生成覆盖 |
| `mofa-yuri-vita-platform` | 平台桥（存储/输入/内存/计量/呈现适配），承接所有 `mofa_yuri_*` 接缝 |
| `mofa-yuri` | 最终可执行体（`mofa-yuri.self` → `eboot.bin`） |
| `mofa-tool` / `mofa-tests` / `mofa-retail-compatibility` | 主机工具、主测试套件、零售兼容性审计 |
| `mofa-retail-armv7-runtime` | Cortex-A9 板卡/真机对照用的 ARMv7 运行器 |
| `mofa-vita-krkr.vpk` | 打包目标（含契约校验） |

### 7.3 测试面

| 层 | 内容 |
| --- | --- |
| ctest（21 个，按条件注册） | `test_main.cpp` 主套件（TJS 语义、XP3/过滤器、补丁、SFO、PNG、SHA256……）+ 事件参数 / ObjectList / OpenCV 兼容层 / Squirrel 桥 / SQLite 模块 / RSA-PSS 签名 / PSB / AJPM / KAG 内联脚本 / KAG 日志策略 / 渲染任务分派 / 内核选择 / 像素计量 |
| 硬件试验台（不在 ctest） | `test_yuri_arm_alpha`、`test_yuri_texture_aliasing`、`test_yuri_layer_composite`（含 `yuri_layer_harness_stubs.cpp`）、`test_ajpm_frame_budget`，由 `scripts/run-*.sh` 驱动 |
| 板卡 / 真机 | `scripts/run-cortex-a9-*.sh`、`run-retail-matrix-on-cortex-a9.sh`、`verify-vita-run.sh` |
| 兼容性审计 | `scripts/audit-kirikiroid2-patches.py`、`run-retail-compatibility.sh`（用 `yuri_plugin_capabilities.hpp` 的同一张清单） |

### 7.4 代码约定

1. **引擎行为只能从 `cmake/YuriBackend.cmake` 改**，并同步 `VerifyYuriBuild` 契约；
   生成的 `generated/yuri/**` 永远不要手工编辑。
2. **注释解释"为什么"**，不是"做了什么"——本仓库的注释密度是刻意为之，它是后来人
   唯一能知道"这行为什么必须这样"的地方。
3. **不改游戏数据**：`ux0:data/mofa-vita/` 只读（启动器只往那里写自己的日志）；
   离线数据加工要做成可回滚的独立产物。
4. **不把平台细节泄漏进可移植层**：`include/mofa/` 只放策略与常量，实现放 `src/platform/vita/`。
5. **失败要带原因**：解析/过滤器/补丁/插件这类"猜错会解坏数据"的地方一律 fail-closed。

---

## 8. 插件支持清单

**权威来源是 [include/mofa/yuri_plugin_capabilities.hpp](include/mofa/yuri_plugin_capabilities.hpp)**
（`yuri_plugin_fidelity()` / `yuri_plugin_is_link_only()` / `yuri_plugin_surface_contracts`）。
主机端兼容性审计与 Vita 侧封闭加载器共用同一张清单；下表与它保持一致，
**改等级时三处要一起改**（头文件、`src/engine/retail/` 的实现、`VerifyYuriBuild` 契约）。

### 8.1 分级含义

| 等级 | 含义 |
| --- | --- |
| `portable_equivalent` | 上游或等价的可移植实现直接编进来（真正的功能） |
| `behavioral_subset` | 用替代方案实现可观察行为（例如把闭源转场映射到交叉淡入） |
| `control_flow_fallback` | 暴露脚本可见的名字与状态、控制流能继续，但像素或音频是空实现 |
| `link_only` | 只登记名字，`Plugins.link()` 不再中断启动，行为完全不存在 |
| `unsupported` | 名字都没登记，链接会抛异常 |

### 8.2 Yuri 内核内置（`MOFA_YURI_INTEGRATED_PLUGIN_MODULES`，6 个）

`menu.dll`、`kagparser.dll`、`wuvorbis.dll`、`krmovie.dll`、`motionplayer.dll`、
`squirrel.dll`

### 8.3 由 mofa-yuri-plugins 提供（`MOFA_YURI_INTERNAL_PLUGIN_MODULES`，44 个）

`addfont.dll`、`csvparser.dll`、`dirlist.dll`、`fftgraph.dll`、`fstat.dll`、
`getabout.dll`、`getsample.dll`、`perspective.dll`、`savestruct.dll`、`varfile.dll`、
`win32dialog.dll`、`wutcwf.dll`、`xp3filter.dll`、`extrans.dll`、`extnagano.dll`、
`krflash.dll`、`gfxeffect.dll`、`layerexdraw.dll`、`layerexbtoa.dll`、
`scriptsex.dll`、`layerexsave.dll`、`layereximage.dll`、`layerexareaaverage.dll`、
`layerexfilter.dll`、`layerexparticle.dll`、`layerexyadraw.dll`、`filter.dll`、
`clipboardex.dll`、`windowex.dll`、`stringutil.dll`、`equations.dll`、`json.dll`、
`snow3d.dll`、`messenger.dll`、`systemextouchimage.dll`、`base64.dll`、`qrcode.dll`、
`registory.dll`、`win32ole.dll`、`shrinkcopy.dll`、`sigcheck.dll`、`psd.dll`、
`psbfile.dll`、`sqlite3.dll`

### 8.4 本作真正依赖、而且不是空壳的

| 插件 | 状态 |
| --- | --- |
| `layerExAreaAverage.dll` | **真实实现**：上游 wamsoft 的区域平均 `Layer.stretchCopyAA`，逐像素按原算法计算 |
| `layerExImage.dll` | **真实实现**：light / colorize / modulate / noise 等 |
| `layerExBTOA.dll` | **真实实现**：透明通道与选区（province）搬运 |
| `layerExSave.dll` | **真实实现**：PNG / TLG5 截图与存档图 |
| `shrinkCopy.dll` | **真实实现**：`Layer.shrinkCopy` / `shrinkCopyFast` |
| `fstat.dll`、`savestruct.dll`、`sqlite3.dll`、`psbfile.dll`、`psd.dll`、`extrans.dll` | **真实实现**（含补齐的吉里吉里Z 存储 API） |
| `krmovie.dll` | **真实实现**：FFmpeg 解码 + 自己的呈现后端（`src/platform/vita/yuri_video_overlay.cpp`） |
| `gfxEffect.dll`、`motionplayer.dll`、`layerExDraw.dll` | `control_flow_fallback`：状态与调用面保留，像素/渲染是空实现 |
| `extNagano.dll` | `behavioral_subset`：闭源转场的 provider 名映射到交叉淡入 |
| `krflash.dll` | `link_only`：从不播放 Flash，只保证启动不中断 |

### 8.5 仅可链接（本作靠 `patch.tjs` 接管，缺的只是纯视觉效果）

`layerExFilter.dll`、`layerExParticle.dll`、`layerExYaDraw.dll`、`filter.dll`、
`clipboardEx.dll`、`windowEx.dll`，以及长尾 Windows 专用名字 `stringUtil.dll`、
`Equations.dll`、`json.dll`、`snow3d.dll`、`messenger.dll`、
`SystemExTouchImage.dll`、`base64.dll`、`qrcode.dll`、`registory.dll`、`win32ole.dll`。

其中 `stringUtil.dll` 的 `isNumber/parseKeyFrame/initSpline` 是关键帧动作会用到的样条
助手，目前只有链接；`Equations.dll` 由兼容补丁里的 TJS `class Equations` 替代；
`json.dll` 由引擎自带的 `Scripts.evalJSON` 覆盖实际调用。

### 8.6 新增一个插件模块的流程

1. 在 `include/mofa/yuri_plugin_capabilities.hpp` 的
   `MOFA_YURI_INTERNAL_PLUGIN_MODULES` 里登记发布名（小写），并用
   `yuri_plugin_is_link_only()` / `yuri_plugin_fidelity()` 标出**真实**等级——
   不要为了让审计通过而谎报等级。
2. 在 `src/engine/retail/` 新增同名模块文件（参考
   `yuri_layerexareaaverage_module.cpp`：用 ncbind 静态注册；Vita 上 `Plugins.link()`
   只认这个注册表，不加载 Windows DLL）。
3. 在 `cmake/YuriBackend.cmake` 的插件目标里加入该文件，并在启动路径打一条
   `retail-<名字>-ready` 或 `-load-only-ready` 追踪；
   `src/engine/vita/early_boot_trace.c` 的中英对照表与 `cmake/VerifyYuriBuild.cmake`
   的契约同步更新。
4. 如果脚本会在 `try/catch` 之外立刻使用它的全局/类，把它补进
   `yuri_plugin_surface_contracts`，让主机端审计在这种情况下仍然失败（否则会静默给出
   「看着像能用」的结果）。

---

## 9. 附录

### 9.1 日志标记速查

| 标记 | 含义 |
| --- | --- |
| `yuri-tvpgl-neon-kernels-installed` / `yuri-tvpgl-scalar-kernels` | 内核来源（NEON 或强制标量） |
| `[mofa-tvpgl] ... pick=neon\|scalar` | 逐内核的实测选择结果 |
| `yuri-pixel-meter-installed` | 像素计量已装载（其后的 `[mofa-pixels]` 才有效） |
| `yuri-additive-alpha-scalar-exact-ready` | 加法 alpha 保持与标量逐字节一致 |
| `yuri-bitmap-tiered-allocator-ready` / `-user-rw-memblocks-ready` | 位图分级分配器启用 |
| `yuri-bitmap-memory-pressure` / `-oom-recovered` / `-emergency-tier-used` | 内存压力 / 回收后成功 / 用到紧急下限 |
| `yuri-render-tasks-hybrid-large` | 渲染任务采用"大图才并行"的混合策略 |
| `retail-runtime-5s/30s-stable-with-video` | 稳定运行里程碑（含视频解码） |

### 9.2 证据与对照记录

性能/正确性对照的原始日志放在 `build-release-vita/perf-evidence/`（**已被 gitignore，
不进仓库**），命名约定 `<阶段>-<构建名>-<类型>.txt`，例如：

```text
before-boot-status.txt                   没有任何计量层时的对照
after-boot-status-final.txt              内核自适应之后
p1a-boot-status.txt                      P1a（补齐 100 个内核）之后
p2-kernel-authoritative-boot-status.txt  P2（内核权威分配）之后
p3-load-breakdown-boot-status.txt        P3（场景加载拆解）
meter-v1-error.txt                       位图 OOM 的原始 error.txt
```

对照时按 5.7 的约定：同一场景、同一时长、区分新旧日志。

### 9.3 相关文档

- [docs/性能适配与优化总结.txt](docs/性能适配与优化总结.txt) —— 合并后的性能/优化总结
  （目标、实测、被否掉的假设、优先级与验收线）。
- 本 README —— 开发入口：架构、覆盖机制、测量、排错、插件清单。
