# r5 全游戏 GPU 工作量优化

本轮基线是本地 `perf/render` 的 `24b0467`。交付标签为
`stage-effect-r5-material-20261105`，保持 960×540 演出尺寸、现有资源身份、混合顺序、输入、音频和存档格式。

## 已实现

- 优化选项和 shader 能力使用独立的 `u64` 掩码，保留原 bit0–29；`ps-plain-material`、`group-bounds`、`half-color`、`tiled-targets`、`blur-weights` 和 `rt-scenes-2/4` 是独立开关，2/4 互斥，容量失败回退 1。
- 执行观测只在 native draw 成功后结算。记录 OP、实际程序、材质功能、pass 角色、源 draw 数和按最终裁剪三角形估算的 `estimated_covered_pixels`。固定 128 项材质表、overflow 和 top 项摘要限制在每个日志窗口 4 KiB 内；这不是硬件 fragment 计数或 GPU 时间。
- OP1 Add 使用程序 30，OP6 Dodge 使用程序 31；OP13 的已有程序 21 扩大到相同的 plain Sprite/Fill/NoAlpha 合同。Haze、rule、soft clip、mask 或未知参数保持通用路径。程序不可用时回退 native fetch17，再回退 snapshot PS14。
- Group 覆盖状态随真实纹理 owner、内容版本、cache/ring 和 encoder 提交传播。只裁最终 Composite；producer、clear、copy 和尾部写入仍完整执行。Unknown 保持全屏。裁剪证明在提交前失效时恢复原始全屏顶点和 UV；Empty 状态记录但本轮不省略最终写入。
- Blur 权重实验使用程序 53、`n≤16`、最多 16 组和 4 KiB 载荷，保留原核宽、顺序、三种模式和 FP32 累加。
- Tiled 实验只探测 data-null 的 960×540 RGBA8 离屏目标，按完整 tile padding 计费并使用线性 staging 回读；生产路径仍回退 Linear。Scene 容量 2/4 只作用于离屏目标，驱动查询、预算或创建失败完整清理并回退 1，显示目标保持容量 1。
- 生产 MZX/运动表盘点覆盖 249 个场景、493,213 条解析命令、8,279 个运动 ID 和 16,163 个材质身份；表达式、动态分支和继承/HOLD 状态保持未知，不以静态盘点替代运行时路径。

## 固定回放与默认配置

设备数据根中放置 `gpu-replay.txt` 可请求一次当前生产 Stage 捕获：`current` 在当前场景/PC 处触发，或写入 `scene pc` 指定门槛。回放冻结普通 `Stage::draws` 的结果，不推进剧情、UI、动画时钟，不写存档；使用实际素材和正式 `render_stage_draws`，以 ABBA 顺序各运行 30 帧预热和 120 帧测量。输出 `gpu_replay` JSON 行，分别给出 CPU 录制、提交到 GPU 完成、回读耗时、p50/p95/p99、RGBA8 差异、执行覆盖和程序选择。

回放拒绝需要前一帧 Group 像素、电影/可写纹理或未验证 eager source 的样本。它是当前 Stage 的诊断，不是全剧情等价证明；分配独立目标时可能触发现有可选纹理回收，回放结束前会完成 GPU drain 并恢复正式 Game/Renderer 所有者。

Half 和 Tiled 仍是候选/探针路径，没有 RGBA8 实机像素差分就不会进入普通 draw。Blur 权重也默认关闭。r5 候选默认通过构建配置启用 plain 材质路径，现场可在 `optimization-profile.txt` 中完整覆盖；日志启动行带 build tag、掩码、shader 能力和素材身份。

## 验证边界

已通过 host 渲染 155 项、`knife` 118 项、基础 crate/API 测试，native effects 输入 933 cases/2,799 checks，实验输入 615 checks，权重上传 22 checks，GXM 容量与 tiled 边界 47 cases/6,659 checks，以及 ARM Vita cargo check、VPK `KNIF00540`/`ATTRIBUTE2=12` 检查。生产 shader 的 CPU 数学回归包含 691,526 个实际 Cg body 向量和独立锚点。

这些结果证明接口、回退和资源边界；尚未证明 Vita 实机 GPU 像素一致性、scene38 的 20 FPS 目标或 Half/Tiled 的运行时收益。只有固定回放的 RGBA8 一致、命中样本中位数下降至少 5%、p95/p99 约束和内存预算同时满足时，候选才可以升级为默认路径。
