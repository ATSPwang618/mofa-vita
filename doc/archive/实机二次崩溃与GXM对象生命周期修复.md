# PS Vita 第二次实机崩溃与 GXM 对象生命周期修复

2026-10-28，针对用户提供的 `psp2core-1793187197-0x00017129f9-eboot.bin.psp2dmp` 完成定位、源码修复、主机回归和 ARM 发布构建。新 VPK 已替换交付目录，**尚未在实机或模拟器上执行新包**。

## 本次 dump 的直接证据

压缩 dump 为 443,408 字节，SHA-256 `5492e7d23f39a8f8a4edaa2718029cb0642b02c7e87ba2e1daa5def9f0ff57dd`。解压 core 为 6,522,540 字节。TTY note 内包含：

```text
thread 'main' (1) panicked at knife/src/game.rs:6249:13:
submitting Vita game frame: allocating deferred alteration conf_stxt01_en: GXM: effects render sync: 0x805b0017 (0x805b0017)
scene=1 pc=131
```

这把失败位置锁定在离屏 UI 图片 `conf_stxt01_en` 的同步对象申请。使用本次崩溃包匹配的旧 ELF，运行地址 `0x811f6866` 按 RX 映射换算为 ELF 地址 `0x811eb866`，定位到 `_kill_r` 内的 `udf #255`；寄存器 R2 为 6，对应 SIGABRT。因此 dump 里的未定义指令是 panic 后的主动 abort，不是该地址执行了不受支持的编译指令的证据。

冻结旧 VPK 的 SHA-256 为 `bb0a3b5bd507c2e7146dbf5c40704a455a1cba107a7e53d2e1638cf574eb1ae9`；旧 ELF 为 `d0ed97767585597bbdfb14fe1c099d5472449c94649c355831c05890768bff62`。旧源码、构建收据、dump、ELF/map、安装包已保留在本机 `.work/vita/hardware-crash-1793187197/`，没有拿新 ELF 去套旧崩溃地址。

本地 VitaSDK 将 `0x805b0017` 定义为 `SCE_GXM_ERROR_DRIVER`。对用户本地 `libgxm_es4` 的静态检查确认 `sceGxmSyncObjectCreate` 经过驱动调用，失败分支返回该值；系统模块未执行、未打入 VPK 或源码 ZIP。core 没有足以确定驱动对象池状态的数据，**不能从这个错误推出普通 Rust 堆耗尽、精确同步对象上限或所有内存问题均已消除**。

## 修正的对象使用与释放

代码审查确认了两处不必要的资源持有：普通离屏图片每张都创建类似显示缓冲区的同步对象；烘焙完成的缓存图片仍长期保留临时 render target。图片变小只降低像素占用，不会自动消除这种逐图片增长的原生对象开销。

本次改动如下：

1. 普通离屏目标不再调用 `sceGxmSyncObjectCreate`。实际显示缓冲区的同步保留；DEV 直接 scanout 只在明确请求时创建专用同步对象，发布版未开启该诊断路径。
2. 新增 `kv_gxm_texture_seal` / Rust `seal_texture`，在烘焙成功且场景已结束时，依次完成 `Finish → TransferFinish → DestroyRenderTarget`，之后才提交缓存驻留。
3. 释放临时绘制附件时，保留纹理像素块、descriptor、ready 状态与 GPU 写入来源。后续复制仍使用 GPU 路径，避免释放 target 后误读 CPU backing 导致黑图。
4. seal 失败时不提交驻留记录、不计入成功缓存；资源仍有明确 owner，后端进入关闭状态。已有缓存复用时不重复烘焙；未增加每帧全量 LRU 遍历。

对应实现位于 `knife/vita/effects_backend.inc`、`knife/vita/gxm_backend.c`、`knife/knife-platform-vita/src/gxm.rs`、`knife/knife-render/src/vita.rs`。此次保留此前 sRGB 64-bit/HALF4 与线性/显示 32-bit/UCHAR4 的输出配对修复。

图片仍是 270p PNG/JPEG，内部画布仍为 480×270、拉伸到 960×540，单张全屏 RGBA8 对齐后 512 KiB。没有扩大 CDRAM、映射 MAIN、普通堆或纹理缓存预算，没有重做或替换游戏 assets。同步完成点在缓存烘焙阶段会有等待成本，实机帧时间和切换耗时仍需测量。

## 已执行的验证

| 验证 | 结果 | 证据 |
|---|---|---|
| 修复前 C 回归 | 21 案例、4,933 断言中 193 处失败，新增对象压力用例检出缺陷 | 本机 `.work/vita/gxm-copy/20261028T114841Z-291/receipt.json` |
| 当前实际 C 后端：画布、copy、输出、资源释放 | 26 案例、5,746 断言，ASan/UBSan 通过 | [vita-gxm-copy-host-tests.json](../vita-gxm-copy-host-tests.json) |
| 当前 viewport/BeginScene 失败注入 | 9 案例、215 断言，通过 | [vita-gxm-viewport-host-tests.json](../vita-gxm-viewport-host-tests.json) |
| 当前特效输入 | 610 案例、1,830 断言，通过，5 个故意错误变体均被检出 | [vita-effects-inputs-host-tests.json](../vita-effects-inputs-host-tests.json) |
| 当前呈现与清理 | 18 案例、418 断言，通过；错误变体检出 | [vita-gxm-present-host-tests.json](../vita-gxm-present-host-tests.json) |
| 完整 Rust 渲染套件 | 64 passed、0 failed、0 ignored；44 个选定源文件测试期间身份未变 | [vita-render-host-tests.json](../vita-render-host-tests.json) |
| ARMv7 正式版构建 | `wsl -d pocket-ubuntu -- bash tools/build-vita.sh game release` 成功；180 源文件和 9 产物身份核对 | [vita-game-release-build.json](../vita-game-release-build.json) |
| VPK 与内含 eboot/SFO | CRC/哈希一致，两套检查均为 0 错误 | [vita-270p-vpk-validation.json](../vita-270p-vpk-validation.json) |

关键回归包括：禁止离屏同步对象申请时连续烘焙 96 张图片；只给一个额外临时 render target 时 96 张图片仍能完成；下一帧缓存命中不再烘焙；seal 失败后驻留/计费不提交并可验证重试；GPU 写入来源在释放 target 后继续保留。主机 mock 只验证调用关系、对象生命周期和错误处理，不能代替实机 GXM 驱动执行或像素验证。

解码器未在此修复中修改；先前 JPEG/PNG 独立套件仍作为历史证据，不冒充此次重新执行的测试。历史模拟器运行使用更早安装包，也不作为新包实机稳定性证明。

## 当前交付和实机复测

新 VPK：`dist/Witch-on-the-Holy-Night-Chengkong-270p-Vita/knife-vita-270p.vpk`。

```text
大小：4,542,233 字节
SHA-256：55ae023fc25d833a98bc209872266e8513318b42fbda303e83e13bdbce26658a
TITLE_ID：KNIF00270
APP_VER：00.01
```

匹配 ELF SHA-256 为 `7d4d75c0ba3767c296da46b5606535ff5f73e67490d48f7698bac58839816b15`，eboot SHA-256 为 `39f356c317e790ab94dc6ff9b5e6438ec5997ebd8ba22e9ba24f5e8bf394f16d`。构建使用 production 存档空间，无自动退出帧数限制，关闭 DEV 逐帧诊断。完整机读证据见 [vita-gxm-sync-crash-fix-1793187197.json](../vita-gxm-sync-crash-fix-1793187197.json)。

实机上使用 VitaShell 覆盖安装这个 VPK 即可，继续复用 `ux0:data/knife-woh/assets/`，无需重新传输约 1.83 GiB 的资源。不要删除 `ux0:data/knife-woh/saves/`。标题 ID 和版本字段相同，通过上述哈希辨认新旧包。

优先验证进入标题/设置菜单，反复打开和关闭系统菜单、设置、回看，再推进剧情和转场，核对画面、颜色与透明边缘。若需要记录性能，在 `ux0:data/knife-woh/perf-enable.txt` 写入无 BOM ASCII `1` 后重启，结束测试后先复制 `performance.log`，因为下次启动会重建。若仍崩溃，保留新的 dump 和对应操作；当前包的 ELF/map 已本地保留，可做下一次准确符号定位。

源码 ZIP 更新为匹配当前构建的本地工作树快照，资源不重复塞入源码包。所有修改留在 `work/woh-540p-chengkong`，未提交、未推送，未查询远端引用。
