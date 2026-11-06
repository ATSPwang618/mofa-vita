# r7：全游戏离线专用Shader

本文记录r7交付状态。用户后续要求将系列优化默认开启测试，当前r8默认配置及实际Half/Tiled接入见[34](34-hardware-test-defaults.md)；下述r7默认关闭说明不再代表最新测试包。

版本：`stage-effect-r7-all-ops-offline-20261106`。本轮实现全部24种PS混合运算的完整材质专用入口及PC离线GXP，不再只扩展OP1/6/13。所有修改留在本地 `perf/render`，基线 `24b0467`，未提交或推送。

## 作用范围

程序64–87依次对应OP1–24。实际准备好的Quad决定选路，适用于所有章节的普通Stage绘制、Group Pixel/Mask中的PS绘制和Group最终Composite；没有scene38、PC1246、图片名或图层编号特例。全游戏生产MZX/运动表盘点见[29](29-script-census.md)。动态状态在运行时使用完整源处理，不要求玩家先遍历所有剧情。

每个程序保留通用 `ps_source` 的Haze、rule、soft clip、group mask、Fill与NoAlpha处理、几何clip、原alpha公式与RGBA8量化顺序，只把当前OP固定为编译期常量，移除其他OP的选择代码。共享源码生成器提取实际通用公式，独立锚点与错误实现对照约束生成结果。

简单材质原有30/31/21/20/25/23程序继续优先。更复杂材质才选择64–87；某入口不可用时，按已有29/54–56等兼容入口、generic native fetch17、snapshot14逐项回退。NoAlpha原有直接路径仍可优先。无新增图层融合、不改混合顺序、不删除GPU同步。

“全游戏专用”指全游戏按OP复用有限专用程序，所有24种PS操作都有完整材质入口；并非为每张图片生成重复程序。普通Sprite、文字、Group等已有程序一同离线打包。电影、纹理数据和CPU游戏逻辑不会因新增Shader而变化。

## PC离线编译

新增 `tools/offline_vita_shaders.py`，支持本机 `psp2cgc` 和用户提供的 [Vita3K-Shacc](https://github.com/shoui520/Vita3K-Shacc) 两种后端。Vita3K-Shacc需要自行准备配置/固件/编译服务；这里提供自包含Cg输入与命令适配，**该后端未在本机实际运行**。

本次实际使用本机已安装的 `psp2cgc.exe`，编译参数为 `-O3 -bestprecision -nofastmath -nofastint -W3 -Werror`。已生成63个实际GXP，共111,076字节，含全部24个新入口，嵌入 `knife/vita/shaders/generated/offline_programs.h`。PC编译器可执行文件及其DLL未加入VPK；原有设备编译器回退模块保持不变。

```powershell
# Windows，在项目根目录；默认使用本机已有PC编译器
python -X utf8 tools/offline_vita_shaders.py
# 后续普通构建只校验已嵌入GXP，不要求PC编译器
python -X utf8 tools/offline_vita_shaders.py --check
# 可选Vita3K-Shacc适配；需先完成该项目的setup
python -X utf8 tools/offline_vita_shaders.py --backend vita3k-shacc --compiler C:/tools/vita3k-shacc.exe --config C:/tools/shacc.json
```

源文件变更后先运行 `tools/generate-ps-operation-shaders.py` 和 `tools/generate-runtime-shaders.py`，再离线编译。`--check`验证源码身份、63个入口的GXP元数据和逐项SHA；构建会拒绝陈旧结果。Cargo依赖包括两个生成头和生成工具，避免只更新GXP却不重编译。

当前源码SHA：`d5667aa3a405f26ba1934ef4b1186edc8458c62d6c6267e86c00f0f19fb031cf`。PC编译器SHA：`dae6c92bb97b2b8adfa26a3f85589e80eacf4ba88d2d6189bff4eaa6c87a4e27`。精简逐程序记录见[evidence/33-offline-shaders.json](evidence/33-offline-shaders.json)。完整命令与编译结果保存在 `.work/vita/offline-vita-shaders/`。

本次同一PC编译器输出中，generic17的primary指令记录为577；新入口为163–246。它们是静态程序记录，不能换算成FPS、GPU周期或一定的加速比例。

## 配置、能力及日志

独立开关 `ps-all-operations` 为优化位38。优化集合仍为u64；Shader能力扩为独立的两个u64 bank，现有编号不变，高bank保存64–87，禁止把程序编号直接作为优化位或对u64移位64以上。

本候选编译默认值及配套 `optimization-profile.txt` 为：

```text
all ps-all-operations
```

设备数据根 `ux0:data/WitchontheHolyNight/optimization-profile.txt` **完整覆盖**编译默认值。旧文件只写 `all` 会关闭本轮入口；需覆盖该小文件，位置与assets、saves并列。对照可改为 `all` 并重启。`all`自身不纳入未完成实机验收的位38，避免其他构建悄悄改变配置。Half/Tiled/Blur/Scene容量和sparse仍未加入本候选默认集合。

启动日志应包含：

- 版本 `stage-effect-r7-all-ops-offline-20261106` 和有效配置中的 `ps-all-operations`。
- `[knife-shader] stage=offline embedded_ready=... enabled=...`：通过设备验证的内嵌程序数；两者相等时本次无需运行时编译。
- `all_operation_candidate`：每个OP的compiled、registered、patch_status、ready。
- `all_operation_capabilities`：`selected=1`；24个入口全部成功时 `ready_mask=16777215`，失败位会回退，不能把selected当作成功。
- `gpu_work.program_counts`：64–87在**成功执行**后的累计计数。缓存复用帧不一定新增；取消、拒绝、录制未提交不计入。

不增加纹理池、顶点环槽或帧缓冲。ARM32的新增入口管理表占7,968字节；GXP载荷及patcher driver内存另外计费，后者必须实机观测，不能仅凭表大小宣称总内存没有变化。日志仍走有界后台VitaDeck输出；根目录 `perf-enable.txt` 内容0仍是明确关闭规则，1为开启。

## 验证与尚未完成的实机门槛

本轮通过渲染163项、游戏121项、优化配置4项；native effects 933 cases/2,799 checks；GXM边界51 cases/7,204 checks；缓存38 cases/2,660 checks；完整PS输入11,144项；离线回放分析14项。24OP数学验证包含1,969,593组、121个独立RGBA8锚点及11个故意错误的负对照。63个实际GXP均通过生产元数据验证函数的host检查；host替代的 `sceGxmProgramCheck` 不代表设备驱动已接受。

ARM release已构建，VPK检查标题KNIF00540、ATTRIBUTE2=12、仅引擎内容及外置/包内EBOOT一致。最新ELF/map、构建收据在 `.work/vita/game-release-540p/`。交付身份另见同目录构建收据与dist的版本说明；上一轮小体积程序回退保存在 `.work/vita/rollback/stage-effect-r5-material-20261105/`，不复制游戏assets。

已有固定Stage ABBA回放在位38开启时仅翻转位38，A关闭、B开启，其余配置相同；每区块预热30帧后测120帧。报告同时记录24OP候选及实际执行增量。缺能力、未命中、计数丢失或非实机环境均不能判定通过，详见[31](31-GPU回放诊断.md)。

**尚无r7实机GPU像素差分或帧时间结果**，因此不能宣称画面逐像素一致、全游戏普遍加速或最重场景已达到20FPS。原计划中Group局部合成、Half、Tiled、Blur、Scene容量的硬件准入仍分别待验收；本轮不借离线编译绕过它们的门槛。
