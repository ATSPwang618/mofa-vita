# GPU 提交、顶点与 CDRAM 实现记录

本轮完成优化计划第 2、6（native 部分）、8 项的代码；完成依据是明确的生命周期协议、原有路径回退和针对性边界检查。真实画面、帧率及提升毫秒数由用户后续 Vita 日志测量，不把 host 检查当作 GPU 执行或像素等价证据。

## 开关和调用顺序

- `BATCH_ARENA = 1<<2`：整个命令批次共用单调顶点区，双槽复用。
- `ASYNC_SUBMIT = 1<<5`：要求同时开启 `BATCH_ARENA`，GPU scene 依赖及 completion 取代逐目标/渲染来源 copy/呈现的 CPU 全局等待。
- `COMPACT_STREAM = 1<<6`：要求 `BATCH_ARENA`；零 haze/soft 的顶点使用 32 B 流，其余仍 112 B。
- `TEXTURE_POOL = 1<<8`：纹理 64 B 对齐子分配到 CDRAM 物理页；关闭时保留 256 KiB 独立分配。

这些 options 在 `Gxm::new` 之前设置。每次 `execute` 完成 deferred 准备后、首个 Begin/Copy 之前，调用 `Gxm::begin_batch(basic_quads, effect_vertices)`；内部 bake 的独立 execute 也是独立批次。GPU API 继续只在创建线程运行。

平台接口位于 `knife/knife-platform-vita/src/gxm.rs`；native 主体为 `knife/vita/gpu_lifetime.inc`，调用点覆盖 `gxm_backend.c`、`effects_backend.inc`。电影借用帧的 rebind/释放仍执行实际 GPU finish，不把 AvPlayer 外部内存当作本后端可自行退休的普通像素。

## 2：实际完成事件管理生命周期

### 顶点和批次

basic clear/copy/present 与 effect 的数据统一进入当前槽，scene 切换不重置游标。槽容量根据整个批次最坏 112 B/effect vertex 加 basic 128 B/quad 预检，按 64 KiB 增长，单槽上限 4 MiB，共两槽；没有每槽固定分配 16 MiB。原 32760 个 U16 顺序索引覆盖单次 effect draw 上限，索引与 shader/uniform 存储不会跟随 scene 被 CPU 覆写。

合法的大批次允许受控分段：下一个 draw 的数据超出当前槽时，先 EndScene，等待已提交 fragment completion，再复用/扩大槽并重开同一目标，恢复 viewport/depth/cull；重开不再次 clear。分段发生在本次 draw 的 uniform/vertex 写入之前。

槽复用只有在保存的 `last_sequence` 已完成时才直接进行；否则等待最新已提交通知。通知值接近 u32 回绕时先排空、回收，再重置全部序号，不跨代比较。通知失败、EndScene/重新 Begin 失败会保留 cleanup owner，禁止正常重用。

### GPU 顺序和 CPU 可见性

ASYNC 路径每个 scene 使用 `SCE_GXM_SCENE_FRAGMENT_SET_DEPENDENCY`；从第二个 scene 开始同时带 `SCE_GXM_SCENE_VERTEX_WAIT_FOR_DEPENDENCY`。这是 GXM context 内 fragment → 下一 scene vertex 的 GPU 依赖，保守覆盖前面目标读写带来的 RAW/WAR/WAW；没有把 CPU notification 当作纹理 cache barrier。

每个成功 EndScene 用 `sceGxmEndScene(..., NULL, &fragment_notification)` 写单调序号。notification 只回答 CPU 何时可以复用映射顶点和释放资源。每次 draw 的输入及 Begin 的目标都记录 `last_use`；失败的 draw 也保守记录已绑定的输入。

普通纹理 destroy 先撤销逻辑可用性，并保留 native 纹理、target、descriptor、pixel owner，直到 `last_use` 完成。`retiring` 单独标记，不会误回收构造中尚未 ready 的纹理。`reclaim_retired(false)` 只回收已完成项；`true` 留给明确的内存压力，等待真实完成后再回收。

CPU update、读回、seal、电影帧 rebind、不同 transfer 引擎的 copy/update/readback 仍保留必要的真实同步。渲染来源 copy 是同一 context 的 textured-quad scene，可沿 GPU 依赖提交。普通 CPU 来源 transfer 前后仍排空 transfer，并先等 producer，不把两条队列假设成自动有序。

### 呈现

保留三张独立显示缓冲和其 sync object。ASYNC 初始化显示队列最多两项 pending；`DisplayQueueAddEntry` 与显示 sync 提供有界背压，取消每帧无条件 GPU Finish 和 DisplayQueueFinish。CPU arena 槽和显示缓冲是分别管理的 owner。callback 错误字段改为 C11 atomic；退出先完成 GPU、显示队列和 scanout detach，再销毁 owner。DEV 直接 surface crossover 拒绝交换池内 slice，避免破坏物理页所有权。

## 6：相同 VS 的 32 B 顶点流

不生成新的通用顶点 GXP。第二个 patched vertex program 仍使用现有 `vs_main`：position/UV/color 来自 stream 0（stride 32），haze/soft 五组 float4 来自 stream 1（stride 0、不可变零 float4）。零向量放在 basic index block 的 offset 16；实际六个 U16 index 只使用前 12 B，二者不重叠。

仅当每个顶点额外 20 个 float 的位模式全部为零时使用 compact 流。负零、非零 haze/soft 均回完整 112 B 流；现有 finite、packed 参数校验保留。由于 VS 和 varying ABI 相同，fragment patch 不改变。compact patch 被 driver 拒绝时记录一次状态，保留完整流，不能使游戏因优化试验启动失败。

`vertex_copy` 记录实际写入字节，`compact_vertex_write` 是其中 compact 子集，不能重复相加为总复制量。

## 8：物理页与逻辑 slice 分开计费

纹理像素按 64 B 分割物理 CDRAM 页。新页至少 256 KiB，大纹理按完整请求向 256 KiB 圆整。页内 free span 按位置拆分/合并，活动 slice 不搬移；物理映射统计仍只按实际 kernel page 计一次。已完成退休的空页可缓存，合计不超过 2 MiB。

Rust 逻辑 admission 使用 `texture_charge(payload)`：池开时向 64 B 圆整；RGBA/R8 的 payload 含八像素 row pitch，GXT 用完整编码 backing payload。`cdram_available` 取“可新增物理映射余量”与“现有最大连续 free span”的较大值；不把零碎 holes 相加，也不把未完成退休内存当 free。物理页 slack、JPEG/parameter reserve 和退休中 owner 仍进入 native 实际预算。压力场景允许显式 completion reclaim 后重新查询。

上传 source 会覆盖全部逻辑像素，所以只清 row padding 和 slice 尾部。GXT 只清 payload 以外尾部，不再先把整个 256 KiB 圆整块清零再复制。`data=NULL` 的 render target 仍完整清零，保留初始像素契约；不会清到相邻 slice。

## 可复核依据

- 原 eboot `SetTarget` 的 EndScene/Finish 调用地址 `0x81031f18`、`0x81031f20`；显示 EndRender 三缓冲、末尾 Finish，但没有每帧 DisplayQueueFinish。详见 [`doc/09-Artemis渲染与MJA对照分析.md`](../09-Artemis渲染与MJA对照分析.md)。这证明原版也有同步，并未被用来宣称任意删等待一定安全。
- [VitaSDK gxm.h](https://github.com/vitasdk/vita-headers/blob/master/include/psp2/gxm.h)：`SceGxmSceneFlags`、`sceGxmGetNotificationRegion`、`sceGxmNotificationWait`、`sceGxmEndScene`、显示队列 API；texture alignment 16 B、color surface alignment 4 B，因此 slice 的 64 B 对齐满足这两项公开 ABI。
- [vitaGL 9bf3e3f4 source/gxm.c](https://github.com/Rinnegatamante/vitaGL/blob/9bf3e3f4e55efd1f4a191b179144f7f9a98cd925/source/gxm.c#L574)：scene_end 写 fragment notification，display/FBO scene_reset 重新 Begin，普通呈现经 AddEntry 有界提交。此实现使用额外 SET/WAIT 依赖，比该参考更保守。
- [同版本 buffers.c](https://github.com/Rinnegatamante/vitaGL/blob/9bf3e3f4e55efd1f4a191b179144f7f9a98cd925/source/buffers.c#L87)：notification region 初始化、查询完成值与 NotificationWait。
- Hazuki 当前 port `platform/vita/port/render/gpu/vitagl/native_pixels.c:143`：以 fence collector 实际释放事件而非猜测帧延迟退休像素；compact stride-zero attribute 直接参考其使用的 vitaGL `custom_shaders.c`、`ffp.c` 的常量属性路径。

## 可观测项与针对性验证

新增 `batch_slot_wait` 是实际 NotificationWait 时间；`batch_capacity_drain` 包含它，分析时不能重加。`scene_gpu_dependency` 给出真实带依赖的 scene 数，`texture_retire` 给出完成后回收次数/slice 字节，`texture_pool_hit`、`texture_pool_page` 区分 slice 命中和物理页创建。取消的 Finish 不会在旧计数器中虚增调用。

本轮在 Docker 中对实际源码进行了：

- ARM C11 `-Wall -Wextra -Werror` 编译。
- 新 `tools/test-vita-gxm-lifetime.sh`：55 个 delayed-completion / 双槽 / 溢出 / 退休 / pool / 呈现 / 失败保留 owner 检查通过，ASAN/UBSAN 无失败；无 GPU 或像素结论。
- 原 copy 5939、viewport 215、present 418、compressed 1128 个检查通过，保持旧 flags=0 路径；compressed/present 的私有 RED 版本失败是预期断言，不是生产版本失败。
- effect input 621 cases、1863 checks，通过现有完整输入和新增 shader 17 opaque 限制；仅运行 production GREEN，没有扩大本轮 mutation 验证。

原始基线日志的每帧 152.630 ms target/copy Finish 和呈现相关 34.074 ms 是改造动机，不是已经兑现的节省。下一份真机日志应同时比较 Finish、batch_slot_wait、DisplayQueueAddEntry、work/frame wall 和同场景 copy/scene 数，防止把等待换位置误算成提速。
