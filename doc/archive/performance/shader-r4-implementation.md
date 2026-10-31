# r4：接入实际编译器与 FRAGCOLOR 热点路径

r3 日志中的 `group15=0, ps_fetch17=0` 证明两条路径未启用。r4 修复了依赖接入、输入 semantic、绑定方式及诊断缺口，新增 PsOp 19 专门入口。**本轮尚无新的设备 GXP、patch 或像素结果**；后续用户日志必须显示各阶段成功，才能将对应实际 copy 数减少计入收益。本轮没有运行 Vita3K，也没有通过修改 GXP 指令制造成功产物。

## 真正接入现有构建依赖

使用 Hazuki 已有工作文件：`platform/vita/sce_module/libshacccg.suprx`，1,776,523 字节，SHA-256：

```text
0c58b5d3e1cc47d9837b8c59ab6f3c299bfe7adf55a83f547d553f9fbb776552
```

Hazuki 原构建已将它放入 `app0:/sce_module/`。主构建现在按同一部署方式接入该文件。启动优先使用随包路径，再查原有三个全局路径。每个存在的候选先读取并核对完整 SHA，拒绝此前遇到的同名错误文件；这里没有下载新模块。

模块初始化不再依赖 weak import 是否重绑定，也不再猜测 ARM 跳板形式。正确 SHA 文件的 module-info offset 为 `0x276a40`；使用 Vita toolchain 的标准 0x20 字节 export 结构，从 `GetModuleInfo` 返回的实际映射段中读取已重定位的 NID／entry 表。模块名必须为 `SceShaccCg`，库 NID 必须为 `A05BBEBB`，所有指针和范围必须落在模块段内；7 个入口还逐一通过 `GetModuleIdByAddr` 归属检查，再构造函数指针。

该流程读取的 API 包括 InitializeCompileOptions、InitializeCallbackList、SetDefaultAllocator、CompileProgram、DestroyCompileOutput、ReleaseCompiler 和 GetVersionString。标准编译选项仍为 O3，禁用 fastmath、fastprecision、fastint。首次缺缓存时会有一次启动编译成本，完整缓存命中时不加载编译器。

## 使用编译器本身明确支持的入口

工作模块的只读静态诊断字符串明确要求：目标输入语义为 `FRAGCOLOR`，入口带 `__nativecolor`，输入与 COLOR 输出均带 `__regformat`，输入／输出尺寸一致。它们位于解压后第 0 段的 `0x296510`、`0x296560`、`0x2965b4`、`0x296620`。

r4 删除了 r3 的三种 COLOR 候选与重试循环，仅保留这个有编译器证据的声明合同：

```c
__nativecolor __regformat half4 fs_ps19_fetch(
    FragmentInput i,
    __regformat half4 destination : FRAGCOLOR) : COLOR
```

它是修正后的源码合同；静态诊断并不等于本次已成功编译。真正输出仍须通过 `NATIVECOLOR_USED|FRAGCOLOR_USED`、HALF4、无 `tex1` sampler、完整参数／指令／名字边界、uniform ABI 和 `sceGxmProgramCheck`。之后由 native backend 在实际目标格式上 patch，只有可用能力才允许 Rust 省掉 snapshot。

新增 shader ID 18 为 PsOp 19 专门路径；ID 17 继续覆盖其余全部 24-op 入口；group ID 15 不变。编译顺序是 **18 → 15 → 17**。r3 已测 95.22% PS 走 op19，因此热点不再执行完整 24-op 条件链。

`ps_source` 被 snapshot、通用 fetch 和 op19 fetch 共同使用，保留 group mask、HALF／RGBA8 中间转换、haze、soft、fill、noalpha、rule 与 opacity。op19 保留 `a<=0` 时返回整个原 RGBA 的早退，非零时使用原公式：

```text
D.rgb * D.a + a * (S - D.rgb * D.a), alpha = 1
```

没有把它改成忽略目标 alpha 的普通 source-over。通用 PsOp 的原数学及早退仍保留。group 继续使用上一轮实现的两次 sRGB／RGBA8 及 HALF 中间边界；它的实际 GPU 像素结果仍由用户检查，不能仅凭主机计算宣称精确等价。

## 缓存 ABI 3 与逐阶段控制台日志

新缓存名字绑定完整 source SHA 和 compiler SHA：

```text
ux0:/data/knife-vita/shader-cache/effects-<sourceSHA>-c<compilerSHA>-a3-s18.bin
```

同类文件分别有 s15、s17、s18。固定 160 字节头包含 cache ABI=3、entry ABI=1、shader ID、实际字节数、source SHA、compiler SHA 和完整 GXP 校验和。旧 ABI 2 文件不会被误用。损坏的新缓存会明确拒绝并移除该单个内部文件；新程序以临时文件关闭后 rename 发布。写缓存失败仅影响后续复用，不会丢掉本次成功的内存程序。

诊断仍仅使用 `sceClibPrintf`，没有性能文件日志。`[knife-shader] stage=...` 现在包括：

- `initialize`：cache／entry ABI 和两种身份。
- `cache`：每 shader hit／miss／具体拒绝原因。
- `module_stat`、`module_identity`：每条路径的真实返回值、大小和 SHA。
- `module_load`、`module_info`、`export`：UID、启动返回值、模块名及每个真实 API 地址／归属。
- `compiler`：实际版本字符串、allocator 结果；失败同时保留 first_error 与 last_error，不能再让最后一条 ENOENT 遮盖前面的错误。
- `compile`、`diagnostic`、`metadata`：入口、启动编译耗时、输出字节、完整 native flags 和反射拒绝原因。
- `cache_write`、`compiler_release`、`programs`：缓存与释放结果，以及三种内存程序是否存在。

`programs` 仍只是编译／缓存阶段，最终是否采用应看 native 的 shader capabilities／patch 状态及实际 copy 计数。失败或不适用的组合继续使用原 snapshot／scratch。

## 本轮限定检查与证据边界

- 生产 `runtime_shaders.c` 通过 Docker VitaSDK 的 ARM C11、`-Wall -Wextra -Werror` 编译。
- 76 项 ASAN＋UBSAN 主机检查通过：SHA-256 标准向量、实际 1.7 MB 编译器文件的完整 SHA、生产 export parser 对真实工作模块 7 个 NID／Thumb 地址的读取，以及越界／缺失／重复导出、源／编译器／ABI 缓存错配、损坏 GXP／反射、空文件等拒绝路径。
- 主机没有执行 Sony 模块函数或 GPU 指令；测试正例使用已有 legacy group GXP 仅检查文件／参数格式，没有伪造新 fetch GXP。
- 具体来源、对象及源文件 SHA 见 [shader-r4-evidence.json](../../performance/shader-r4-evidence.json)。测试记录为 `.work/shader-compiler/runtime-shader-cache-r4-test.log`，ARM 对象为 `.work/shader-compiler/runtime_shaders-r4.arm.o`。
- `tools/generate-runtime-shaders.py --check` 已通过，内嵌 Cg 与源码 SHA 相符；新增导出及 SHA 头已纳入 native 构建依赖。

用户下一轮对照应先确认 module、compile、metadata、patch 和 capability 均成功，再核对 op19 使用专门路径的数量、snapshot／scratch／MaskAlpha 减少量，以及重画帧的 work、EndScene、batch wait。没有拿 r3 已生效的其他优化替这两个核心路径填写收益。
