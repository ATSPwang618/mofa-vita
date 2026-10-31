# r4 资源链实施证据：doc14 P3 / P4

本文件记录已实现的代码路径和验证边界。r2 原始基线与 r3 捕获不改写；r4 尚未有实机捕获，下面的减少次数与字节是专项验证结果，不是 Vita 提升毫秒数。实机收益仍需用户用相同场景、PC、窗口对照。

## P3：atlas 按真实 output need 准入

r3 的 `reclaim(state, CAP)` 使每次 miss 都要求给新条目腾出完整 32 MiB；除外部 pin 外的旧条目会全部清空。因此 32 MiB cache 在实际路径中退化成单条目缓存。DISP、NOISE、BORDER 之间的切换也会引发完整 NX 再解压。r3 两次 DISP_PARTS_EN 的约 775 ms NX 必须按各次相同 key 对照，不能用不同捕获长度的累计时间声称节省。

实现位置：`knife/knife/src/atlas_cache.rs`、`knife/knife-core/src/format/archive.rs`。

- `Archive::read_plan` 只读 NX 的 16 字节头，验证 stored/output 上限与文件 extent，不读完整 body、不分配解压 output。
- 单 worker 将 `plan.decoded_bytes` 作为新 entry 的 need；LRU 只回收到 `cached bytes + need <= 32 MiB`。命中保持 LRU，多条目可共同驻留。
- ByteView 的任意 range 持有同一个完整 owner；whole Vec capacity 计费。外部 pin 的条目不能淘汰，也不会从预算中消失。
- output 预留后才执行 `read_bounded_lengths(index, stored_bytes, decoded_bytes)`，再次检查头与两个独立上限。压缩输入临时 buffer 与驻留 output 分开约束，避免把 stored 大小误充作长期 output cache 的 need。
- 解压完成再核对 output 长度和实际 Vec capacity，容量不符/空间不足则不发布 cache entry。异常、限额拒绝均清掉 active/inflight 并唤醒需求者。
- IO/NX 不在 cache metadata mutex 内执行。队列仍为最多 4，单 worker 仍限制同时解压 1 项。Atlas 超过 32 MiB 或 pins 占满时明确失败，不绕开预算逐子图无界 full decode。

新增证据：`atlas_load` 覆盖计划、准入、body read 与解压；`atlas_wait` 单独记录需求线程等 worker 的时间；`nx_full` 带 `archive:index:entry` 热点标签。保留 card read、archive zero-fill、lock 与 range 指标，不能把不同层的 inclusive 时间直接相加。

开关：`shared-atlas` 开走修复后的共享 owner cache；关继续原有 ByteStage/每 range reload 路径，便于旧路径对照。

## P4：精确 Bake kernel 与完整烘焙结果复用

r3 的 `cpu_grade` 8 次累计约 3.749921 s；最慢 frame 6925 / PC1142 窗口内两次 grade 约 1.505112 s。它不是全日志所有 1 s 卡顿的解释。旧灰度 Bake 每次构造 65,281 个 float3 的 LUT，并在每像素每颜色的最终编码中调用 `powf`。

实现位置：`knife/knife/src/grade_cache.rs`、`graded_cache.rs`、`grade.rs`、`assets.rs`、renderer 的 Image deferred prepare。

### 4 MiB kernel cache

完整 key 包含 Build（Japanese/Overseas/Witch）、grade mode、所选 table 行全部 20 个 f32 bit、raw、source sRGB、bright/contrast bit。按 bit 而非浮点近似或字符串四舍五入区分。灰度 LUT 及 pre 表可复用，非灰度直接复用 256 项通道字节映射。

灰度插值保留旧公式的浮点运算顺序与 alpha，不把结果近似为低精度 RGB LUT。最终 sRGB 编码器在**运行目标**调用原 `srgb_byte` 构建 255 个 f32 精确转移阈值，检查每阈值前后及邻近值；没有带入 host libm 生成的固定表。像素循环按 bit bucket 与阈值比较取原来的 byte，移除每像素 `powf`。目标编码器边界检查失败即退回旧公式，不启用近似结果。

4 MiB 预算计入 Kernel 结构和灰度 Vec capacity，活跃 Arc pin 仍计费；LRU 仅淘汰未 pin 的 kernel。无法准入则走旧循环。cache 构造时的 mutex 不包含 IO/GPU；一次像素应用借出 immutable Arc 后释放 metadata 锁。

日志：`grade_lut_build`（首次编码器构建也计在首次 build）、`grade_lut_hit/miss`、`grade_pixel`、`grade_lut_bytes`。build/pixel 热点标签带 build、grade、raw、sRGB、bright/contrast bit 与 table hash；完整 key 比较仍用整行 bit，hash 仅供日志显示。

### 12 MiB 完整结果 cache

仅缓存实际 CPU Bake 完成的 PNG/JPEG（包括 KIMG 包装）；原 BNTX 路径继续原解码路由，但也可使用精确 kernel。缓存 key 再包含原 Archive owner identity、entry index、image name、strip。持有原 Archive Arc 防止 pointer identity 被回收复用；Build/完整 Bake/sRGB key 同上。

- 原 resolver 的 archive 优先级与 THUMB index 映射先无 IO 定位，再按该**精确 archive/index**查 cache。低优先源的缓存不会跳过首选源。首选读失败的原 fallback 仍交给旧 `image_data_limit`；没有改变读取失败的语义。
- 第一次 decode/grade 保留原 C malloc owner，`freeze(self)` 消耗独占 DecodedImage，只提供 immutable slice。没有为缓存复制整幅 C pixels 到 Rust Vec。
- `FrozenImage` private 内部不暴露 `DecodedImage`、mutable slice 或 FFI mutation；`Sync` 的安全依据是写入权已交还、C codec/driver context 已销毁，唯一剩余所有权只在最后 Arc drop 调用 free。原独占 DecodedImage 仍为 `!Sync`。
- `PixelView` 可取 strip range；计费仍为 strip 前完整 native owner 的 byte length 或 Vec capacity。12 MiB 包含 retained 与正在构建结果的预留，pins 不能被回收。metadata/Arc 小对象不作为 pixel payload budget；它们随有界条目数存在。
- 相同 key 的并发 miss 等待同一项完成，不重复 codec/grade。失败与 unwind 清 active/预留并唤醒等待者。超过 cap 或已有 pins 无法回收时走普通 bounded decode，不扩大可选缓存预算。
- 首次上传直接复用 worker 的已烘焙结果。以后 GPU residency 被驱逐，Image deferred 的 optional cached callback 在读取 encoded source **之前**查精确缓存；命中仅重新上传已烘焙 RGBA，不再 card/NX/codec/CPU grade，也不提交 Alter。
- 未命中则保留已有 source reload、尺寸与色彩空间校验、codec/grade 和上传。缓存不是所有首次独有 Bake 的免计算承诺。

日志：`grade_result_hit/miss/evict`、`grade_result_bytes`。hit/evict 的 aux 是该条目上次实际 decode+grade 的观测 us，只表示重建代价参考，**不能直接累计称为本轮节省 ms**。GPU texture 的 `rebuild_us` 初值来自首次 decode+grade，真实 prepare 成功后更新为本次读取/解码/上传的观测耗时；父实现的准入优先按年龄分层、同层按有上限的重建代价选择 victim。缓存命中导致重建变便宜后，观测值随之降低。

开关：`grade-cache` 关时不用 kernel/result cache，Bake 走旧逐像素公式，renderer 也使用原纯 LRU。`decode-owner`、`gray-r8` 仍分别控制 r3 已有 native owner / R8 路径，未合并成不可单独对照的开关。

## 仍然存在的必要工作与复制

首次 encoded archive body 仍需 bounded card→CPU read；NX 仍从压缩输入输出完整 atlas；PNG/JPEG 首次必须产生 decoded pixels。CPU 烘焙改变像素内容，GPU 纹理首次或重新驻留仍需上传复制，完整结果 cache 没有声称这一复制消失。未用 native owner 的老路仍可能 C→Vec；CPU strip 老路仍可能 copy_within。KIMG/GXT 与共享 atlas 的 r3 优化独立保留。

新增 CPU 长期 payload 上限合计为 4 MiB kernel + 12 MiB baked result；与已有 32 MiB atlas cache 相互独立。普通 decode 的原 bounded 工作 buffer、压缩输入 scratch、GPU CDRAM 等不混称在这 16 MiB 内。缓存首用与 cache miss 仍会有分配和构建成本。

## 针对性验证

| 验证 | 结果 | 证据文件 | 范围 |
|---|---|---|---|
| 真实 AtlasCache / ByteView 源码，mock bounded archive | 4/4 通过 | `.work/performance/r4-atlas-cache-tests.log` | join 只读一次；range 同 owner；26 MiB 条目及 pinned cap；21 MiB DISP +2 MiB NOISE+100 KiB BORDER 循环分别只读一次，26 MiB CG 只按需逐出旧 DISP |
| 真实 grade.rs / grade_cache.rs standalone harness | 2/2 通过 | `.work/performance/r4-grade-tests.log` | 6 个 grade ×raw/sRGB组合×8192 RGBA 像素×重复应用，与旧循环逐 byte 相同；阈值 ±8 ULP 与20万 f32 随机样本与原编码器一致 |
| 真实 graded_cache / PixelView，mock Archive/decoded owner | 1/1 通过 | `.work/performance/r4-result-cache-tests.log` | 同 key 三线程只 decode 一次；source、strip、raw、sRGB、bright bit 分隔；12 MiB pinned cap；失败释放预留后可重试 |
| 实际 C decoder + 实际 Rust owner wrapper | 12/12 通过 | `doc/logs/vita/r4-image-rust-host.log` | PNG/JPEG 像素、截断、RAII、原 owner `!Sync`；新增冻结后四线程共享地址一致且仅最后 Arc drop free |

另已增加 `archive::range_tests::planned_lengths_reserve_real_output_and_independent_input_budget`，覆盖 NX gzip/zlib 头计划、stored/output 独立限额、stored 大于 output 与截断拒绝。已增加 renderer `cached_baked_residency_reload_skips_encoded_loader_and_decoder`，覆盖 GPU 驱逐后的缓存重传无 encoded load/decode/Alter、像素保持与重建成本更新；两项由父任务统一编译/测试并记录结果，避免并发 Cargo 及重复全量验证。

以上 host 验证确认所有权、边界与公式一致性；不将 host 运行时间换算为 Vita FPS。最终目标编译、VPK receipt 由父任务记录，实机 before/after 数值待用户下一份捕获。
