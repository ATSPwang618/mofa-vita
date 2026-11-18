# 魔夜 Vita 资源流水线

仓库入口为 `prepare-mofa-vita.py`，默认输出 `dist/ux0/data/WitchontheHolyNight/assets/`。按本体、更新、澄空 MOD 的顺序合并输入，保留本项目的 70% 变体与固定 PVRTC 策略，音频输出为 `voice.mrg`、`bgm.mrg`。

操作说明集中维护在以下文档：

- [资源构建：输入、工具准备、样本验证、全量构建与 CUDA](../doc/10-Switch原版资源提取.md)
- [Asset build guide (English)](../doc/10-switch-asset-extraction.en.md)
- [VPK 构建与工具链](../doc/03-构建与工具链.md)
- [交付目录与安装](../doc/08-素材与安装.md)

全量构建须通过与输入、转换代码绑定的样本门禁。完整交付验收后自动同步项目清单、启动文件表、图集索引与音频索引；VPK 构建检查 `knife/knife/data/vita_asset_binding.json`，防止内嵌元数据与资源错配。主机验收不等同于 Vita 实机验证，零缓存全量重建尚未独立验收。

`production/` 保留 main `09d9a49a653629f9a6cc1cae08df38259b629cbb` 的必要生产依赖，逐文件来源 SHA 见 `production/upstream.json`。其中包括 127 项历史编码二进制差分及三个自编译宿主辅助程序，源码与许可见 [source_replay 说明](production/tools/source_replay/README.md)。构建不依赖外部 `tools/` 转换脚本或月姬工程。

本目录的 `.runtime/` 是本机工具缓存；工具、密钥和游戏成品不提交 Git。首次运行仍需合法取得的密钥、固件字体与 SDK 编码工具等输入，详细要求见资源构建文档。Pillow 许可保留在 [PILLOW-LICENSE.txt](PILLOW-LICENSE.txt)。
