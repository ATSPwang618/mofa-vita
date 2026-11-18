# 从 Switch 输入制作 Vita 资源

当前入口为仓库根目录 `prepare-mofa-vita.py`，转换代码和依赖集中在
[vita_asset_pipeline/](../vita_asset_pipeline/README.md)。
默认输出 `dist/ux0/data/WitchontheHolyNight/assets/`。
旧 `tools/rebuild_final_assets.py` 重放历史交付，用于调查，不再作为当前安装教程。

## 输入和环境

先合成本体与 1.02 更新的 RomFS，再覆盖澄空 MOD 的同路径文件。
MOD 同名文件优先，原输入不修改。默认路径以 [switch.py](../vita_asset_pipeline/switch.py) 为准：

| 输入 | 默认位置 |
|---|---|
| 本体 | `C:/Users/26241/Desktop/switch魔夜原数据包/WITCH ON THE HOLY NIGHT v0 (010012A017F18000) (BASE).nsp` |
| 更新 | 上述目录下 `魔夜重制版-switch版/魔法使之夜1.2升级档/1.02补丁/WITCH ON THE HOLY NIGHT v131072 (010012A017F18800) (UPD).nsp` |
| MOD | 上述目录下 `魔夜重制版-switch版/魔夜澄空汉化mod(12月27日更新)/010012A017F18000/romfs` |
| 密钥 | `%APPDATA%/Ryujinx/system/prod.keys` |
| 共享字体 | 从 `%APPDATA%/Ryujinx/bis/system/Contents/registered` 固件中提取三个匹配的 BFTTF |

可用 `--base`、`--update`、`--mod`、`--keys`、`--firmware` 覆盖默认值。
本入口使用合并 RomFS 的 `movie/logo.mp4`，不要求历史桌面定制 logo。

使用 Windows x64 Python **3.13.1**，固定 Pillow **11.3.0**、NumPy **2.3.3**、
ispc_texcomp **1.0.1**，缺少的包安装到流水线 `.runtime/`。
Hactool 有校验下载入口；FFmpeg **8.0** 必须匹配工具锁，可用 `--ffmpeg` 指定。
Vita SDK `psp2pvrt64.exe`、`psp2gxt.exe` 首次从本机
`D:/unity/PSVITA/sdk/host_tools/bin/` 复制到工具缓存并校验 SHA。

只有原数据文件夹仍不足以在空白机器重建，密钥、固件字体和编码环境是额外依赖。
脚本支持无转换缓存启动；截至本次交付，未完成清空全部转换缓存后的单次全量重建验收。
已有验收采用分阶段转换与已验证缓存续跑。

## 样本、全量和续跑

从仓库根目录执行：

```powershell
python prepare-mofa-vita.py --setup
python prepare-mofa-vita.py --work-dir .work/mofa-asset-build --verify-samples
python prepare-mofa-vita.py --work-dir .work/mofa-asset-build --gate .work/mofa-asset-build/samples-main/verification.json --jobs 8
python prepare-mofa-vita.py --check
```

样本门禁绑定源输入和转换代码签名，覆盖原图、变体、70% 缩放、回退、PVRTC、
预览与音频。代码或来源改变后重新执行样本。
同一工作目录支持阶段续跑和内容寻址缓存；不要对同一目录并行启动两个构建。
已有完整输出不会被覆盖，重建另一份用 `--output <新目录>`。

并行最多使用 8 个工作进程，子进程线程数受限。
工作目录保存提取数据、中间归档和编码缓存，所需空间显著大于约 4.69 GiB 成品；
按输入、缓存和并发输出预留空间，失败时保留现场。

NVIDIA 机器可选：

```powershell
python prepare-mofa-vita.py --setup-cuda
python prepare-mofa-vita.py --work-dir .work/mofa-asset-build --verify-samples --image-backend cuda
python prepare-mofa-vita.py --work-dir .work/mofa-asset-build --gate .work/mofa-asset-build/samples-main/verification.json --image-backend cuda --jobs 8
```

CUDA 只用于有收益的大图缩放、调色和 BC3 解码，70% RGBA8 路径匹配 Pillow 整数滤波。
原图变体不符合固定输出 SHA 时回 CPU 重算；小图保留 CPU。
DXT 仍用锁定 ISPC，PVRTC 仍用原 SDK 编码器，按纹理块有界并行。
这是局部加速，没有 CUDA PVRTC 编码实现，也不把缩放倍率当整包加速倍率。

## 验收与项目同步

保留项目既有 70% 变体策略、保护纹理、低内存 fallback、逻辑坐标、HED/NAM、
分卷和固定 11,616 项 PVRTC 选择。音频为 `voice.mrg` 与 `bgm.mrg`，
9,470 个 Ogg 加 `M10.bgmstart.mrg` 共 9,471 个逻辑成员。

当前交付为 **103 个物理文件、5,030,884,638 字节**。
101 个文件与主分支历史锁逐字节一致；
`movie/logo.mp4` 和实际清单按本次输入与门禁独立验证。
不要求这两项等于历史定制 logo 包，也不改写旧锁来接受差异。

验收检查完整物理集合、清单内容、所有音频成员、归档 SHA 和纹理预算，
成功后发布 `assets/`、`asset-validation.json`、`delivery.json` 和安装说明，
自动把清单、启动 profile、图集及音频索引同步到项目，并保存 SHA 绑定收据。

已有完整包需要重新同步时：

```powershell
python prepare-mofa-vita.py --sync-project
python vita_asset_pipeline/build_binding.py --check
```

同步先验收资源，随后按[构建指南](03-构建与工具链.md)重建 VPK。
完整资源验收、元数据绑定和实机运行是不同检查。

## 生产配方与专项开发

`production/` 固定主分支生产依赖、合同、工具锁及宿主辅助程序。
其中有 **127 项历史二进制编码差分**，必须匹配源生成中间结果与输出 SHA；
不是纯元数据，也不表示完整还原所有旧编码算法。
来源、源码和许可见[配方说明](../vita_asset_pipeline/production/tools/source_replay/README.md)。
不得为了接受不同输出改写固定锁。

原版 PNG 母版导出是独立历史工具，入口为 `tools/export-original-switch-png.py`、
`finalize-original-switch-png.py` 和 `repair-original-switch-scaling.py`。
它只处理声明的源版本，不替代含更新、汉化、变体及音频的完整包。
HD×0.5、普通图×0.75、控制图原采样，以及按生产表拆分语言、保留逻辑尺寸等规则
见脚本；历史审计见[证据索引](evidence/README.md)。

[安装](08-素材与安装.md) · [构建](03-构建与工具链.md) · [English](10-switch-asset-extraction.en.md)
