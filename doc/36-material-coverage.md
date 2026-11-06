# r10：扩展全游戏材质组合覆盖

版本：`stage-effect-r10-material-coverage-20261106`。基于本地`perf/render`提交`248ea872c0d7d7b8d0d0a1e74d3685b377a7e563`。修改留在本地，未提交、未推送。

本轮目标是让更多重场景绘制命中精简程序，从而降低每像素计算量、改善最低帧数。选路只看准备后的材质、混合操作和程序能力；没有章节、PC、图片名或图层编号特例。960×540、现有素材精度、混合顺序、存档、音频和输入保持原方案。

## 新增17个有限入口

| ID | 材质组合 | 相对旧程序的静态主指令数 |
|---|---|---|
| 88 / 89 | OP1 Fill / NoAlpha | 26 → 18 / 18 |
| 90 / 91 | OP6 Fill / NoAlpha | 36 → 28 / 31 |
| 92 / 93 | OP7 Fill / NoAlpha | 36 → 27 / 25 |
| 94 / 95 | OP13 Fill / NoAlpha | 42 → 34 / 36 |
| 96–99 | OP1 / 6 / 7 / 13，仅Haze及预乘标记 | 228 / 238 / 233 / 244 → 75 / 86 / 81 / 92 |
| 100–104 | OP3 / 4 / 5 / 11 / 14，plain Sprite / Fill / NoAlpha | 227 / 229 / 237 / 241 / 246 → 29 / 31 / 40 / 44 / 49 |

静态指令数不能换算为FPS增幅。尤其plain OP1/6/7/13已在r9精简，新增88–95主要去掉Fill/NoAlpha动态判断；它们不会让所有现有绘制再次获得数倍收益。96–104的潜力更大，但必须在实际演出中命中。

- 88–95直接从当前基线函数生成，保留各自Half或FP32精度，只固定两个材质标志。Fill仍读取纹理透明度；NoAlpha只强制源alpha。
- 96–99保留原Haze坐标计算、预乘处理及之后的源色计算顺序，使用FP32；没有规则、软裁剪或组遮罩时才选用。
- 100–104仅接替对应完整OP程序。更早已有的简单Sprite专用路径继续优先，例如OP11原ID20。
- 现有OP7普通Sprite的ID48、相邻双层融合ID57保留。新增材质选路不把Fill/NoAlpha放进该融合合同。
- 不满足条件的rule、soft clip、group mask及其他复杂材质保留原完整处理；不会为了“全覆盖”删除效果。

运行时C输入验证、程序元数据、离线入口及Rust选路同步更新。新增程序先注册、patch，再录制绘制；能力位范围由64–87扩到64–104，仍在原128位能力集合内。静态成本不低于对应基线、程序校验失败或patch失败，只关闭该项并回退。

## 离线程序与内存

PC实际编译82个GXP，合计125,292字节，较r9增加12,776字节。程序源码SHA-256：`7e0edbb8e7ae13f22aa1a2d989787ef92b5f5c72ccf51db0d559fdaccd4c30c2`。生成器`tools/generate-material-variants.py`与原运算生成器共享公式来源。

ARM程序句柄表新增5,644字节；纹理、离屏目标、顶点环和缓存预算没有扩张。新增程序的驱动patch内存尚未实机测量，不能把GXP文件大小视为全部运行内存增量。新增诊断复用既有固定容量统计；只有成功执行的命令计数。

## 默认开启、日志与单项对照

`hardware-test`新增bit41 `ps-material-variants`，默认mask=`0x3cfdfffffff`。稳定`all`不纳入本轮尚未验收实验。沿用r9其余开关，sparse-coverage仍关闭。

默认VitaDeck日志开启并打印版本。新增启动记录`material_variant_capabilities`包含selected、ready_mask与table_growth_from_r9；17项均可用时ready_mask=131071。`material_variant_cost`说明各程序成本准入。实际使用须看`gpu_work.program_counts`中的88–104，ready不等于执行、更不等于性能已提升。

设备数据根`ux0:data/WitchontheHolyNight/optimization-profile.txt`：

```text
hardware-test
```

关闭本轮17项并恢复r9选路组合：

```text
hardware-test -ps-material-variants
```

`perf-enable.txt`显式为0仍优先关闭日志；交付目录的该文件为1。旧标准配置自动展开规则保持；其他自定义profile仍完整覆盖默认，测试时应检查有效mask。不要只更新EBOOT后假定旧自定义文件自动包含新位。

固定GPU回放默认只翻转bit41，ABBA每块预热30帧、测120帧，其余状态不变。分析器要求B块实际执行新增程序、A块无新增程序、RGBA8差分为零；未命中时报告inconclusive。它无需重新通关，但一个冻结相位的结果也不能代表全游戏。

## 验证及边界

- Rust串行回归：游戏122项、渲染169项、性能配置8项通过。测试使用共享全局优化配置，串行执行避免测试间干扰。
- 原生GXM注册/提交/释放/拒绝：68 cases、8,653 checks通过；包含新程序能力范围、成本拒绝、单项patch失败及关闭开关后拒绝提交。缓存55 cases、3,090 checks通过。
- 原生输入933 cases、2,799 checks及五种反向变异控制通过；新增材质真实C输入993 checks通过。既有plain/half 746、完整源11,144和Blur 22 checks通过。
- 340,000组作者源码CPU数学差分通过；保留Half局部舍入，控制采样输入，反向测试可检出丢弃Fill纹理alpha和Haze预乘。该测试不能代替真实GPU的插值、采样和舍入验证。
- 回放分析15项、配置收据4项通过。82个PC编译GXP通过生产元数据/身份校验；host ProgramCheck为模拟。
- ARM release及VPK检查通过，KNIF00540 / ATTRIBUTE2=12，21个引擎安装条目。无需转换或重传游戏素材。

**状态：已实现、已通过host/native/ARM/VPK检查；r10尚未实机验证帧率和GXM逐像素一致性。** 用户报告r9明显提速是扩展此路线的依据，但不作为r10增益证明。下一份日志应优先比较同一重演出阶段的重绘耗时、p95/p99、实际新增程序命中与内存失败情况，不把跨章节平均FPS作为最低帧数结论。

机器证据：[36-material-coverage.json](evidence/36-material-coverage.json)。

## 交付身份

- `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/eboot.bin`：10,148,863 字节；SHA-256 `b438f1fa0678ebfe4608fc243aae9396509a31c179f26957face0011297b4a17`。
- `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/knife-vita-540p.vpk`：7,265,529 字节；SHA-256 `78337235c66fc25ece3da78d7a332e7a613b1a30d9f6d9aa1e96c601b87b2e76`。
- `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/build-receipt-r10.json`：85,901 字节；SHA-256 `04b138005abe1afd896ecdb056568ddd6a3ece67f89bf3b649d92360f78c4b29`。

启动profile记录大小：[785, 810]字节，均低于900字节限制。独立EBOOT与VPK内EBOOT一致。ELF/map保存在`.work/vita/game-release-540p/`，r9小体积程序回退在`.work/vita/rollback/stage-effect-r9-op7-work-20261106/`。
