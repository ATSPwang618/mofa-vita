# GPU 回放诊断（默认关闭）

Vita 游戏启动后读取 `ux0:data/WitchontheHolyNight/gpu-replay.txt` 一次。文件内容可以是 `current`，也可以是 `scene pc` 两个十进制数字。满足条件的正常生产帧会先生成一次 Stage draw list，然后冻结这份列表；回放不会再次运行 `Stage::draws`、故事脚本、动画时钟、UI 或存档。

回放在同一个 EBOOT 内执行固定的 A/B/B/A 顺序。A 使用原始 `ps-plain-material` 位，B 在相同的其他优化位上加上该位。每个区块先预热 30 帧，再记录 120 帧。每次提交都等待现有 `movie_gpu_fence` 完成，最后读取独立线性 RGBA8 Stage target。日志保留录制 CPU 时间、提交至 GPU 完成时间和读回时间；每区块保留 120 个提交完成样本，便于离线计算总体分位数。

回放报告包含冻结来源（scene、PC、update frame、物理尺寸、纹理来源数和 draw recipe signature）、plain material 输入候选数、每个区块的 RGBA8 字节比较、FNV-1a 摘要、执行工作计数以及时延样本。原始像素不会写入普通诊断文件，避免诊断本身占用数 MiB 的持久空间。console 传输仍通过现有有界 `audit_chunk` 协议。

离线分析器：

```text
python tools/vita_gpu_replay.py capture.log --environment hardware
```

`tools/test_vita_gpu_replay.py` 是标准库测试夹具；夹具只验证日志解析、分块重组、门槛和拒绝路径，不模拟 GPU。分析器要求完整 header、四个 ABBA 区块、每区块 120 个提交完成样本、非零 plain material 输入候选和每区块增长的 executed work。它将两段 A 样本合并为 baseline，将两段 B 样本合并为 candidate，使用 nearest-rank p95/p99：

* candidate median 至少比 baseline 低 5%；
* candidate p95 不得比 baseline 高 5% 以上；
* candidate p99 不得比 baseline 高 10% 以上；
* 所有区块的 `differing_bytes`、`differing_pixels`、channel delta 必须为零。

任一 RGBA8 差异或完整样本下的门槛失败都会得到 `fail`。缺失 header、区块、样本、来源、分块记录或工作计数得到 `inconclusive`。emulator 和 unspecified 环境即使满足数值门槛也只能得到 `inconclusive`。输出固定写入 `default_promotion_applied=false`；该工具不会修改优化配置，也不会启用生产 gate。

当前报告的来源范围是“触发时当前生产 Stage 的冻结输入”。它不证明 scene 38 自动启动成功，不覆盖整个故事、其他 Stage、UI、菜单、电影、音频、存档、纹理解码或全局帧 pacing。分组目标若依赖上一帧未清除像素会被入口拒绝；movie、可写 target 和未验证的 eager source 也会被拒绝。独立 replay target、cache 和 production target identity guard 用于保证诊断结束后生产状态仍可复用。

## r6 full-material comparison

With `ps-full-material` selected, replay toggles only bit37 (A off, B on), preserving other options; otherwise it keeps the prior plain comparison. `full_input_candidates` includes Stage, Pixel, Mask and Composite OP1/6/13. Both B blocks require successful54/55/56 deltas; A must have none. Missing/unavailable programs or incomplete counters are inconclusive. The request file is read once per startup and is not deleted automatically. Remove it to stop replay on later launches.

## r7全部OP对照

启用 `ps-all-operations` 时，优先只翻转位38，A关闭、B开启，位37及其他配置不变。Header的 `all_input_candidates` 是24项OP数组，包含Stage/Pixel/Mask/Composite；B两个区块均须实际执行64–87之一，A不得出现这些程序的执行增量。程序计数范围扩为0–127。缺能力、未命中或程序计数被省略时均为inconclusive，不能用配置开启或静态指令数代替GPU验证。

`gpu-replay.txt` 仍按每次启动读取一次，运行后不会自动删除。若要正常游玩，移走该请求文件。请求可为 `current` 或指定scene/pc；当前冻结回放只覆盖触发相位，不能代表全游戏画面验收。离线Shader全局覆盖本身不依赖该文件。
