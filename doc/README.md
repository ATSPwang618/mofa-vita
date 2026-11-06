# 技术文档索引

当前本地实机候选为`stage-effect-r10-material-coverage-20261106`，基于`perf/render / 248ea872`。新增17个全游戏按材质选路的精简程序，82个离线GXP；`hardware-test`默认开启新增组合，VitaDeck日志默认开启。已通过host/native/ARM/VPK检查；r10实机FPS和GXM像素差分待验证。详见[36-材质覆盖](36-material-coverage.md)，上一版OP7与融合见[35](35-op7-work.md)。

代码是实现事实来源。旧文档内的行号只辅助定位，以当前文件、常量和函数名为准。历史捕获、构建验证与实机验证分开表述：r2/r3/r4 使用旧的 480×270/720×404，后续 540p 优化记录也不能替代本次新素材包的实机回归。

## 当前文档

| 文档 | 内容 |
|---|---|
| [01-架构总览](01-架构总览.md) | crate、帧循环、线程、坐标系与关键文件 |
| [02-渲染与GPU后端](02-渲染与GPU后端.md) | GXM、pass、批次、依赖、shader、缓存与后续优化记录 |
| [03-构建与工具链](03-构建与工具链.md) | SDK/WSL、固定 Rust、VPK、收据、元数据与排错 |
| [04-资源调度与缓存](04-资源调度与缓存.md) | 资源所有权、图集、异步加载、音视频与预算 |
| [05-优化记录与实测数据](05-优化记录与实测数据.md) | 30 个优化开关、历史实测及验证状态 |
| [06-性能采集与分析](06-性能采集与分析.md) | 编译/运行开关、日志字段、采集边界和分析依赖 |
| [07-已知限制与未验证项](07-已知限制与未验证项.md) | 当前画质包尚需实机验证的内容与工具限制 |
| [08-素材与安装](08-素材与安装.md) | 当前清单、素材处理、设备路径与配套更新 |
| [09-仓库维护与清理](09-仓库维护与清理.md) | 资源唯一位置、产物保留、清理范围和校验规则 |
| [10-资源加载失败修复](10-资源加载失败修复.md) | Scene 3 / PC 1148 定位、共用上传恢复、回归边界与设备复测 |
| [11-高清纹理显存与崩溃修复](11-高清纹理显存与崩溃修复.md) | 最新 core、长立绘占用对比、页碎片修复、PVR 限制及回退 |
| [12-透明调色板纹理排查](12-透明调色板纹理排查.md) | 全库颜色/alpha 统计、P4/P8 精确保留候选及实际 GPU 收益边界 |
| [13-VitaDeck日志接收修复](13-VitaDeck日志接收修复.md) | 实机 EBOOT 身份、PC 长连接阻塞、接收补丁和日志保存 |
| [14-演出材质离线显存限额](14-演出材质离线显存限额.md) | 全库预处理、20,181 个条目缩小、单张 RGBA 1 MiB 限额与配套更新 |
| [15-读档与转场目标分配恢复](15-读档与转场目标分配恢复.md) | old frame 崩溃定位、备用目标复用、有限回收、未提交帧回退与 targetfix 交付 |
| [16-BGM预读缓冲调整](16-BGM预读缓冲调整.md) | Opus BGM 一秒预读、300 KiB 单路增量、播放控制与 bgmbuf 验证 |
| [17-书库与图录加载修复](17-书库与图录加载修复.md) | 整图集峰值、四个 raw KATL、675 张成员直接读取、缓存回收和配套资源更新 |
| [18-关闭手动快进](18-关闭手动快进.md) | 禁用 Vita R/面板/其他手动向前加速，保留 L 自动播放、输入检查及稳定性边界 |
| [19-特效前缀与显示冗余优化](19-特效前缀与显示冗余优化.md) | 历史 r2 实验与初次验证记录；实机复核后已撤回 |
| [20-r2实机复核与回退](20-r2实机复核与回退.md) | r1/r2 重绘窗口对照、精确回退范围与当时回退包 |
| [21-重场景op7专用着色器](21-重场景op7专用着色器.md) | 约 6 FPS 窗口定位、完整材质 shader29、回退与实机验收 |
| [22-op7专用shader实机复核](22-op7专用shader实机复核.md) | 专用29已执行，最重窗口约6.08→7.03 FPS，剩余GPU等待与数据边界 |
| [23-scene38-PC1246-脚本素材与融合可行性分析](23-scene38-PC1246-脚本素材与融合可行性分析.md) | 原始MZX逐条核验、27层/26图、屏幕透明覆盖、局部融合与mask边界；仅分析未改运行版本 |
| [24-scene38-透明覆盖优化](24-scene38-透明覆盖优化.md) | r4 精确源身份、DXT5 alpha 支持域、单 Draw 稀疏顶点、验证与默认日志；实机收益待验 |
| [31-stage-effect-r5-gpu-work](31-stage-effect-r5-gpu-work.md) | r5 全游戏 GPU 工作量优化、执行覆盖统计、Group owner 证明、有限专用 shader、固定回放和验证边界 |

工具分类见 [tools/README.md](../tools/README.md)；感谢页维护见 [credit-intro](../tools/credit-intro/README.md)；全剧情图库处理见 [quality540](../tools/quality540/README.md)。

## 证据

[`evidence/`](evidence/) 保留以下机器可读摘要：

- `09-render-audit-evidence.json`：历史渲染审计。
- `10-asset-copy-evidence.json`：历史资源复制链。
- `12-hardware-render-evidence.json`：历史真机基线及捕获身份。
- `13-native540-art-quality.json`：全图库重建时的构建、逐图解码与存储记录。
- `14-logo-media-update.json`：启动视频更新、素材身份与当时配套 ARM 包记录。
- `15-resource-upload-recovery.json`：第一轮分配恢复、全归档元数据核验、回归与当时配套 ARM 包记录。
- `16-high-art-memory-crash.json`：最新 core、高清立绘占用、页碎片回归及当前 ARM 包记录。
- `17-texture-alpha-palette-audit.json`：全库透明度、完整 RGBA 颜色计数、当前纹理格式与调色板候选统计。
- `18-vitadeck-log-reception.json`：日志接收阻塞复现、实际安装补丁、程序身份与实机日志恢复。
- `19-offline-story-memory-budget.json`：全库离线缩小、独立解码、发布预算检查和 prebudget 包身份。
- `20-save-load-target-recovery.json`：读档 core 身份、必需捕获恢复、114 项渲染测试与 targetfix 交付身份。
- `21-bgm-pcm-buffer.json`：第二章断供证据、BGM 队列调整、29 项音频测试与 bgmbuf 交付身份。
- `22-extra-ui-direct-ranges.json`：书库/图录失败窗口、675 张成员字节核验、直接读取分配上界、回收回归与 uifix 交付身份。
- `23-auto-only-input.json`：禁用手动快进的实际输入路径检查、ARM/VPK 构建及配套 EBOOT 身份；不代表此前内存问题已经修复。
- `24-stage-effect-r2.json`：r1 日志字段与重场景窗口、r2 prefix/空 draw/present clear 的验证范围与交付身份；r2 实机收益未验证。
- `25-stage-effect-r2-rollback.json`：后续 r2 实机对照、重场景收益不足的回退决定及当时交付身份；覆盖 24 的后续实机结论。
- `26-stage-effect-r3-ps7.json`：op7 完整材质专用程序、host 验证、带日志 ARM/VPK 身份；不代表实机收益。
- `27-stage-effect-r3-hardware.json`：后续用户实机日志、shader29实际启用和PC1246目标窗口收益；不代表全程20 FPS或像素等价。
- `28-scene38-fusion-feasibility.json`：原始MZX/生产解析器一致性、运行素材alpha与屏幕覆盖、具体融合候选和当前成品未变的身份；不是新shader性能验收。
- `34-hardware-test-defaults.json`：r8默认组合、真实Half/Tiled路径、回归收据、EBOOT/VPK交付身份及尚未实机验收的边界。
- `29-stage-effect-r4-sparse.json`：IMG0436 精确 DXT5 支持域和单 Draw 覆盖优化、host/ARM 验证、默认日志及新交付身份；未实机验证 FPS。

旧 `doc/archive/` 可从提交 `a670193` 查询。旧探针 JSON、失效的 Windows 打包/交付记录及临时日志不再散放于当前工作树。证据内的旧工件路径是当时的身份记录，不保证相应临时副本仍保留。

本机详细清晰度报告压缩保存在 `.work/reports/native540-art-quality/`；最新 ELF/map 在 `.work/vita/game-release-540p/`。这些本地产物不入库，也不参与源码编译。

## 维护

修改渲染更新 02/05，修改资源与预算更新 04，修改构建更新 03，修改日志更新 06，修改素材和安装身份更新根 README 与 08。目录保留政策更新 09。

生成日志放 `.work/` 或已忽略的 `doc/logs/`，验证结束后清理；游戏资源只在 `dist/` 的两个指定目录保留，不恢复多套旧素材或整份回滚资源。

- [32-r6完整材质Shader](32-stage-effect-r6-full-material.md)：三个完整材质入口的历史扩展。
- [33-r7全游戏离线专用Shader](33-global-offline-shaders.md)：24种PS运算与63个PC预编译GXP的来源；默认配置以r8为准。
- [34-r8默认开启实机测试路径](34-hardware-test-defaults.md)：最新hardware-test默认、Group/Half/Tiled/Blur/Scene实际选路、旧标准配置展开及日志核对。

- [36-r10全游戏材质组合覆盖](36-material-coverage.md)：新增17个有限入口、独立开关、真实离线成本、原生回退和交付身份。

- [37-r10长时间实机日志](37-r10-long-hardware.md)：剩余重场景及UI大图集失败证据。
- [38-r11 UI恢复与组混合](38-r11-ui-group.md)：当前程序/资源配套、全游戏组混合扩展、验证和回退。

- [39-r12存读档高亮焦点修复](39-r12-save-focus.md)：格子/页码焦点一致性、真实触屏与旧hit区分、重新打开恢复、130项host测试与交付身份；本轮不更新资源，实机验收待进行。
