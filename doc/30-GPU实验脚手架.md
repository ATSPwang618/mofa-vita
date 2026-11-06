# GPU 实验脚手架（默认关闭）

`knife-render-api::experiments` 提供第 5 阶段的主机侧候选验证和资源会计。模块只保存可复现的输入、输出和回退原因，没有接入生产绘制路径；`ExperimentFeatures::default()` 的四个开关全部为 `false`。

## Half Color

`validate_half_color_candidate` 同时保留 FP32 `uv`、`wpos`、flags、denominator，以及 native `UCHAR4` 和候选 `RGBA8` 字节。任一 RGBA8 字节不同就拒绝候选，并把 `default_promotion_applied` 固定报告为 `false`。这只证明候选字节相等，不能替代实机像素和性能证据。

## Tiled RGBA8 target

`probe_tiled_target` 只接受 `data_is_null + RGBA8 + render_target`，并返回 `TargetStorageKind::TiledRgba8Candidate`。布局记录逻辑行字节、tile 后行字节、逻辑总字节、padding 字节和 padded 总字节。读回必须走 CPU 线性 staging；没有 staging 能力时返回安全回退。R8、压缩、图片和电影路径不会被该 probe 接纳。

## Blur weights

`BlurWeightCache` 以 `sigma.to_bits()` 和半 tap 数 `n` 为键，按 shader 的 `k = -n ..= n` 顺序计算原公式。缓存最多 16 组、权重 payload 不超过 4 KiB；`n > 16`、无效 sigma 或预算不足都保留原 shader 计算。

## Scene capacity

`select_scene_capacity` 先执行驱动显存 preflight，再尝试分配请求的 1/2/4 场景容量。多场景 preflight 或分配失败会调用 cleanup 并重试容量 1；容量 1 失败会在 cleanup 后返回错误。该函数不宣称驱动可用容量，也不会改变现有场景 ring。

只有在 RGBA8 一致性、分配失败清理和实机帧时序证据齐备后，才能在调用方显式开启对应 gate。主机测试覆盖候选拒绝、padding 计算、缓存上限和 fallback 清理；测试通过不代表 Vita 硬件收益。
