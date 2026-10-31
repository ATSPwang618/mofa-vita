# 渲染与 GPU 后端

> 适用范围：当前工作树（`perf/render` 提交 `a670193` + 2026-10-31 文档重构）；Vita 正式版固定 960×540。
> 标记约定：**[代码]** = 当前源码常量/协议；**[历史实测]** = 仓库归档的 r2/r3/r4 真机捕获（分辨率 **480×270 scene + 720×404 composite，不等于终版 960×540**，只作结构证据）；**[未验证]** = 仓库内无设备日志/像素证据。
> 历史原文已从工作树清理；追溯见 **git 提交 `a670193` 的 `doc/archive/`**。优化开关总表与实测数据见 [05-优化记录与实测数据](05-优化记录与实测数据.md)；资源与缓存策略见 [04-资源调度与缓存](04-资源调度与缓存.md)。
> 下文 `file:line` 的短名约定：`vita.rs`=`knife/knife-render/src/vita.rs`，`game.rs`=`knife/knife/src/game.rs`，`gxm_backend.c`/`gpu_lifetime.inc`/`effects_backend.inc`/`runtime_shaders.c`/`effects.cg`=`knife/vita/`，`gxm.rs`=`knife/knife-platform-vita/src/gxm.rs`，`memory_budget.rs`=`knife/knife-perf/src/`。

---

## 1. 分层与数据流

Vita 渲染栈自下而上分为四层：

```
Game/UI/Stage（knife）──Quad/CommandEncoder──► Renderer facade（knife-render/vita.rs）
                                                        │ FFI（knife-platform-vita/gxm.rs）
                                                        ▼
                                        C 后端：gxm_backend.c
                                        ├─ gpu_lifetime.inc（批次/完成/池/rename）
                                        ├─ effects_backend.inc（程序 patch/组/文字缓存）
                                        ├─ movie_gxm.inc（AvPlayer 帧重绑）
                                        └─ runtime_shaders.c（启动编译/GXP 缓存）
                                                        │ GXM API
                                                        ▼
                                        GPU（固定 960×540 scene/composite → 960×544）
```

桌面后端是同一 facade 的 WGPU 实现（`knife-render/src/lib.rs:1-15`、`desktop.rs:1-5`）；本文件只描述 Vita 侧。C 后端是**单一静态库** `knife_vita_platform`，由 Cargo `build.rs` 调 CMake 编译并链接（`knife/vita/CMakeLists.txt:14`、`knife-platform-vita/build.rs:54-83`）；`gxm_backend.c` 在固定位置包含三个 `.inc` 与电影实现（`gxm_backend.c:343,848,850`）。

| native 文件 | 角色 |
|---|---|
| `knife/vita/gxm_backend.c` | GXM 初始化、scene/纹理/present/readback/销毁、能力查询 |
| `knife/vita/gpu_lifetime.inc` | 批次槽、completion、依赖图、纹理退休、CDRAM 池、源 rename |
| `knife/vita/effects_backend.inc` | effect 程序注册与 patch、compact 顶点、组融合、文字缓存、copy/present |
| `knife/vita/runtime_shaders.c` | 运行时 Cg 编译、编译器模块加载、GXP 校验与落盘缓存 |
| `knife/vita/{shader_sha256,shader_module_exports,shader_extensions}.h/.c` | 编译器身份、SCE ELF 导出解析、Cg 扩展启用 |
| `knife/vita/perf_audit.c` / `perf_counters.def` | 108 个 native 指标与计时 |
| `knife/vita/shaders/effects.cg` | 效果/PS/组/文字全部着色器源；`generated/runtime_source.h` 为内嵌生成副本 |
| `knife/vita/platform.c`、`image_decode.c`、`jpeg_hw.c`、`movie_player.c` | 堆/时钟、软件图像解码、硬件 JPEG、AvPlayer |

## 2. Rust 渲染抽象

共享类型在 `knife-render-api/src/lib.rs`：画布 1920×1080（`:5-8`）；112 B `Vertex`（`:10-33`，含 2×float2 + float4 + 2×float4 haze + 3×float4 soft）；`Blend`（`:35-58`，含 `Ps(PsOp)`/`Over`/`Footprint`/`MaskAlpha`）；`PsOp` 24 种目标色公式（`:62-113`）；`Shade`（`:115-131`）；`SoftClip`（`:147-167`）。Vita 侧再导出这些类型（`vita.rs:26-29`）。

| 类型 | 含义 | 依据 |
|---|---|---|
| `Renderer` | 一帧的渲染器；持有 `Device`（GXM owner）、backdrop 缓存、命令/顶点复用池、soft LUT、white 纹理 | `vita.rs:1062-1071` |
| `Device` | `RefCell<Gxm>` + 退休队列 + deferred 预算；**不得离开创建线程** | `vita.rs:328-345` |
| `GpuTexture`/`TexRef` | `Arc<GpuTexture>` 逻辑纹理；native 句柄与内容版本在 `NativeTexture` | `vita.rs:638-655` |
| `RenderTarget` | 包一层 `TexRef` 的目标 | `vita.rs:694-696` |
| `CommandEncoder` | 待提交命令、已记录顶点字节、当前打开的 pass | `vita.rs:698-707` |
| `Command` | `Begin/End/Draw/Copy` 四种；`Draw` 含 4 个纹理槽与 8 个 uniform | `vita.rs:815-833` |
| `Quad` | 一次 quad 绘制：pos/uv/color、Blend、Shade、clip、rule、haze、soft、masked | `vita.rs:929-944` |

FFI 面在 `knife-platform-vita/gxm.rs:6-49`（创建/纹理/effect draw/copy/present/readback/批次/计划/能力/CDRAM）。GXM 句柄是线程绑定的（`gxm.rs:138-146`），纹理槽用 `Vec<Option<TextureRecord>>` 管理，失败时 owner 进入“仅可销毁”状态（`gxm.rs:149-199`）。host 测试用模拟后端替换整个 `Gxm`（`vita.rs:18-24`、`vita_test_backend.rs`）。

帧级硬上限：命令数 32,768、顶点字节 16 MiB、单次合并 draw 16,000 顶点（`vita.rs:45-57`）；超限返回错误而不是提交部分帧（`:836-866`）。

## 3. 命令编码与提交

`CommandEncoder::append` 负责三件事（`vita.rs:835-926`）：① 检查帧级命令/顶点预算；② 给 `Begin/Copy` 的目标递增内容版本（供缓存 key 失效）；③ 把相邻、shader/blend/params/haze/纹理全部相同的 `Draw` 合并（`:874-925`）。合批只合并完全同状态，跨 haze 不合并（haze 是逐 draw 的 uniform，`:907-910`）。

提交入口 `try_present` → `execute_with_admission`（`vita.rs:2484-2545`、`:3012`）：

1. `prepare_deferred` 准备本帧所有缺失纹理（上传/解码/淘汰，`:2770`）；
2. 统计 basic quad 与 effect 顶点，调用 `begin_batch` 预留整批 arena（`:3012-3030`）；
3. 顺序执行 `Command`：`Begin` 前用 `plan_scene` 提交目标与读取集（`:3043-3071`），`Draw` 绑定程序/纹理/uniform 后 `sceGxmDraw`，`Copy` 走 `copy_texture`（`:3154`）；
4. `present_canvas` 把画布交给扫描输出（`:2518-2525`），最后回收命令与顶点 `Vec`（`:2528-2543`）。

命令与 native 调用的对应关系：

| `Command` | native 入口 | 关键动作 | 依据 |
|---|---|---|---|
| `Begin{target,clear}` | `kv_gxm_begin_target` → `BeginScene` | 先 `plan_scene`，按 clear 决定是否先画一个 basic quad | `effects_backend.inc:547-574`、`vita.rs:3043-3071` |
| `Draw` | `kv_gxm_draw_effect` | 绑 VS/FS/4 纹理/uniform，compact 时设第二路流，`sceGxmDraw` | `effects_backend.inc:631-720` |
| `End` | `kv_gxm_end_target` → `EndScene` | 写当前槽 completion 序号 | `effects_backend.inc:575-590`、`gpu_lifetime.inc:155-169` |
| `Copy{src,dst}` | `kv_gxm_copy_texture` | 渲染来源走 textured quad；纯线性走 `sceGxmTransfer` | `vita.rs:3154`、`effects_backend.inc:746-793` |
| present | `kv_gxm_present_canvas` | 画布采样到内容区 + `DisplayQueueAddEntry` | `effects_backend.inc:795-812`、`gxm_backend.c:616-654` |

## 4. 批次 arena 与顶点

**批次 arena**：`BATCH_SLOTS = 2`、单槽上限 **4 MiB**、按 64 KiB 增长，整批的 basic clear/copy/present 顶点与 effect 顶点共用当前槽（`gxm_backend.c:130-134`、`gpu_lifetime.inc:194-235`）。槽容量按整批最坏情况预检，`batch_vertices` 在下一个 draw 超容量时触发**受控分段**：EndScene → 等该槽真实完成 → 复用/扩容 → 重开同一目标且不重复 clear（`gpu_lifetime.inc:243-290`）。scene 切换不重置游标，因此普通场景切换不产生额外分配。

**顶点两条流**：完整 `KvEffectVertex` 为 112 B/vertex（`vita.rs:929-944`、`knife-render-api/src/lib.rs:10-33`）；`COMPACT_STREAM` 打开时，若一次 draw 的全部 haze/soft 在 float 位模式上完全一致，则只写 32 B position/uv/color，并把 80 B haze/soft 作为第二路 stride=0 常量流；否则保留 112 B（`effects_backend.inc:53-85`、`:124-155`）。两者使用同一个已 patch 的 VS 与相同 varying ABI，fragment patch 不变。Rust 侧复用 `Vec<Vertex>`/`Vec<Command>`，单帧回收顶点容量上限 16 MiB（`vita.rs:2043-2046`、`:2528-2543`）。

[历史实测] r2/r3/r4（480×270/720×404，不等于终版 960×540）中 compact 覆盖 100% effect draw，顶点输入 120,111,936→47,003,776 B（−60.87%）；终版同口径数据 [未验证]。

## 5. Scene/pass、依赖图与完成

GXM 的每个 `sceGxmBeginScene/EndScene` 都是一个受限 pass；native 侧对 scene 分类供审计（`gxm_backend.h:7-11`）。提交路径：

- `execute_with_admission` 在 `Begin` 前收集该 scene 后续到下一个 `Begin/End/Copy` 之间的全部采样纹理，调用 `plan_scene(target, reads, kind)`（`vita.rs:3048-3065`）；
- native `kv_gxm_scene_plan` 用每纹理的 `last_read/last_write` 与“连续完成前沿”算 RAW/WAR/WAW；只有 `need > covered` 才在该 scene 加 `VERTEX_WAIT_FOR_DEPENDENCY`，每个提交 scene 都 SET fragment dependency（`gpu_lifetime.inc:88-151`）；
- 缺计划/漏读由 `require_scene_read` 在真实完成边界兜底（`:291-297`）；批次容量分段同样走真实完成（`:243-276`）；
- 每个 `EndScene` 把单调序号写入**当前批次槽自己的** notification word（`end_submitted_scene`，`:155-169`）；全局退休前沿取所有未完成槽区间中最早的未完成序号−1，绝不取两 word 的最大值（`:32-47`）；复用槽只等自己的 `last_sequence`（`:56-74`）。

CPU 侧资源复用由完成值保护：纹理销毁先逻辑 retire，`last_use <= completed` 后才销毁 native target/descriptor/像素（`gpu_lifetime.inc:170-186`、`gxm_backend.c:527-549`）；内存压力才显式 `kv_gxm_reclaim(wait=1)`（`gpu_lifetime.inc:188-193`）。CPU-origin 小纹理更新可走双版本 rename：复制新 backing、旧 owner 按完成退休，避免整段 GPU 等待（`gpu_lifetime.inc:411-472`、`effects_backend.inc:495-516`）。

## 6. Present 与显示队列

`kv_gxm_present`（`gxm_backend.c:616-654`）顺序：EndScene → `sceGxmPadHeartbeat` → `DisplayQueueAddEntry` → 切 back/front。异步路径（`ASYNC_SUBMIT` + 有效完成区）**不再**调用 `sceGxmFinish`/`sceGxmDisplayQueueFinish`，依赖 GXM 场景依赖、fragment 完成、独立批次槽与有界显示队列；传统 A/B 路径仍保留这两个排空。显示队列最大 pending 数在异步时为 `BUFFERS-1 = 2`（`gxm_backend.c:348`、`:34`）；保留三张 scanout buffer 与 sync 对象。

仍保留真实同步的路径：CPU readback（`:732-743`）、DEV 直扫/交换（`:655-730`）、电影帧 rebind/finish（`movie_gxm.inc:34-45`）、非渲染来源的 transfer copy/update/readback（`effects_backend.inc:746-793,813-826`）。present 前的 canvas blit 是 `present_canvas`：一条 5 指令 basic FS 采样画布并画到 960×544 内容区，避免 effect 着色器与大顶点 ABI（`effects_backend.inc:795-812`）。

## 7. PS 目标色读取、组融合与 pass 级缓存

### 7.1 PS destination fetch

`Blend::Ps(op)` 的公式需要读取“本 draw 之前的目标像素”（24 种 op，`knife-render-api/src/lib.rs:62-113`）。传统实现是 **EndScene → GPU copy 目标到 backdrop → 重开同目标 → PS draw**；fetch 实现直接读目标颜色寄存器，省掉 copy 与两次 scene 边界：

- 着色器入口：`__nativecolor __regformat unsigned char4 ... : FRAGCOLOR`，raw 0..255 UCHAR4，按原公式/早退计算（`effects.cg:481-560`）；覆盖通用 24-op（17）、op19（18）、组 op19（19）、op11（20）、op13（21）、plain sprite（22）、plain op19（23）、plain op2（25）。
- Rust 选择：`PS_FETCH` 开关 + native 能力位可用才用 fetch，否则完全回退 snapshot 链（`vita.rs:1951-1973`、`:2049-2068`、`:2213-2270`）。op18 非 Fill 即使没有编译器也可直接用既有 NoAlpha/Opaque 程序（`:2663-2666`）。

能力位只在“RegisterProgram + 全部必需 patch 成功”后置位（`effects_backend.inc:156-184`），启动时逐条尝试并把结果打印为 `shader_capabilities` JSON（`:279-318`）。[历史实测] r3/r4（480×270/720×404，不等于终版 960×540）中 fetch 未成立、PS 仍走 snapshot；终版是否成立 [未验证]，见 [07](07-已知限制与未验证项.md)。

### 7.2 组融合与组批次

图层组原本需要 pixel surface、mask surface、composite 三次 pass。`group-fused` 用一次全屏 draw 采样 pixel/mask 并保留旧中间量化（`effects.cg:330-345`、`vita.rs:2225-2271`）；PS 组同样可选 fetch。能力不足或不满足布局时回退 scratch 链（`vita.rs:2162-2172`）。

`group-batch`（`knife/knife/src/game/group_batch.rs:19-107`）把“完整 clear 的独立 group producer”提前到 stage 之前，最多 `GPU_GROUP_ENTRIES=6` 组，再按原顺序 sample 结果；`group_cache` 对完全 clear 的组表面做精确 memoize（`group_cache.rs:12-80`）。不满足条件时记录 bypass 并走原路径（`game.rs:6316-6361`）。

### 7.3 文字缓存与 stage 前缀

- **text cache**：连续 ≥8 个 Text/RimText Alpha glyph 画到透明目标（专用 `CoverageOver=9` 混合），再用 `fs_alter` 原样合成；命中时用一次合成替代整段 glyph draw（`knife/knife/src/game/text_cache.rs:92-195`、`vita.rs:1869-1930`、`effects_backend.inc:207-215,260-277`）。预算 12 MiB/8 项、≤1024 glyph、600 帧空闲淘汰（`text_cache.rs:14-16`）。
- **stage prefix**：纯 stage 前缀（clear.a=1、≤64 quad、全部 Alpha/Ps、源为不可变 deferred 纹理或 white、前缀≥2 且含 PS）按 `Quad::render_key()` 精确 memoize，命中后用一次 Opaque+NoAlpha 全屏 quad 替代（`stage_prefix.rs:46-130`、`game.rs:6307-6313`），预算 3 MiB（`knife-perf/src/memory_budget.rs:13`）。
- **direct stage**：无 ghost/focus/transition 时画布直接读 `stage_rt`，跳过 stage→scene 复制 pass（`game.rs:6726-6735`）。

## 8. 纹理、CDRAM 与 budgets

纹理创建路径：RGBA/R8 线性、GXT/PVRT4/BC1/BC3 压缩、render target、AvPlayer movie 表面。压缩 GXT 按编码字节直传，不 CPU 解码；`gxt_decode.rs` 只服务确实需要像素的操作（`knife-core/src/format/gxt_decode.rs:1-2`）。

`TEXTURE_POOL` 打开时：逻辑收费按 **64 B** 对齐，物理 CDRAM 页至少 **256 KiB**，页内按 span 拆/合并，空页缓存 ≤2 MiB（`gpu_lifetime.inc:299-406`）；上传只清 row padding 与 slice 尾部，不再清零整块（`:379-399`）。`cdram_available` 取“可新增映射余量”和“最大连续空闲 span”的较大值，不把 holes 相加（`gxm_backend.c:745-760`）。

| 预算 | 值 | 依据 |
|---|---:|---|
| native CDRAM 策略上限 | 104 MiB − 3 MiB JPEG reserve = **101 MiB** | `gxm_backend.c:35`、`jpeg_hw.h:7` |
| MAIN 映射上限 | 16 MiB | `gxm_backend.c:36` |
| newlib 堆 | 256 MiB | `platform.c:19` |
| CPU source / graded | 64 / 32 MiB | `memory_budget.rs:4-5` |
| GPU resource / source | 56 / 32 MiB | `memory_budget.rs:6-7` |
| 文字 / 组 / stage 前缀 | 12 MiB(8 项) / 12 MiB(6 项) / 3 MiB | `memory_budget.rs:8-13` |
| 批次槽 | 2 × 4 MiB | `gxm_backend.c:130-134` |

## 9. 运行时 shader 编译与缓存

构建期：CMake 先执行 `tools/generate-runtime-shaders.py --check`，拒绝陈旧的 `runtime_source.h`（`knife/vita/CMakeLists.txt:6-12`）；`SceShaccCgExt` 归档与 7 个第三方静态库在 CMake 里逐 SHA 固定（`:15-35`）。生成头绑定 Cg 源码 SHA、编译器 SHA/字节数与 entry ABI（`shaders/generated/runtime_source.h:4-9`）。

启动期（`runtime_shaders.c:331-418`）：

1. 先查 `ux0:/data/knife-vita/shader-cache/`；10 个条目全部命中则不加载编译器（`:337-343`）；
2. 否则依次检查 4 个候选路径（`app0:/sce_module/libshacccg.suprx` 等），核对 1,776,523 B 大小与完整 SHA-256（`:344-362`、`:309-330`）、模块名与 7 个导出的归属（`:202-253`、`shader_module_exports.h:23-58`）；
3. 用 `sceShaccCgExtEnableExtensions()` 启用 `__nativecolor`/`__regformat` 等 Cg 扩展（`shader_extensions.c:25-54`）；失败则跳过 native 声明编译（`runtime_shaders.c:390-412`）；
4. 编译 O3、禁用 fastmath/fastprecision/fastint；产物先过完整校验：GXP 魔数、大小、stage/native-color 标志、输出寄存器格式（fetch 要求 UCHAR4）、参数/采样器/边界、`sceGxmProgramCheck`（`:93-150`）；
5. 通过后写缓存：168 B 头（ABI 5、entry ABI 2、policy 1、source/compiler SHA、FNV-1a 校验和）+ 程序字节；先 `.tmp` 再 `rename`（`:24-46,183-201`），路径同时嵌 source/compiler SHA 与 ABI/ID（`:151-155`）。

10 个运行时 ID 与入口（`runtime_shaders.c:49,56-80`）：

| ID | 入口 | 用途 |
|---:|---|---|
| 11 | `fs_alter` | encoded RGBA 原样直通（文字缓存/合成） |
| 15 | `fs_group_composite` | 全屏组像素×mask 合成 |
| 17 | `fs_ps_fetch` | 通用 24-op 目标色读取 |
| 18 | `fs_ps19_fetch` | op19 专用目标色读取 |
| 19 | `fs_ps19_group_fetch` | 全屏组 op19 |
| 20 | `fs_ps11_fetch` | flat op11 HardLight |
| 21 | `fs_ps13_fetch` | flat op13 Overlay |
| 22 | `fs_sprite_plain` | 无 haze/soft/rule 的普通 Sprite |
| 23 | `fs_ps19_plain_fetch` | 无 haze/soft/rule/group 的 op19 |
| 25 | `fs_ps2_fetch` | 普通 Sprite 的 op2 Darken |

每个 ID 独立接纳；`effects_backend.inc:156-184,279-318` 把失败降级为旧程序并清除能力位，Rust 只按能力走快路径。

## 10. 颜色契约与 540p/letterbox

当前只保留 **encoded UNORM8 + UCHAR4** 一条契约：没有 sRGB/HALF4 双输出模式（`gxm_backend.c:42,241-255`），fragment program 只 patch `SCE_GXM_OUTPUT_REGISTER_FORMAT_UCHAR4`（`effects_backend.inc:192-220`），cache 校验要求输出 varyings 为 UCHAR4（`runtime_shaders.c:119-120`）；Alter/present 直接过采样、`output_level`/`target_level` 恒等（`knife-render/src/vita.rs:295-298`），perf header 记录 `color_contract=encoded-unorm8-uchar4`（`vita_entry.rs:77`）。HALF4/sRGB 与 gamma 设置只存在于历史版本，已从当前代码移除；历史原文见 git 提交 `a670193` 的 `doc/archive/`。

尺寸固定：正式 540p 构建 `RENDER_W/H = 960×540`（`vita.rs:36-43`）；`scene_size()`/`composite_physical()` 由 1920×1080 画布折半（`:3324-3334`）；扫描输出 `WIDTH=960, HEIGHT=544, CONTENT_H=540, CONTENT_Y=2`（`gxm_backend.c:32-36`），present 只画内容区，形成上下各 2 px letterbox（`effects_backend.inc:795-812`、`README.md:38`）。270p 变体由 `assets-540p` feature 切换，仍保留但不属正式素材链。

## 11. 失败回退矩阵

| 场景 | 条件/失败 | 回退行为 | 依据 |
|---|---|---|---|
| 异步提交不可用 | 无 batch arena 或 completion region | 每 pass `sceGxmFinish` + display drain（传统路径） | `gpu_lifetime.inc:28-30`、`gxm_backend.c:554,634-644` |
| 依赖计划缺失/漏读 | `need > covered` 或 draw 读未计划纹理 | 真实完成边界重开同目标，计 `scene_plan_fallback` | `gpu_lifetime.inc:243-276,291-297` |
| 批次容量溢出 | 下一 draw 超出当前槽 | EndScene→等完成→扩容/复用→重开（不重复 clear） | `gpu_lifetime.inc:243-290` |
| 可选 shader 编译/注册/patch 失败 | 运行时程序不被接纳 | `disable_optional_shader`，恢复 legacy 程序与能力位 | `effects_backend.inc:156-184,238-256` |
| PS fetch 不可用 | 无对应能力位或非目标布局 | 旧 snapshot copy + 重开同目标 | `vita.rs:2049-2068,2213-2270` |
| 组融合不可用 | 布局/能力不符 | scratch pixel/mask/composite 路径 | `vita.rs:2162-2172` |
| compact 顶点 patch 被拒 | driver 拒绝第二路流 | 保留完整 112 B/vertex 路径 | `effects_backend.inc:124-155` |
| 文字缓存不可用 | 能力位 24 缺失/构建失败/输入不符 | 逐 glyph 原路径，计 `TextCacheFallback` | `text_cache.rs:100-104,136-142,176-183` |
| stage 前缀不合格 | clear/混合/quad 数/源不合规 | 正常重画 stage，计 bypass | `stage_prefix.rs:50-100` |
| 组批次不合格 | alias/能力/预算不满足 | 原 stage/group 绘制顺序 | `group_batch.rs:58-77` |
| 纹理池关闭/满 | flag off 或页分配失败 | 256 KiB 独占分配 | `gpu_lifetime.inc:341-348` |
| rename 不合格 | target/尺寸/预算超限 | 原有 completion 等待后更新 | `gpu_lifetime.inc:411-433` |
| 素材校验失败 | 缺文件/尺寸/内容不符 | 启动错误界面并中止 | `knife/knife-platform-vita/src/asset_check.rs:36-52`、同目录 `boot_error.rs` |
| GXM 初始化失败 | SDK 返回错误 | 保留 cleanup-only owner，不可渲染 | `gxm.rs:149-168` |
| 帧提交失败 | 绘制/准入错误 | panic 并携带 scene/pc/layer dump（fail-fast） | `game.rs:6906-6912` |

## 12. Host 测试与设备验证

**Host 测试（不需要 Vita）**

- Rust：`cargo test -p knife-render` 使用模拟 GXM 后端（`vita.rs:18-24`、`vita_test_backend.rs`），`vita_tests.rs` 有 76 个 `#[test]`，覆盖命令编码、批次、PS/组/文字缓存、预算与回退。
- C：`knife/vita/tests/*.c`（GXM copy/compressed/present/viewport/budget、effect 输入、图像解码、JPEG、音频），由 `tools/test-vita-*.sh` 在 host 编译运行；native 代码按 `-Wall -Wextra -Werror` 编译（`CMakeLists.txt:37`）。

**设备验证（opt-in，默认静音）**

- 开关：`perf-enable.txt`（`knife-platform-vita/src/perf_switch.rs:10-22`）；优化 profile 文件启动读取一次（`vita_entry.rs:342-349`）；正式包默认 `KNIFE_VITA_QUIET=1`（`tools/build-vita.sh:30`），测量需 `KNIFE_VITA_QUIET=0` 重建。
- 日志：控制台 JSON 每 5 s 一个 window/audit，header 记录 build tag、时钟、scene/composite 尺寸、素材清单 SHA、优化 mask、颜色契约、缓存预算（`vita_entry.rs:45-111,467-481`）；native 侧 108 个累计计数器（`perf_audit.c:39-78`、`perf_counters.def`）覆盖上传/清零/copy/Finish/EndScene/display/批次等待/池/rename/PS 路径等；watchdog 每 500 ms 输出 `runtime_state` JSON（`vita_watchdog.rs:131-186`）。
- 离线分析：`tools/read-vita-performance.py` 把日志转为 JSON 报告（`:1-40`），`tools/test-vita-performance.py` 做解析回归；构建收据/包检查见 `tools/vita-build-receipt.py`、`tools/check-vita-package.py`。注意离线分析器依赖未入库的 `tools/vita_audit_log.py`（见 [07](07-已知限制与未验证项.md)）。
- [历史实测] r2/r3/r4 的 480×270/720×404 捕获只用于验证结构（如 group 融合后 scratch 3→0、compact 覆盖与字节减少、PS fetch 未成立）；**终版 960×540 的 FPS/帧时间、PS fetch 是否成立、各缓存命中收益、像素等价性均 [未验证]**，详见 [05](05-优化记录与实测数据.md)、[07](07-已知限制与未验证项.md)。

## 相关文档

- [01-架构总览](01-架构总览.md)：项目结构、入口/帧循环、线程/坐标/格式。
- [03-构建与工具链](03-构建与工具链.md)：CMake/vendor/工具链与发布。
- [04-资源调度与缓存](04-资源调度与缓存.md)：atlas/GXT/图像 worker/预算压力。
- [05-优化记录与实测数据](05-优化记录与实测数据.md)：20 个开关、历史实测与验证状态。
- [06-性能采集与分析](06-性能采集与分析.md)：日志字段、采集框架、A/B 复现。
- [07-已知限制与未验证项](07-已知限制与未验证项.md)：终版未上机项与平台/许可限制。
- [08-素材与安装](08-素材与安装.md)：素材清单、安装与更新。
- [文档总览](README.md)。
