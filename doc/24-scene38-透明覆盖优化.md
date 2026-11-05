# scene38 透明覆盖优化（r4）

2026-11-05，本地版本 `stage-effect-r4-sparse-20261105`。本轮先实施 [23](23-scene38-PC1246-脚本素材与融合可行性分析.md) 中能单独验证的 IMG0436 星点层透明覆盖优化；未加入 layer35→34 融合，也未修改 group0 的 NoAlpha/mask 合成。没有新的实机性能或画面对照结果。

## 实际改动

scene38/PC1246 附近 layer26 使用 IMG0436 的 Sprite/op7 路径。原先即使大部分 texel 透明，仍提交完整四边形。本版在原始 Quad 的顶点生成阶段，把已证明需要绘制的区域编码为 **68 个互不重叠的矩形、408 个顶点，共用一次 Draw**。保留原纹理、位置变换、UV、混合顺序和 shader29/17。即使关闭 framebuffer fetch，也只为这一个原始 Quad 产生一次背景复制，不会为 68 个矩形分别复制或开 pass。

纹理上传时先核对内嵌素材清单、归档、条目、尺寸、长度及完整 KIMG SHA-256，再给唯一拥有的 GpuTexture 设置不可变覆盖标记。GPU 重新驻留保留该身份；可写目标或内容版本发生变化则不采用覆盖表。身份不匹配时走原完整 Quad。

| 身份项 | 值 |
|---|---|
| 归档、索引、文件 | `jallpac` / `1093` / `IMG0436.NXZ` |
| 逻辑尺寸 / 物理尺寸 | 1750×1165 / 624×415 |
| KIMG 字节数 | 524,384 |
| 清单 SHA-256 | `0f161f5c868f109cbcaca0da8db2c57a8a1cce7260ad2d0335ba70952e3ae1a3` |
| KIMG SHA-256 | `d40403d93424babb37a4a40f956cf9df19763467f7d96f089f96f00398fd2e38` |

仅准入未修改的完整 UV、有限仿射位置、无 clip/rule/soft/masked/haze 的 Sprite + DodgeAlpha。镜像 UV、裁剪 UV、非仿射位置、其他材质及所有未知状态均回退。CPU 调色、raw 和 strip 不会取得这个原始 GXT 标记。

## 素材覆盖证明

`tools/audit-vita-sparse-coverage.py` 只读当前运行归档，按 DXT5 端点和索引判断 alpha 是否严格大于零，随后向外扩张两个源 texel，按 16 px tile 合并。不能先用 8-bit PNG 的 alpha 判空：原数据有 132 个极小但非零的 alpha，经 8-bit 取整后变成零。本版已保留这些像素及其过滤邻域。

| 指标 | 像素数 |
|---|---:|
| 原完整源域 | 258,960 |
| DXT5 精确非零 alpha | 7,068 |
| 解码为 u8 后非零 alpha（仅对照） | 6,936 |
| 扩张两 texel 后的支持域 | 20,051 |
| 最终矩形覆盖 | 67,696 |
| 遗漏的扩张支持域 / 重叠像素 | 0 / 0 |

源域覆盖减少 **73.9%**。这是单个星点层的几何覆盖变化，不是 GPU 耗时或整场景 FPS 的实测值。增加的顶点也有开销，最终收益须由相同场景日志判断。源域证明不等于实机逐像素等价；仿射顶点插值和 GXM 光栅边界仍需画面回归。

## 验证与交付

- 游戏 host 测试 115 项、渲染 host 测试 127 项、perf 测试 3 项通过。
- 原生特效输入检查 933 个用例、2,799 次检查通过，五个负向变异均被检测。
- 原生图像 ASAN/UBSAN 检查、SHA-256 已知答案检查、ARM 编译/链接及 Rust 图像接口 12 项测试通过。
- 完整 Vita release 构建、VPK 内容/SFO 检查通过，标题 `KNIF00540`、`ATTRIBUTE2=12`。scene/composite 保持 960×540。
- host 游戏构建另加 `GROUP_SHAPES` 的目标平台条件保护；Vita 统计逻辑保持原样。

交付仍为 `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/eboot.bin` 和 `knife-vita-540p.vpk`。详细身份及测试日志摘要见 [证据 29](evidence/29-stage-effect-r4-sparse.json)。已有配套素材无需重传。本轮没有改变 runtime shader 源身份，无需因本改动重新建立 shader 缓存。

默认 `KNIFE_PERF_CONSOLE=1`、`KNIFE_VITA_QUIET=0`，启动及性能窗口包含版本标识。`SPARSE_SOURCE` 中 `candidate=true verified=true` 表示源身份命中；`sparse_coverage_hit`、`sparse_coverage_tiles`、`sparse_coverage_fallback` 分别统计顶点生成命中、输出矩形数和标记源的材质回退。命中时每个原始 Quad 为 68 个矩形、408 个顶点。这些计数发生在命令生成阶段，缓存复用可能不重复计数，不能解释为每个显示帧的实际 GPU 执行次数。

字段精确定义：`sparse_coverage_tiles.calls` 是网格生成命中次数，矩形累计数在 `aux`；`sparse_coverage_hit.aux` 是顶点累计数，每次命中 408。已有 `perf-enable.txt` 仍优先于编译默认值：数据根目录优先，其次 `assets/`；文件内容为 `0` 会关闭采集，内容为 `1` 才开启，修改后需重启。详见 [日志开关](06-性能采集与分析.md)。

同一 EBOOT 的可逆对照：在 `ux0:data/WitchontheHolyNight/optimization-profile.txt` 写入 `all -sparse-coverage` 并重启可关闭本优化；默认是 `all`。r3 的 `ps-op7` 专用程序保持启用。

本地分支为 `perf/render`，改动未提交、未推送。实机画面、命中率、PC1246 同阶段帧时间及最坏帧时间尚待新日志验证，不能据 host/ARM 构建声称已达到 20 FPS。
