# r4 GPU 只读诊断

本次只读取新 stdout、固定 r3 capture 和当前交付源码；没有修改运行时代码、编译、启动 Docker／模拟器或连接设备。计算脚本 `analyze-r4-gpu-diagnosis.py` 调用现有 `vita_audit_log.normalize_console/analyze_audit/analyze_frame_classes`；完整原始分子／分母、窗口、分类、源码定位和排序保存在同名 JSON。

## 已经确定的瓶颈

r4 的问题仍集中在真实重画的 `sceGxmEndScene`，特别是 group pixel target，CPU顶点复制、upload和显式Finish已不是稳态主项。新 dependency graph、常量compact和group融合都有实际计数证明生效；PS destination fetch仍未生效，因此反复 snapshot／重开原目标的反馈链保留。

新log止于 frame2201、scene1/PC212，没有PC246。不能把新数据当r3 PC246的同输入A/B。新旧PC212的原始混合窗为：

|窗口|audit帧分母|重画／复用计数|work ms/帧|EndScene ms/帧|named wait ms/帧|scene/帧|PS/重画|group copy/重画|
|---|---:|---:|---:|---:|---:|---:|---:|---:|
|r3 PC246，2171–2216|46|27/19|108.354|87.092|8.338|23.196|8.556|3|
|r3 PC212，1953–2082，3窗|130|83/47|114.240|97.503|7.788|25.008|8.518|3|
|r4 PC212，1958–2201，5窗|244|148/96|101.605|84.357|7.025|20.180|8.486|0|

r3没有frame_classes，故不能反推其单独重画／复用耗时。表中r4混合work减少约11.06%只是这些观察窗口的差值，不是排除轨迹／reuse／文本差异的单项收益。r3 PC212重画率63.85%，r4为60.66%。

r4原始frame_classes末5窗的**实际settled范围为1957–2200**，比同audit窗口早1帧；以下分类只使用各自明确的分母，不把其counter与audit硬凑成完全同一批帧：

|r4真实分类|帧数|work ms/帧|render ms/帧|EndScene ms/帧|named wait ms/帧|其中batch wait ms/帧|scene/帧|PS copies/帧|
|---|---:|---:|---:|---:|---:|---:|---:|---:|
|redraw|148|151.179|150.727|138.902|0.308|0.276|31.959|8.480|
|reuse|96|25.049|24.621|0.174|17.382|16.991|2.000|0|

重画EndScene占work **91.879%**。复用虽没有stage重画／PS copy，仍在有界batch复用处等待GPU尾部，不能再用混合窗平均值宣称“重画已到约10FPS”。reuse的25ms也不是绘制cached stage本身花25ms。

## EndScene在何处停留

PC212五个audit完整窗的End分类合计严格等于总计：**4,924 calls／20,583,122 us**。

|结束目标／consumer|calls|总us|ms/audit帧|us/call|
|---|---:|---:|---:|---:|
|group pixel|1700|20,128,659|82.495|11,840.388|
|group mask|888|234,251|0.960|263.796|
|PS copy|1256|129,221|0.530|102.883|
|stage|592|49,045|0.201|82.846|
|canvas|244|21,806|0.089|89.369|
|present|244|20,140|0.083|82.541|

group pixel占EndScene **97.792%**；全capture累计也占96.707%（53,598,354／55,423,462 us）。但这只是CPU停在该目标EndScene的时间，不能直接叫作该pass的GPU执行时间。

关键结构恒等式：**1700个group pixel Ends =1256次PS后重开 +3×148次重画的初始group pass**。当前scene kind按target分类（`vita.rs:2790,2839`），没有把PS-resume与初始sprite pass分开。因此不能把20.129秒全部归给PS shader执行，也不能凭新group15编译成功推断它导致该时间：group融合的输出是stage；它删除scratch链，而组内PS反馈链仍在。

稳态五窗PS snapshot **1256次／651,110,400 B**，每重画约4.399MB逻辑GPU copy。全部3446个PS都是op19。原生copy通过sample＋quad完成（`effects_backend.inc:675`），没有CPU像素readback/memcpy。它自身CPU父scope只有0.749ms/audit帧；EndCopyPs是其中子scope，不能再相加。抹掉copy的CPU计时不等于收回group pixel的82.495ms。

## “仍然串行”的准确范围

- 原始全段17,460 scenes中 **4,866个（27.869%）省略VERTEX_WAIT**；重PC212为1,214／4,924（24.655%）。`scene_gpu_dependency`统计每个producer SET，仍等于scene数，不代表每scene都WAIT。RAW3377／WAR2879／WAW2953是可重叠类型，不能求和。没有缺plan恢复或arena容量溢出记录。
- 所有命令仍由同一个线程交给一个GXM context（`gxm_backend.c:345`）。图只收窄是否添加barrier；遇到必要依赖时，SDK flag仍等待**最新global SET**（`gpu_lifetime.inc:78`），未建立逐纹理sync token。如果need早于latest，会保守覆盖中间无关job；本日志没有need/latest差距的trace，不能量化这一额外等待。
- 热点PS顺序是 End(group pixel)→GPU Copy到backdrop→Begin同group pixel→PS draw（`vita.rs:1818`）。目标写后被snapshot读，再写原目标，同时读backdrop，是实际RAW／WAR／WAW反馈；无法通过增加CPU线程或盲删flags消除。
- 纹理退休／arena的global notification是完成观察，不是内存可见性barrier。单个notification地址本身没有证据导致每scene CPU同步；与vitaGL shared query fence模式一致。真实CPU等待只在命名wait调用处出现。

因此可以说“单提交队列、许多真实反馈依赖、仍有global保守边界”；不能说“r4仍把所有独立scene强制全串行，所以只要删同步就能几倍提升”。

## 已定位的slot过度等待

`gpu_lifetime.inc:160`先判断被复用slot的`last_sequence`是否未完成；但进入的`wait_submitted`（:41）等待的是**g->sequence最新全局提交**，不只是旧slot最后使用点。两个槽的容量安全，但一旦慢旧slot触发等待，CPU可能连更新的另一个batch一起等完。

重窗31次batch wait总1,671,947us，平均每次 **53.934ms**；真实reuse类96帧平均16.991ms，而重画类只有0.276ms。它解释“reuse也慢”的位置，并不证明全部等待都多余：当前log没有slot目标序号与等待前completion差距，不能给可回收毫秒。

后续应使用经过协议证明的per-slot/per-batch完成身份或阈值等待来只等必需的slot，保留texture retirement与scanout协议。**不能直接将当前single-address等值NotificationWait的value改成过时旧seq**：该word可被更新的completion越过，旧值未必再出现。也不应直接增大槽数来掩盖持续GPU吞吐不足。

## RT／driver背压的证据界限

当前每texture一个RenderTarget，`scenesPerFrame=1`（`effects_backend.inc:303`）；context PB／四种ring也用SDK默认容量（`gxm_backend.c:331`）。重复提交同group pixel而backdrop目标很快返回，符合目标场景元数据复用／依赖队列背压的调查方向。但捕获没有SDK队列占用、PB峰值或GPU timestamps，**还不能证明某个ring、RT容量或GPU ALU是根因**。vitaGL默认也为1，且提供`vglSetupRenderTargetScenesNum`（reference `gxm.c:1030`），因此“1必错”没有证据。

下一轮只需在热点限定区分初始group pass／PS-resume，记录need/latest/covered及slot目标/已完成序号，必要时做有界RT scenes数1→2/4对照，观察group pixel End＋batch合计及物理内存；单独EndScene从一个API搬到另一个API不算收益。

## CPU复制已经减少多少

启动能力实际是group15=1、ps_fetch17=0、ps_fetch19=0；zero compact和constant compact都patch成功。全段174,597／174,597个effect draw均compact，输入 **120,111,936 B→实际47,003,776 B**，少73,108,160B（**60.867%**）。重PC212也100%compact，19,416,096→7,497,696B，少61.384%；`vertex_copy`只有0.154ms/audit帧。常量流的额外80B已计入实际写入，不能套r3的固定80/32推算公式。

重PC212五窗新upload／update／zero字节均零，arena drain零。故“频繁CPU→GPU素材复制”不符合当前稳态主瓶颈。全capture glyph source rename六次，共复制6MiB，91.098ms；finish_texture_update仅3次18us。rename确实把在途覆写等待改成有界CPU copy；这些loading事件也不能解释稳态151ms重画。

## 后续优先顺序

1. **消除op19的目标反馈分pass，收益证据最强。** 有效fetch能力仍0，热点仍100%op19。先修可被真实compiler/硬件接纳的native color路径；若此能力不可用，再研究有界双目标ping-pong，把旧目标copy与PS写入合并到同一新目标scene，减少独立copy scene和反复原RT重开。必须保留区域外像素、gamma、mask、alpha与退休；不能无证据替换普通alpha blend。可减少结构确定，毫秒收益未知。
2. **对group pixel重开进行限定细分和RT场景容量调查，定位置信度高、具体driver原因未证明。** 先区分3个初始pass和约8.486个PS-resume；再以有界容量与同依赖下的End＋wait总量判断，不扩大无目标的CPU微优化。
3. **收窄slot复用等待，代码过度覆盖事实置信度高，FPS收益置信度中。** 复用帧16.991ms batch等待是明确位置，但不能承诺这些全可去除；GPU重反馈吞吐不变时更早enqueue只减少主线程等待。
4. **调查图的最新SET保守覆盖。** 当前已省约1/4 WAIT，再前进需need/latest证据和真正GPU ordering合同。真实PS反馈依赖占大头，不应盲删。
5. **保留compact／rename，暂缓更多CPU memcpy微调。** 已100%compact且byte reduction超过60%，当前稳态0.154ms复制远小于138.902ms/重画EndScene；不是下一轮首要收益项。

日志只有CPU API计时和时钟设置（ARM444／bus222／GPU222MHz），没有CPU/GPU利用率或GPU执行时间。“硬件占用不到一半”若来自外部overlay，也可能是平均核心／阶段占用、串行反馈、RT/PB背压和CPU阻塞的组合；本日志无法推出GPU ALU或带宽具体剩余百分比，更不能将未占满百分比直接换成FPS潜力。
