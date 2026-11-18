# 历史证据

本目录保留有版本归属的 JSON 收据，用于追溯源码、输入、构建、失败和设备观察。
其中的“当前”、路径、哈希和测试数量均属于当时快照，不是最新安装教程；
临时文件也不保证仍存在。

当前操作见[文档入口](../README.md)。本次资源/VPK 身份由交付目录的
`delivery.json`、`asset-validation.json`、`companion-binding.json` 与项目
[vita_asset_binding.json](../../knife/knife/data/vita_asset_binding.json) 共同记录。

| 主题 | 主要收据 |
|---|---|
| 原图制作及语言/缩放审计 | [13](13-native540-art-quality.json)、[原版 PNG](original-switch-png-20261114.json)、[缩放复核](original-switch-scaling-20261114.json) |
| 原生渲染、shader 与脚本盘点 | 09、12、17、24–36 |
| 历史实机性能与 UI 修复 | [r3](27-stage-effect-r3-hardware.json)、[r10](37-r10-long-hardware.json)、[r11](38-r11-ui-group.json)、[r12](39-r12-save-focus.json) |
| 资源恢复、背板、字号和整合 | [r14](42-r14-resource-guard.json)、43–48、52 |
| 历史资源候选与提取调查 | 50–58 |
| 历史最终 103 文件验收锁 | [59-final-assets-lock.json](59-final-assets-lock.json) |
| 历史源重放门禁与验收 | [60](60-source-reproduction-guards.json)、[61](61-source-exact-replay.json) |
| 早期已移除文档的提交和路径 | [history-index.json](history-index.json) |

59 是历史包的文件锁，61 对应旧入口、旧 logo 与当次缓存续建；
不能要求当前请求源的 logo 和新清单继续匹配这两项旧身份。
历史收据内容保留原样，构建通过、字节一致和实机运行分别判断。
