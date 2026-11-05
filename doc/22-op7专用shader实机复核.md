# 2026-11-05：op7 专用 shader 实机复核

本次收到 `stage-effect-r3-heavy-ps7-20261105` 的实机 VitaDeck 日志。**目标最重窗口从约 6.08 FPS 改善到 7.03 FPS，有收益，但仍未达到流畅或 20 FPS。** 本次保留 shader29，没有修改游戏代码、EBOOT、VPK 或素材，没有推送远端。

## 捕获和路径身份

- 日志 1,552,264 字节，SHA-256：`01640f55f0c07eea0be3a30abad2d0768c3fc1d88b42901c4f7d8e1a1a61420f`。
- 新旧运行均为 CPU 500 MHz、bus/GPU 222 MHz，scene/composite 960×540，同一素材清单与 native UNORM profile。
- 新 Cg 源身份 `dab0c1fb6b8b521d…` 与交付收据匹配。shader29 实机编译无诊断错误、metadata `ok=1`，程序 2,884 字节，primary 232；generic17 为 5,724 字节、primary 570。静态 primary 数减少不等于每像素执行次数或 GPU 耗时同比下降。
- 启动能力行 `ps_fetch7_29=1`、`fetch7_patch_status=0`。最后恢复的累计 `ps_op_7=3279`、`draw_ps7=3279`；目标窗口内为165/165，专用程序确实执行。

## 最重窗口对照

取 scene38、PC1246 末端的全重绘窗口，r2 frame8077→8108 与 r3 frame3887→3923。PC 是窗口末值，运行输入和动画阶段并非逐帧同步；以下是跨运行观测，不是同一个 EBOOT 开关 A/B 或像素等价证明。

时间使用 `frame_classes.redraw` 已结算帧口径。`sample.render_us` 的边界略有差别，r3 为140.2677 ms；分类口径为140.27325 ms，统一取后者作对照。

| 指标 | r2 通用路径 | r3 op7 专用 | 变化 |
|---|---:|---:|---:|
| 窗口 FPS | 6.076 | 7.034 | +15.8% |
| 重绘 / 复用帧 | 31 / 0 | 36 / 0 | 两版全重绘 |
| 每帧 work | 164.114 ms | 141.751 ms | -13.6% |
| 每帧 render | 162.652 ms | 140.273 ms | -22.379 ms / -13.8% |
| 每帧 batch 等待 | 147.478 ms | 126.576 ms | -20.902 ms / -14.2% |
| 每帧 EndScene 调用耗时 | 0.409 ms | 0.415 ms | 没有明显改善 |
| 每帧渲染 scene/pass | 5 | 5 | 相同 |
| 每帧 group fused | 2 | 2 | 相同 |
| 每帧 PS draw | 11.452 | 11.444 | 基本相同 |
| 每帧 effect draw 总数 | 26.387 | 26.306 | 基本相同 |
| 每帧 op7 | 4.548 | 4.583 | 新版略多；全部用29 |
| 窗口资源 miss / 上传字节 | 2 / 1,470,928 | 2 / 1,470,928 | 相同 |

目标窗口不是靠复用帧或明显少画获得更高FPS。资源 miss、上传量、每帧组和场景数量也相当，支持 shader 专用化减少 GPU 工作量的解释。精确收益仍受动画相位、遮罩与覆盖面积、GPU队列边界影响；不能把所有22.38 ms都当作独立shader GPU计时。

第二个同末PC3017的全重绘窗口也观察到改善：r2 frame14441为96重绘/0复用，19.131 FPS，render51.2904 ms；r3 frame7842为109重绘/0复用，21.726 FPS，render45.0589 ms，下降约12.15%。新窗口104次op7全部使用29。此段其余draw/alter负载略有不同，作为辅助证据，可信度低于PC1246；不能因此声称最重场景已超过20 FPS。

余下 `batch_wait` 约126.58 ms，占约141.75 ms work 的89%。这是CPU等待GPU进度的时间，不是CPU持续做126 ms计算；也不能在保证正确性的前提下直接删掉栅栏。`draw_ps7.us` 只计提交侧耗时，不能拿它作为GPU像素着色时间。

本窗口 op1 67次、op6 36次，仍使用通用程序；其余 op13/op19 和完整材质源准备、组像素/遮罩合成也继续存在。下一步候选是对这些实际路径做进一步专用化，或在语义允许时减少组的无效像素覆盖。需先证明各自触发条件、覆盖范围与画面语义，不能由本次数字推算必达20 FPS。

## 数据完整性与稳定性边界

收到47个sample、51个frame_classes、54个audit，末值报告丢4批，另有6条坏JSON、5个不完整分片payload、frame3572仅28/39个bank。不能以此断言全程无错误。目标比较两端3887/3923的op7、draw_ps7、draw总数、scene/group和batch等待均有直接bank，分类窗口也完整；本段差分不依赖缺失3572的旧值。

已收到资源事件为170 pending / 170 ready，无failed记录；`worker_failed=0`、设备音频underrun=0，最低CDRAM空闲11 MiB。这不代表所有未捕获时段无错误，也不是长期稳定性验收。

当前通用分析脚本 `tools/read-vita-performance.py` 因缺少 `vita_audit_log` 模块无法启动。本次用已有原始JSON/分片重组流程并逐项核验端点bank，未为此改动引擎。完整冻结日志、重组结果和比较脚本在本机 `.work/vita/effect-r3-evaluation/`，紧凑证据见 [27-stage-effect-r3-hardware.json](evidence/27-stage-effect-r3-hardware.json)。
