# 着色器优化实现与能力边界

本轮实现 PS 目标颜色读取、组融合的运行时编译和缓存，并保留原 destination snapshot／scratch 路径。状态是 **implemented_runtime_gated**：代码已集成，是否能采用新片段程序取决于设备现有编译器产生的程序是否通过完整检查。尚无新着色器的真机像素或耗时结果。不能将缓存命中、编译成功或 native flag 验证等同于视觉等价，更不能据此填写提速数值。

## 已核对的输入证据

- [原版对照报告](../09-Artemis渲染与MJA对照分析.md)及 `.work/render-audit/native-gxp-comparison.json` 保留了 Hazuki 193 个原生 GXP 与参考 eboot 的字节比对。192 个片段程序的 flags 为 `0x001828c1`，同时带 native colour 与 fragment colour read 位；五个按完整 SHA 匹配的 USSE 样本有 `VPCKF32U8 ... o0...` 读取目标颜色。它们证明平台存在该机制，不证明 Artemis 的 192 个程序与本项目的 24 个 PsOp 一一等价。本轮没有移植这些第三方 GXP，也没有修改其指令字节。
- Hazuki 的 `platform/vita/third_party/borealis/library/include/borealis/extern/nanovg/nanovg_gxm_utils.h:640` 已使用 Cg `__nativecolor __regformat unsigned char4` 声明原生输出。它证明该编译器扩展有实际代码依据；目标颜色输入的准确签名仍需本机编译器输出确认。
- 本项目现有 `knife_quad_f_gxp` 数组是 296 字节，GXP program size 为 293 字节，SHA-256 为 `b05dd6838e58d67c3ec97a6e70eafb50e089bd5cbd2cd8d95c94cb279df61784`。varyings 表中输出 type=1、components=4，即 HALF4。`gxm_backend.c::quad_program` 在 sRGB 目标上确实选择 `SCE_GXM_OUTPUT_REGISTER_FORMAT_HALF4`。因此组路径的第一次 pixel→scratch copy 有 HALF4 输出边界，不能只模拟最后一次 alpha 的 UNORM8 量化。
- Hazuki 实际 vitaGL 的 `source/custom_shaders.c::disable_draw_attrib` 和 `source/ffp.c::setup_lighting_attributes` 使用 GXM stride=0 的常量属性。紧凑顶点流因此可复用已生成的 VS：32 字节 position／UV／F32 colour 流，加一个 stride=0 的零属性流。只对 haze／soft 的全部 20 个 F32 位模式均为零的顶点采用，其他顶点保持原 112 字节路径。

## PS 与组融合的实现

`effects.cg::ps_apply` 是 snapshot 与 fetch 两个入口共享的 24 个 PsOp 实现，原有零 alpha 早退、rule、haze、soft 和 group mask 分支都保留。新 fetch 的 native shader ID 为 17，普通 snapshot 仍为 14。Rust 必须先查询 native capability，只有可用时才能省略 snapshot；不能在省略 copy 后再拿旧 `fs_ps` 作为绘制回退。

运行时尝试三种 Cg native colour 参数声明。**这些签名尚未获得成功离线编译的 GXP 证据。** 候选仅在输出同时满足下面条件时可被接受：

1. GXP magic、program size、尾部 padding、指令、参数、名字及 varyings 的范围全部有效。
2. fragment、native colour 和 fragment colour read 位都存在；普通插值 `COLOR` 输入不满足 fragment colour read 检查。
3. 输出是 HALF4，采样器不存在 `tex1`，其余 sampler unit 与七个既有 uniform 的 F32 ABI 一致。
4. `sceGxmProgramCheck` 成功。

这证明被采用的二进制具有目标颜色读取机制，仍需用户真机检查 gamma、像素中心、透明边缘和重叠绘制。只成功解析源码或返回一个 GXP 不会开启 capability。

组融合仍使用 shader ID 15。新的运行时版本显式模拟 pixel copy 与 MaskAlpha 的 HALF／RGBA8 边界：RGB 经两次 sRGB encode→UNORM8→decode 回转；alpha 先经过 pixel copy 的 HALF／UNORM8，再经过 mask 的 HALF 结果及乘积写回。原来的单次 `round(pixel.a*mask.a*255)` 不作为精确等价结论。Cg 中的 HALF 转换及设备的 gamma／blend 精度最终仍需真机对照，当前没有将数学近似或主机结果描述成 GPU 像素验收。

独立于编译器，PsOp=18、非 fill 的 NoAlpha／Opaque 快速路径直接复用现有 GXP：两者都输出 `sample_img.rgb * colour.rgb, 1`，该操作不使用最终 alpha 或 rule 权重，也不参与零 alpha 早退。其余 fetch 不可用组合继续使用 snapshot。组 capability 不可用时继续使用原 scratch。

## 编译器及缓存

实现文件为 `knife/vita/runtime_shaders.c`、`runtime_shaders.h`。首次初始化先读缓存；缺失、过期或损坏时才加载用户设备已有的编译器。查找顺序沿用实际 vitaGL 路径，并兼容现有 Artemis 数据目录：

1. `app0:/sce_module/libshacccg.suprx`
2. `ur0:/data/libshacccg.suprx`
3. `ur0:/data/external/libshacccg.suprx`
4. `ux0:/data/ArtemisVita/libshacccg.suprx`

不下载或分发该模块。`LoadStartModule` 返回成功后还必须通过 `GetModuleInfo` 的 `SceShaccCg` 名字检查，并核对六个编译函数的实际绑定。仅函数指针非空不足以确认 weak import 已解析。实现检查真实 ARM MOVW r12／MOVT r12／BX r12 或 B 跳板，目标地址必须属于刚加载的模块；未解析的 MVN r0,#0／BX lr stub 会被拒绝，然后继续保留已有渲染路径。

缓存路径：

```text
ux0:/data/knife-vita/shader-cache/effects-<64字符源码SHA256>-a2-s15.bin
ux0:/data/knife-vita/shader-cache/effects-<64字符源码SHA256>-a2-s17.bin
```

缓存 ABI 为 2。固定 96 字节头包含 magic、ABI、shader ID、候选版本、GXP 大小、源码 SHA-256 和完整 GXP 字节校验和。group 只接受 variant=0；fetch 只接受 variant=1..3。读取时重新进行所有 GXP／反射检查；编译后先写 `.tmp`，关闭成功后 rename。缓存失败不影响本次已成功编译的内存程序。新 Cg 的 SHA 变化会自然使用新名字；不扫描或删除用户其他缓存。

诊断全部使用 `sceClibPrintf`。设备上的文件写入仅限用户本轮明确允许的着色器缓存，不写性能日志。控制台 `[knife-shader]` 行分别报告 cache hit、cache rejected、compiled、metadata rejected、compiler exports rejected 及最终 capability。首启编译可能有一次启动成本，后续有效缓存直接读取。

`tools/generate-runtime-shaders.py` 将自写 Cg 嵌入 `shaders/generated/runtime_source.h`，并生成源码 SHA。`--check` 比较完整生成文本，拒绝 Cg 改动后未更新的头文件；构建配置已经接入该检查。旧 `generated/effects.h` 保留为原始回退字节，不伪装成新源码已经离线编译的产物。

## 本轮限定验证

- `runtime_shaders.c` 使用既有 Docker VitaSDK 通过 ARM C11、`-Wall -Wextra -Werror` 编译。
- 生产缓存／GXP 检查函数在 ASAN＋UBSAN 下通过 35 项检查，包括真实旧 group GXP 的正例，以及普通 COLOR 冒充 fetch、越界 size／参数／指令／varyings、坏名字／类型、过期 ABI／SHA、错误 variant、校验和不符、空文件和反射损坏等拒绝条件。测试源为 `knife/vita/tests/runtime_shader_cache_test.c`；没有在主机执行 GPU 指令。
- 本地记录：`.work/shader-compiler/runtime-shader-cache-test.log`、`.work/shader-compiler/implementation-evidence.json`；ARM 对象为 `.work/shader-compiler/runtime_shaders.arm.o`。
- 新 Cg SHA-256：`a587e72635e07fc6fd49905645e7518c6c65a2cc8a7d4ac44e871d13c50a18f1`。

最终收益按冻结的真机基线与后续用户日志比较：先核对 capability，再核对 PS snapshot／group scratch／MaskAlpha 是否按采用数量减少，最后比较同场景 work、render、等待及分位帧时。capability 未成立时须如实记为保留原路径，不将“计划存在”或“缓存存在”计作实测提升。
