# r6 全游戏完整材质专用 Shader

版本标签：`stage-effect-r6-full-material-20261105`。本轮在 r5 上扩大专用程序的材质覆盖，不改变 960×540、资源尺寸、混合顺序、RGBA8 量化、存档、音频及输入。

这是r6实现阶段记录，未单独交付r6包。后续已扩展为[r7全部24种操作及PC离线GXP](33-global-offline-shaders.md)，当前ARM/VPK身份以r7构建收据和dist版本说明为准。

## 全局选路

r5 已按材质条件作用于所有场景，但 OP1/6/13 的简单路径遇到 Haze、rule、soft clip 或 group source 会回到通用程序17。r6 新增完整源处理入口：

| OP | 简单材质优先 | 完整源处理候选 | 回退 |
|---|---|---|---|
| 1 Add | 30 | 54 `fs_ps1_full_fetch` | 17 → 14 |
| 6 Dodge | 31 | 55 `fs_ps6_full_fetch` | 17 → 14 |
| 13 Overlay | 21 | 56 `fs_ps13_full_fetch` | 17 → 14 |
| 7 DodgeAlpha | 保留既有路径 | 29，原有实现 | 17 → 14 |

54/55/56 共用通用程序的 `ps_source`，保留 Haze、soft clip、rule、Group mask、Fill 和 NoAlpha 的原始处理，只删除其它 OP 的选择分支。普通 Stage、Group Pixel/Mask 中的 PS 绘制、Group 最终 Composite 及 scratch 回退均接入。没有 scene、PC、图片名或图层编号的特例。有限的三个入口不会按效果排列生成大量变体。

简单路径仍优先：开启完整候选不会把本可使用30/31/21的绘制改成较复杂的54/55/56。完整源路径也不会把mask254当作255。未知/非法输入继续拒绝；编译、元数据、程序注册或 patch 失败，能力检查会让录制阶段选择旧路径。

## 配置与日志

独立开关为 `ps-full-material`，优化位37；它与程序编号及 shader capability 位54–56相互独立，均使用明确的64位值。`all` 保持 r5 基线，不自动纳入尚未通过实机验收的完整候选；本轮候选包显式使用：

```text
all ps-full-material
```

数据根 `ux0:data/WitchontheHolyNight/optimization-profile.txt` 完整覆盖编译默认值。已有文件若只有 `all`，新候选不会启用。随包提供同名小文件，需要覆盖到数据根，不放在 assets 内。关闭本轮候选可写 `all`，重新启动后生效。

启动版本和能力日志确认包身份与54/55/56是否准备成功；`gpu_work.program_counts` 的54/55/56增量才证明实际执行，配置开启不代表编译成功或每帧命中。保留原有 VitaDeck 控制台输出与 `perf-enable.txt` 覆盖规则。

## 固定回放

当有效配置启用位37时，现有 Stage ABBA 回放优先只翻转该位，其余位固定；否则继续测试旧 plain 位30。报告写明 `toggle`、`toggle_mask`、`full_input_candidates` 和四路候选范围。full 两个B区块必须分别实际执行54/55/56之一；未命中、缺能力或计数不完整均不能判定有收益。参见 [GPU回放诊断](31-GPU回放诊断.md)。

## 验证范围

已通过：渲染161项；完整源数学421,205组、21个独立RGBA8锚点和11个错误实现对照；旧简单路径691,526组；新增native输入1,242项、旧plain输入615项；native effects 933 cases/2,799 checks；缓存14 cases/439 checks；GXM边界48 cases/6,750 checks；离线回放分析13项。

数学回归使用实际Cg函数体，但纹理输入和部分Haze/soft输出由host夹具控制；native缓存夹具不执行GXP。这些验证不等于实机shader编译、像素等价或FPS提升。r6未单独打包交付；后续ARM验证及构建收据见r7。尚无r6实机验收，不承诺全游戏固定收益或最重场景达到20 FPS。

当前ELF/map和构建收据保存在 `.work/vita/game-release-540p/`；上一轮小体积程序回退保存在 `.work/vita/rollback/stage-effect-r5-material-20261105/`。不复制游戏资源，所有改动仅在本地。
