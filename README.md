# mofa-vita-krkr

**《魔法使いの夜》（魔法使之夜 krkr 版）的 PS Vita 启动器。**

引擎侧是 [Yuri](https://github.com/YuriSizuku/Kirikiroid2Yuri)（Android 版
KiriKiri2 / Kirikiroid2 一脉）加 VitaGL 呈现层：游戏保持原样，继续使用它自己的
脚本、XP3 数据包、存档和插件，不做任何数据上传。

| 项目 | 说明 |
| --- | --- |
| 平台 | PS Vita / PS TV（需 HENkaku 或 enso），也可用 Vita3K 测试 |
| 标题 ID | `MOFA00001` |
| 数据根目录 | `ux0:data/mofa-vita/`（游戏固定在 `game/`，补丁在 `patch/`） |
| 实机需要 | `libshacccg.suprx`（真机导出后放到 `ur0:/data/libshacccg.suprx`） |
| 目标 | 实机稳定 40fps、无 >33ms 尖峰、不再出现位图分配失败 |

本文档以**开发/维护**为主：先讲清这套东西怎么搭、怎么测、怎么改，再是安装与使用。
结论性的性能数据在 [docs/性能适配与优化总结.txt](docs/性能适配与优化总结.txt)。

## 当前状态

| | |
| --- | --- |
| Vita3K | 已实测：可进标题，自动/点击推进到 `1-0.ks` page6；序章含转场与视频解码 |
| 真机 PS Vita | 构建通过全部契约检查，**尚未实机验证**（预算推算见文档） |
| 已知缺口 | 见「插件支持清单」的 link-only / fallback 分类；`stringUtil.dll` 的 `isNumber/initSpline` 仍是占位 |
| 已知风险 | 连续游戏约 5 分钟后可能位图分配失败；因果链已在文档中定位 |

---

## 一、开发者视角：这套东西是怎么搭起来的

### 1.1 引擎来源与覆盖方式

上游引擎是固定版本的 Yuri，由 `cmake/Dependencies.cmake` 的 FetchContent 拉取
（`MOFA_YURI_SOURCE_DIR`），**子模块内容不允许改动**。所有引擎侧改动都通过
`cmake/YuriBackend.cmake` 里的「读取源码 → `string(REPLACE)` 打补丁 → 生成到
`build-release-vita/generated/yuri/` → 用它替换原文件」完成。

这套机制是 **fail-closed** 的：每个补丁都先检查锚点文本是否存在，替换后再比对
一次；上游一升级、锚点漂移，配置阶段就直接 `FATAL_ERROR`，不会静默生成一份语义
不明的引擎。`cmake/VerifyYuriBuild.cmake` 在此之上再做一轮「生成结果必须包含哪些
字符串」的契约检查，所以**新增任何接缝都要同时更新契约**。

### 1.2 目录结构

```text
CMakeLists.txt           主机端工具/测试，以及 Vita 构建入口与测试注册
cmake/Dependencies.cmake 固定的 Yuri / Oniguruma / KrKr2-Next / SQLite 版本
cmake/YuriBackend.cmake  引擎补丁与生成（唯一允许改引擎行为的地方）
cmake/Verify*.cmake      fail-closed 构建契约、包内容与追踪校验
docs/                    性能适配与优化总结（非构建输入）
include/mofa/            可移植部分（mofa）的公开头文件；策略常量都在这里
resources/vita/          气泡资源、默认过滤器、启动后脚本
scripts/                 构建、兼容性审计、真机/A9 板卡与纹理试验脚本
src/common/              XP3 读取、过滤器启发式、补丁解析、PNG/SFO 等
src/engine/retail/       零售运行时用到的插件实现（每个模块一个文件）
src/engine/vita/         启动解析、VitaGL 呈现、早期追踪、内核计量与基准
src/platform/vita/       存储、输入、音频、字体、线程、分阶段计时等平台桥接
src/yuri/                TJS 平台适配与过滤器虚拟机
tests/                   主机端测试套件、纹理/图层试验台与零售兼容性样本
vita/booter/             气泡/引导模板
```

与性能工作直接相关的文件（改性能先看这几个）：

| 文件 | 作用 |
| --- | --- |
| `src/engine/vita/vitagl_presenter.cpp` | 上传/呈现：5 张轮转纹理、脏区部分上传、`[mofa-perf]` 计数 |
| `src/platform/vita/yuri_window_layer.cpp` | 图层→呈现桥：完成 surface 的生命周期、光标、指针 |
| `src/platform/vita/yuri_stage_profile.cpp` | `[mofa-stage]` / `[mofa-pixels]` 汇总与输出 |
| `src/platform/vita/yuri_tvpgl_meter.cpp` | 像素计数壳 + 每家族 ns/像素探针 |
| `src/engine/vita/tvpgl_kernel_policy.cpp` | `tvpgl-scalar` 硬覆盖标记 |
| `src/platform/vita/yuri_tvpgl_benchmark.cpp` | 设备自测的内核快慢选择 |
| `src/platform/vita/vita_bitmap_allocator.cpp` | 位图分级分配器（memblock / newlib 回退） |
| `include/mofa/vita_memory_budget.hpp` | newlib 堆、VitaGL 池、保留区的预算常量 |

### 1.3 启动顺序（`boot-status.txt` 里的里程碑）

```text
preinit-entered → main-entered → vita-threading-self-test-passed
→ retail-xp3filter-selected → retail-launch-resolved
→ vitagl-initialized → yuri-platform-ready
→ yuri-storage-preflight-complete → yuri-startup-script-entered
→ retail-layereximage-ready（插件从这里开始注册）
→ yuri-pixel-meter-installed（内核计量装好，性能日志从这行之后开始）
→ yuri-startup-script-complete → vitagl-first-game-frame-presented
→ retail-runtime-5s/30s-stable-with-video
```

每个英文标记的**下一行**是以 `#` 开头的中文说明；英文标记本身保留原样，因为
`cmake/VerifyHardwareBootTrace.cmake` 按整行精确匹配。

---

## 二、性能与测量（本轮新增，改性能的入口）

### 2.1 三层日志

都写进 `ux0:data/mofa-vita/boot-status.txt`，除注明外都是**每帧**单位，每秒一行
（60 帧一个窗口）。

```text
[mofa-meter]  ns/px blend=.. adddest=.. add=.. stretch=.. sadd=.. affine=..
              copyfill=.. cmap=..
```

启动时一行，由**设备上真正装着的那个内核**微基准测出（1024/512 像素 × 64 次 ×
3 轮取最小）。真机是 NEON、模拟器可能是标量，两边必须各测各的，
**不要拿主机的数字外推真机**。

```text
[mofa-stage]  frames=60 loop=.. busy=.. engine=.. script=..(events=.. timer=..
              kag=.. cont=.. tags=.. rest=..) composite=..(%,calls) present=..
              input=.. recycle=.. idle=..
```

`engine` 是引擎一整帧，`script = engine - composite`。四个原生接缝：

| 字段 | 接缝 | 含义 |
| --- | --- | --- |
| `events` | `Application::ProcessMessages` 的消息循环 | 输入/窗口更新事件投递 |
| `timer` | `TVPTimer::ProgressAllTimer` | TJS 定时器 |
| `cont` | `TVPDeliverAllEvents` | **KAG Conductor 在这里**：标签派发、关键帧求值、图层属性写入 |
| `kag` | `KAGParser::GetNextTag` | 原生 KAG 标签解析（`tags` 是解析次数） |
| `composite` | `LayerManager::UpdateToDrawDevice` | 软件合成（图层树 → DrawBuffer） |

```text
[mofa-pixels] frames=60 blend=..k stretch=..k add=..k adddest=..k sadd=..k
              affine=..k copy=..k cmap=..k calls=.. est=..ms
```

100 个热点 TvPgl 内核上装了「只做整数累加」的转发壳，按 8 个家族统计目标像素数
（blend / adddest / add / stretch / sadd / **affine** / copy / cmap）。计数用
relaxed 原子加法，因为行分割混合会跑在两个渲染 worker 上；每次扫描线一次加法，
代价远小于它描述的那次混合。`est` 用 `[mofa-meter]` 的 ns/像素把像素折算成毫秒，
回答「这一帧的钱有多少花在像素上」。

覆盖这一块时注意：`LayerBitmapIntf` 按 `basename / _o / _HDA / _HDA_o` 逐扫描线
选择内核，而其中 `hda = true if destination has alpha`——**往普通 alpha 图层里画
走的是 `_HDA` 内核**。漏掉 `_HDA` 会让计量结果看起来比合成小两个数量级（本项目
确实踩过这个坑，见 `docs/性能适配与优化总结.txt` 的 P1a）。仍未覆盖的是 Photoshop
混合模式族 `TVPPs*`（本作脚本不使用 `blendMode`）与一次性格式转换
`TVPReverseRGB` / `TVPRedBlueSwap*` 等。

呈现侧另有一条 `[mofa-perf]`：`frame=.. upload=.. draw+swap=.. full=.. partial=..
px/frame=.. video_up=.. total_up=..MB`，描述上传与脏区行为。

**读日志的顺序**：先看 `[mofa-stage]` 决定钱花在哪一段，再用 `[mofa-pixels]` 判断
那一段里有多少是像素活，最后用 `[mofa-perf]` 看上传是否退化。

### 2.2 内核的自适应选择

Yuri 的 ARM 后端只要看到 CPU 特性位就装一整套手写 NEON 内核。真机上那是对的，但
模拟器的 JIT 可能把 NEON 退回解释器（结果正确、慢很多）。现在启动时对 8 个吃像素
的内核各测一小段（单轮封顶 25ms），**NEON 必须快 15% 以上才保留**，测不出来就用
标量版。日志为 `[mofa-tvpgl] <内核> scalar=..us neon=..us pick=neon|scalar`，
末尾一行 `yuri-tvpgl-kernels-measured-on-device`。

`ux0:data/mofa-vita/tvpgl-scalar` 仍然有效，语义是「**永远不许装 NEON**」的硬覆盖
（命中时跳过测量，打 `[mofa-tvpgl] scalar-requested-skip-benchmark`）。
**真机数据目录里不要放这个标记。**

### 2.3 内存预算（改内存相关代码前先读）

| 池 | 大小 | 说明 |
| --- | --- | --- |
| newlib 堆 | 128MiB | 启动时固定，`free()` 无法归还内核；脚本/TJS/SQLite/FreeType/小位图 |
| VitaGL 池 | 32MiB | `vglInitExtended` 的阈值按「实际空闲 USER_RW − 32MiB」推导；软件合成下呈现只需约 20MiB（5 张 1024×576 轮转纹理 + 覆盖层 + 交换链），省下的留给位图 |
| 位图 memblock | ≥1MiB 的位图 | USER_RW memblock。分三层：先用完整的 12MiB 保留线判断 → 再用 4MiB 紧急下限 → 最后才退回 newlib 堆 |

预算常量集中在 `include/mofa/vita_memory_budget.hpp`，分配器在
`src/platform/vita/vita_bitmap_allocator.cpp`；两者都有构建契约保护，改动要同步更新
`cmake/VerifyYuriBuild.cmake`。

为什么保留线从 32MiB 降到 12MiB：实测发现**压力时刻的真实状态是 `free_user` 停在保留线
上，而位图只占 28MiB**——卡住分配的不是"真的没内存"，而是那条保留线本身；结果几十 MiB
的多兆位图被挤进**预先固定分配的** 128MiB newlib 堆，堆一旦填满就直接抛
`Cannot allocate memory for Bitmap`。而这个后端里除了位图分配器，没有任何模块申请
USER_RW memblock（脚本 / FreeType / SQLite / 音频 / 视频都从同一个堆分配，VitaGL 的池
在初始化时一次性认领），所以那条大保留线保护不了谁，只是把内存从游戏手里拿走。
现在只留一条小余量应对内核侧增长，真正的"满了"由内核自己的拒绝来报。

### 2.4 已知瓶颈（带证据）

细节与实测表格见 [docs/性能适配与优化总结.txt](docs/性能适配与优化总结.txt)，摘要：

1. **合成 = 像素活，已结案**：补齐 `_HDA` 等内核后，活跃帧 composite 2.16ms/帧
   对应 385k 混合 + 193k 拷贝像素/帧，折算 2.67ms/帧（est/composite ≈ 1.24）。
   真机 NEON 生效时这部分约 0.5-1.5ms/帧，不是 40fps 的障碍；原先「调用开销」
   的假设已被数据推翻。
2. **加载尖峰已拆解**：`kload`（场景读取+切行）峰值 0.85ms、`klabels`（标签缓存）
   0.02ms、`khooks`（游戏自己的 onScenarioLoad）0.00ms —— 45ms 全在 `GetNextTag`
   内部（本作序章的大 `[iscript]` 块），紧随其后 85ms 是游戏自身脚本执行。也就是
   说这是**进章节时的一次性初始化**，不是每帧成本，原先计划的预解析/索引缓存取消。
3. **长跑会位图分配失败**（`1-0.ks:81` 的 `fgact`，3MB 位图）——**已修复首刀**。
   实测快照显示内核报告的 `free_user` 恰好停在我们 32MiB 保留线上，而位图 memblock
   只占 28MB：保留线把多兆位图赶进**预先保留的** 128MiB newlib 堆，堆满即失败。
   现在改为「内核权威」分配（≥1MiB 先申请 memblock）、保留线降到 12MiB、紧急下限
   4MiB、VitaGL 池 48→32MiB。验证：越过原失败点（page6 → page13），0 内存标记。
4. 上传/呈现不是瓶颈（0.36ms/帧），计量层加入前后一致。

---

## 三、诊断

| 文件 | 内容 |
| --- | --- |
| `ux0:data/mofa-vita/error.txt` | 本次启动失败的原因（每次运行重写，先中文后原文） |
| `ux0:data/mofa-vita/boot-status.txt` | 启动追踪 + 三层性能日志（每次运行重写） |
| `ux0:data/mofa-vita/engine.log` | 引擎日志：过滤器、补丁、插件与存储的决定（追加） |
| `game/savedata/krkr.console.log` | KiriKiri 控制台输出，包含脚本异常 |

区分新旧运行：`boot-status.txt` 每次运行重写，可直接当作本次；`engine.log` 是追加的，
用最后一次 `Loading startup script`（或时间戳）切分。采证时建议把 `boot-status.txt`
复制出来并标明构建版本，例如 `perf-evidence/meter-v3-*.txt`。

---

## 四、构建与测试

### 4.1 一条命令（WSL2 完整流水线）

```bash
wsl -d <发行版> -u root bash /mnt/<盘符>/<路径>/mofa-vita/scripts/build-vita-wsl.sh
```

它会自己导出 `VITASDK`、检查依赖（主机包 `libcurl4-openssl-dev zlib1g-dev
libpng-dev libsqlite3-dev libssl-dev libfreetype-dev nasm`；VitaSDK 包
`vdpm install boost libarchive vitaGL openal-soft opusfile`），再跑
「主机测试 → Vita 版 FFmpeg → Vita 后端 → VPK」。首次构建要下载编译 FFmpeg，
约半小时。

### 4.2 日常增量开发

```bash
# 只编译 VPK（改 src/ include/ cmake/ 之后；会自动重新 configure）
wsl -d <发行版> -u root bash -lc 'export VITASDK=/opt/vitasdk; \
  cd /mnt/<盘符>/<路径>/mofa-vita && \
  cmake --build build-release-vita --parallel 12 --target mofa-vita-krkr.vpk-vpk'

# 主机端测试（不需要 VitaSDK）
cmake -S . -B build-release-host -DCMAKE_BUILD_TYPE=Release -DMOFA_BUILD_TESTS=ON
cmake --build build-release-host --parallel
ctest --test-dir build-release-host --output-on-failure                    # 全部
ctest --test-dir build-release-host -R "mofa-tvpgl" --output-on-failure    # 子集
```

主机端注册了 29 个 ctest 用例（其中零售样本、字体黄金样本、兼容性声明这几项是
按条件注册的）：TJS 字节码/字符串语义、事件参数、ObjectList、OpenCV 兼容层、
Squirrel 桥、SQLite 模块、RSA-PSS 签名、PSB/AJPM、MS Gothic 字形黄金样本、
渲染任务分派、纹理别名与图层合成试验台，以及本轮新增的
`mofa-tvpgl-kernel-benchmark`（内核选择规则）与 `mofa-tvpgl-pixel-meter`
（像素→时间换算与舍入）。

### 4.3 构建契约（改引擎行为时必须一起改）

Vita 构建会跑 `cmake/VerifyYuriBuild.cmake`，核对生成的引擎源码以及我们自己的源
文件/头文件里必须出现的关键字符串。它的意义是「删掉某个接缝会直接构建失败」，
而不是靠 review。新增接缝的流程：

1. 在 `cmake/YuriBackend.cmake` 里加 `string(REPLACE)` 生成补丁，带锚点检查
   （替换前存在、替换后已变）并在失败时 `FATAL_ERROR`；
2. 在 `cmake/VerifyYuriBuild.cmake` 里加 `require_text(...)`，写清这条接缝为什么
   必须在；
3. 在 Vita 构建里看到 `Yuri generated-source and backend-boundary contracts
   passed` 才算过。

### 4.4 在 Vita3K 上验证

```powershell
& '<vita3k>\Vita3K.exe' '<仓库>\build-release-vita\mofa-vita-krkr.vpk'
# 已安装过也可以按标题 ID 启动
& '<vita3k>\Vita3K.exe' -r MOFA00001
```

两个坑：

- **重新安装确认框**：同一版本再次启动时 Vita3K 会弹「是否重新安装该内容？」并一直
  等待——窗口标题停在应用名、`vita3k.log` 为 0 字节、游戏不启动，直到点 OK。
  自动化/远程验证时要注意这一步。
- **着色器编译器**：把真机导出的 `libshacccg.suprx`（约 3MB，`SCE\0` 开头）放到
  `ur0\data\libshacccg.suprx`。0 字节占位文件会在现场编译着色器时把模拟器打成崩溃，
  本构建会拒绝占位文件并给出中文提示。

数据目录与真机一致：`<存储卡>\ux0\data\mofa-vita\`。

---

## 五、在真机上安装

1. 用 VitaShell 安装 `mofa-vita-krkr.vpk`。
2. 准备数据目录（**目录名不能改**）：

   ```text
   ux0:data/mofa-vita/
   ├── game/                     复制整个游戏目录的内容
   │   ├── data.xp3              游戏启动
   │   ├── bg.xp3  bgm.xp3  fg.xp3  image.xp3  rule.xp3  sound.xp3     游戏资源包
   │   ├── mahoyo.xp3  mahoyo.dll  *.exe
   │   ├── plugin/               游戏附带插件（原样保留）
   │   └── savedata/             存档（可留空，首次运行自动建）
   ├── patch/
   │   ├── xp3filter.tjs         XP3 解密过滤器（必需，否则读不出正文）
   │   └── patch.tjs             兼容补丁（必需，插件入口靠它接管）
   ├── msgothic.ttc              字体
   ├── mofa.ini                  可选：按键映射（见第六节）
   └── boot-status.txt / error.txt / engine.log   日志
   ```

3. 启动游戏。

**解密过滤器**按固定顺序确定：`patch/xp3filter.tjs` → 游戏目录自带的
`xp3filter.tjs` → **Phase 1 归档推断**（直接采样真实 XP3、合成规则，且用归档数据
验证通过才采用，结果缓存在 `heuristics/<指纹>/`）→ 都不成立时带着确切原因停下，
而不是猜一个会把数据解坏的过滤器。

**兼容补丁** `patch/patch.tjs` 把本作用到的 Windows 插件入口换成 TJS 实现，这是本作
能在非 Windows 平台跑起来的关键。

---

## 六、按键映射（可选）

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
可绑定来源：`cross circle square triangle ltrigger rtrigger dpad_up dpad_down
dpad_left dpad_right start select left_stick front_touch`。
可绑定动作：`mouse_left mouse_right mouse_wheel_up mouse_wheel_down mouse_cursor
mouse_absolute`、按键 `key_space key_enter key_escape key_pageup key_pagedown
key_up key_down key_left key_right key_control key_shift key_tab key_backspace`、
`menu`（等同 Escape）以及 `disabled`。

默认：左摇杆移动指针，前触摸屏直接定位，L = 鼠标左键，× = 鼠标右键，○ = 回车，
△ = 滚轮上滚，R = 按住 Ctrl（快进），方向键 = 方向键。

---

## 七、插件支持清单

**权威来源是 [include/mofa/yuri_plugin_capabilities.hpp](include/mofa/yuri_plugin_capabilities.hpp)**
（`yuri_plugin_fidelity()` / `yuri_plugin_is_link_only()`）。主机端兼容性审计
（`scripts/run-retail-compatibility.sh`）与 Vita 侧封闭加载器共用同一张清单；下表
与它保持一致，**改等级时三处要一起改**。

### 7.1 分级含义

| 等级 | 含义 |
| --- | --- |
| `portable_equivalent` | 上游或等价的可移植实现直接编进来（真正的功能） |
| `behavioral_subset` | 用替代方案实现可观察行为（例如把闭源转场映射到交叉淡入） |
| `control_flow_fallback` | 暴露脚本可见的名字与状态、控制流能继续，但像素或音频是空实现 |
| `link_only` | 只登记名字，`Plugins.link()` 不再中断启动，行为完全不存在 |
| `unsupported` | 名字都没登记，链接会抛异常 |

### 7.2 Yuri 内核内置（`MOFA_YURI_INTEGRATED_PLUGIN_MODULES`，6 个）

`menu.dll`、`kagparser.dll`、`wuvorbis.dll`、`krmovie.dll`、`motionplayer.dll`、
`squirrel.dll`

### 7.3 由 mofa-yuri-plugins 提供（`MOFA_YURI_INTERNAL_PLUGIN_MODULES`，44 个）

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

### 7.4 本作真正依赖、而且不是空壳的

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

### 7.5 仅可链接（本作靠 `patch.tjs` 接管，缺的只是纯视觉效果）

`layerExFilter.dll`、`layerExParticle.dll`、`layerExYaDraw.dll`、`filter.dll`、
`clipboardEx.dll`、`windowEx.dll`，以及长尾 Windows 专用名字 `stringUtil.dll`、
`Equations.dll`、`json.dll`、`snow3d.dll`、`messenger.dll`、
`SystemExTouchImage.dll`、`base64.dll`、`qrcode.dll`、`registory.dll`、
`win32ole.dll`。

其中 `stringUtil.dll` 的 `isNumber/parseKeyFrame/initSpline` 是关键帧动作会用到的
样条助手，目前只有链接；`Equations.dll` 由兼容补丁里的 TJS `class Equations` 替代；
`json.dll` 由引擎自带的 `Scripts.evalJSON` 覆盖实际调用。

> **链接 ≠ 有实现。** 登记名字只保证「链接成功」。真机上某个特效没出现时，先看
> `game/savedata/krkr.console.log` 是不是某个空实现被调到了，而不是启动失败。

### 7.6 新增一个插件模块的流程

1. 在 `include/mofa/yuri_plugin_capabilities.hpp` 的
   `MOFA_YURI_INTERNAL_PLUGIN_MODULES` 里登记发布名（小写），并用
   `yuri_plugin_is_link_only()` / `yuri_plugin_fidelity()` 标出真实等级——不要为了
   让审计通过而谎报等级。
2. 在 `src/engine/retail/` 新增同名模块文件（参考
   `yuri_layerexareaaverage_module.cpp`：用 ncbind 静态注册；Vita 上
   `Plugins.link()` 只认这个注册表，不加载 Windows DLL）。
3. 在 `cmake/YuriBackend.cmake` 的插件目标里加入该文件，并在启动路径打一条
   `retail-<名字>-ready` 或 `-load-only-ready` 追踪；`src/engine/vita/early_boot_trace.c`
   的中英对照表与 `cmake/VerifyYuriBuild.cmake` 的契约同步更新。
4. 如果脚本会在 `try/catch` 之外立刻使用它的全局/类，把它补进
   `yuri_plugin_surface_contracts`，让主机端审计在这种情况下仍然失败（否则会静默
   给出「看着像能用」的结果）。

---

## 八、常见问题

| `error.txt` 里的报错 | 原因 | 处理 |
| --- | --- | --- |
| `没有找到精确匹配的 xp3filter.tjs…` | `patch/` 里没有过滤器，且只靠归档数据解不出来 | 把本作的 `xp3filter.tjs` 放进 `ux0:data/mofa-vita/patch/` |
| `vitaGL 需要真实的 ur0:/data/libshacccg.suprx 着色器编译器` | 没装着色器编译器，或只放了 0 字节占位文件 | 安装/复制真的 `libshacccg.suprx` |
| `Member "isExistentDirectory" does not exist` | 用的是补齐 Z 版 API 之前的旧构建 | 用本仓库重新编译 |
| `Cannot load Plugin <插件名>.dll` | 作品链接了尚未登记的 KAGEX 插件 | 按 7.6 登记并实现（或至少登记为 link-only） |
| `找不到游戏目录：ux0:data/mofa-vita/game` | 游戏没放到固定目录 | 放进 `ux0:data/mofa-vita/game/` |
| `Yuri 没有跑完游戏的启动脚本（启动脚本中途中止）` | 游戏自己的启动脚本抛了异常 | 看 `game/savedata/krkr.console.log` 的脚本异常 |
| `Cannot allocate memory for Bitmap ...` | 位图内存耗尽（因果链见文档） | 已知问题，按 `docs/性能适配与优化总结.txt` 的 P2 处理 |
| 画面正常但某些特效没有 | 对应插件只是 link-only / fallback | 见第七节 |

### Vita3K 上偏慢 / 模拟器日志暴涨

Vita3K 的 CPU 模拟（Dynarmic）不是每条 ARM NEON 指令都能翻译：混合内核里的
`vsubhn` 序列会退回解释器，日志里刷 `InterpreterFallback` / `Undefined
instruction`，模拟器会明显变慢。**本构建现在会自己测量并留在标量内核**（日志
`[mofa-tvpgl] <内核> ... pick=scalar`），一般不需要手动干预；确实需要强制时再放
`ux0:data/mofa-vita/tvpgl-scalar` 标记（真机不要放）。
