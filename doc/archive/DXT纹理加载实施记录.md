# 魔法使之夜 540p：Vita DXT 纹理加载实施记录

修订日期：2026-10-27。路线为直接 GXM；本记录说明已实现的有限纹理实验、实际证据和未完成边界。Windows 的 540p PNG/JPEG 资源作为参照保留，Vita 使用独立实验目录。没有批量替换全部图片，没有引入 WebP，也没有恢复高清源图。

## 1. 按实际像素分流

| 资源 | 当前处理 | 原因及限制 |
| --- | --- | --- |
| 全部像素 alpha=255 的普通背景/CG | 可离线转 DXT1 | 先验证颜色处理、尺寸和脚本用途，再选择替换；文件后缀不足以判断 |
| 任一像素透明或半透明的立绘/CG/UI | 可离线转 DXT5 | 包括调色板展开后才出现的透明度；仍需检查压缩边缘、渐变和叠色 |
| PNG 的 P/PA、PLTE、tRNS | 先按调色板展开为 RGBA，再做以上选择 | 不把调色板索引当 RGB；tRNS 的透明/半透明值纳入判断 |
| 字体、细文字、数据纹理和颜色 LUT | 保留原有用途及格式，逐项评估 | `soft_lut_rows` 等供 shader 读取的数据不能套用普通图片的 sRGB/Alter/DXT 转换 |
| 需要 CPU grade、raw sample、语言 strip 的图片 | 当前 codec 8 路径明确拒绝 | 不能把预烘焙颜色当作未处理像素，也没有原图 sidecar 或高清回退 |

已核对的真实反例：`IMG1227` 的 486,000 个像素均不透明，可进入 DXT1 实验；`IMG2328` 存在 218,752 个半透明像素，必须保留 alpha。`IMG2343` 虽然不透明，但存在载入后的 CPU grade 指令，不能仅凭透明度批量转换。审计范围和具体脚本位置见 [背景审计](../07-历史归档/调查笔记/DXT背景评审.md)。缩略图和同名高清条目必须按归档及索引区分，不用名称替代身份。

## 2. 离线转换和颜色契约

[转换器](../../tools/vita-game-gxt.py) 将选定 KIMG/KATL 成员转换为 KIMG codec 8，内含单张 GXT。DXT1/DXT5 的块由 PC 端产生并通过本机 SDK 工具封装；不在 Vita 运行时解码 PNG/JPEG、转 RGBA 或再次运行默认 Alter。

当前 codec 8 只表示默认 Alter 已离线烘焙的 sRGB 图片。RGB 变换为 `srgb_encode(pow(srgb_decode(src_RGB), 1/2.2))`，alpha 保持 straight alpha；后续 DXT 编码本身有损。原始 RGB 字节按 IEC sRGB 解释，当前工具不做 ICC 色彩管理。不能把这条路径推广成任意 grade 的等价替换。

运行时仍使用实际图片尺寸、脚本逻辑尺寸及原来的 UV/布局。GXT 的二次幂 backing 只是存储布局，不能把 900×540 的可见图错误缩放到 1024×1024。源纹理和画布的 sRGB、linear clamp 采样、透明混合均沿现有生产 GXM 路径。

| 实验纹理 | 逻辑尺寸 | 实际图片尺寸 | backing | 块数据 |
| --- | --- | --- | --- | --- |
| `allui[11]` 内 `menu_btn_en`，DXT5 | 1748×840 | 960×461 | 1024×512 | 524,288 B |
| `jallpac[1884] IMG1227.JPG`，DXT1 | 1400×840 | 900×540 | 1024×1024 | 524,288 B |

这些逻辑尺寸用于兼容原脚本坐标，不表示恢复大图。当前运行时允许的物理图片上限仍为 960×1080，其中纵向双语条带每语言视图为 960×540。

转换器已有 [41 项实际 SDK 自检（原证据目录 .work 已清理），包含调色板、tRNS、PA、透明 DXT1 拒绝及损坏元数据等情况。该自检收据未记录当时的工具哈希和时间，因此当前工具源码另行冻结，不能把它说成完整可重现输入证明。`IMG1227` 的转换收据见 [img1227-dxt1.json（原证据目录 .work 已清理）：完整 KIMG 为 524,384 B，SHA-256 `d0ee4e9ad76868f2e4db619287af661bade610c53fc3046f0110989355976a2b`。DDS 中间结果相对预烘焙 RGBA 的 RGB MAE 约 2.19 / 1.16 / 2.38，最大通道误差 98；这是 CPU 中间结果，不能据此宣称真实游戏画面通过像素验收。

## 3. 普通 CG 的加载、预取与内存

[assets.rs](../../knife/knife/src/assets.rs) 已接入普通 CG 的 codec 8 路径。资源 worker 读取并解析 KIMG 后，仅把去除归档扇区 padding 的一个拥有所有权的 `Vec`、`Arc<Archive>` 和精确条目索引传回主线程。主线程注册 deferred GXT；首次实际绘制时才上传。注册后不长期保存原编码 `Vec`，GPU 缓存缺失通过 `read_uncompressed_range(index, 0, len)` 有界重读，不整体载入 `jallpac.mrg`。

CPU 侧解析和文件读取可以在资源 worker 中推进，GXM 注册/上传仍在拥有渲染上下文的主线程。不能把这个边界称为所有上传都已异步化。UI 图集按归档条目和 Renderer 身份共享，现有 GPU LRU 上限 32 MiB，压缩源缓存另有预算。

普通 RGBA 路径已按对齐后的 CDRAM 用量进行上传准入，同时计入源图和 Alter 目标短暂共存的峰值。缓存回收只选没有活动强引用的空闲图片；资源暂时分配失败不缓存成永久“文件不存在”。剧情预取使用接下来的 24 个 STCH/SCH2 窗口，跳转后清理窗口外的待上传 CPU 结果，避免过时预取挤占当前场景显存。提交后的资源还必须遵守帧引用及 GPU 完成时序。

默认路径、CPU grade、raw、strip 各自有路由测试。当前限制明确报错，不会静默双重 Alter、按文件名选择其他图片或回退到高清归档。`IMG1227` 已审阅的线性脚本使用符合此限定，但未证明全篇跳转、全部存档恢复和所有脚本用途。

## 4. 资源体积与实验目录

DXT 的收益首先是减少解码、像素复制及 GPU 纹理用量，不保证缩小安装包：

| 选定文件 | 原文件 | 新文件 | GPU 块数据 / 分配比较 |
| --- | --- | --- | --- |
| `IMG1227` KIMG | 36,115 B（JPEG 负载 36,083 B） | 524,384 B | DXT1 512 KiB；原 RGBA 按 CDRAM 对齐约 2 MiB，且默认 Alter 还会短暂共存另一份目标 |
| `menu_btn_en` KIMG | 182,173 B | 524,384 B | DXT5 512 KiB；原 RGBA 有效像素为 1,770,240 B，另计对齐和 Alter 目标 |

KIMG 还经过外层 NX 压缩。实际 `IMG1227` 所在区段从 36,864 B 增至 73,728 B，`jallpac.mrg` 从 1,861,562,368 B 变为 1,861,599,232 B，归档净增 **36 KiB**；不能用未封装 KIMG 的增长量推算最终安装体积。

因此现在不把“35,007 张图全部转 DXT”当作空间压缩方案。最终格式要结合真机加载时延、帧时间、MAIN/CDRAM 峰值和画质选择；可以按热点选择 DXT，保留小 JPEG 和精细 UI 等资源。

菜单实验位于 `.work/vita/game-gxt-stage/menu-btn-en-dxt5/assets`。组合实验位于 `.work/vita/game-gxt-stage/menu-and-img1227-dxt1/assets`，只私有重建 `jallpac.hed/.mrg/.nam`，其余资源通过硬链接复用父实验目录。硬链接资源不得就地写入；归档替换必须创建独立文件。重建一个归档需要额外的实验磁盘空间，这不是最终游戏安装体积。

本次组合 stage 的三个私有归档和清单实际新增磁盘分配为 1,901,977,600 B，约 **1.77 GiB**，主要是重建的独立 MRG；9,566 个硬链接不新增素材负载。这个额外空间必须与上面的最终归档净增 36 KiB 分开核算。详见 [资源验证及空间收据（原证据目录 .work 已清理）。

[归档工具](../../tools/stage-vita-gxt-cg.py) 核验 22,156 个条目，仅改变 index 1884；其他 22,155 个条目的存储和解码字节相同，重名和共享区段均保留。工具 [41/41 自检（原证据目录 .work 已清理） 与冻结的最终源码哈希匹配。父菜单清单完整保留，过时的 `jallpac.compact.json` 不复制。运行验证器还会重新核对父清单、私有归档的实际大小、哈希及文件身份，遇到损坏或意外硬链接会拒绝运行。

## 5. 验证状态

本节记录最新结果；较早的 [CG 汇总](../vita-dxt-cg-validation.json) 保留其原始失败状态。完整调查见 [存读档与模拟器内存调查](存读档与模拟器内存调查.md)，机器可读结果见 [内存及保存恢复汇总](../vita-gxm-memory-save-validation.json)。

- 当前 Vita ARM 交叉编译及包审计通过，DEV VPK 为 [knife-vita-game-dev.vpk（原证据目录 .work 已清理），TITLE_ID `KNIF00010`，限制 **7,200 帧**，SHA-256 `e983986f9fd0f0d9995325eaabc2126636e753c1e4d1cf10759df417215ba00a`。产物、当时选定源码及运行证据保存在 [冻结清单（原证据目录 .work 已清理），32 文件共 15,954,727 B，未复制游戏大归档；源码快照不是完整构建认证。
- 最新真实手柄流程完成 **空槽 0 保存 → scene 1 pc50 推进到 pc56 → 读档回 pc50**，文字页从 1 条/59 字符变为 2 条/143 字符，再恢复；12 次控制器按下/释放及对应状态均验证通过。保存及恢复时实际存档均为 4,175,872 B、完整文件和槽记录 SHA 相同。客体提交 7,200 帧、`RESULT PASS` 并自然停止；详见 [完整运行收据](../vita-game-save-inputs-staged-1793120052058-a3c92f-vulkan-disabled-runtime.json)。这只覆盖选定状态/文件，不是完整剧情或像素恢复证明。
- 本轮减少 Vita 常驻目标：菜单/存档共用一对目标节省 **4 MiB**，通用 blur scratch 改为首次需要时创建，所测流程再省 **2 MiB**。未放宽 CDRAM 预算、帧内 pin、Alter 上传峰值准入或 GPU 完成后退休规则。6 MiB 是本条流程的常驻目标节省，不能写成全程峰值降低 6 MiB。
- 本轮记录 **117 次原生块分配/117 次释放**，其中 CDRAM **103/103**；CDRAM 峰值 **50,331,648 B / 48 MiB**，非 CDRAM 原生块峰值 8,212,480 B，结束无账本中的存活块。不包含全部 GXM 隐式分配、Rust 堆或宿主堆；48 MiB 峰值仍达到准入预算。较早的 backlog 准入失败仍单独待修复/验证。
- 本轮日志为 `IMG1227` **1 次 DXT1 上传**，`menu_btn_en` **2 次 DXT5 上传**，均 `runtime_decode=false`、`runtime_alter=false`。此前另一个 [3,600 帧 CG 运行（原证据目录 .work 已清理） 到 scene 1 pc125，记录 IMG1227 三次上传；两个运行的次数、资源状态不得混写。有界 range 重读与缓存所有权另有测试，不将缓存重载误报成只上传一次。
- 当前资源/输入/保存/压缩/颜色证据测试 **50/50**，`knife-render` 库测试 **47/47**；分别见 [主机日志（原证据目录 .work 已清理） 和 [渲染测试日志（原证据目录 .work 已清理）。真实组合 stage 启动前核验父清单、归档大小/哈希/身份。此前 [Windows 回归](../vita-windows-cg-codec8-regression.json) 的 16 个新生成 x64 PE、186 项测试通过，是最新目标生命周期 cfg 改动之前的快照；此轮未重打 dist EXE、未运行 Windows 游戏窗口。
- 原版 Vita3K 在实际 SDK 两轮 USER_RW/CDRAM/PHYCONT 分配/释放中，六次均只把可用计数加回 24 B。已创建独立、哈希锁定的普通释放计数诊断副本，**没有替换原 EXE**；[实际 SDK 收据](../vita-platform-runtime.json) 的六次回收现均等于完整分配量。`FreeMemBlockForVM` 未改未验，模拟器报告的池容量也不等于真机预算。
- 最新游戏及 SDK 探针使用上述诊断副本、Vulkan、memory-mapping disabled；宿主关闭仍为 **3221226356 / `0xC0000374`**，因此 `runtime_verified=false`、`hardware_verified=false`、`emulator_shutdown_verified=false`。已在原版字节、未启动客体的 [空闲关闭对照（原证据目录 .work 已清理） 复现同码并保留转储，栈包含 Qt GUI 销毁路径；最早的堆破坏写入和根因仍未知，不能直接归因于 Qt 或客体析构。
- 已人工检查新运行 [720 阈值截图（原证据目录 .work 已清理）、[推进后截图（原证据目录 .work 已清理） 和 [恢复截图（原证据目录 .work 已清理）：蓝色云层、英语文字可见；左侧黑/白底色及模糊效果没有完成严格对照。白色左区在读档前已出现，不能据此认定为读档回归。阈值抓图会晚于对应帧，文件名不是精确帧时点。截图 960×544 为显示输出，素材仍为 960×540。
- 生产 GXM 颜色合成探针的三份 GPU 截图，在固定 RGB tolerance=8、最大坏像素比例 2% 下均 0 坏像素，alpha 无超限；覆盖 Sprite/Alpha、预烘焙 RGBA/DXT5、sRGB、有效尺寸和线性采样，见 [像素比较](../vita-ui-color-vulkan-disabled-display-comparison.json)。它不等于真实 CG 全链路通过，且该探针宿主异常，整体 `gpu_verified=false`。

真机性能、长时切场景、完整剧情、分支/多槽/重启后读档，以及严格游戏帧像素对照尚未验收。模拟器的有限耗时不作为 Vita 硬件 FPS 或全游戏流畅度结论。

## 6. 可重复命令和下一道门槛

PowerShell 示例，`python`/`node` 须指向本机已验证运行时；SDK 工具只从本机使用，不随项目分发。诊断模拟器及 debugger 的哈希、生成方式和限制见 [调查](存读档与模拟器内存调查.md)：

```powershell
python tools/vita-game-gxt.py --help
python tools/stage-vita-gxt-cg.py --self-test
node --test tools/vita-save-evidence.test.mjs tools/test-vita-game-evidence.mjs tools/vita-compressed-evidence.test.mjs tools/vita-ui-color-evidence.test.mjs tools/vita-game-stage.test.mjs
wsl -d pocket-ubuntu -- env KNIFE_VITA_DEV_MAX_FRAMES=7200 bash /mnt/d/AI-CODEX/SWITCH-魔法使之夜/tools/build-vita.sh game
$env:KNIFE_VITA3K = 'D:/AI-CODEX/SWITCH-魔法使之夜/.work/vita/emulator-memory-fixed/Vita3K.exe'
$env:KNIFE_VITA3K_DEBUGGER = 'D:/AI-CODEX/SWITCH-魔法使之夜/.work/vita/debugger/vita3k-owned-debugger.exe'
node tools/verify-vita.mjs --game-save-inputs --memory-mapping disabled --renderer Vulkan --game-assets-root 'D:/AI-CODEX/SWITCH-魔法使之夜/.work/vita/game-gxt-stage/menu-and-img1227-dxt1/assets'
```

真实 stage 已存在时，转换/归档工具拒绝覆盖；重建资源使用新的目标目录。下一道门槛是在同一资源、脚本状态及时点下做 Windows/GXM 游戏帧对照，定位底色和模糊差异；独立覆盖 backlog、更多效果/切场景和多槽/重启读档。宿主堆破坏仍需定位最早写入，并保留正常 host exit 0 门槛。随后真机统计加载与帧时间 p50/p95/p99、MAIN/CDRAM 峰值、音频 underrun 和透明边缘画质，再决定 DXT 转换范围。最终交付继续直接调用 GXM。
