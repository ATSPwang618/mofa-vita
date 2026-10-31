# r4 shader 编译失败的只读诊断

结论：r4 已解决 r3 的模块不可用问题，当前失败点是 **绕开 vitaShaRK 后遗漏 `sceShaccCgExtEnableExtensions()`，PSM 编译器未注册 Vita native-color 扩展关键字**。这是本轮 runtime 集成缺陷。不能把它归因于模块丢失、导出未绑定、目标寄存器不支持或 shader patch 失败。

本报告只分析原始日志与源码／二进制；没有修改运行时代码、调用设备、启动 Vita3K、编译 VPK 或产生新的 fetch GXP。机器可读证据见 [`shader-diagnosis.json`](../../../../performance/captures/2026-10-31-r4/shader-diagnosis.json)；静态指令摘录见 [`r4-shader-evidence/compiler-disassembly.json`](../../../../performance/captures/2026-10-31-r4/r4-shader-evidence/compiler-disassembly.json)。

## 硬件日志已确认的阶段

输入 `${CAPTURE_ROOT}/capture-r4.txt`，SHA256 `f69760a35c8f17422ed37a5c6c9621c62422d88645196de8030867f74c994809`。

- 原始行 31–43：`app0:/sce_module/libshacccg.suprx` 身份检查成功，1,776,523 bytes，SHA256 `0c58b5d3e1cc47d9837b8c59ab6f3c299bfe7adf55a83f547d553f9fbb776552`；LoadStart、GetModuleInfo、7 个导出归属与 allocator 均成功，真实版本为 SDK 3.0.0 / shacc r13225。
- 行 44–48：18 / `fs_ps19_fetch`，123.838 ms，输出 0 bytes，4 条语法诊断。
- 行 49–51：15 / `fs_group_composite`，425.070 ms，输出 1032 bytes，0 诊断，flags `0x00181801`，metadata 验证及缓存写入成功。
- 行 52–56：17 / `fs_ps_fetch`，124.220 ms，同样输出 0 bytes、同样 4 条诊断。
- 行 57–60：编译器释放成功，最终 `group15=1, ps_fetch17=0, ps_fetch19=0`。group 的硬件成功已成立；fetch 根本没产生 GXP，还未进入反射、native patch 或像素验证阶段。

这三次编译总计 673.128 ms，发生于启动阶段。日志没有逐帧反复编译证据，不能把启动编译时间当作持续 FPS 损耗。持续损耗来自 fetch 未启用后仍走旧 snapshot 路径：frame=2201 时 `copy_ps_snapshot` 累计 3446 次、1,786,406,400 逻辑 bytes、578,990 CPU us。这里的 bytes 是 GPU copy 逻辑量，**不是 CPU→GPU 上传量**；复制引出的 scene/EndScene 代价应由父分析与分类日志另行归因，不能把整个 EndScene 都算成这 578.99 ms。

## 报错映射到实际 r4 源

`knife/vita/shaders/effects.cg` SHA256 与 r4 embedded 源一致：`33d2e81b8396b6a8813488dea963efdccdf7f83ccb28c863b523b47c6d4dfadb`。

| 行 | 原源码与报错 | 判断 |
| --- | --- | --- |
| 517 | `__nativecolor __regformat half4 fs_ps_fetch(...)`；5496 expected type specifier / identifier | 原本应作为 qualifier 的扩展词落为 identifier，入口声明解析失败 |
| 522 | `__nativecolor __regformat half4 fs_ps19_fetch(...)`；同上 | 同一个翻译单元内另一个入口也受影响 |
| 527 | `float4 d = float4(destination);`；5416 destination 未声明 | 参数声明已失败后的连锁错误，不是 FRAGCOLOR 名称被证明错误 |
| 528 | `return half4(...)`；5496 expected type specifier / return | 声明解析失败后错误恢复的连锁诊断 |

日志只打印行号，没有 column。因此“517/522 第一条 identifier 对应首词 `__nativecolor`”是结合声明顺序和静态词法证据的推演，不能冒称编译器实际打印了该词。已直接证明的是 **`__nativecolor` 与 `__regformat` 都受同一个关闭的关键字开关控制**。不要根据这组诊断随意改 `destination` 的名字、FRAGCOLOR 语义、half 的数学含义或 PsOp19 的 destination-alpha 分支。

两个 compile 请求都提交完整 effects.cg，并同时定义 `KNIFE_RUNTIME_NATIVE_FETCH=1`，所以即使 entry 选 18，编译器仍需解析 517 的 17 入口；17 同样会解析 18。这解释了两者完全相同的报错行，不能认为两套运行路径各存在 4 个独立语义 bug。

## 实际模块的词法逆向证据

对上述**同一 SHA 模块**解压第 0 段，和已有 `hazuki-module-seg0.bin` 做逐字节相等校验成功。以下地址采用模块链接基址 `0x81000000`；实际加载基址会改变，右列 module offset 不变。

| 项目 | 模块地址 / offset | 数据 |
| --- | --- | --- |
| token spelling table | `0x81290A30` / `0x290A30` | token 到英文拼写的表 |
| token flags table | `0x81290840` / `0x290840` | `GetTokenFlags`，访问下标为 token−0x40 |
| `__nativecolor` | token `0xE6`，flags 地址 `0x8129098C` | flags=`0x11`，包含 bit4 扩展门控 |
| `__regformat` | token `0xE2`，flags 地址 `0x81290984` | flags=`0x11`，同上 |
| `half` | token `0x4A`，flags=`0xC9` | 不包含 bit4；group 实际也使用 half/half3 并编译成功 |
| `float` | token `0x4B`，flags=`0x1CD` | 不包含 bit4 |
| keyword registration | `0x8100AA28` / `0xAA28` | 在普通 Cg 分支按 flags 和配置注册关键字 |

关键指令链为：`+0xAA38 lsls r3,r0,#0x1b` 检查 flags bit4；`+0xAA3A bpl +0xAA40` 让普通关键字直接进入注册；对 bit4 关键字执行 `+0xAA3C ldrb r3,[r7,#0x14]`；`+0xAA3E cbz r3,+0xAA70` 在配置为 0 时跳过注册。这是实际机器码证明，不仅是从诊断字符串猜测语法。

模块里的 FRAGCOLOR 检查文案仍是真实的：`+0x296510` 要求 entry 的 `__nativecolor`；`+0x296560` 要求输入 `__regformat`；`+0x2965B4` 要求 COLOR 输出 `__regformat`；`+0x296620` 要求输入输出尺寸一致。但这些文案只证明后端存在合同，**不能证明公开 PSM 编译入口默认启用了对应前端关键字**。r4 前次推断恰好漏掉了这一层。

## Hazuki 已有依赖的交叉证明

Hazuki 已有 `platform/vita/third_party/android_compat/lib/libvitashark.a`（3400 bytes，SHA256 `d42001edfb8f5cca319bd8174f18b8c52cf93a7c2791b086d582a9f53a5514b1`）和 `libSceShaccCgExt.a`（60514 bytes，SHA256 `dd1af9118cb3095d0686f1788a06c6cb4ac7b5f2fb9a76d371209dfad4abcbc4`）。读取 ELF 重定位和 Thumb 指令确认：

1. **现有** libvitashark 的 `shark_init` 内 `.text+0x4E` 调用 `sceShaccCgExtEnableExtensions`，`shark_end` 的 `.text+0x9E` 调用 Disable；并非仅依赖上游当前版本做假定。
2. **现有** Ext 库的 `FUN_811fe7a4_patch` 有 `movs r5,#1; strb r5,[r4,#0x14]`，实际将编译配置的扩展开关置 1；对应上游 hook 的原模块 offset 为 `0x1FE7A4`。
3. 本机真实模块 module-info `+0x276A74` 的 NID 是 `0x6C3C7547`，属于 Ext 明确支持的两个 NID 之一。
4. Hazuki 的 vitaGL.patch 1589–1603 行在实体机普通分支选择 `shark_init`；仅 `HAVE_VITA3K_SUPPORT` 分支选择不启用扩展的 `shark_init_simple`。r4 自己实现 raw-export 流程，遗漏了普通分支所做的扩展初始化。

取证时保存的上游原码及 commit/sha 在 `r4-shader-evidence/`，主要链接为 [SceShaccCgExt 启用与配置 hook](https://github.com/bythos14/SceShaccCgExt/blob/86d78db0d2a0a3ee65769c7833becc69a19f0eff/src/shacccg_ext.c)、[vitaShaRK 初始化／释放](https://github.com/Rinnegatamante/vitaShaRK/blob/df24065e65098b2d1ac533760109ad4367573f28/source/vitashark.c)。上游 README 也说明该库恢复 PSM 去掉的 Vita Cg 扩展。

另一个差异是 r4 `options.useFx=0`、vitaShaRK=1；它不是上述扩展门控的替代开关，当前没有硬件实验证明单改 useFx 能恢复原生关键字。应记录并按已有集成合同决定，不能先靠翻这个值碰运气。

## 可交付修正路线与证据边界

后续实施时，应复用 Hazuki 已有 Ext 依赖，在 full-SHA/module/export 检查成功之后、任何 fetch compile 之前启用扩展，检查返回并记录独立 `extension_init` 阶段。失败保持旧 snapshot fallback；不要把 module ready 视为 native-language ready。保留已验证的导出归属校验，避免重新引入 weak 导入绑定问题。

遵循已有生命周期：所有 compile output 已销毁后，ReleaseCompiler、DisableExtensions、卸载 module；部分初始化失败也需释放 hook/injection。重用已验证库并保留它的模块 NID 检查，不直接猜写私有偏移或修改 GXP。编译策略／扩展 ABI 应纳入 cache key 或 bump cache ABI，避免同一 source/module hash 下复用不同编译配置的缓存。

保留 FRAGCOLOR 入口及 PsOp19 的 a≤0 返回完整 destination、其余分支使用 D.rgb×D.a 的原数学逻辑。只在语法入口被恢复后做最小单入口原生颜色读取编译与 op19 编译，检查真实 GXP flags、寄存器格式、反射、无 tex1、native patch 成功，之后才暴露 capability。现有 clear shader 的 `__nativecolor __regformat unsigned char4` 示例证明 qualifier 次序有可用先例；它不证明可以把此项目的 HALF4 归一化数学直接换成 uchar4 0..255。

**目前仍无新的 17/18 GXP、无 native fetch patch、无像素等价／性能提升实机证据。**本轮可以确定应修哪一层，以及不能继续以语法猜测改数学；不能承诺启用 Ext 后不存在后续 metadata、register-format 或像素问题。
