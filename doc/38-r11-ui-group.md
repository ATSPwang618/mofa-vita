# r11：UI 按成员读取与组混合专用程序

2026-11-06，基线 `perf/render` / `ac5863b`，交付版本 `stage-effect-r11-ui-group-20261106`。本轮改动仅在本地，未提交、未推送远端。

## 故障依据与修复

输入是 [r10 长时间实机分析](37-r10-long-hardware.md) 对应的桌面 `log.txt`：scene40/PC690 的帧准备错误明确落在 `allui:7 DISP_PARTS_ZC.NXZ` 的 `allocating bounded NX output`。原压缩入口要求一次申请 17,562,705 B（16.75 MiB）完整输出；其分配失败被 AtlasCache 保存，后续 UI 绘制重复取得同一个错误。存读档页面依赖公共 UI 图集，因此状态机进入菜单并不代表能完成该帧准备。这不是该日志已经证明的 GPU 显存分配失败，也不能仅由总 headroom 断定连续空闲块足够。

本次采用现有 raw KATL 成员读取协议，扩展到 DISP/MENU/SAVE/CONF/TITLE/MODEPNL/BORDER/NOISE/EXTRA/SOUND 的所有已有语言入口，以及之前已支持的 CG/ARCHIVE/ARCHTHUMB。18 个原压缩入口发生布局变化。共核验 979 个选中成员，KIMG 字节、名称、顺序、尺寸和 alpha 不变；未重编码 PNG/DXT。其他 64 个 UI 成员仍由原规则处理，全部 UI 索引共 1,043 项。

- 对话图集不再为一个部件分配 16.75 MiB 完整 NX 输出。选中图集成员的最大编码读取长度为 1,048,672 B（约 1 MiB）。这是源数据分配上界，不能等同于所有解码、GPU 或同时显示 UI 的总内存。
- ByteStage 将读取所得 Vec 移交 ByteView，共享所有权时不再复制整块负载；UI 取出后也直接交付该 view。既有非 UI 消费者的 owned Vec 转换保持其接口语义。
- 初始和运行中 UI 预热均跳过 direct 图集，避免后台预热重新读取整张图集。
- AtlasCache 保留带类型的失败分类：TryReserveError 与共享源预算拒绝属于可重试压力，真实 demand 可回收未被引用的源 owner 后重试一次；再次失败仍返回明确错误。校验失败和工作线程 panic 不会自动重试。已在途任务和被引用数据不会被强行取消或释放；显式新 VM generation 的重试入口保留。
- 不增加无效存档或忽略丢失资源；该修复消除本次公共 UI 大块解压及错误持续缓存路径，不声称所有潜在 OOM 已不可能发生。

代价：`allui.mrg` 从 81,532,928 B 增至 143,323,136 B，存储增加 **58.93 MiB**。这是用少量卡空间换取按需小块读取和取消整张图集解压，图像驻留格式、纹理清晰度及预算未改。

## 全游戏组混合优化

在实际 Group composite 入口按 OP、目标格式和就绪能力选路，不检查章节、PC、素材名或图层号。独立开关 `ps-group-material` 为 bit42，`hardware-test` 默认开启。

| OP | 新程序 | 原完整程序 | 原主指令数 | 新主指令数 |
|---|---:|---:|---:|---:|
| 6 Dodge | 105 | 69 | 238 | 47 |
| 8 Difference | 106 | 71 | 227 | 35 |
| 14 SoftLight | 107 | 77 | 246 | 58 |

专用入口只接受固定的 Group source 合同：无 Haze、rule、soft clip、Fill、NoAlpha；仍读取 tex0 的 Pixel 和 tex2 的 Mask，保留原 `group_rgb`、`group_alpha` 的 RGBA8 量化、非1目标 alpha 和原混合运算顺序。254 遮罩不改成255。保持112字节顶点、4槽/8参数和完整清除/生产绘制/同步。

Rust 选路、C 验参、离线/运行时元数据检查及缓存 ABI 一起更新。原生 patch 失败或静态成本不优于基线，只禁用对应程序，继续完整 OP 程序、通用 fetch17、snapshot14 回退链。关闭 bit42 不撤销 r10 的17个材质程序。统计包含105–107，固定ABBA回放优先只翻转bit42；已执行计数和输入机会同时检查。

85个离线GXP共127,500 B已嵌入程序。新增三个GXP共2,208 B，ARM侧程序表增长996 B；不增加目标/纹理/顶点环预算。shader driver注册内存仍需实机观测，静态指令减少**不是帧时间或FPS提升的证明**。

## 验证状态

**已实现、已通过自动检查：**

- Rust 302项（游戏124、perf8、render170），含临时失败分类/重试次数、组混合选路、独立关闭和逐级回退。
- UI离线工具6项：原字节验证、重复范围、幂等、错误清单/截断拒绝和多语言选择；实际979成员逐项校验。
- 材质40万组CPU数学对照；Fill alpha、Haze预乘和Group mask三个破坏性负例均能检测差异。CPU对照使用受控采样，不代替GXM像素差分。
- 实际C材质输入1,170项；GXM后端68场景/8,686断言；缓存58场景/3,159断言；公开效果输入933场景/2,799断言及五个负例控制。
- replay分析器15项、构建配置4项；85个真实离线GXP经过生产元数据校验。
- ARM发布构建、SFO `ATTRIBUTE2=12`、纯引擎VPK检查；VPK内EBOOT和独立EBOOT一致。
- 默认VitaDeck日志开启，启动配置记录794/819 B，低于900 B限制。

**待实机：** r11实际重绘中位数/p95/p99、GPU输出一致性、长时间进入存读档与附加页面后的稳定性。旧日志的12–18FPS不能作为r11成绩，不承诺本轮所有场景达到20FPS。

## 更新与回退

交付目录：`dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/`。

1. 关闭游戏。将本目录 `assets/allui.hed`、`assets/allui.mrg`、`assets/vita-540p-assets.json` 覆盖到设备 `ux0:data/WitchontheHolyNight/assets/` 同名文件。
2. 上传同目录 `eboot.bin` 至 `ux0:app/KNIF00540/eboot.bin`，或安装同目录 VPK。VPK 不含上述资源，不能只安装VPK。
3. `allui.nam` 没变，其余剧情素材/音视频/字体和存档无需重传。设备数据根 `perf-enable.txt` 显式0仍会关闭日志，请使用1。若有自定义优化配置，用 `hardware-test` 开启本轮组合。
4. 启动 `build_tag` 核对 `stage-effect-r11-ui-group-20261106`，mask `0x7cfdfffffff`。20项材质程序都就绪时 `ready_mask=1048575`；实际新程序使用查看 `gpu_work.program_counts` 的105、106、107。

仅比较本轮GPU改动：`hardware-test` 与 `hardware-test -ps-group-material`。两者都保留UI修复及r10已有优化。

完整r10回退：`.work/vita/rollback/stage-effect-r10-material-coverage-20261106/`。保存的是程序/ELF和必要UI归档，不是整套游戏数据；恢复该目录 EBOOT 与三个配套资源文件，避免混用清单。

清单SHA256：`c317f7da0d07d01af0c13d92720013f0e33b214f5d1356c6a39b8dced168e832`。

- `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/eboot.bin`：10,162,583 B；SHA256 `7292b8c2b4ea40542fdfd704659065762013c6e99196f2db1653bcbedd7c423b`。
- `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/knife-vita-540p.vpk`：7,270,288 B；SHA256 `5c662468ef0f0532578b176862e0ed5f6ad289da80d009fef6529800926595ac`。
- `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/build-receipt-r11.json`：86,549 B；SHA256 `323097d47c118f0a5af8837425e296bf43cdf90413e37c6a4b979d6fda44085f`。
- `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets/allui.hed`：568 B；SHA256 `afa333f7afee931f5169b2a9ed916d1aa8bcf5ce6cfc735d4cdbd5a4489e947e`。
- `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets/allui.mrg`：143,323,136 B；SHA256 `cacb75d05ca00fcdcc0d580f7fab2d50715daf605bad6fad2720c8841885742b`。
- `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets/vita-540p-assets.json`：1,605,592 B；SHA256 `c317f7da0d07d01af0c13d92720013f0e33b214f5d1356c6a39b8dced168e832`。

详细收据：[38-r11-ui-group.json](evidence/38-r11-ui-group.json)。
