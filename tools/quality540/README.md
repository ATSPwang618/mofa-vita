# 历史 540p 基础图制作

本目录用于早期剧情基础图重建及整图 RGBA 预算检查。
当前完整资源另有离线变体、KQTL 分块、分卷和两个音频 bank，
统一使用[当前流水线](../../doc/10-Switch原版资源提取.md)。

## 工具

| 脚本 | 用途 |
|---|---|
| `stage_originals.py` | 按归档名与索引准备来源可核验的原图 |
| `rebuild_scene_art.py`、`rebuild_all_art.py` | 定向/全库重建，保留逻辑坐标，独立解码核验输出 |
| `limit_runtime_art.py`、`verify_runtime_art.py` | 基础条目整图 RGBA 离线预算及超限拒绝 |
| `test_limit_runtime_art.py` | 预算处理回归 |

例：从自备 RomFS 生成独立候选目录，具体参数见脚本帮助：

```powershell
python tools/quality540/rebuild_all_art.py --assets <基础包> --romfs <原始RomFS> --output <新目录> --jobs 4
```

按历史合同使用 HD×0.5、普通图×0.75 的采样密度，
长图保留平移内容、语言条带和原逻辑尺寸；不把每张图都塞进 960×540。
DXT 是有损编码，候选以原图和黑白底合成质量比较，不能把已经压缩的输出当无损母版。
`identity-preserve.json` 仅对匹配身份的条目生效。

基础条目的整图 RGBA 上限不直接套用于所有新增 KQTL 变体。
当前预算与运行机制见[架构](../../doc/01-架构总览.md)，历史验证见
[13-native540-art-quality.json](../../doc/evidence/13-native540-art-quality.json)。
修改实际运行包后必须统一同步清单/索引并重建程序，见[构建](../../doc/03-构建与工具链.md)。
