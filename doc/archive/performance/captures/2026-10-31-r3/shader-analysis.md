# r3 着色器能力未启用：只读核对

本次只读核对日志、现有源码、VPK 内容及已有 Hazuki 编译器模块。没有修改运行时代码，没有启动 Vita3K、Docker、设备或编译任务。

## 日志事实

输入：`${CAPTURE_ROOT}/capture-r3.txt`。

- 第 26 行：`00:28:52 [knife-shader] compiler unavailable error=80010002; legacy snapshot/scratch retained`。
- 第 27 行：`group15=0, ps_fetch17=0, group_patch_status=0, fetch_patch_status=0`。
- 整份捕获只含这一条 `[knife-shader]` 行，没有 cache hit、compiled、metadata rejected、exports rejected 或 SceShaccCg 模块加载行。
- 日志末尾可见累计 `copy_ps_snapshot=5920`、`copy_group_scratch=2261`；没有 `draw_group_fused` 记录。这与保留旧路径一致。缺少的 shader 编译／加载日志，不能靠 optimization_mask=4095 或 features 名称补作启用证据。
- `effects_backend.inc:198` 把两个 patch status 初始化为 0，只有 runtime program 存在才进入注册／patch。因此本次两个 status=0 是 **未走到 optional patch 的默认值**，不能解释为两个 shader patch 成功。

结论：本次最高优先级的“PS snapshot 消除”和“组 scratch／MaskAlpha 消除”均未兑现。r3 已生效的 arena、异步 scene 提交、紧凑流等变化不能拿来替这两个尚未启用的核心项报完成。

## 最后一个路径错误被误当成整体错误

`runtime_shaders.c:236` 当前依次查找：

1. `app0:/sce_module/libshacccg.suprx`
2. `ur0:/data/libshacccg.suprx`
3. `ur0:/data/external/libshacccg.suprx`
4. `ux0:/data/ArtemisVita/libshacccg.suprx`

每轮把 `module` 覆盖成最新结果，最后只打印一次。`0x80010002` 是最后一个 Artemis 路径的 NOENT；它 **不能证明前面三个路径全都不存在**。前面可能是文件不存在、SELF 加载／启动错误或导入检查失败，目前都缺每路径的返回值与阶段。捕获又包含数秒断连，不能仅凭某行缺失完全排除当时没被采集到的中间行。

下一实现应打印每个候选路径的 open/stat、LoadStart 返回值、模块名／UID、六个导入目标验证结果，最终保留第一个有意义的非 NOENT 错误。stdout 汇总即可，不写性能日志文件。显式报告 cache missing、compile not attempted、compile failed、metadata failed、patch failed 五个阶段，避免 default 0 冒充成功。

## 可直接复用的真实构建依赖已经存在

工作版 Hazuki 使用的文件：

```text
${REFERENCE_PORT_ROOT}/platform/vita/sce_module/libshacccg.suprx
bytes = 1776523
SHA256 = 0c58b5d3e1cc47d9837b8c59ab6f3c299bfe7adf55a83f547d553f9fbb776552
```

`Hazuki/platform/vita/CMakeLists.txt:121` 已将这份文件加入 VPK 的 `sce_module/libshacccg.suprx`，即用户实机的 `app0:/sce_module/libshacccg.suprx`。该依赖不是需要另找的 Windows psp2cgc，也不需要下载新模块。

r3 的 `tools/build-vita.sh:137` 只打 eboot、SFO、sce_sys 和 LICENSE；实际 release VPK 仅这八项，没有 `sce_module/libshacccg.suprx`。receipt 又显式写 `compiler_module_bundled=False`。因此 mofa 没有复用 Hazuki 已验证的随包部署方式，转而依赖用户设备恰好有全局安装。这是 r3 构建依赖未接入的问题，不能归咎于用户没有额外安装编译器。

具体可交付路线：下一次本地私用 VPK 直接以环境变量／明确文件参数选中这份用户已有依赖，核对上述 SHA、大小和 SELF 结构，放入 `app0:/sce_module/`；receipt 记录真实来源 SHA 与打包状态。无需把模块加入公共源码仓库，也不需要下载、重分发到第三方或重新询问用户路径。是否采用由父任务按当前授权执行，本次没有复制或打包。

静态解包核对：SELF segment 0 正常 zlib 解出 3,139,924 字节，模块名为 `SceShaccCg`，模块 info 的 version 字段为 `0x0101`，导出库 version=1、library NID=`0xA05BBEBB`，含 33 个函数导出。`0x0101` 也是 SDK 模块 info 结构约定值，**不能把它说成 Cg 编译器产品版本 1.1**。可见产品字符串为 `PSP2 Cg Compiler (runtime version)`；精确 compiler build 需之后成功加载时调用 GetVersionString，不在本次伪造。

六个必要导出均存在：

| API | NID | 原始 Thumb 地址 |
|---|---|---|
| CompileProgram | 66814F35 | 81204B09 |
| DestroyCompileOutput | AA82EF0C | 812048E1 |
| InitializeCallbackList | A8C2C1C8 | 81204859 |
| InitializeCompileOptions | 3B58AFA0 | 812047FD |
| SetDefaultAllocator | 6F01D573 | 81204A2D |
| ReleaseCompiler | 95F57A23 | 81204C55 |

此前本地模拟器目录的两个同名文件 SHA 分别为 `d9d051b6...` 和 `0ee59c7a...`，与这份真实工作版依赖不同。其先前失败结果不代表这份 Hazuki 文件不可用，也不构成任何新 shader 的编译证据。本次没有再运行模拟器。

## 发现第二个实现缺口：目标颜色 semantic 已有确切二进制佐证

即便只补模块，当前三个 fetch 候选仍不应被当作正确方案：`effects.cg:494,499,504` 都把目标输入写为 `COLOR`，而真实编译器自身的诊断字符串明确要求 `FRAGCOLOR`。

以下偏移是 **上述 SHA 对应文件，正常解压后的第 0 个 ELF segment 内偏移**，可重复只读复核：

| 偏移 | 编译器诊断内容 |
|---|---|
| 00296510 | using 'FRAGCOLOR' requires the entry function to be declared as '__nativecolor' |
| 00296560 | 'FRAGCOLOR' varyings must be declared with the additional '__regformat' specifier |
| 002965B4 | if __nativecolor is used, the 'COLOR' output must be declared with the addtional '__regformat' specifier |
| 00296620 | the size of the 'FRAGCOLOR' input and 'COLOR' output must be identical |

由此得到有编译器证据支持的入口声明方向，而非继续试三种 COLOR 猜测：

```c
__nativecolor __regformat half4 fs_ps19_fetch(
    FragmentInput i,
    __regformat half4 destination : FRAGCOLOR) : COLOR
```

这只是入口 ABI 的明确源码修正方向，**本次没有编译该声明，尚无新 GXP、native flags、patch 或像素通过证据**。之后仍需检查二进制 `NATIVECOLOR_USED|FRAGCOLOR_USED` 两个位、HALF4／输入输出尺寸、无 tex1 sampler、完整反射和真实目标 patch。目标 gamma／HALF 精度也不能仅由这几个 flag 推导成逐像素等价。

若本次 PS op 直方图确认只有 19，应先编译该单入口：保留 `a<=0` 返回原 RGBA 的早退，以及 `d.rgb*d.a + a*(s-d.rgb*d.a)` 的精确公式；不先保留整套 24-op 动态选择及三套候选声明。group 组合、haze／soft／rule 仍应按实际调用保留必要部分。成功能力日志、PS copy 数下降和后续用户实机画面才是推进依据。

## SceLibm 警告不是本次 compiler 缺失的证据

- r3 第 19 行 `00:28:48 # Library not found: [SceLibm, ver=1]` 早于 header 与 renderer 内的 runtime 初始化；它未表明是哪个模块的依赖。
- 冻结的 r2 基线 `doc/performance/baselines/2026-10-30-r2/capture.txt` 第 56、115、148、198 行已经有完全相同警告，当时尚无新增 runtime compiler 路径。
- 工作版 Hazuki 编译器的静态 import table 仅包含 `SceLibKernel`（22 函数，库 NID CAE9ACE6）与 `SceRtcUser`（5 函数，库 NID 6704D3C6），不导入 SceLibm。
- r3 `knife.velf::.sceImport.rodata` 的 20 个库名也没有 SceLibm。

所以不能将 `SceLibm not found` 直接解释为这次 shader 没编译的原因，也不能据此许诺加一个 libm 即启用 fetch。该警告若要修复，应另定位产生日志的模块与实际未解析函数；本次没有这份证据。

## Hazuki 绑定协议与 mofa 的差异

Hazuki `port/CMakeLists.txt:113` 链接的是 `SceShaccCg_stub` 普通导入库，经 `vitaGL/source/gxm.c:307` 的 `start_shader_compiler` 调 vitaShaRK 的 `shark_init`。它会逐路径报告载入结果。mofa 使用 `SceShaccCg_stub_weak`，并增加 module name 与 ARM import trampoline 真实目标归属检查。

这不等于已证明 weak 导入有错，但也不能声称两边绑定协议完全相同。补随包依赖后，若出现 exports rejected，必须确认实际硬件导入跳板形式／弱导入重绑定，而不是删除检查后冒险调用空 stub。当前捕获没有提供成功加载的 UID 或 exports rejection 细节，所以不能把这一点误判成已确认根因。

另一个已确认 A/B 开关问题：`gxm_backend.c:420` 以 `(1<<0)|(1<<1)` 决定是否初始化 runtime shader，这两位实际是 shared-atlas／gxt-ready；应随 group-fused／ps-fetch（bits 3／4）请求初始化。本次 mask=4095 包含前两位，因此该错误 **不是本次 capability=0 的原因**，但只开启 ps-fetch 或 group-fused 的单项 profile 会错误跳过初始化，必须在下一实施修正。

## 下一轮优先级

1. 接入现有正确 SHA 的 Hazuki 随包编译器，完善每路径／绑定／缓存阶段日志，并修正初始化开关位。
2. 以编译器内部 FRAGCOLOR 合同实现实际高频 PsOp 单入口，成功后再扩展需要的组合；保留原 snapshot 和独立开关。
3. 组融合获得真实 compile＋metadata＋patch 能力后再计入启用数，继续对照用户实机 RGB／alpha／gamma。
4. 用用户下一份日志核对 capability=true、copy_ps_snapshot／copy_group_scratch／draw_mask_alpha 的实际减少，再报告与冻结基线的耗时收益。

目前可以确认“依赖打包未接入”和“输入 semantic 尚未依据真实编译器修正”这两个明确缺口，不能以 implemented_runtime_gated 标签代替核心优化已经生效的事实。
