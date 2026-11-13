# 验证证据索引

收据固定记录当次源码、输入和程序身份；历史测试与实机结果不能直接归给最新构建。当前字号成品看47，前次维护状态看46，实际硬件性能仍需对应版本日志。旧临时路径可能已清理。

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
