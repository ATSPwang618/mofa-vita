# r6 mask 与同步聚合

`Stage::clip_draw` 在 first 且已有 replacement mask 时，不再发出会被紧邻第二次完整清除覆盖的 first-mask。group blend/alpha 的保存照旧；mask=None、child、非first和Pixel路径保留。两次mask之间没有消费者，第二条仍 clear=true，因而删除只影响被丢弃的旧mask写入。r5热点每重画3组的结构预测为15→12个scene；实际毫秒收益待真机日志。

从保留旧源码和当前源码抽取实际clip_draw、footprint与ClipRect方法，使用相同Quad数据作192项定向对照：36条冗余mask删除，156条输出完全一致；最终full-clear后mask命令后缀、Pixel以及group blend/alpha/first/mode相同。覆盖own/child、first/nonfirst、局部／全屏／旋转inset、tint、fill、rule、haze、soft及原group alpha为0。测试只证明命令与状态合同，不冒充GPU像素测试。

新增控制台audit聚合，不增加逐scene打印：

- `scene_barrier_need/latest/covered`：仅实际设置GPU WAIT时同次数记录，us/bytes=0，aux为所需producer序号／最新提交序号／已覆盖序号之和。
- `scene_barrier_stage/pixel/mask/canvas/present/other`：同一必要barrier只进入一类，aux为latest−need之和，用于发现global SET保守覆盖。序号差不是等待微秒。
- `end_after_no_ps/ps19/ps_other/ps_mixed`：按紧邻前一**成功提交scene**内实际PS op分类，互斥分解EndScene；与现有EndScene计时共用相同起止时间。它只表示回压相关位置，不表示该op GPU耗时。目标End分类与前序op分类是同一总量的两种切分，不能相加。
- shader路径的 `draw_ps_group19/ps11/ps13` 由成功draw记录，PS位集合也仅在成功draw后更新。

生产native ASAN/UBSAN定向207 checks通过，12 alloc=12 free；新增barrier need/latest/covered精确值、场景类型lag、四类End互斥精确加总、failed End不推进前序op。最终ARM严格编译与整包由父代理统一执行。

RT `scenesPerFrame` 保持1。SDK定义它为预期scene次数及driver GPU数据结构配置，现有实际Hazuki libvitaGL.a的FBO创建同样硬编码1（gxm.o:0x9f0/0x9f4→0xa22）。没有硬件driver内部队列或容量公式证据，直接调8只可能搬移回压并增加未计入原native pool的LPDDR结构；本轮不把未证明收益的容量tuning混入确定修正。
