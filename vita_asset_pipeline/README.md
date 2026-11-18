# 魔夜 Vita 资源构建

入口为 `prepare-mofa-vita.py`，默认输出 `dist/ux0/data/WitchontheHolyNight/assets/`。
先合成本体与更新，再覆盖澄空 MOD 的同名 RomFS 文件；默认路径见 `switch.py`，
可通过 `--base/--update/--mod/--keys/--firmware` 调整，原始输入不会修改。

使用 Windows x64 Python 3.13.1，依赖及编码工具按版本与 SHA 锁定并缓存到
本目录的 `.runtime/`。首次运行需要本机合法取得的密钥、固件字体与 Vita SDK
`psp2pvrt64.exe/psp2gxt.exe`；工具缓存、密钥和游戏成品不提交 Git。

```powershell
python prepare-mofa-vita.py --setup
python prepare-mofa-vita.py --work-dir .work/mofa-asset-build --verify-samples
python prepare-mofa-vita.py --work-dir .work/mofa-asset-build --gate .work/mofa-asset-build/samples-main/verification.json --jobs 8
python prepare-mofa-vita.py --check
```

样本门禁绑定输入来源及转换代码，必须通过才能全量构建。阶段断点与 SHA 校验缓存
允许续跑；完整输出通过验收后发布，不覆盖已有成品。验收覆盖全部物理文件、音频
bank 成员、图集/分卷/纹理合同及内存预算；这些是主机检查，未替代 Vita 实机验证。

保留主分支的 70% 变体缩放、保护纹理、回退图与固定 PVRTC2 4bpp 策略，
音频为 `voice.mrg`、`bgm.mrg`，没有散装 `stream/`。101 个物理文件须与历史锁
逐字节一致；指定源的 `movie/logo.mp4` 及实际清单另行验证。

NVIDIA 机器可先运行 `--setup-cuda`，再在样本和全量命令中都加
`--image-backend cuda`。CUDA 用于有收益的大图缩放、调色与 BC3 解码，70% 阶段匹配
Pillow RGBA8 和整数 Lanczos；有差异时回退 CPU。DXT 编码仍使用锁定的 ISPC，
PVRTC 仍使用原 SDK 编码器，以最多 8 个独立进程拆分纹理块，逐块和整包 SHA 均须一致。
默认 CPU；未启用 CUDA 时不安装其依赖。Pillow 许可见 `PILLOW-LICENSE.txt`。

完整交付后自动把实际清单、启动文件表、图集索引和音频索引同步到项目，并写入
`knife/knife/data/vita_asset_binding.json`。现有包可用 `--sync-project` 重新验收并同步。
`tools/build-vita.sh` 的 game 构建先检查绑定，拒绝清单与索引不一致的 VPK。

```powershell
python vita_asset_pipeline/build_binding.py --check
wsl -d pocket-ubuntu -- bash /mnt/d/AI-CODEX/mofa-vita/vita_asset_pipeline/packaging/build-companion.sh
```

配套入口复用主分支的编译、链接与签名流程，输出 `dist/mofa-vita.vpk`、
`release/mofa-vita.vpk` 和 `dist/ux0/app/KNIF00540/eboot.bin`，不改 Rust 运行逻辑。
VPK 不内置 Sony 着色器编译模块，沿用主分支的嵌入着色器及设备外部模块查找。

`production/` 为 main `09d9a49a653629f9a6cc1cae08df38259b629cbb` 的必要生产依赖；
逐文件来源 SHA 见 `production/upstream.json`。构建不依赖外部 tools 转换脚本或月姬工程。
其中包括 127 项历史编码二进制差分及三个自编译宿主辅助程序，不能称为纯元数据；
对应源码、工具锁与许可见 `production/tools/source_replay/README.md`。
