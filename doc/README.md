# 项目文档

按任务选择入口；安装说明不再混列旧包哈希，构建参数和资源制作步骤各维护一处。

| 要做的事 | 文档 |
|---|---|
| 安装、升级、阅读与背板设置 | [08 安装与设置](08-素材与安装.md) |
| 从本体、更新和 MOD 制作资源 | [10 资源制作](10-Switch原版资源提取.md) · [English](10-switch-asset-extraction.en.md) |
| 编译引擎、打包 VPK、维护工作目录 | [03 构建与工具链](03-构建与工具链.md) |
| 理解模块、渲染、缓存和音频 | [01 架构与运行机制](01-架构总览.md) |
| 采集日志、定位故障、比较性能 | [06 诊断与性能](06-性能采集与分析.md) |
| 确认哪些能力还未验收 | [07 限制与验证边界](07-已知限制与未验证项.md) |

工具入口见 [tools](../tools/README.md) 和 [资源流水线](../vita_asset_pipeline/README.md)。
具体交付身份读取交付目录的 `delivery.json`、`companion-binding.json`，
项目绑定见 [vita_asset_binding.json](../knife/knife/data/vita_asset_binding.json)。

旧优化数据、候选包和维护过程集中在 [历史证据](evidence/README.md)；
已合并的章节可通过 Git 历史找回，不再作为并行操作教程。
源码实现、主机检查、构建通过和实机验证分别说明，历史结果不自动归属于新版本。

[项目首页](../README.md) · [English](../README.en.md)
