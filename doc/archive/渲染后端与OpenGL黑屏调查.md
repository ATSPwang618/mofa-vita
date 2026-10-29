# Vita 原生渲染 API 与 Vita3K OpenGL 黑屏调查

本机核验日期：2026-10-28。当前 Vita 目标使用原生 **GXM**，没有链接 VitaGL 或 Vulkan。Vita3K 的 OpenGL/Vulkan 选项决定 Windows 主机怎样模拟并显示 GXM 指令，不决定 VPK 在真实 Vita 上调用什么 API。

## API 与构建证据

| 执行位置 | 当前接口 | 证据 |
|---|---|---|
| PS Vita 上的 ARMv7 游戏 | Rust → C 边界 → `sceGxm*`，由 `sceDisplaySetFrameBuf` 交给系统显示 | `knife/knife-platform-vita/src/gxm.rs`、`knife/vita/gxm_backend.c`、`knife/vita/effects_backend.inc` |
| 桌面 Windows EXE | 桌面 WGPU 渲染模块 | `knife/knife-render/src/lib.rs` 的 `cfg(not(target_os = "vita"))` |
| Windows 上的 Vita3K | 把客体 GXM 指令转换成主机 OpenGL 或 Vulkan | 本机模拟器运行日志中实际创建的后端 |

`knife/knife-render/src/lib.rs` 对 Vita 选择 `vita` 模块，桌面模块在这个目标上不编译。`tools/build-vita.sh game release` 使用 `armv7-sony-vita-newlibeabihf` 与 `vita-gxm`；`knife/knife-platform-vita/build.rs` 链接 `SceGxm_stub`、`SceDisplay_stub`、`SceSysmem_stub`。Vita 着色器使用预编译 GXP/USSE，运行时不要求 OpenGL/Vulkan 驱动，也不要求 VitaGL 的运行时 shader 编译路径。

同时核对了两份实际链接 map：当前 `.work/vita/game-release/knife.map` 与发布目录旧 VPK 对应的冻结 map。两者都有 `sceGxmInitialize`、`sceGxmBeginScene`、`sceGxmDraw`、`sceDisplaySetFrameBuf`，均未发现 `libvitaGL`、`vglInit`、`vkCreateInstance`、`vkCreateDevice` 或 `libvulkan` 标记。链接 map、源文件和产物的身份不同，不能把源文件审查单独当作任意 VPK 的构建证明。

## 相同 VPK 的主机后端对照

本次完整对照使用同一份新构建、无自动退出帧数限制的 release VPK：

```text
.work/vita/game-release/knife-vita-game-270p.vpk
4,123,670 bytes
SHA-256 32810f980e54643b057efab45732241f659091dcf6a692a3292470ce83dd3399
```

它包含新增加的启动资源身份检查。`dist/.../knife-vita-270p.vpk` 目前仍为先前的 2,522,853 字节版本，SHA-256 `3cda38f0a170b2382edb431182d8f017c14d01a70abefd4fb92b1262c50f6f33`；本调查没有把这份新测试 VPK 冒充为已经更新的 dist 包。两份构建都使用 GXM。

测试使用私有副本 Vita3K `v0.2.1 4087-1a9ad6aa`，Windows NVIDIA RTX 4060 Laptop GPU，OpenGL 日志为 `4.6.0 NVIDIA 610.62`。这个副本包含已有的模拟器内存记账修正，没有扩大 Vita 游戏预算。每次建立独立 pref 目录和新 shader 缓存，使用同一套 270p 资源、同一份生成的澄空语言设置和同一组控制器输入；没有改动个人存档或用户原模拟器设置。

| 实际创建的主机后端 | 剧情、菜单、返回的捕获 | 客体/主机状态 |
|---|---|---|
| Vulkan | 剧情与中文可见，菜单有可见内容，返回有可见剧情；不等于所有菜单项目已验证 | 有限输入流程结束后主动停止，正常退出 |
| OpenGL | 多次完整重跑的三项捕获均为 RGB 全零的黑帧，960×544 | `frameReady=true` 且继续读取剧情、音频和字体；停止时主机访问异常 `0xC0000005` |

Vulkan 对照的启动阶段也曾捕获黑帧，随后进入有画面的剧情。因此启动黑帧本身不判作故障；OpenGL 的问题是进入应当有画面的后续阶段后仍持续全黑。不能凭非零 FPS 或 `frameReady` 判定正常显示。

原始对照包括 `vita-270p-release270-zc-1793144960160-84553c.json`，以及 OpenGL 的 `vita-270p-release270-zc-opengl-1793145378477-03dc60.json`、`...1793145664454-a6c466.json`、`...1793145854383-a41a89.json`。最后一次 OpenGL 主机日志在 `Running -> Stopping` 后记录访问异常；这里将停止异常和运行中黑帧分别保留，没有证明两者同根因。

修正截图判定后的 [Vulkan 运行收据](../vita-270p-release270-zc-vulkan-1793146373597-c18b6a.json) 记录剧情、菜单、返回三项均有非黑内容，主机正常退出。对应的 [四次运行对照记录](../vita-render-backend-audit-20261028.json) 重新核验了这次 Vulkan 与上述三次 OpenGL 的产物、日志和截图身份：四次的 VPK、模拟器、调试器和资源清单哈希均相同，实际主机后端取自各自日志。它记录有限流程的后端差异，实机状态和具体根因仍为未验证。

另做了 Windows 窗口区域检查：`.work/vita/vita3k/opengl-window-1793145889885121900.png` 中游戏区域可见全黑，但首次捕获有 DPI 坐标偏差，包含窗口周边，不能当作完整且精确的客户区像素核验。修正 DPI 后另一次捕获使用了 PrintWindow，而硬件加速内容可能无法读回，也不单独用它证明 GPU 画面。Computer Use 工具当时两次初始化失败，原始错误为 `failed to write kernel assets` / OS error 3；只读窗口捕获作为补充，主要可重复像素证据仍为模拟器捕获的 RGB 黑帧。

## 可以确定与仍待定位的部分

可以确定的事实是：当前 ARMv7 游戏调用 GXM；同一 VPK 和资源在这份 Vita3K 的两个主机后端产生不同结果；OpenGL 黑屏已复现。OpenGL 上下文创建成功，日志也记录了多个 shader 编译完成；`C7050` 寄存器未初始化警告不能直接改称为 shader 编译失败。

目前不能确定黑屏具体发生在 GXM shader 翻译、离屏 surface/纹理同步、帧缓冲呈现，还是某一处本工程 GXM 用法与模拟器实现的交互上。仅凭 Vulkan 有画面，不能宣布游戏 GXM 实现真机正确；也不能宣布所有 Vita3K OpenGL 版本都有同一个 bug。黑屏本身不证明这次发生了内存不足。

核对过与该 fork 版本对应的主机源码：

- [OpenGL renderer.cpp，固定 commit](https://github.com/DeQxJ00/Vita3K-mcp/blob/1a9ad6aa1c73468012cd1e3c23741b6810b67614/vita3k/renderer/src/gl/renderer.cpp)：`render_frame` 从 surface cache 获取呈现纹理，取不到时退到上传客体内存；源码明确提及该回退可能是黑图。
- [OpenGL surface_cache.cpp，固定 commit](https://github.com/DeQxJ00/Vita3K-mcp/blob/1a9ad6aa1c73468012cd1e3c23741b6810b67614/vita3k/renderer/src/gl/surface_cache.cpp)：记录颜色 surface 分配失败的位置。旧小探针曾有该日志，本次新 release 的完整 OpenGL 运行没有这条日志，不能把旧错误直接认作新黑屏的根因。

上面的源码路径只说明待追踪的位置。本次没有记录证明实际黑帧一定走过哪一条回退分支。真实 PS Vita 的画面、帧时间及内存仍待实机验收。

## 验证工具修正

`tools/verify-vita-release-270p.mjs` 现在显式支持 `KNIFE_VITA3K_BACKEND=OPENGL|VULKAN`，收据区分 `guest_renderer` 与 `host_emulator_backend_requested`。配置只写进私有模拟器测试目录。

旧收据的 `visible_run_completed` 实际只检查进程状态和标题 ID，字段名易使人误解；原始历史收据保持不变，新脚本改称 `control_sequence_completed`。新增 `tools/vita_capture_check.py` 检查截图哈希、分辨率和真实 RGB 像素，要求 `story/menu/return` 各出现一张 960×544 的非全黑图片；任一全黑、缺失、重复或尺寸错误都不能让 `runtime_verified` 通过。非黑图只是必要条件，不等于画面内容正确。

七项测试覆盖不透明全黑、仅 alpha 变化、极暗蓝色像素、合法启动黑帧、阶段缺失/重复/错误尺寸、图片身份变动和 Windows 中文路径 UTF-8 管道。Windows 重定向 stdin 默认代码页会误读 UTF-8 路径，已改为显式读取 UTF-8 字节；对应回归测试先失败后通过。

本机最终执行七项测试全部通过；修正后的完整 Vulkan 对照也通过了非黑截图判定与正常退出检查。OpenGL 的历史收据经同一像素检查全部未通过。

离线对照工具 `tools/audit_vita_render_backends.py` 可重新验证既有收据、日志和 PNG 的哈希并输出派生记录。它不会重写原始收据或修改游戏。

## 后续处理边界

实机交付继续保持直接 GXM。当前这台 PC 上调试可临时使用已得到画面的 Vulkan 主机后端；这不会给 Vita VPK 引入 Vulkan 依赖。OpenGL 兼容项保持未解决，后续应以最小 GXM 清屏/纹理/离屏/呈现探针逐步追踪，分别记录实际 GPU surface 和主机 framebuffer，不用扩大缓存或增加堆来掩盖黑屏。

本次仅修改本地核验脚本和说明，没有修改 GXM 实现、替换 dist VPK、提交或推送代码。所在分支为 `work/woh-540p-chengkong`；本次未查询远端。

## 2026-10-28 补充：最小 GXM 探针对照

重新构建既有的 `gxm` 与 `pipeline` DEV 探针，分别在同一私有 Vita3K 的 OpenGL、Vulkan 主机后端运行。每种探针在两个后端使用的 VPK 哈希相同，模拟器、调试器哈希也一致；mapping 均为 `disabled`。这些 DEV 运行每次启动新进程，但复用私有 pref 和各 title 的 shader 缓存，不能写成与上文 release 运行相同的全新缓存试验。

这两种探针不加载商业游戏资源。基础探针使用 2×2 RGBA 和内置小 PNG/JPEG；pipeline 使用 2×2 色块、64×64 离屏纹理，以及末尾交叉呈现所需的一张 960×544 缓冲区。pipeline 在普通色块阶段记录已跟踪的 CDRAM 映射 5,242,880 字节、MAIN 映射 8,212,480 字节和 GXM 参数预留 16,777,216 字节；创建末尾显示测试纹理后，native 日志记录 CDRAM 为 7,340,032 字节。这些是该探针的部分内存记账，不是整个进程或真实机器的峰值。

| 最小测试 | OpenGL 主机后端 | Vulkan 主机后端 |
|---|---|---|
| 基础清屏、纹理、透明混合和内置 PNG/JPEG | 捕获为 RGB 全黑；参考比对失败 | 522,240 像素 RGB 参考比对通过，容差 2/255，最大差值 1 |
| 基础纹理、effect fill/sprite、离屏纹理采样 | 色块阶段捕获全黑 | 五项未调色测试块的内部像素比对通过 |
| 交换 display/离屏 surface 及 backing block 后呈现 | 捕获全黑 | 左蓝右红两区域比对通过 |
| 直接显示离屏缓冲区，跳过纹理采样 | 捕获全黑 | 左蓝右红两区域比对通过 |

三项 pipeline OpenGL 捕获均有 0 个非黑 RGB 像素；Vulkan 对应为 518,400、518,400、522,240 个非黑像素。最后一项直接显示完整 960×544 缓冲区，其余两项保留上下黑边。pipeline 的另外两个 `Present` 色块虽有可见颜色，本补充只记录颜色值，没有为其颜色变换建立完整参考，不能随其它五块一起宣布全部 shader 通过。

必须保留的失败与限制：

- 基础 GXM 探针的客体 CPU framebuffer 回读在两个后端都未匹配参考，且在“关闭后第二次初始化 GXM”阶段均出现主机 `0xC0000409`，所以两份基础探针的 `runtime_verified` 均为 false。Vulkan 截图 RGB 通过只代表捕获的这组显示像素。
- pipeline 的 OpenGL 客体日志到达 `RESULT PASS`，但画面全黑，随后主机退出码为 `0xC0000374`；这次运行不通过。Vulkan pipeline 客体有限检查通过，主机正常退出 0，另做的上述像素检查也通过。该有限流程不能代表完整游戏或长时间稳定性。
- DEV `gxm-native.log` 路径由多个探针共用；后一 runner 可能在前一 runner 收尾时开始写同一文件。派生对照不使用基础探针收据内的这份共享 native 日志来独立绑定根因，采用冻结的各探针专属日志、收据、VPK 和唯一截图。这里没有把存在采样歧义的共享日志当作可信的运行归属证明。

可复核的派生记录为 [最小探针对照 JSON](../vita-minimal-gxm-backends-audit-20261028.json)，由 `tools/audit_vita_gxm_probe_backends.py` 离线生成；包含冻结收据身份、配对 VPK 身份、截图哈希、实际 RGB 数量、局部参考比对和各自的退出状态。最初的 `...-draft-region-counts.json` 有局部裁剪区域数量沿用整幅画布数量的报告错误，已由当前记录替代，不能再用该草稿的计数。

由此可把排查范围进一步缩到无需游戏素材即可触发的基本 GXM 绘制／呈现与主机转换路径。不能再仅用游戏图片太大来解释这次黑屏，也没有证据支持靠进一步降低素材尺寸、扩大内存预算或改用 Vita Vulkan 来解决它。尚未定位到具体的 shader、surface 或 GL 调用；下一项有效证据应来自主机 OpenGL GPU 跟踪，比较 GXM scene 的颜色附件、surface cache 命中和最终呈现纹理。游戏的 GXM 用法、该 fork 的实现与驱动交互仍需共同审查。

本补充未修改游戏 GXM 实现或 dist VPK，没有新增 VitaGL/Vulkan 依赖。真实 PS Vita 的启动、画面与性能仍未验证。
