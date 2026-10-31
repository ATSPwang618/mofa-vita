# 资源读取、解码与上传实现记录

本文件对应 [优化计划](../12-真机日志耗时归因与优化计划.md) 第 4、5、9 项。固定硬件基线为 [2026-10-30-r2](../../performance/baselines/2026-10-30-r2)，实现状态记在 [implementation-status.json](../../performance/implementation-status.json)。代码与针对性边界验证已完成；新版本的实机耗时、FPS 和每项提升数值等待用户回传控制台日志，未以主机测试代替真机结果。

## 开关与路径

开关来自 [optimizations.rs](../../../knife/knife-perf/src/optimizations.rs)，与性能日志开关独立。关闭单项只切换该项的实现；混合开关运行不等于重新使用原始二进制。

| 开关 | 关闭时 | 开启时 |
| --- | --- | --- |
| `shared-atlas` | UI 子图保留原有 ByteStage/range reload；非 GXT 子图仍尝试单独保留编码字节；已上传 CG 仍可发起旧预读 | KATL 的整份输出只由一个 worker 读取/解压；所有子图以同一 owner 的 range 借用，按需求准备；已上传 CG 不再立即预读同一编码资源 |
| `gxt-ready` | 常规 CG 的 worker 首次 GXT 只建立 descriptor，使用时按 locator 重读 | worker 的原 Vec 移交 `DeferredBytes::Ready`；首次提交直接解析、验证、上传这份数据，成功后释放 |
| `decode-owner` | 常规 PNG/JPEG 解码输出复制到 Rust Vec，再经 worker 交给主线程；UI 的旧 Vec decoder 保持兼容 | CG 首读/重载和 shared-atlas UI decoder 通过 C owner / `PixelOwner` 借用上传；语言 strip 使用 view，grade 原地处理 |
| `gray-r8` | compact 灰度 PNG 仍扩成 RGBA8 | 仅 RAW、无 CPU grade、无语言 strip 的灰度/灰度 alpha PNG，直接解码并上传 R8；这一单通道路径自身使用 C owner，即使单独关闭 `decode-owner` 也不扩回 RGBA |

UI 的 GXT 由 `shared-atlas` 控制其共享缓存路径；`gxt-ready` 针对常规 CG worker 的首个 payload。两个 CPU 缓存与 GPU 驻留缓存分别记账，不以 GXT descriptor 登记冒充已上传。

## 4：共享 atlas 与有界按需读取

代码：[atlas_cache.rs](../../../knife/knife/src/atlas_cache.rs)、[ByteView](../../../knife/knife-render/src/byte_view.rs)、[Assets::ui_textures](../../../knife/knife/src/assets.rs)。

`ByteView` 使用 `Arc<Vec<u8>>` 加检查过的 range。把 Vec 放入 Arc 只移动 Vec 的指针、长度与容量，原像素/编码 payload 不复制；没有使用会重新分配切片的 `Arc<[u8]>::from(Vec)`。第一个 KATL 元数据解析与后续 GXT/PNG 准备共享同一整块 owner。纹理描述符只保存 locator 和 range，冷缓存可回收整块 atlas。

缓存只有一个名为 `knife-atlas` 的 worker，pending locator 上限 4；同一 archive/name + entry 的在途需求加入同一工作。当前 UI 路径完全按需，不再给全部子图发 speculative 任务，因此不存在需要取消的旧 UI 子图预读队列。CG/动画预读仍由已有 `BoundedWorker` 的 context/window generation 协议管理。

整块缓存上限为 **32 MiB**，按 Vec 的实际 `capacity()` 收费，而非按子范围长度收费。当前 `CG_PARTS` 的完整输出约 **25.106 MiB**，能进入缓存；24 MiB 的旧 cap 不足以覆盖它。一个块无论被多少 range 使用都只计一次，仍被借用的块不能淘汰，且继续占用预算。

冷读之前先回收可淘汰块，再把剩余额度交给 `Archive::read_bounded`；不会因为借用 pin 而把另一个整块输出排除在收费之外。若借用的总量使新 atlas 无法容纳，当前加载返回可重试的错误，释放 pin 后可重新请求。32 MiB 是缓存编码/解压输出预算，**不等于整个进程或 libz 输入/工作区的峰值内存**。

同一个 atlas 在有效缓存生命周期内只读/解压一次。超过预算导致的真实冷淘汰仍需重新读/解压，不能宣称整个游戏只解压一次。缺资源、损坏 NX、范围越界和 worker 不可用继续返回错误，不把未完成请求当作空纹理。

## 5：首次 GXT payload 移交

代码：[DeferredBytes 与 GXT 注册/准备](../../../knife/knife-render/src/vita.rs)、[Assets::upload_pixels](../../../knife/knife/src/assets.rs)。

常规 CG 的 worker 输出 Vec 移到 `ByteView`；descriptor 登记保留 `Ready` owner，首次 native prepare 借用它解析 GXT、复核尺寸与格式并复制到 GPU 可见的正式纹理分配。所有 GXM 调用仍在渲染线程。上传失败保留 CPU owner，重试不重新读盘；正式 residency commit 成功才释放 owner 和 CPU 编码字节收费。

Ready payload 与原有 retained 编码源共用 renderer 的 **32 MiB** CPU 编码预算。登记新资源前可丢弃较旧的 Ready payload，但保留已验证 locator；如果刻意混合开关时旧 retained atlas 占满预算，新 GXT 退回 locator，保持有效资源可加载。真实 GPU 淘汰或 Ready payload 因预算被释放后，后续 miss 才按 locator 读取。

因此“首次不重读”适用于 Ready payload 仍在预算内的资源；缓存压力回退仍会产生冷读，日志应反映这一事实。UI shared-atlas 的编码块计入 atlas cache，不在每张纹理上重复收取整块大小。

## 9：C owner 与单通道保留

代码：[DecodedImage](../../../knife/knife-platform-vita/src/image.rs)、[native decoder](../../../knife/vita/image_decode.c)、[PixelOwner](../../../knife/knife-render/src/byte_view.rs)。

`DecodedImage` 独占 C malloc 分配，由 `kv_image_free` 析构。成功返回前 codec context、JPEG driver 临时映射已经释放；留下的 owner 不保留编码输入指针或 GPU context。因此可在 worker 与渲染线程之间移动所有权（`Send`），但保持 `!Sync`。没有把 C 指针伪装成 Rust Vec，也没有在 owner 释放前保留失效借用。

首次 CG、shared UI 和 CG 重载的 native PNG/JPEG 输出通过 `PixelOwner` 直接借到同步 native upload。语言 strip 使用有界 range，保持原先的行取整和 logical height 语义；CPU grade 只修改该独占 range。KIMG 的 codec 标记与实际 PNG/JPEG payload 仍须一致，快捷路径补回了旧 decoder 的格式一致性检查；编码尺寸也与 envelope 核对。

R8 仅用于 RAW 灰度源。C decoder 接受灰度与灰度 alpha PNG；color PNG/JPEG 不走隐式亮度转换。灰度 alpha 的 alpha 被忽略，与原 RGBA 上传后 RULE shader 只读 `.r` 一致；16 位截取高 8 位、低位深展开与原 RGBA 解码保持一致，未加入 gamma 转换。R8 以非 sRGB source 视图上传。持久 group mask/render target 不改成 R8。

## 仍存在的复制与代价

以下路径有明确用途，或不属于此次修改的受影响调用链，不能声称“已零复制整个渲染系统”：

- GXT/PNG/R8 从 CPU 输出到 GPU 可见纹理分配仍有一次正式上传；pitch/padding 布局处理仍由 native backend 执行。
- libpng 的输入 callback 仍将编码字节交给 codec；C decoder 的输出初始化与 interlace 所需工作仍存在。
- 硬件 JPEG 仍保留编码流入驱动缓冲、CSC 输出到可移动 heap owner 的复制；当前硬件基线未覆盖 JPEG，未改动其缓存/stride 协议。
- BNTX/BC7 的既有 payload 提取和必要 CPU RGBA 解码未在此处重写。
- `decode_image` 这条 Vec 兼容接口仍用于 soft-LUT 建立等调用；它与显式关闭 `shared-atlas` 的旧 UI decoder 仍可能产生 `decoded_c_to_vec`。不能要求所有运行场景的该计数绝对为零。
- CPU grade 的像素处理、真正冷 atlas 的完整 NX 解压以及 GPU 淘汰后的资源重载仍然发生。

## 基线指标与后续判读

| 基线已观察项 | 原始基线量 | 新实现应核对的结构变化 |
| --- | ---: | --- |
| range 完整解压／选中范围 | 146 次；约 1.43 GiB／55.6 MiB | 同 atlas 的重复 `rust.nx_range` 显著减少，冷整块读转由 `rust.nx_full` 与 Stage 记录；仍核对卡读取和首次慢帧 |
| ByteStage 任务与结果 | 130 个任务；在途峰值 63；只消费命中 4 次 | shared UI 单 worker，在途 ≤1，取消全部子图预读；`stage_hit.bytes` 为请求 range，`aux` 为 owner capacity |
| GXT 主线程重载 | 20 次、4.178 s；实际上传 62.475 ms | Ready 内首用 miss 有正确 native upload，未再产生同 key 的 `gxt_reload`；压力冷读分开记录 |
| 无用 UI GXT retain | 41.907 MiB、647.706 ms | shared 路径不再调用 `ui_gxt_unused_retain` |
| Vec→Arc／Arc→Vec | 42.862 MiB／1.313 MiB | shared 路径不再执行 `stage_vec_to_arc`、`stage_arc_to_vec` 的 payload 复制 |
| C 输出→Rust Vec | 8.294 MiB、76.525 ms | owner 覆盖路径的 `decoded_c_to_vec` 减少，兼容/LUT 调用仍可能非零 |
| 语言 strip | 原路径会执行 copy_within | owner 路径 `language_crop.bytes=0`，`aux` 保留选中范围大小 |

以上基线值与后台重叠作用域不能相加作为预计节省时间。R8 对受影响的 w×h 源，逻辑像素上传字节从 `4*w*h` 变为 `w*h`；实际 CDRAM 收费还取决于纹理 pitch、池和对齐。ms、p95/p99 与 FPS 提升必须等待同口径用户控制台捕获。

## 已完成的针对性验证

未新增大范围反复测试，以下均为主机或编译验证，不代表 Vita GPU 像素/速度已验证。

| 证据 | 内容与结果 |
| --- | --- |
| [resource-c-tests.log](../../../.work/performance/resource-c-tests.log) | 原有 PNG/JPEG 夹具 ASAN/UBSAN；新增 R8 与旧 RGBA 红通道逐样本一致，覆盖灰度 alpha、16 位截断、byte cap 与拒绝 color；ARM C11 `-Wall -Wextra -Werror` 解码对象编译通过 |
| [resource-rust-owner-tests.log](../../../.work/performance/resource-rust-owner-tests.log) | 11 项 Rust/C owner 测试通过；真实跨线程后地址不变、独占析构一次，静态保持 `!Sync` |
| [resources-render-tests.log](../../../.work/performance/resources-render-tests.log) | 资源 API 迁移后 renderer 69 项通过；使用 host SDK double，未执行 GPU |
| [ready-gxt-test.log](../../../.work/performance/ready-gxt-test.log) | 1 项定向测试通过：首用/失败重试不读 locator，成功 commit 释放 CPU owner/收费，resident 不重读，模拟真实 eviction 后仅重载一次 |
| [atlas-cache-tests.log](../../../.work/performance/atlas-cache-tests.log) | 生产 ByteView/AtlasCache 源码加 bounded archive double 共 3 项通过：同 key 双需求仅一次读取、range 地址共享、26 MiB atlas、pin 占预算、释放后冷读可重试 |

上述 .work 日志保存在本地工作区。最终统一 Vita 编译、打包与其源码身份由主构建记录提供；本文件不把一次并行改动期间未完成的全项目 check 写成通过。用户已明确无需代理连接真机，`hardware_status` 为 `awaiting_user_capture`。
