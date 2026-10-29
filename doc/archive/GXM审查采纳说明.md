# PSVita GXM 移植计划：审查采纳说明

日期：2026-10-27。结论：采用“**VitaGL 开发验证、最终直接 GXM**”的路线；先抽离共同绘制接口，再做有限 VitaGL 验证，最后以直接 GXM 版本完成全部验收。已修订 [实施计划](GXM移植实施计划.md)，尚未实现 Vita 后端、构建 VPK 或做真机测试。

用户提供的审查是本次核查输入。审查自身说明未读取本地工程，因此其推断不能直接当作本机事实。本次对照实际源码、WSL pocket-ubuntu 的工具/SDK 及上游一手资料逐项判断。附件中的建议没有被视为执行命令。

## 意见与处理

| 审查主题 | 判断 | 已写入计划的处理 |
| --- | --- | --- |
| Rust 与 C 链接责任不明确 | 采纳，属于实施前必须固定的边界 | Rust/Cargo 唯一链接最终 ELF；CMake 只生成 STATIC 库；入口、堆及 FFI 释放方唯一 |
| ELF→SELF 的链条不完整 | 采纳；本机确有 vita-elf-create | 补齐 ELF→VELF→SELF，SFO 作为并列元数据合包；保留转换所需 relocation，避免提前 strip |
| shader 编译工具是前置门槛 | 采纳，当前尚未解除 | psp2cgc 在所查范围未发现；任何候选工具必须通过 GXP 检查、参数核对和直接 GXM 真机绘制 |
| 漏算 GXM Parameter Buffer | 采纳，旧预算确有遗漏 | 默认计 16 MiB，ring/patcher/内部存储单列；纹理缓存由真实余量反推 |
| 显示 stride/块对齐不能忽略 | 采纳；1024 stride 作为候选例子 | 示例每面 2.125 MiB，按 256 KiB 分配块向上对齐后 2.25 MiB；3 显示面加 2 效果面加 PB 已 27.25 MiB，尚未加 ring/素材/媒体 |
| PB_LPDDR 可降低 CDRAM 使用 | 有条件采纳，不能当作已验证优化 | 普通 sceGxmInitialize 注明默认 flags；额外 flags 的声明为 sceGxmVshInitialize。先核查 stub/导入、普通应用条件和真机结果 |
| AudioOut 阻塞影响主线程 | 采纳，SDK 明确写明阻塞 | 专用输出线程，有界混音/PCM 队列；时钟统计区分提交帧、等待及实际延迟 |
| AvPlayer 的回调、格式与帧寿命不清 | 采纳，并补充本机 API 限制 | 明确内存/纹理/文件/事件回调、64 位偏移；本机视频结构无 pitch/format/plane 独立字段，须验证输出契约；不能证明可跨 GPU 使用期保留时先复制到自有缓冲 |
| 按用途划分媒体“额外内存”可能漏算 | 采纳 | 按 MAIN/CDRAM 等实际内存类型计费，媒体只是用途；与纹理退休、切场景、seek 的重叠峰值一起核算 |
| 素材缺少完整 profile、条带要按语言读取 | 采纳并限定能力 | 清单含路径/归档条目、内容 ID、哈希、尺寸、alpha、语言槽、别名和条带视图；按语言上传，解码是否能省略无关行另做探针 |
| 公共接口不能保留 wgpu 类型 | 采纳，实际源码已证实 | Device/Queue/CommandEncoder 从共同接口移出；VitaGL/GXM 也不能泄漏平台句柄；同一帧命令供三个后端消费 |
| 必须逐项验收全部演出 | 采纳 | 建立效果、pass 顺序、alpha/gamma、参考图、阈值与真机覆盖矩阵，不静默退化为 Alpha |
| 外置素材部署与缺失诊断不足 | 采纳 | 引擎/资源清单绑定、安装校验、增量复制、缺目录/版本不符诊断，避免重复保存整套资源 |
| ATTRIBUTE2 默认值与 256 MiB 预算矛盾 | 采纳 | 不继承 cargo-vita 默认 12；显式设置并读回 SFO，保守候选 0 在 M0 验证；扩展模式另立预算 |
| TITLE_ID 改用常见 4 字母+5 数字 | 采纳为工程约定，不宣称原 ID 必然非法 | 候选最终 KNIF00001、开发 KNIF00002，检查冲突并通过 SFO/安装探针后固化 |
| 21–40 个工作日过于乐观 | 采纳，不沿用为承诺 | M0 后重估工具/依赖，M2 后重估内存/特效/媒体；VitaGL 适配成本单独纳入 |

## 对用户 VitaGL 方案的具体约束

共同接口先于 VitaGL 适配。开发包只验证 PNG/JPEG、透明贴图、字体、简单混合、资源/输入/音频及平台集成；不先完成整套 VitaGL 演出再重写渲染。这样能保留资源和平台验证成果，同时尽早暴露 GXM shader 与显存问题。

计划中的 vita-vitagl / vita-gxm feature 互斥；最终构建固定后者，不自动回退。最终用链接 map、保留符号 ELF、输入库和包清单核验不链接 VitaGL、不调用开发 GL 接口，也不依赖为开发渲染引入的运行时编译库。仅观察某张截图或搜索剥离后的 eboot 字符串不能证明这一点。

开发 VitaGL 的内存、同步与 shader 编译路径单独记录。本机默认 circular pool 注释为 32 MiB，不能把开发包占用直接套入 GXM 预算；本机头文件 API 与当前上游存在差异，必须与实际库匹配。真机 libshacccg.suprx 是否存在尚未核查，不能仅因 WSL 有静态库就宣称开发 shader 路径已经可用。

## 本次校正的本机事实

- vita-elf-create 已存在，SHA-256 为 `46ff8758d9f985e71a06136bec2343c1f7e292860b31ddfb0daa1ca046a962d2`；此前遗漏的是计划中的转换步骤。
- 默认 nightly 没有 rust-src；`nightly-2026-05-28` 已有匹配 rust-src，rustc 为 `1.98.0-nightly (57d06900f 2026-05-27)`。它是待编译探针验证的候选，当前不能概括为“所有 rust-src 都缺失”。
- `/opt/vitasdk/arm-vita-eabi/lib` 已有 libvitaGL.a、libvitashark.a、libSceShaccCgExt.a、libmathneon.a、libtaihen_stub.a 与相关 stub；存在性/哈希已记录，未编译、链接或运行本项目 Vita 版本。
- psp2cgc 在 PATH、SDK bin、Cargo bin 等实际检查范围未发现，不推断整个硬盘不存在。没有独立验证其他 shader 编译器的最新能力，故不把审查中的相关概括当作确定事实。
- 实际渲染源码公开 Device/Queue，并将 CommandEncoder 放在绘制/提交接口；应用条件还会给 Vita 引入桌面 GStreamer/gilrs，必须改造。

完整路径、版本、头文件摘录和哈希见 [补充审计 JSON](../vita-review-toolchain-inventory.json) 与 [日志](../logs/vita-review-toolchain-inventory.log)。这些是工具/源码观察，不是运行性能证据。

## 一手依据与判断范围

- [Rust Vita 目标说明](https://doc.rust-lang.org/rustc/platform-support/armv7-sony-vita-newlibeabihf.html)：标准库与目标限制。
- [cargo-vita README](https://github.com/vita-rust/cargo-vita/blob/master/README.md)：构建链、relocation、默认 ATTRIBUTE2。
- [VitaSDK GXM 头文件](https://github.com/vitasdk/vita-headers/blob/master/include/psp2/gxm.h)：Parameter Buffer/ring 常量、初始化 flags 与 API 声明。
- [AudioOut 头文件](https://github.com/vitasdk/vita-headers/blob/master/include/psp2/audioout.h)、[AvPlayer 头文件](https://github.com/vitasdk/vita-headers/blob/master/include/psp2/avplayer.h)：阻塞输出、回调与结构体。
- [VitaGL README](https://github.com/Rinnegatamante/vitaGL/blob/master/README.md)、[GXM 初始化源码](https://github.com/Rinnegatamante/vitaGL/blob/master/source/gxm.c)：开发后端依赖和初始化参考。所查源码将 PB_LPDDR 放在 system_app_mode 条件下，不能推断本项目普通应用已经可用。

预算数字中 stride=1024、独立块 256 KiB 对齐是清楚标注的候选例子，不是实测值。最终链接方案、阶段边界和缓存策略是本次工程决策。完整游戏可移植性、GXP 执行、AvPlayer 输出格式/寿命、帧率与峰值内存仍须按计划验证。
