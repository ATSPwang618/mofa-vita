# r3 GPU 只读分析

没有修改运行时代码，没有启动 Docker、Vita3K 或真机工具。数字由 `.work/performance/analyze-r3-gpu.py` 和 `write-r3-gpu-findings.py` 从给定 stdout 重组并计算；完整分子、分母与 counter delta 在 `r3-gpu-findings.json`。使用固定 r2 capture，而不是当前实现代替旧基线。

## 核心判断

1. 本次最高收益路径尚未实际启用：`[knife-shader] compiler unavailable error=80010002`，随后 `shader_capabilities group15=0/ps_fetch17=0`。因此 PS 继续 snapshot，组继续 scratch/MaskAlpha；不能把 optimization_mask=4095 当作有效路径覆盖率。
2. 安全 arena/异步提交生效，但主要 driver 阻塞从 `Finish` 搬到 `sceGxmEndScene`。PC246 窗 EndScene=4,006,243 us/46=87.092 ms/帧，占 work 80.38%；batch 等待=7.066 ms/帧。当前 named wait 汇总排除 EndScene，慢帧 work182.453 ms却可报告wait29 us，不能据此得出同步基本消失。
3. PS op19 支配负载，优先解决它的目标颜色读取/静态原生程序通道，比先扩展稀有24种op覆盖更有收益依据。保留其他op snapshot fallback；不能直接改成普通 alpha blend。

## 窗口分母

|版本|frame范围|帧数|scene/PC|work ms/帧|stage重画率|scene/帧|EndScene ms/帧|batch wait ms/帧|(End+batch) ms/scene|
|---|---|---:|---|---:|---:|---:|---:|---:|---:|
|r2|2444–2467|24|1/246|208.331|100.00%|39.000|3.073|0.000|0.07878|
|r3|2171–2216|46|1/246|108.354|58.70%|23.196|87.092|7.066|4.05931|
|r3|7423–7527|105|2/80|47.663|100.00%|15.010|40.563|0.000|2.70249|
|r3|7528–7624|97|2/90|51.508|100.00%|15.010|39.239|0.000|2.61417|

r3 PC206/212连续窗也只有约62–65%重画；没有找到 PS≈8.5、group=3 且100%重画的 r3 窗。scene2/PC80、90 是相邻稳定全重画窗口，PS=3/group=1/约4 copy每帧；PC与负载都和 r2 最后窗口不同，只用于分解 r3 核心耗时，不能称同负载 A/B。旧实现把 GPU 工作拦在显式Finish，新实现把背压暴露在EndScene，因此单独比较跨版本EndScene耗时也不是有效回归判断。

## 同PC246结构对照，不能忽略stage reuse

- r2 frame2444–2467：24帧、24重画、204PS snapshot（8.5/重画）、72group scratch（3/重画）、24stage→scene copy、共300 GPU copy、936scene。
- r3 frame2171–2216：46帧、27重画+19reuse、231PS snapshot（8.5556/重画）、81group scratch（3/重画）、0stage→scene copy、共312 GPU copy、1067scene。copy仍然大量存在；direct_stage 确实删除了中转，但PS/组链没有被融合。
- r3 work108.354 vs r2 208.331 ms/帧的表面差值为99.977 ms，混入了41.30% stage cache hits、不同文本/进度、不同提交等待位置。不能把这99.977 ms分摊成各feature收益。
- r3全部 named wait + EndScene = 95.430 ms/帧，占 work 88.072%。该和不叠加GPU copy、draw_total、batch_capacity_drain等父scope。
- 此窗还有一次仅589 B的纹理更新：CPU memcpy16 us，前置finish_texture_update=57,001 us，transfer wait37 us。这是GPU仍使用该纹理时的安全覆写等待，不能记为“589 B复制花了57 ms”。其平均影响约1.239 ms/帧，小于EndScene主项。

## Native代码对应

- `gxm_backend.c::perf_end_scene` 的计时直接包 `sceGxmEndScene`，`effects_backend.inc::kv_gxm_end_target` 与 present 都经过它。日志只证明CPU在该driver调用内停留，不能区分GPU执行、PB/SDK队列回收或具体驱动内部等待。
- `gpu_lifetime.inc::scene_flags` 给每个scene SET_DEPENDENCY，第二个起再加 VERTEX_WAIT_FOR_DEPENDENCY；counter scene_gpu_dependency累计48,146，与Begin/End scene总数完全一致。此实现保守串联全部scene，arena生命周期已安全，但独立target之间也被这条global GPU依赖串联。后续减少依赖必须使用真实纹理读写/跨队列图，不能直接清掉flag。
- `wait_submitted` 的真实 NotificationWait 计为batch_slot_wait；r3全段1,666次、58,085,636 us。PC246仅6次、325,046 us。它不能解释该窗4,006,243 us EndScene成本。
- `batch_capacity_drain` 全程未出现；无证据显示4 MiB槽容量溢出导致重场景停顿。不应优先扩大arena/slot数量猜测解决EndScene。
- 销毁退休有101次、86,030,016 B slice回收；pool命中94次。它们是结构计数，不足以给出其单项FPS收益。

## Compact stream实际收益边界

全段compact累计53,322 calls、10,355,328 B，约7.9745% effect draw采用compact。按等价112B输入计数465,489,024 B，实际native vertex写439,600,704 B，少25,888,320 B（5.56153%）；差值严格等于compact bytes×(112−32)/32。PC246窗相应减少368,640 B（6.97484%）。

这是直接测得的字节改善。全段vertex_copy累计1,556,322 us；PC246仅0.3768 ms/帧，占work0.3478%，无法解释当前约100ms的render主项。独立compact_stream启动行没捕获，但实际counter足以证明compact在用。没有matched A/B，不能把字节比例当作耗时/帧率比例。

## PS分布与op19下一步

r3累计执行PS=5,920，sum(ps_op_*)恰好等于native draw_ps：op19=5,637（95.2196%），op13=159（2.6858%），op1=71（1.1993%），op11=53（0.8953%）；其余op未出现。PC246窗231个PS全是op19。

`effects.cg::ps_apply` op19是 `mix(D.rgb * D.a, S.rgb, a)`，输出alpha=1；a<=0时完整保留D RGBA。普通alpha blend只实现 `D.rgb*(1-a)+S.rgb*a`，缺少D.a且alpha规则不同。除非目标alpha恒1有逐目标/覆盖区域的有效证明，否则不能替换。优先获取/生成并验证op19专用native目标COLOR程序，同时保留mask/rule/fill/gamma路径的fallback；现有24-op通用snapshot程序继续用于其他op。具体可删除的copy/scene数可以结构预测，真实毫秒收益仍需下一份实机日志。

## 数据完整性

父代理负责完整capture身份和全程汇总。这里选取的四个表格窗口均具有相邻完整audit、可重组timing记录；frame7715 audit不完整及其附近缺失的sample未用于分窗口性能结论。全程op/compact数字取最后frame8993完整累计bank，因此不依赖中间窗口丢片。
