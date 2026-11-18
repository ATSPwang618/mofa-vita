# 验证证据索引

2026-11-17 提取教程与70%包复核：[53-assets70-extraction-audit.json](53-assets70-extraction-audit.json)，103个物理文件、9,571项完整SHA、44项启动profile通过；区分历史原图来源、现存转换入口与缺口，未改资源/重打程序。

2026-11-17 书签栏按钮禁用：[52-bookmark-panel-lock.json](52-bookmark-panel-lock.json)，70%候选程序中禁用快退/快进按钮，6项控制策略测试、ARM构建和包内外EBOOT核验通过；实机待验。

2026-11-17 70%材质候选：[51-assets70-candidate.json](51-assets70-candidate.json)，实测4.8556 GiB，比80%再减509.9 MiB；资源检查、175项串行host、ARM构建与程序配对通过，实机待验。

2026-11-17 80%材质候选：[50-assets80-candidate.json](50-assets80-candidate.json)，资源容量6.73→5.35 GiB；完整性、串行host与ARM构建通过，实机待验。

2026-11-17 文档与死代码维护：[49-documentation-maintenance.json](49-documentation-maintenance.json)。该记录区分本轮检查和此前构建/实机结果。

收据固定记录当次源码、输入和程序身份；历史测试与实机结果不能直接归给最新构建。当前large-image/R10交付及2026-11-17清理汇总看[48](48-large-image-integration.json)，字号47和维护46均为历史快照，实际硬件性能仍需对应版本日志。旧临时路径可能已清理。

## 当前与近期证据

| 收据 | 范围 |
|---|---|
| [47-story-font.json](47-story-font.json) | 剧情字号、重排、滚动及本轮程序验证 |
| [46-repository-maintenance.json](46-repository-maintenance.json) | 字号接入前维护、自动验证与清理拦截 |
| [45-release-path.json](45-release-path.json) | 固定 release 输出规则的首次构建 |
| [44-ux-headroom.json](44-ux-headroom.json) | 显存余量误拒绝的选择性接入 |
| [43-ux-selective.json](43-ux-selective.json) | 背板、PS 提示、CI 首次接入 |
| [42-r14-resource-guard.json](42-r14-resource-guard.json) | r14 资源恢复与保护存档 |
| [41-r13-panel-lock.json](41-r13-panel-lock.json) | r13 剧情控制禁用 |
| [40-repository-maintenance.json](40-repository-maintenance.json) | 2026-11-06 历史清理 |
| [39-r12-save-focus.json](39-r12-save-focus.json) | r12 存读档焦点 |
| [38-r11-ui-group.json](38-r11-ui-group.json) | r11 UI 原始成员读取与 Group |
| [37-r10-long-hardware.json](37-r10-long-hardware.json) | r10 实机长日志与帧率分析 |

## 早期调查

其余收据按原编号保留，涵盖资源格式、崩溃、音频、脚本盘点和渲染优化；以下名称即原始调查主题：

- [09-render-audit-evidence.json](09-render-audit-evidence.json)
- [10-asset-copy-evidence.json](10-asset-copy-evidence.json)
- [12-hardware-render-evidence.json](12-hardware-render-evidence.json)
- [13-native540-art-quality.json](13-native540-art-quality.json)
- [14-logo-media-update.json](14-logo-media-update.json)
- [15-resource-upload-recovery.json](15-resource-upload-recovery.json)
- [16-high-art-memory-crash.json](16-high-art-memory-crash.json)
- [17-texture-alpha-palette-audit.json](17-texture-alpha-palette-audit.json)
- [18-vitadeck-log-reception.json](18-vitadeck-log-reception.json)
- [19-offline-story-memory-budget.json](19-offline-story-memory-budget.json)
- [20-save-load-target-recovery.json](20-save-load-target-recovery.json)
- [21-bgm-pcm-buffer.json](21-bgm-pcm-buffer.json)
- [22-extra-ui-direct-ranges.json](22-extra-ui-direct-ranges.json)
- [23-auto-only-input.json](23-auto-only-input.json)
- [24-stage-effect-r2.json](24-stage-effect-r2.json)
- [25-stage-effect-r2-rollback.json](25-stage-effect-r2-rollback.json)
- [26-stage-effect-r3-ps7.json](26-stage-effect-r3-ps7.json)
- [27-stage-effect-r3-hardware.json](27-stage-effect-r3-hardware.json)
- [28-scene38-fusion-feasibility.json](28-scene38-fusion-feasibility.json)
- [29-script-census-summary.json](29-script-census-summary.json)
- [29-stage-effect-r4-sparse.json](29-stage-effect-r4-sparse.json)
- [33-offline-shaders.json](33-offline-shaders.json)
- [34-hardware-test-defaults.json](34-hardware-test-defaults.json)
- [35-op7-work.json](35-op7-work.json)
- [36-material-coverage.json](36-material-coverage.json)

已移除的逐轮文档可用 [history-index.json](history-index.json) 查原提交/路径。字号构建与检查日志位于 `.work/story-font-20261113/`；此前维护日志位于 `.work/cleanup-20261113/`，不随源码分发。

- [54 全尺寸PVRTC2候选](54-pvrtc100-candidate.json)：容量、转换边界、全量资源核验、测试、EBOOT/VPK哈希；实机未验证。

- [55 70%混合纹理候选](55-pvrtc70-candidate.json)：源包哈希、尺寸/回退保留、容量、配套程序身份与日志配置；实机未验证。

- [56 最终70%混合包提取教程审计](56-final-mixed-extraction-audit.json)：真实NSP有界读取、生产提取函数往返、真实PVRTC重编码、全包校验与定向清理；不是完整NSP端到端重建。

- [57 NSP重建验证](57-nsp-reconstruction-audit.json)：恢复配方、私人补充输入、真实BASE解包、完整资源重建及配套程序检查；请以status字段判断整链是否完成，实机验证另列。

- [58 原始输入来源审计](58-reconstruction-source-audit.json)：MOD文本逐字节重放、字体直拷、更新索引及历史制作链声明；不含全包重编码。
- [59 最终交付文件锁](59-final-assets-lock.json)：唯一最终assets的103个物理文件SHA，不是生成配方。
- [60 复现门禁与文档清理](60-source-reproduction-guards.json)：测试、CLI拒绝检查、删除的冗余文档身份；该收据生成时完整源重建目标尚未完成；后续结果见61。

- [61 源文件精确重放与清理](61-source-exact-replay.json)：完整103文件验收、实际源转换缓存复用、正式入口运行、确定性影片、历史编码差分范围与磁盘清理；不含新的实机测试。
