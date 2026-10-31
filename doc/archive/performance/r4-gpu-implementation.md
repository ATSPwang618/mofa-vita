# r4 GPU 依赖图、小更新 rename 与 shader 接纳

本轮对应 doc/14 的 P2、P6，并完成 P0/P1 所需 native 接口。实现已经进入实际 Vita 路径；没有运行 Vita3K 或部署真机。新的实际帧率、EndScene 降幅及每项独立收益仍须用户的真机日志，不能把 host 边界测试当作这些数值。

## P2：只对未覆盖的真实资源边加入 GPU 等待

新增 `dependency-graph`（bit12），依赖 `batch-arena` 和 `async-submit`。关闭后仍是 r3 的每 scene SET/WAIT，可单独 A/B。

Rust 在每次 Begin 前收集该 scene 内 Draw 的实际纹理读集，调用 `Gxm::plan_scene(target, reads, kind)`。native 同步检查 owner 并消费读集，不保留该指针数组；平台层复用自己的指针 Vec 容量。全局 white 不需要特别加入：它从未被 GPU 写入，`last_write=0`，没有 RAW hazard。

每个纹理分别维护 `last_read`、`last_write`，同时保留 `last_use` 供实际完成退休：

- 对目标的写入遇到未完成旧 reader，是 WAR；遇到旧 writer，是 WAW。
- 对采样输入的读取遇到未完成 writer，是 RAW。
- 已被实际 fragment completion 或先前 GPU barrier 覆盖的序号，不再次加入等待。
- 缺少 plan 时保守保持完整此前依赖。实际 draw 如发现调用者漏了一个未覆盖 producer，在任何本次读提交前结束当前 scene、等实际完成、重开同一目标，再继续；不会在已 Begin 的 scene 上假装补入一个不存在的 barrier。

producer 仍使用 `SCE_GXM_SCENE_FRAGMENT_SET_DEPENDENCY`，有必要的 consumer 才加 `SCE_GXM_SCENE_VERTEX_WAIT_FOR_DEPENDENCY`。等待最新 SET 保守覆盖该 context 更早的 fragment job，不用 CPU notification 冒充 GPU cache barrier。当前 API 没有为每条纹理边创建新的 OS sync object，因而不会重引入大量 offscreen sync owner。

内部渲染来源 Copy 会按实际 source/destination 重算边，保留 Rust 指定的 PS/group consumer 分类；Present 自动提供 canvas 读集。CPU transfer copy 仍在跨引擎边界等待 producer 和 transfer 完成。display buffer 的独立 sync object／有界队列协议继续负责 scanout 复用。uniform、索引和双槽顶点 arena 的实际完成协议保留。

`scene_gpu_dependency` 仍统计 producer SET；因此它不应被要求降到零。`scene_independent` 才是实际省略 VERTEX_WAIT 的 scene 数。RAW/WAR/WAW 计数是可能重叠的边类型，不可相加充当唯一等待 scene 数。未提供 plan 和漏读恢复进入 `scene_plan_fallback`，漏读恢复不计为 `batch_capacity_drain`。

## EndScene 的 target／consumer 拆分

scene kind ABI：0 other、1 stage、2 group_pixel、3 group_mask、4 scratch、5 canvas/scene、6 copy_ps、7 copy_group、8 present、9 bake。

保留 `end_scene` 总计，同时输出 `end_stage`、`end_group_pixel`、`end_group_mask`、`end_scratch`、`end_canvas`、`end_copy_ps`、`end_copy_group`、`end_present`、`end_bake`、`end_other`。每次 API 调用只进入一个分类；分类和总计共用同一 started/finished 时间，检查保证所有分类的调用数与微秒数之和等于总计。

这些分类是 `end_scene` 的子集，不能再与总计相加，也不是 GPU timestamp。`scene_plan_fallback`、`batch_capacity_drain` 包含其内部 completion 等待；只将互斥的 API 边界用于总量解释。这样下一份日志能区分 group pixel 和 copy producer 的真实 driver 停留位置。

## P6：有界 CPU source 双版本 rename

新增 `source-rename`（bit13），同样依赖 batch/async。只处理 CPU-origin 的线性 RGBA8/R8 source；有 target、曾被 GPU 渲染或 movie/compressed 的资源继续保守同步，不从可能陈旧的 CPU backing 推断 GPU 成果。

当前 backing 仍有 GPU reader 时，创建替代 backing，复制完整 pitched 逻辑像素和 padding，再保留 descriptor 的尺寸、过滤、格式、gamma，仅通过 `sceGxmTextureSetData` 改数据地址。完成 descriptor validate 后才提交新 owner。调用者的 partial patch 随后只写入新版本；旧版本保持不变，按其原 `last_use` fragment completion 退休。

边界为每个纹理最多一个未完成旧版本（当前＋旧，共两份）、全局最多32个旧 owner／8 MiB、单 backing 不超过2 MiB。当前 Vita glyph atlas 实际 `VITA_PAGE_SIZE=1024`、R8=1 MiB，因此覆盖 r3 的589 B glyph 更新热点。连续 CPU 更新尚未提交给 GPU 的新版本可直接修改，不再重复 rename。

如果两个版本都被 GPU 使用，或分配／descriptor 接纳失败，仍回原来的完成等待再原位修改。所有额外物理页都进入 native CDRAM mapped/available，旧 slice 未完成前不会当 free；cleanup 失败保留确切 owner 并限制为 teardown。source/target owner 和退休节点的计费使用稳定 charged 字段，支持部分释放失败后重试。

这是以一份有界的全图 CPU copy 换取小更新的在途等待；不声称“零复制”。`texture_rename_copy` 记录新增字节与时间，`texture_rename` 是包含分配/复制的父scope，`texture_rename_retire` 记录真实完成回收，`texture_rename_fallback.aux`：1不可安全rename／尺寸，2全局预算，3身份耗尽，4同纹理旧版本仍在途，5节点分配，6backing或descriptor接纳失败。应同时比较额外 copy 与 `finish_texture_update`，不能只报告后者归零。

## P6 补齐：逐 draw 恒定的 haze／soft 也使用紧凑流

r3 的 compact 只接受20个额外 float 全部 bit-zero，日志约8% draw符合。当前在同一 draw 内逐位比较 haze[2]／soft[3] 的全部80 B；完全相同即可复用原 `vs_main`、原 attribute resource index 和 varying，不改变 shader 算式。五组 float4 通过第二条 stride=0 stream 读取，offset 分别0／16／32／48／64，因此每一组值仍保持独立。

非零常量保存在当前 batch 顶点 arena，与 `32 B × vertex_count` 一次连续预留，额外80 B按相同 fragment completion 生命周期保护。全 bit-zero 情况继续使用原16 B immutable zero块；soft在不同顶点变化、signed-zero bit不同，或常量版 vertex patch被驱动拒绝时回112 B原流。原有finite validator仍先拒绝NaN／Inf；比较不会数值近似或规范化负零。

`vertex_copy.bytes` 与 `compact_vertex_write.bytes` 均记录实际 `32×N+80`（非零常量）或 `32×N`（零常量）；不能再用r3的compact字节乘固定80/32推算省略量。三顶点非零常量draw为176 B，原336 B，结构上少160 B；六顶点为272 B，原672 B，结构上少400 B。这些只是CPU复制字节差值，实际覆盖率和耗时须下一份日志。启动 `compact_stream` 保留原 `enabled`／`patch_status`，增加 `constant_enabled`／`constant_patch_status`，两类patch能力独立回退。

## Shader native 接纳修正

runtime 初始化条件由错误的 bits0/1 修为 group/PS 的 bits3/4。保留 shader15 group 和17通用 fetch，新增 shader18=`PsFetch19`；18只允许 Opaque，且 params[0] 必须19。17/18没有 legacy 无 snapshot 程序替代。

17/18 的 COLOR 输入是经编译器验证的 HALF4；native只为本游戏真实 sRGB RGBA8 target 接纳 HALF4 mode1，不再因为未使用的 UCHAR4 mode0 patch 失败而整体关闭能力。native Draw 同时拒绝 mode0，Rust facade也要求真实 sRGB target。group15保留原双输出接纳；Rust只在已证明的 sRGB pixel/mask/target 链选择融合。

optional program 仍需 compiler metadata、ProgramCheck、Register、所需 fragment patch 全部成功才进入只读 capability。启动一次输出 group15、ps_fetch17、ps_fetch19 和各 patch 状态；新runtime源码、模块导出绑定与缓存身份由 shader 实现文档单独记录。

## 代码依据

- `knife/vita/gpu_lifetime.inc`：真实读写序号、计划消费、边覆盖、漏读恢复、rename owner 和 completion。
- `knife/vita/effects_backend.inc`：Begin/Copy/Present 的实际依赖边，R8/RGBA partial update，新shader接纳和 guard。
- `knife/vita/gxm_backend.c`：互斥 EndScene 分类与相同时间戳、native option、native owner 总释放。
- `knife/knife-platform-vita/src/gxm.rs`：线程绑定的 plan／capability 接口。
- VitaSDK `gxm.h` 的 dependency flags、fragment notification 和 texture data 接口。现有 [vitaGL 9bf3e3f4 gxm.c](https://github.com/Rinnegatamante/vitaGL/blob/9bf3e3f4e55efd1f4a191b179144f7f9a98cd925/source/gxm.c#L574) 在同 context 保持单调 fragment query fence，换 FBO 使用普通 BeginScene，并无逐 scene CPU Finish。本实现使用真实边上的额外 GPU dependency，保留更保守的缺计划回退。
- 同版本 [textures.c `_glTexSubImage2D`](https://github.com/Rinnegatamante/vitaGL/blob/9bf3e3f4e55efd1f4a191b179144f7f9a98cd925/source/textures.c#L986) 也在采样过的纹理更新前分配新位置、复制、退休旧位置、SetData。这里用真实 completion 和有界双版本，替代按估计帧延迟判定可释放。

## 限定验证

仅运行与变更对应的检查，没有扩大成多轮全量测试：

- actual native C 的 ASAN/UBSAN delayed-completion test：132 checks，0失败；涵盖独立/已覆盖/RAW/WAR/WAW边、缺plan、漏读安全重开、R8/RGBA partial、真实1 MiB glyph页589 B patch、双版本压力回退、allocation/descriptor失败及 owner 释放、EndScene分类精确加总。
- production effect input：624 cases／1872 checks通过，含新增18的19-op/Opaque合同；不启动GPU。
- 原flags=0 copy路径：5939 checks通过，保持旧输出模式和回收契约。
- 常量compact实际production helper的2项定向ABI／复制检查，ASAN/UBSAN 63 checks通过：五组非零haze/soft独立offset、32×N+80精确写入及界限、signed-zero逐位保留／不一致回退、varying soft完整112B与NaN原验证拒绝。
- 最终整包 `-O3` 内联发现 optional shader 关闭函数的索引边界需显式声明，已在入口检查 g/effects owner 与 shader < EFFECT_SHADERS；最后变更后 ARM C11 `-O3 -Wall -Wextra -Werror` 编译通过，没有重复全套测试。最终全包链接/打包由主代理统一执行，避免并行重复构建。

六个运行时文件的冻结 SHA 位于 `.work/performance/r4-gpu-source-freeze.json`。本轮没有新的实机日志，因此没有填写推测FPS或把r3的95 ms driver总量承诺为可回收收益。
