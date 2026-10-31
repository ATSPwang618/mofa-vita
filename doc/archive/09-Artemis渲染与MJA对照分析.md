# mofa vita 渲染与 Artemis MJA 对照分析

后续诊断真机日志已完成归因及优先级重排，见[真机日志耗时归因与优化计划](12-真机日志耗时归因与优化计划.md)。本篇保留最初静态分析的基线与证据口径。

实际 540p 素材到位后的重复解压、CPU/CDRAM 复制和分配粒度量化，见 [实包素材与内存复制分析](10-实包素材与内存复制分析.md)。其中区分了 UI staging 与普通 CG GXT 的不同加载路径。

2026 年 10 月 30 日。分析基线为 `955bb5e3e09ee476abb53c242183febb3c9e3382`。

当前后端有明确的结构优化空间。优先方向是减少整面复制和组表面中转、修正 GPU 数据的复用协议、让 CPU 提交与 GPU 执行重叠。现有证据不能支持“39 个 pass 必然要耗时约 190 ms，必须接受约 5 FPS”的结论，也不能据此保证优化后的帧率。原版 Artemis 自身同样存在同步绘制；值得迁移的是具体的数据布局、混合实现和资源协议。

本次只增加分析与复查工具，没有改变游戏运行路径。仓库内没有对应当前提交的原始实机性能日志或游戏资源；下文将**源码事实、二进制观察、待实机验证的方案**分别说明。README 中的帧率属于原仓库报告的数据。

## 输入与地址

| 输入 | 大小 | SHA256 |
| --- | ---: | --- |
| Hazuki ARMv7 `libartemis.so` | 8,360,900 B | `6089b5e0c4933925d56d6ccb78ab2be37533eeac8d7aa033f4b6d4cc51d70f2d` |
| `KZJJ00001/mai_eboot.bin.elf` | 4,778,472 B | `4c0ce60e2594ecbf99cd0dca208ac2c9f98b5d0aada1b400ac64d2ae82b061b8` |
| `RaspberryVita/vita-base/eboot.bin` | 1,891,447 B | `d85b2b608e856b46843e03531d27ad1e08737b3dc9878cdb946d4ef962db905a` |
| 上一项按明文段标志展开的 ELF | 4,780,744 B | `82dd8e3f2d70a3597c73840607e8d9c2930af90de7bc5a3cba45ca9c1b1a36c2` |

Hazuki 源码基线为 `deaa8402a1e5ee0d40b1f9fe0144c107c3f7b6e3`，so Build ID 为 `f324c92b3ca91e726709bc78d49619a6f3cd7264`。so 地址使用 ELF RVA；原生 eboot 使用 `0x81000000` 起的链接地址，两者不能直接相减匹配。

SELF 的所有段均标为明文，只进行段提取及 zlib 展开。六段关键代码已经与 `mai_eboot.bin.elf` 逐字节核对一致：MJA Read、MJA Init/Display、Renderer Init/Begin/End/SetTarget、Layer DisplayInner、Surface Init、CJpg 内存解码。不能因此宣称两个 ELF 整体一致，也不能把仓库旧文档中的 PCSG01127 地址用于这份二进制。

Ghidra 11.3.1 使用 `ARM:LE:32:v7` 分析，so 在分析前归零映像基址；另用 ELF 导出符号、实际 ARM 指令、RTTI/vtable 和 VitaSDK NID 表交叉核对。全程序自动分析触及 120 秒限制，所选入口单独完成反编译；未应用完整运行时重定位，间接调用图不完整。下表是已核对的关键入口，不代表整个引擎已经完整恢复。

| 节点 | 当前 so RVA | 原生 eboot 地址 |
| --- | --- | --- |
| `CGpuRenderer::BeginRender` | `0x005d63ec` | `0x81031cb8` |
| `EndRender` | `0x005d64fc` | `0x81031d64` |
| `Clear` | `0x005d6534` | `0x81031e9c` |
| `SetTarget` | `0x005d655c` | `0x81031ef4` |
| `CGpuLayer::DisplayInner` | `0x005d4f2c` | `0x8102f718` |
| `CGpuSurface::Init` | `0x005d95dc` | `0x810328d8` |
| `CGpuSurface::EnableCopy` | `0x005d9804` | 本次未把原生接口强行对应到相同方法 |
| `MOGLTexture::UpdateTexture` | `0x0074d354` | 不适用，原生路径直接使用 GXM |
| `CMja::Init` | `0x0056eaf0` | `0x8101621c` |
| `CMja::Read` | `0x0056f140` | `0x81015f60` |
| `CMja::Display` | `0x0056f3d8` | `0x81016ab4` |
| `CMja::IsPlaying` | `0x0056f894` | `0x8122c418` |
| `CJpg::Load` 内存重载 | `0x0055f080` | `0x8100eb74` |

## 原版也同步 但同步边界不同

原生 `BeginRender` 在 `0x81031d18` 或 `0x81031d54` 调用 `sceGxmBeginScene`，本函数内没有 `Finish`。`SetTarget` 在场景开启时执行 EndScene → Finish → 经虚表重新 Begin；关键调用点为 `0x81031f18`、`0x81031f20`。它并非无条件的异步多目标渲染器。

原生 `EndRender` 执行 EndScene → 可选系统对话框更新 → PadHeartbeat → DisplayQueueAddEntry → 更新模 3 的显示索引 → Finish。本次所选显示函数内没有 `DisplayQueueFinish`。对应 NID：BeginScene=`8734FF4E`，EndScene=`FE300E2F`，Finish=`0733D8AE`，DisplayQueueAddEntry=`EC5C26B5`，核对来源为 [VitaSDK SceGxm 数据库](https://github.com/vitasdk/vita-headers/blob/master/db/360/SceGxm.yml)。

so 的 `BeginRender` 主要查询 EGL surface 尺寸；`SetTarget` 调用 `glBindFramebuffer` 和 `glViewport`；`EndRender` 调用 `eglSwapBuffers`。这些入口本身未显示逐目标的显式 `glFinish`。在 Vita 上最终是否等待，必须连同 vitaGL 后端一起看，不能把 GLES 调用返回理解为 GPU 已完成。

knife 当前呈现路径是：

```text
全部离屏命令
  → kv_gxm_begin 中 Finish
  → 显示清屏及画布放大
  → EndScene / PadHeartbeat / DisplayQueueAddEntry
  → Finish
  → DisplayQueueFinish
  → 回调等待 VBlank
  → CPU 返回下一帧
```

见 [gxm_backend.c](../../knife/vita/gxm_backend.c) 的 `kv_gxm_begin` 与 `kv_gxm_present`，约 405、454 行。三张显示缓冲仍有其生命周期价值，但每帧排空显示队列使主线程无法充分利用队列跨帧推进。仅增加显示缓冲不会自动获得 CPU/GPU 重叠。

GXM 场景切换和 CPU 阻塞不是同一个概念。[vitaGL 的场景切换实现](https://github.com/Rinnegatamante/vitaGL/blob/master/source/gxm.c) 提供了多个 scene 连续提交的参考；它不要求把所有离屏目标塞进一个 scene。多目标合成仍有 tile store/load、目标调度和带宽成本，不能把这些成本当成零。

## 优先修正性能测量

### 绘制的 0 点 2 ms 没有覆盖整个提交

[effects_backend.inc](../../knife/vita/effects_backend.inc) 的 `kv_gxm_draw_effect` 约 411–460 行，执行顺序为：

1. 校验全部顶点，每顶点 28 个 float，另检查 haze 与 soft 参数。
2. 检查四个纹理的所有权和反馈别名。
3. 按需 patch 程序，设置顶点/片元程序，绑定四个纹理。
4. reserve uniform buffer，写至多七组 uniform。
5. 把顶点复制到 GPU 可见内存。
6. **此处才开始 `draw_cpu_us` 计时**，实际只包住 `sceGxmSetVertexStream`。
7. `draw_gxm_us` 只包住 `sceGxmDraw`。

因此两项相加约 0.2 ms，只能说明这两个 API 片段耗时较小，不能排除命令构造、校验、程序/纹理绑定、uniform、顶点拷贝是瓶颈。首用 shader patch 与稳态也必须分开计量。

### Finish 时间不是独立 GPU 工作时间

当前 `finish_us` 主要统计 begin_target 与 present 的部分 Finish。显示 Begin、copy、纹理更新/销毁、seal、readback 等路径还存在未进入相同计数的等待，`TransferFinish` 也未完整分类。`Copy` 会内部提交一个 scene，却不通过 Rust 的 `Command::Begin`。

`begin_target_us` 包含其 Finish、状态和 clear；`present_submit_us` 也包含内部同步。它们与 Finish 等字段存在包含关系，不能直接相加。Finish 的 CPU 等待时长包含剩余 GPU 执行和调度，不能把它等同于 GPU busy 时间，更不能据此计算 GPU 利用率。

原文档给出的 78 + 0.2 + 0.1 + 12 + 25 约为 115.3 ms，本身不能解释 190 ms，且这些分类是否互斥没有被证明。需要补齐口径后再做归因。

建议先增加互斥的 CPU 阶段：逻辑更新、绘制列表构造、deferred 准备、命令提交、呈现与主动限帧。提交内部细分校验、shader patch、状态/纹理、uniform、顶点拷贝、Begin/End、不同原因的 Finish。另用 GPU completion 记录完成时刻；若使用 busy 指标，需要真实 GPU 性能计数器，不能用两次 CPU 时钟相减代替。

## 删除 Finish 之前必须修正顶点复用

[effects_backend.inc](../../knife/vita/effects_backend.inc) 的 `effect_scene` 在每次 BeginScene 后执行 `g->effects->used = 0`，约 320 行。随后每个 pass 从同一个 `e->vertices.base` 开头写数据。clear 和纹理 copy 还会把 basic quad 的 `g->quads` 归零，复用另一块 `g->vertices`。

这会产生一条完全不属于“纹理 RAW/WAR”的依赖：

```text
CPU 写 pass A 顶点到地址 V → 提交 A
CPU 开始 pass B → 游标归零 → 用 B 顶点覆盖 V
GPU 尚未读取 A 的 V → A 实际读到 B 的数据
```

所以“按纹理依赖删 Finish 后黑屏”有一个已经存在于源码的竞争条件。它不能证明该竞争是此前所有黑屏的唯一原因，但足以否定“实验已经排除了实现缺陷，证明驱动要求逐 pass 等待”的推断。

此外，`kv_gxm_draw_effect` 前半段先检查 `count > EFFECT_VERTICES - e->used` 并报错；后面的 `if (e->used + count > EFFECT_VERTICES) { Finish; used=0; }` 在正常不变量下不可达。注释中的“溢出自动 drain”并没有提供实际保护。

可先采用保守改造：每帧 arena 单调增长，最后提交完成后才整体复用，**先保留现有 Finish 验证像素完全一致**。之后才逐类替换同步。再进一步才是两槽或三槽 frame arena，每槽具有完成通知、纹理引用和延迟回收列表。

不能只扩大顶点数组：当前 CPU 命令允许 16 MiB 顶点，native effect arena 是 `32760 × 112 = 3,669,120 B`。需要完整帧预检或受控分段，并同时覆盖 clear/copy/basic 与 effect 两种顶点流。uniform、索引、纹理和渲染目标也要有独立的存活约束。

`pass_finish_flags` 目前还存在两个分析边界：它在 present 统计和实际 execute 中各计算一次，且正式执行仍强制 `finish_before=true`；更重要的是，合法 `Command::Copy` 位于 pass 之外，当前按 Begin/End 扫描的模型没有完整记录这些 copy 的读写。`pass_need` 不是可以直接启用的完整调度器。

## PS 混合的整面复制值得优先消除

[vita.rs](../../knife/knife-render/src/vita.rs) 的 `try_draw` 约 1606–1640 行，对每个 `Blend::Ps` 执行：结束当前 pass → 复制整个目标到 backdrop → 重新开始目标 → 采样 backdrop 计算混合。小精灵也会触发整面复制。原生 C 的 `kv_gxm_copy_texture` 对渲染来源使用一次实际 draw pass，前面还有 Finish 和 TransferFinish。

这不只是“一个额外 memcpy”：每次都增加 GPU 场景、目标 store/load、全屏采样、CPU 同步和缓存压力。透明绘制必须维持顺序，但这并不要求每层都复制整张背景。

Hazuki 的可直接借鉴证据如下：

- `native_blend_try` 在 so `DisplayInner` 的 `0x005d57b8` 处，在原路径进入 EnableCopy 前选择 native GXP。
- `native_blend.c` 使用 16 B 的 position/UV 顶点；无需额外 destination 纹理。
- 当前 `programs.bin` 内 1 个 VS 和 192 个 FS，**193/193 都能在本次原始 SELF 展开的 ELF 中找到完整相同字节**。
- 192 个 FS 的 flags 为 `0x001828c1`，包含 native-color 与 frag-color 使用位。另有五份按完整 GXP SHA 匹配的现有 USSE 反汇编，能观察到 `VPCKF32U8 i2.xyz o0.xyz [scale]` 读取目标颜色；编码字节也存在于对应 GXP 中。这些是静态证据，不是新的实机测试。

flags 解释交叉参照 [Vita3K GXM 类型定义](https://github.com/Vita3K/Vita3K/blob/master/vita3k/gxm/include/gxm/types.h) 与 [SceGxm 实现](https://github.com/Vita3K/Vita3K/blob/master/vita3k/modules/SceGxm/SceGxm.cpp)。两者说明格式和实现，不替代真机的颜色/执行顺序验收。

对 knife，优先研究可编程混合或 framebuffer fetch，按魔夜的 24 个 `PsOp` 逐项实现与验证。Artemis 的 192 个程序由目标类型、混合、遮罩、反相、灰度、着色等组合产生，**不是魔夜的 24 个 PS 操作的一一替代品**。本次没有将这些 GXP 复制进 mofa 仓库。

短期仍用 snapshot 的操作，可研究缩小复制矩形和同帧只读版本复用；必须保留 PS 坐标、采样边界和绘制次序，不得把有先后依赖的透明层一起排序。

## 游戏当前没有启用组融合

[game.rs](../../knife/knife/src/game.rs) 约 6184 行调用的是 `try_group_composite_via_scratch`，并非 `try_group_composite`。实验融合函数在 [vita.rs](../../knife/knife-render/src/vita.rs) 约 1740 行，注释明确写着未通过精确像素对比、未接入游戏。

实际路径为：

```text
组 pixel 面绘制
组 mask 面绘制
pixel → scene scratch 的一次复制绘制
scratch 上画 MaskAlpha
scratch → stage 的最终合成
```

一次普通组在两张输入面各一次 pass 的前提下，可形成五个 scene；PS 操作或重新 clear 还会增加工作。`scene_rt` 只是复用了 scratch 的存储，并未消除复制和遮罩两次绘制。文档所述“pixel/mask/composite 三个 pass，fs_group_composite 在工作”的描述与当前调用点不一致。

全组融合在结构上可把这五步缩到 pixel、mask、最终 composite 三步。这个计数是控制流推导，**不是已经测得的提速倍数**。必须先解释现有 old/fused 像素差异，再启用候选路径。

融合不能只保留理想实数公式。旧链还经过 RGBA8 alpha 量化、RGB 的 sRGB 写回与再采样、HALF4 输出精度、线性过滤和实际像素中心。现有 `group_alpha` 模拟 `round(pixel.a × mask.a × 255)/255` 是必要因素之一，不足以证明全部 RGB/alpha 路径等价。尤其要核对额外 RGB round-trip 是否带来差异。

建议沿用已有 old/fused fixture，分别核对组 opacity 为 0、0.37、1，各种普通/PS blend，透明和彩色边缘，全部 256×256 输入 alpha 组合，规则遮罩、软裁剪及目标 gamma。屏幕 RGB 对比还需要配套将 alpha 显示成灰度的观察路径；mock 里的“只有一个 Draw”不证明像素正确。

逐层乘遮罩仍不能普遍替代整组遮罩。例如两层 alpha 都是 0.5、遮罩也是 0.5：整组结果 alpha 为 0.375，逐层遮罩再 over 为 0.4375。这个差异与 GPU 快慢无关，应保留既有失败经验。

还有一个独立的正确性边界：`game.rs` 约 6152 行把“mask 为零且 blend 不是 Opaque”的组全部跳过。这个判断过宽。当前 `fs_ps` 的操作 16、17、18、21、22 并不以纹理 alpha 为唯一门控；例如 `PsOp::NoAlpha` 在 alpha 为零时仍输出源 RGB。`Blend::MaskAlpha` 本身还会把目的 alpha 乘零，也不是恒等操作。应按精确 blend 语义建立可跳过的白名单。源码可构造这个反例，但本次没有证明实际剧情进入了该条件。

## 舞台缓存缺少决定画面的字段

[game.rs](../../knife/knife/src/game.rs) 的 `layer_hash` 约 5984–6038 行，仅覆盖纹理 Arc 地址、color 的 R/A、position，以及 Composite 的 alpha。Pixel、Mask、Stage 还走同一哈希分支。

没有完整覆盖的内容包括 UV、G/B、blend、shade、clip、rule 的纹理及 progress、haze、soft clip、masked 标志、Pixel/Mask 的 clear、Draw 类型、Composite blend，以及决定舞台清屏底色的 `backdrop_drawn`。若纹理原地更新，单凭 Arc 地址也无法识别新像素。

例如仅改变 haze 振幅或 UV，或只改变绿色/蓝色，当前指纹可以保持完全相同，而实际画面已经变化。`stage_dirty`、ghost、transition、noise 的四个附加条件不能替代这些数据。这里是缓存有效性条件不完整的源码问题；具体剧情中哪些帧已经触发错误，仍需输入轨迹与画面对比。

应该为 stage target 保存完整渲染签名或明确的内容版本，纳入输入的像素 generation、清屏与所有渲染状态，在加载、重建目标、语言/资源变化时失效。为避免每帧遍历所有大结构，可维护层/组的 mutation version，但版本更新必须覆盖真正的渲染属性。

这一缺陷也影响已有性能结论：

- `stage_repeats` 在 `previous == hash` 时就递增，早于实际 reusable 判定。它不是实际跳过重画的次数。
- `layer_changes` 复用同一个缺字段的哈希，无法严格支持“75% 帧完全不变”和“分段缓存收益一定低于 10%”。
- 纹理大小分桶使用 `q.tex.width/height`，而 `GpuTexture::with_logical_size` 能把它们设为逻辑尺寸；实际 GPU 尺寸在 `physical_size()`。所以“94% large”不能直接证明 94% 纹理的物理面积都大于 512²，也不能据此证明图集化最多只影响 5% 的绘制。

这些修正并不等于图集化或分段缓存必然高收益。它们意味着当前统计不足以给出这样的收益上界。应分别统计物理纹理字节、状态切换、连续可合批区间和实际缓存命中。

## ByteStage 并非固定后台线程

[assets.rs](../../knife/knife/src/assets.rs) 约 174–220 行的 `ByteStage::request` 对每个新 key 调用 `std::thread::Builder::spawn`，随后丢弃 JoinHandle。代码没有固定 worker 数、待处理请求数、在途字节上限或阶段取消 generation。

24 MiB `CAP` 在读取完成、结果加入 `entries` 之后才淘汰缓存。因此它限制的是已完成缓存，不限制并发读取时的 Vec、线程栈或解压临时空间。原有有界图片 worker 的约束不会自动覆盖这一条新路径。

另有两个可避免的重复：

1. `ui_textures` 已通过 `allui.read(i)` 持有整份 KATL，循环中又为每张子图发起 range read。可以直接把已经在手的子图纳入同一有界 staging 所有权，或在单个 I/O 任务中按需拆分。
2. deferred loader 调用 `stage.take(key)`，未完成就立即走同步 `archive.read_uncompressed_range`。它没有接管或合并已在途的同一请求，可能与后台做重复工作；后台稍后产生的结果还会占缓存。

[archive.rs](../../knife/knife-core/src/format/archive.rs) 约 319–353 行区分两种情况：外层未压缩时可直接 seek 子范围；外层是 NX gzip/zlib 时，为校验声明输出与压缩输入，需要读取并流式处理完整条目。后一种情况下，为同一 atlas 的多张图分别请求，会重复处理外层条目。正式资产是否命中该路径，要检查实际 HED/MRG/NX，不能把它视为所有资源的固定开销。

更适合 Vita 的实现是固定 I/O worker、有界 request/result 队列和字节预算，把每个 key 的 queued/reading/ready/claimed 统一起来。前台需求提高优先级或接管在途结果；跳转/语言变化使旧 generation 失效；相邻范围可合并。GXM 操作仍由拥有 context 的线程执行。多个读线程争用存储和分配器并不保证吞吐更高。

## 顶点 状态与目标格式的细节

| 部分 | 对照发现 | 合理改造边界 |
| --- | --- | --- |
| 普通四边形 | 原生 DisplayInner 使用 4 顶点、16 B stride、triangle strip；knife 通用路径通常是 6 顶点、112 B stride | 未裁剪 quad 的顶点 payload 为 64 B 对 672 B，后者约 10.5 倍；这是数据量对比，不是帧率倍数。为无 haze/soft 的普通 sprite 增加紧凑路径，复杂路径保留原 ABI |
| 顶点常量 | haze 与 soft 常量复制到每个顶点，经多个 varying 传到片元 | 紧凑 VS/FS 组合可减少顶点带宽和插值压力；RGB24 rim、规则位域不能盲目改成低精度颜色或插值 |
| draw 状态 | uniform 的“位置”已经缓存，但程序、四个纹理和 uniform 仍基本逐 draw 重放 | 分别缓存实际绑定状态和值，按 shader 真正使用的 sampler 绑定；在 scene、copy、present、native bypass 后明确失效 |
| shader 变体 | 原生为常见组合选专门的 GXP；knife 的 PS 入口包含 24 种操作分支 | 高频 PsOp 的编译变体值得测量，不能只看源码 if 数判断最终 USSE 成本；要记首用 patch 与稳态指令/寄存器压力 |
| mask 面 | knife group mask 是完整 sRGB RGBA8 target | Hazuki 已有 A8 mask 适配经验，但这里须先证明 mask RGB 从未参与后续运算，并重建 alpha blend/channel swizzle；当前 R8 创建接口禁止 render target，不能只改一个 format 参数 |
| 对齐成本 | CDRAM 按 256 KiB 分配粒度计费 | 480×270 RGBA8 约占 512 KiB 对齐块，A8 约占 256 KiB；原始像素访问可少至四分之一，实际分配在此尺寸只减半 |
| 颜色转换 | knife 按 Switch 路径执行 Alter、sRGB target、HALF4 输出、present 转换 | HALF4 输出寄存器不等于纹理存储变成 RGBA16。应保持四字节 RGBA8 存储与 gamma 契约，不能用直接搬原生 shader 的方式假设魔夜色彩一致 |
| CPU 构造 | quad 顶点与多边形使用 Vec，绘制/统计还构造多个 Vec/HashSet | 无裁剪 quad 可先使用栈上固定顶点，再把整帧数据写到复用 arena；减少分配必须由完整 CPU 计时验证收益，不以 Rust/C++ 语言差异作归因 |

`quad_vertices`、`effect_inputs` 与命令录制都进行了输入检查。可以把已经验证且不可变的数据的检查前移，保留信任边界；不能为省时间直接取消损坏资产、跨 renderer 纹理、反馈采样和错误生命周期检查。

原生 Surface Init 在宽、高、格式、flags 一致时直接返回，变化时才重建；CJpg 也会复用同一目标。这支持按规格复用资源，但原生自己的顶点/表面复用依赖其完成点。迁移协议时要连同完成点一起比较。

纹理更新也有额外拷贝差异。当 CJpg 的输出对象是原生 `CGpuSurface` 时，ScanLine 指向该 surface 的已映射存储，解码可以直接写入最终纹理像素；so 则还可能走 CPU 像素与 `glTexImage2D` 上传。Hazuki 的 shared-pixel 适配已经针对这种重复存储做过处理。knife 的 GXT miss 路径仍可能经过 staging 的 `Arc → Vec`，再分配/清零 CDRAM 并复制压缩 payload。后者仍然可能比运行时 JPEG 解码便宜；应先测 miss 与驻留命中。若为动画设计共享槽，必须在 GPU 释放该槽后才写入下一帧，不能省拷贝同时引入覆盖竞争。

## MJA 的实际实现及其可借鉴部分

### 索引与缓冲

两份二进制的 `CMja::Init` 都检查前三字节 `MJA`，第四字节保存为 flags；读取 u32 帧率并计算 `1000.0 / fps`。随后扫描文件，构建每项 12 B 的记录，保存颜色 JPEG 的文件偏移和长度，以及可选 alpha JPEG 的长度。扫描时 seek 跳过 payload，记录最大颜色/alpha 编码长度，并为后续读取复用缓冲。

当 flags 的 bit 0 置位时，每帧颜色 JPEG 后还有长度字段和独立 alpha JPEG。`Read(index)` 按索引 seek，读颜色数据；需要 alpha 时再跳过其长度字段读取 alpha 数据，然后交给 CJpg 的内存重载解码。索引表属于压缩数据的定位信息，**没有证据表明这里预解码并缓存了整段全部帧**。

`CJpg::Load` 的这条路径可观察到 JPEG header/start/scanlines/finish 处理；有 alpha JPEG 时先得到灰度，再把 alpha 写进输出行。输出 surface 的 Init、Lock/ScanLine 和完成接口被复用。so 的辅助灰度缓冲在容量不足时才扩容；原生对应实现也保留缓冲容量。该路径与某些电影播放器使用硬件视频解码不是一回事，本次没有发现这里调用 SceJpeg 硬解队列的证据。

### 时间推进与重复读取

so 的 `CMja::Display` 读取 `+0x40` 开始时间和 `+0x44` 帧周期；原生对应字段为 `+0x44` 和 `+0x48`。它们按经过时间直接求当前帧号，而不是补解码所有中间帧。到末帧边界时，非循环路径清除播放状态并调用完成 listener；循环路径重设时钟并在本次选择末帧。这些细节不能随意替换成普通取模，否则末帧和完成回调时序会变化。

所选 Display/Read 链中没有 worker、解码任务队列或 GPU 完成通知的异步流水线。Read 入口也没有以“上次帧号等于本次帧号”提前返回；因此在外部更频繁调用 Display 时存在重复读/解码相同帧号的可能。不能把“原版 MJA”直接描述成已经完善的异步动画播放器。

`SetDelayMargin` 在这两个版本里都是直接 `bx lr` 返回：so 为 `0x0056f8ac`，原生为 `0x8122c438`。不能从方法名推导出存在延迟容忍、预读深度或丢帧调度功能。

### 对魔夜的帮助

| 可借鉴机制 | 对当前工程的意义 | 不能替代的工作 |
| --- | --- | --- |
| 预建帧偏移索引 | 已知连续动画可预建 locator，减少逐帧路径/元数据查找 | HED/MRG 本来已有索引，不应为了容器名称再造一套重复系统 |
| 按时间直接定位 | 避免落后时读取、解码已经错过的独立图片 | `Stage::advance` 里的随机震动、回调和脚本等待有副作用，不能全部套用跳帧 |
| 复用编码与输出容量 | 为连续 variant 准备有限的 current/next 槽，减少分配和上传抖动 | GPU 仍在采样的槽必须等 completion；不能覆盖当前纹理来换取少分配 |
| 重复帧号快速命中 | 当前时间仍对应同一帧时可跳过读取/解码/上传 | 需要同时比较资源、grade、语言及像素版本，不能只比较整数帧号 |
| 顺序预取与独立 alpha | 数据转换工具可对已知序列优化物理顺序；颜色与 mask 可分别选择 GPU 格式 | JPEG 是有损且仍需解码，当前 DXT/GXT 已避免运行时图像解码，不能认定改成 MJA 更快或画质相同 |

魔夜当前是图层 variant、补间与组特效组合的重实现，不是直接播放 Artemis MJA。MJA 思路主要帮助 I/O、时间选择与缓冲复用，**不会自动消除组遮罩的多个 pass 或 PS backdrop copy**。只有已经证明无交互、无中途脚本状态变化的局部固定演出，才适合另评估预合成动画；不能据此把所有动态场景烘焙成影片。

## 推荐的实现顺序与验收

| 阶段 | 具体工作 | 可观察的成功条件 |
| --- | --- | --- |
| P0 测量与缓存正确性 | 完整提交计时；按原因统计全部 Finish/TransferFinish；区分实际 stage reuse 与同哈希；完善缓存签名 | 时间分类能解释帧耗时，UV/颜色/haze/遮罩/清屏变化不会错误复用 |
| P0 arena 协议 | effect 与 basic 顶点按帧单调分配，预检容量，完成后复用；先保持当前同步 | 多 pass 使用互不覆盖的地址，压力及错误路径像素一致 |
| P1 减少组中转 | 对现有 old/fused 差异逐像素定位；普通组先接入被证明等价的融合 | 对应组从五个 scene 降到三个，所有 RGB/alpha 对照通过 |
| P1 PS 原生混合 | 对高频 PsOp 实现经过验证的目标颜色读取，逐项替代 backdrop snapshot | PS quad 数不变而整面 copy/目标切换显著减少，24 操作边界一致 |
| P1 帧内 GPU 依赖 | 对独立 pass 连续提交；为 RAW/WAR/WAW、copy/transfer、uniform/vertex/texture 建立依赖 | 正确性保持，CPU Finish 次数减少；不是仅把统计里的布尔开关打开 |
| P1 呈现与跨帧流水 | 有界 frame slots、completion、纹理 pin/退休、显示队列背压，退出时 drain | 跨帧 CPU/GPU 有重叠，队列和内存有上限；主线程不再每帧无条件 DisplayQueueFinish |
| P1 资源读取 | 把 ByteStage 纳入固定 worker 和统一在途状态，使用已读 atlas 字节，取消旧 generation | 同 key 一次读取、并发/内存有上限，动画切换 p95/p99 停顿降低 |
| P2 数据与状态 | 紧凑普通顶点、合适的 mask 格式、shader specialization、状态缓存、复用 CPU arena | 总提交时间与 GPU 工作量下降，画质与异常处理保持 |

每次只改变一类结构并保留对照开关。固定设备、频率、分辨率、存档和输入轨迹，分别覆盖静态对白、单层 variant、40 层变化、遮罩/PS 密集演出、菜单模糊、转场、保存/加载和快进。每帧记录实际物理尺寸、scene/copy/PS 数、顶点字节、等待原因、I/O 与上传、帧时分位数；平均 FPS 不能替代最慢帧与连续卡顿。

GPU frame slot 退休必须覆盖纹理/目标、basic/effect 顶点、临时索引与命令引用；错误返回、shader patch 失败、容量耗尽、切换菜单、退出与恢复也要保留所有权。渲染 alpha 次序继续串行表达，允许的是 CPU 构造、资源准备和 GPU 执行在合法边界重叠。

## 复查材料

输入采集工具位于 [tools/render-audit](../../tools/render-audit/README.md)，使用 Python 标准库读取 ELF/明文 SELF、导出符号、RTTI/vtable、源码哈希，并核对 Hazuki GXP 是否存在于原生输入中。Ghidra 导出脚本按输入 SHA256 验证地址清单。

本轮摘要身份与核对结果见 [结构化证据](09-render-audit-evidence.json)。

本地 `.work/render-audit/` 保留以下内容；原始二进制和反编译结果没有加入 Git：

- `reference-inputs.json`：so、原始 SELF、展开后的 ELF、源码与 GXP 身份。
- `inputs.json`：用于 Ghidra 分析的 mai ELF 身份和关键导出。
- `native-equivalence.json`：两份 native 输入的六段代码一致性比较。
- `native-gxp-comparison.json`：193 个 GXP 匹配、五个目标颜色读取指令的编码核对。
- `so-verified/`、`native-verified/`：所选函数的反编译、调用和指令；对应日志保留超时及诊断信息。

验证范围为静态代码、格式和身份对照。没有运行实体 Vita，没有把旧实测数字绑定到本次修改，也没有将已有 mock 通过视为帧 arena、GPU 混合或异步提交已经通过真机验证。
