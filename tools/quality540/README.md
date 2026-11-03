# 540p 演出素材清晰度修复

当前部署在原图重建后还执行了 **全库离线显存限额**：提前缩小所有可能展开为大 RGBA 的剧情彩色图，单张按原生行距/页计费不超过 1 MiB。入口为 `limit_runtime_art.py` 和 `verify_runtime_art.py`，并已接入 profile 生成时的超限拒绝。已处理 20,181 个条目；完整规则、验证和配套更新见 [离线显存限额](../../doc/14-演出材质离线显存限额.md)。这会降低普通显示时同一素材的清晰度，优先减小潜在演出工作集。

下面的 2026-11-01 原图重建规则及尺寸对比记录历史高清输入；其中大于当前限制的输出必须再经过上述限额步骤才能部署。可编辑 PNG 保留为一次较高分辨率输入，运行归档使用处理后的实际尺寸。

全量入口 `rebuild_all_art.py` 扫描 `jallpac`、`jallpachd` 两大剧情图库的
所有完整图片，包括背景、CG、人物与演出图层。所有输出仍由原生 GXM
引擎在 **960×540** 画布合成。

`scenes.json` 是最初定位的雨中道路、撑伞人物、住宅街长图及对应变体，
第一轮用 52 个归档条目验证方法。全量处理覆盖整个剧情图库，最终数量、
保留原因与逐图质量指标由 `quality-audit.json`、`quality-report.json` 记录。

本次累计重做 **21,237 个条目**：第一轮 52 个，全量阶段再提升 21,185 个。
全量阶段扫描了两个归档的全部 33,570 条记录；图库缩略图、标记和小控制
纹理按类型保留，927 个候选因现有质量相同或更好而保留，另有 3 个微小
透明细节经原图复核后保留。后者以原条目 SHA-256 固定在
`identity-preserve.json`，后续数据变动不会被这个清单无条件跳过。

全量新增条目已经通过游戏 `Archive` 与 `gxt_decode` 路径逐图读取、解码，
21,185 张共 11,053,827,336 像素与输出 PNG 完全一致。第一轮另完成了 52 张
GXT 和 7 张原始 BNTX 的独立解码比较。上述是主机验证，不能替代实机
帧时间、显存峰值和最终画面的验证。

## 全量处理规则

- 从原始素材直接处理，重建后的图像只进行一次 DXT 编码。
- 原始 HD 素材按 0.5 密度获得 540p 采样；没有 HD 同名输入时用原普通图
  的 0.75 密度。移除把长图高度也压到 540 的限制。
- 每图总像素仍不超过 1,036,800，单边不超过 2048；普通压缩贴图通常不
  超过 1 MiB，确认需要放大的场景和已有较大素材不超过 2 MiB。
- 1050×630 这种刚越过 PoT 边界的图会选择 1024×614，保留约 95% 像素，
  避免 GPU 分配翻倍。每条记录保留实际物理尺寸与 GPU 字节数。
- 用黑、白两种底色的合成误差同时评价颜色与透明度。只有相对原图参考
  的 PSNR 提高时才替换；候选更差时保留当前图，不把清晰 PNG 再压坏。
- 从随引擎附带的游戏表中读取 84 个语言分栏图名并保护其布局；原始灰度
  蒙版、控制纹理、缩略图、UI、脚本、字体、汉化内容与音视频保留。
- 原图超过 32 MiB 的大图只在 PC 端使用较大的输入上限；输出依然遵守
  相同 Vita 内存边界。没有扩大运行时缓存容量。
- 用 zlib level 9 存放完成后的 GXT，完全相同的编码条目共享归档扇区。
  这只压缩存储字节，不再进行一次图像编码；本轮减少约 352 MiB，
  最终数据相对开始时增加约 172 MiB。

```powershell
python tools/quality540/rebuild_all_art.py --assets <当前 assets> --romfs <用户原始 RomFS> --output <新的输出目录> --jobs 4
```

默认一次处理两个归档，也可用 `--archives jallpac` 或
`--archives jallpachd` 分别处理。`identity-review`、`conversion-review`
表示需要核对的条目，不能据此宣称已经完成这些图片；交付前必须处理
这些记录，或明确说明保留原因。

## 原因与尺寸原则

旧转换把每张图片统一限制在 960×540 内。住宅街 `IMG2331` 的原图高
2940 像素，最终整张图却只有 386×540；截图对应镜头只显示其中约
353×199 的区域，再放大至 960×540。DXT 的重复编码进一步损失细节。

这里从用户持有的原始游戏素材直接解码、Lanczos 缩小，并且只进行一次
DXT 编码。完全不透明使用 DXT1，有透明度使用 DXT5，保存为 KIMG codec 9
（原生 encoded UNORM）。不经过 JPEG 中转、Alter 预烘焙或逆 LUT。

**540p 是显示与渲染画布的尺寸。** 平移、放大使用的长图需要在画面外
保留图像数据，不能把整张长图的高度也强行限制为 540。
这些源纹理允许宽或高大于 960/540，但保持以下边界：

- 每张不超过原有 `960×1080 = 1,036,800` 像素 CPU 解码上限。
- 任一边不超过 2048，压缩数据及 GPU 分配不超过 2 MiB。
- 物理尺寸不大于原 KIMG 逻辑尺寸，脚本坐标和画面布局保持原值。
- 不向运行时提供原始高清归档或额外的高清回退目录。

这次典型变化：

| 素材 | 原物理尺寸 | 修复后物理尺寸 | GPU 格式 |
|---|---:|---:|---|
| `jallpac:IMG0291` 雨中道路 | 900×540 | 1314×788 | DXT1 |
| `jallpac:IMG1192` 撑伞人物合成图 | 960×441 | 1400×642 | DXT5 |
| `jallpac:IMG2331` 住宅街长图 | 386×540 | 860×1204 | DXT1 |

渲染器仅为 GXT 放宽单边输入限制，并显式保留总像素上限。
没有调整合成算法、帧同步、缓存容量或锁帧策略。

## 复现转换

依赖 Python、Pillow、numpy、`ispc_texcomp==1.0.1`；不需要 OpenCV。
原始素材由使用者自己准备，不随仓库分发。

```powershell
python -m pip install Pillow numpy ispc_texcomp==1.0.1
python tools/quality540/stage_originals.py --assets <当前 assets> --romfs <用户原始 RomFS> --output <原图暂存目录>
python tools/quality540/rebuild_scene_art.py --assets <相同 assets> --original-images <原图暂存目录> --output <新的输出目录>
```

脚本以 NAM 名称及索引匹配目标，排除图库缩略图；使用原 `jallpachd`
中的同名图片作为离线输入，但保留目标归档的逻辑坐标。
原图暂存清单记录输入 SHA-256、原始尺寸和目标原条目的 SHA-256。
转换时再核对，且用当前图与原图的黑白底合成误差检查内容身份。
内容或颜色表现不匹配会中止，不自动覆盖。

`ispc_texcomp_py` 1.0.1 的 BC1 Python 包装器分配 `width*height` 字节，
实际 BC1 只写一半。转换脚本只取规定长度的压缩块，丢弃多余分配尾部，
然后进行 Morton 排列并独立解码核对。

局部验证入口输出包含两个重建后的归档及 HED/NAM、素材清单、
`quality-report.json`，以及对应的、由最终压缩块反解得到的 PNG。
未选中条目的存储字节保持一致。
报告中的 PSNR 是离线素材对比，不是实机帧率或实机截图结果。

更新实际 `assets` 后，必须重新生成配套元数据并构建 EBOOT：

```powershell
python tools/generate_vita_asset_profile_540.py
python tools/generate-vita-atlas-index.py dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets knife/knife/data/vita_atlas_index_540p.rs
python tools/generate-vita-stream-index.py dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets/vita-540p-assets.json knife/knife/data/vita_stream_index_540p.txt
```

EBOOT 的内嵌清单校验会拒绝新旧资源混用，更新时需一起替换归档、HED、
`vita-540p-assets.json` 和 EBOOT。NAM 内容未变。

## 产物保留

本机仅保留当前运行素材和一份可编辑图片工程。原图暂存、重建归档副本、
测试图与旧资源回滚副本已在 2026-11-02 按项目所有者要求清理；输入原图
需要从使用者自己保管的原始游戏重新准备，不作为常驻项目资源。

构建与逐图验证摘要见 [清晰度验证记录](../../doc/evidence/13-native540-art-quality.json)。
本地详细元数据报告压缩保存在 `.work/reports/native540-art-quality/`，
不含额外图片；报告中旧临时路径只记录当时的验证身份。目录约定见
[仓库维护与清理](../../doc/09-仓库维护与清理.md)。
