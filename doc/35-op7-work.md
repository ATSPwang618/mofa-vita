# r9：精简 OP7 与有限双层融合实机候选

版本：stage-effect-r9-op7-work-20261106。基于本地 perf/render / 24b0467 加已有 r8 修改；本轮不提交、不推送。目标是降低重演出的实际 GPU 工作量，争取重绘帧低于 100 ms（10 FPS 以上），不是承诺已经达到该帧率。

## 实现范围

- 新增原生程序48：FP32 plain OP7。按准备后的材质选择，支持 Sprite、Fill、NoAlpha 和几何 clip；Haze/rule/soft/group 不符合时保留完整路径。Fill 仍使用纹理 alpha，NoAlpha 只调整源 alpha。源、目标颜色公式和运算顺序沿用原程序。
- 新增内部程序57：连续 OP7 → OP7 的有界像素局部融合。只接受普通 Sprite、非 sRGB 纹理、同一个目标中的相邻绘制，且两个单独程序均选择48。交集至少8,192个物理像素，且至少为较小图层覆盖面积的一半；凸形、仿射 UV、几何 clip、反射缩放均经过现有几何门槛。不能匹配的材质仍单独绘制。
- 交集中的两次混合按原顺序执行，中间显式保留 native_pack/native_unpack 的 RGBA8 存取。交集外仍使用各自原采样坐标和原程序48；不跨越别的图层，不混用 Half 内外精度，不删除 mask 或 alpha=254 的组内容。
- 复用原有两个纹理输入及112字节顶点协议；没有新增纹理副本、离屏目标、缓存层或顶点环槽，没有移除同步和资源所有权保护。
- 新程序在录制前离线装载、反射校验、注册和 patch。57还要求静态主指令数不超过两个48之和。编译/校验/patch失败只关闭候选；默认完整材质及单独绘制回退保留。
- GPU工作统计将48计为 OP7 专用程序；逻辑链27的7/7组合按实际内部程序57统计。仅成功执行计数，重复混合按两个OP记录，交集物理面积只计算一次提交覆盖。

没有任何 scene、PC、图片名或图层号匹配规则，所有场景满足条件均可使用。局部重叠可能将原两次 draw 分成三块，因此本轮不承诺减少所有 draw/scene 数；收益目标是减少交集上的重复混合、原生目标读写和每像素指令。

## 离线编译结果

| 程序 | 用途 | 主指令数 | 采样槽位 |
|---|---|---:|---|
| 29 | 原完整源 OP7 | 233 | 0、2、3 |
| 48 | 新精简 FP32 OP7 | 36 | 0 |
| 51 | r8已有 Half OP7 | 32 | 0 |
| 57 | 新 FP32 双 OP7 | 51 | 0、2 |

51条相对两个36条减少约29.2%，仅是编译后的静态指令比较，不能等同于帧耗时下降29.2%。r8已命中Half51的单独绘制只有32条；本轮优先48是为了融合交集和交集外保持同一FP32合同，**不能声称新单个OP7一定比r8更快**。如实机单独48无收益、或融合未命中，应保留关闭能力并依数据决策。

全部65个作者源码程序在PC编译，合计112,516字节，比r8多1,440字节原始GXP。复用原有88槽程序表，不扩展其容量。驱动patch实际分配仍由既有机制管理，未在真机测量新增程序的驱动内存。

源码SHA-256：ee34d012e4d9466566e62a85b682ff4ae0b5690f69ef4ad240122ed41a65b4c6。

## 默认配置与日志

默认hardware-test，mask=0x1cfdfffffff。在r8测试配置基础上增加bit39 ps-plain-op7、bit40 ps-chain-op7；sparse-coverage仍关闭。原Group bounds、Half、Tiled、Blur权重、Scene容量2保持r8默认策略。all仍是原准入基线。

VitaDeck默认开启，启动打印本版build_tag与optimization_profile。默认和旧标准值迁移的profile记录分别约754/779字节，低于原900字节单记录限制。旧标准文件all / all ps-all-operations / all ps-full-material仍自动展开；其他自定义文件完整覆盖默认值，不擅自取消明确关闭项。

实际命中核对：

- op7_work_candidates：plain48_selected / plain48_ready、chain57_selected / chain57_ready。
- shader_chain_variant：shader=57、op_a=7、op_b=7、primary=51、single_primary_sum=72、cost_admitted / patch_status / admitted。
- gpu_work.program_counts：48、57成功执行增量。打开开关或ready=1不等于该片段实际执行。

在ux0:data/WitchontheHolyNight/optimization-profile.txt中可独立对照（重启生效）：

    hardware-test
    hardware-test -ps-chain-op7
    hardware-test -ps-chain-op7 -ps-plain-op7

最后一项保留r8配置组合，恢复既有Half/full OP7选路。perf-enable.txt的显式覆盖规则保持；本地交付文件仍为1。

## 固定回放

沿用gpu-replay.txt的current或scene pc触发方式，冻结当前生产Stage，不推进剧情与存档。每个ABBA块预热30帧、测120帧，原始纹理、时钟和其他开关固定。

- 两个新开关均启用：只翻转bit40，比较“两个FP32单独绘制”与“FP32融合”，避免把精度变化算成融合收益。
- 关闭ps-chain-op7、保留ps-plain-op7：只翻转bit39，单独检查精简FP32路径；其他Half/完整路径是否替代由原选路决定。
- 两个新开关都关闭：回到上一版本的全24OP对照规则。

分析器必须看到B块实际执行48或57、A块没有相应增量，以及零RGBA8差异，才允许评价该项性能。没有OP7、没有符合融合的相邻组合、程序不可用或计数缺失都为inconclusive。不能把未命中实验的正常帧误报为优化成功。

此回放仍仅覆盖触发时相位；未完成全游戏GPU像素差分。使用后移走请求文件以恢复正常游玩。

## 验证与交付

- Rust：渲染168、游戏122、性能配置8、render-api基础1项通过。
- 原生GXM：67 cases / 8,453 checks；缓存38 cases / 2,699 checks，ASan/UBSan通过。
- 实际输入：plain/half 746 checks、完整源11,144 checks、Blur上传22 checks；原effects检查933 cases / 2,799 checks及5项反向变异控制通过。
- 作者源码CPU数学：单操作855,369组（含9个独立RGBA8锚点），顺序双OP7 400,000组。反向控制能检出丢弃第一层与颠倒顺序。
- 回放日志分析15项、配置收据4项通过。65个真实PC GXP通过生产元数据/嵌入身份检查；ProgramCheck在host被模拟，不能代替GPU执行。
- ARM release构建通过；KNIF00540、ATTRIBUTE2=12、21个引擎安装条目，未含游戏数据。独立EBOOT与VPK内EBOOT字节一致。

交付目录：dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/。

- eboot.bin：10,122,695字节；SHA-256 7529faa5656f6238a544b1753b4bcb28d5041336c1bd384c52f920993f1f1d2a。
- knife-vita-540p.vpk：7,256,454字节；SHA-256 7cb01db07e4cd0680852295c4d96c4bdbcd95371b5cda080e3ff79f6c3e4640b。
- build-receipt-r9.json：完整源码/构建/配置身份。
- ELF/map保留于.work/vita/game-release-540p/；r8小体积回退副本在.work/vita/rollback/stage-effect-r8-hardware-test-20261106/。没有复制游戏assets。

**状态：已实现、已通过host/native/ARM/VPK检查；尚未实机运行、尚未取得FPS收益证明、尚未通过GXM像素差分。** GPU帧时间目标仍为重绘小于100ms；不能拿历史不同阶段窗口或静态指令降幅相加得出10FPS结论。

机器收据：[35-op7-work.json](evidence/35-op7-work.json)。

