# SDD ledger — plan: docs/superpowers/plans/2026-11-05-global-gpu-work.md

## Preflight scan

| scope | shared files/interfaces | finding and ruling |
|---|---|---|
| masks vs shader capabilities | `knife-perf`/audit C FFI vs GXM backend | Keep optimization options and shader capabilities as separate `u64` values; no mixed shifts. |
| plain shaders vs telemetry | `knife-render/src/vita.rs` shader selection and command execution | Shader selector changes stay local; executed-work accounting settles only after native execution. |
| group bounds vs cache | stage/group cache/retained owner | Carry conservative coverage with owner/content version; unknown means full fallback. |
| tiled targets vs copy/seal | GXM texture creation/readback | Restrict to data-null RGBA8 targets and use linear staging for readback; unsupported paths reject safely. |
| blur weights vs reflection | generated shader IDs and uniform arrays | ID53 gets a narrow weight-uniform reflection exception; larger kernels use original path. |
| scene capacity vs memory | target ring and driver budget | Preflight memory, cleanup on failure, and fall back to capacity 1. |

No plan conflict was found. Ruling: implement in bounded, independently gated phases and do not claim unmeasured FPS gains.

## Tasks

- [x] Foundation: 64-bit options/capabilities and independent switches.
- [x] Foundation: executed-work/coverage telemetry and bounded logs.
- [x] Phase 1: plain OP1/OP6/OP13 shader paths and tests.
- [x] Phase 2: Group composite coverage bounds and cache propagation.
- [x] Phase 3–6: half, tiled, blur, scene-capacity candidates and safe fallbacks.
- [x] Census/replay descriptors and fixed replay harness.
- [x] Host/native/ARM/VPK verification and r5 delivery.

交付状态：r5 候选包已本地构建并复制到 `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/`。Half、Tiled、Blur 权重和 Scene 容量仍按独立实验/回退规则运行；固定回放和离线分析器已验证，但尚无实机 GPU 像素差分或 20 FPS 结论。

## r7 全游戏离线专用化（2026-11-06）

- [x] 完整源处理扩展至OP1–24，普通和Group选路独立失败回退。
- [x] PC实际编译63个GXP，含64–87；源码与逐项二进制身份验证。
- [x] 内嵌加载优先、两个能力bank、启动/成功执行日志及位38回放。
- [x] 渲染163、游戏121、配置4、24OP数学/独立锚点/负对照和native输入/缓存测试。
- [x] ARM release构建与包检查。
- [x] 交付目录最终身份核对：外置/包内EBOOT一致，63个实际GXP载荷均在EBOOT，版本/位38/日志默认值及VPK检查通过。
- [ ] r7实机GXP驱动接受、RGBA8像素差分与固定ABBA收益验收。
- [ ] 原计划Group/Half/Tiled/Blur/Scene容量的各项实机准入和组合验收。

原Tasks打勾仅代表实现与host/构建范围，不能代表整个GPU计划已实机验收完成。设备当前未连通，未伪造FPS；仍保留20FPS为目标。

## r8 默认实机测试（2026-11-06）

用户明确要求此前GPU候选默认开启以进行实机验证；本轮覆盖旧的默认关闭交付策略。

- [x] hardware-test默认组合：全24OP、Group bounds、Half、Tiled、Blur权重和Scene容量2；保留独立关闭与条件回退。
- [x] Half49–52真实选路与成功patch能力保留；缺能力/复杂材质回FP32。Half禁止交集FP32合批，保留逐图一致精度。
- [x] Tiled使用跨tile边界及最后一行的全RGBA图案探针；通过后实际960×540目标使用Tiled，复制/读回/seal/失败清理有native测试。
- [x] 只更新EBOOT时识别精确标准旧配置并在内存展开，显式自定义配置不改写。
- [x] Rust渲染166、游戏121、配置7、render-api实验8、配置收据4项通过；native GXM66 cases/8422 checks、缓存38 cases/2660 checks及完整PS输入11144通过。
- [x] ARM release与VPK检查通过，VitaDeck日志默认开启，构建tag为stage-effect-r8-hardware-test-20261106。
- [ ] r8实机各项驱动接受、图像对照与性能收益；不得以默认启用或host检查代替硬件验收。

交付身份与路径见doc/34-hardware-test-defaults.md及doc/evidence/34-hardware-test-defaults.json。原20FPS目标仍待实机检验，默认组合的结果不能冒充单项ABBA归因。
