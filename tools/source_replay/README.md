# 固定历史资源重放配方 / Locked historical replay

本目录服务于 `tools/rebuild_final_assets.py`；不是另一个游戏运行包。操作只看 [中英提取教程](../../doc/10-Switch原版资源提取.md)。目标由不可改写的103文件交付锁验收。

## 必须保留

- recipe、bank-contracts、image-history、original/ui-containers：源条目、历次处理、封装、重名序号及分卷合同。
- grade-luts：生产调色结果表；audio-contract：Ogg页身份与序列号；pvrtc-selection：固定入选条目。
- manifest三份、language-profile、misc-banks、source-identities/source-contract：固定的元数据与输入身份。
- historical-corrections：**127项历史二进制编码差分**。包含游戏派生数据，不能称为纯元数据或据此宣称完整重现旧编码算法。先校验源生成中间结果、再验证差分与最终SHA；绝不从旧运行包静默取缺失资源。
- toolchain-lock：工具、Python版本、生成源码、项目图片、MOD与本目录文件的SHA。不能修改它来掩盖转换错误。

影片先用固定单线程filter/x264生成确定性中间结果，再应用历史编码差分。旧四线程英文片头两次编码存在不同字节，不能直接作为稳定的补丁输入。中英影片分别两次独立编码一致之后才锁定。图片转换仍使用3个有界工作进程。

## 宿主辅助程序与源码

1. `legacy-compact-replay.exe`：源码在 `legacy-compact/`，保留Cargo.lock；调用仓库已有bc7enc与wrapper，重放早期BC7/JPEG缩放。Windows x64 GNU构建，依赖及上游许可沿用各自源码。其作用不是运行游戏。
2. `native-color-blocks.dll`：源码 `../native-color-blocks.c`，离线颜色细化。
3. `reciprocal_compat.dll`：源码 `reciprocal_compat.c`；仅在独立宿主编码进程中适配SHA固定的ispc_texcomp 1.0.1 Windows轮子。模式0/1重放AMD/Intel倒数舍入；不修改磁盘DLL或游戏。Intel表源于Bochs，见源码版权及 `COPYING.LGPL-2.1`；该部分按LGPL v2或后续版本保留。

这些是自建辅助程序，不含Sony官方编码器、密钥、NSP或固件。用对应C编译器的共享库选项可构建两份DLL；Rust辅助程序在其目录运行 `cargo build --release --locked`。精确复现以已锁定二进制为准，重新编译后的字节差异必须重新审查，不能覆盖旧工具锁冒充同一配方。

运行时只需上述源输入、此版本配方和锁定工具，不依赖 .work 私人kit、旧dist运行包或PNG工程。对外分发本目录时仍须处理历史差分及依赖各自的权利与许可；源码的MPL许可不把游戏派生内容变成MPL授权内容。
