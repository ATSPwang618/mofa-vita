# GXM 后端实现

## 文件

| 文件 | 作用 |
|---|---|
| `knife/vita/gxm_backend.c` | 上下文、显示队列、渲染目标、四边形绘制、统计 |
| `knife/vita/effects_backend.inc` | 效果着色器路径：pass 管理、uniform、draw、组合成 |
| `knife/vita/shaders/effects.cg` | 17 个片元入口 + 1 个顶点入口（Cg，`psp2cgc` 编译） |
| `knife/vita/shaders/generated/effects.h` | 编译产物（GXP 字节数组），随源码提交 |
| `knife-render/src/vita.rs` | Rust 侧渲染器：目标、命令录制、pass 统计 |
| `knife-platform-vita/src/gxm.rs` | FFI 与句柄所有权 |

## 顶点格式

```
pos@0   float2   裁剪空间坐标（已在 CPU 端转换，w=1）
uv@8    float2
color@16 float4 预乘 alpha
hx@32   float4   每四边形常量（haze 水平参数）
hy@48   float4   每四边形常量（haze 垂直参数 / 文字开关位）
s0@64   float4   软裁剪参数
s1@80   float4
s2@96   float4
stride 112 bytes
```

`hx`/`hy` 在片元侧通过插值器读取（同一四边形六个顶点值相同，插值结果即原值），
只有 `fs_rim_text` 的打包 RGB24 仍走 uniform——插值的浮点误差会改掉最低位。

## pass 生命周期

```c
kv_gxm_begin_target(g, target, clear, finish_before);
//   finish_before=1 时先 sceGxmFinish（切换目标的前置同步）
//   sceGxmBeginScene + 视口 + 深度状态 + 可选清屏四边形
kv_gxm_draw_effect(...);   // 每绘制：绑定程序/纹理/uniform，写顶点，sceGxmDraw
kv_gxm_end_target(g);      // sceGxmEndScene
```

绘制提交本身的实测成本极低（80 次绘制合计 0.2 ms）；成本集中在每个 pass 开头的
`sceGxmFinish`（约 2 ms/次，驱动往返），这是重场景 190 ms 的主因。

## 状态缓存

- 片元 uniform 位置：按程序句柄缓存 7 个位置（提交时间 133.8 → 41.6 ms）。
- 视口、深度状态：按 (宽, 高) 缓存。
- 顶点环：每个 pass 开头重置（当前实现），出现覆盖风险时由 `EFFECT_VERTICES` 上限兜底。

## 着色器入口

| 入口 | 用途 |
|---|---|
| `fs_sprite` | 普通精灵（含 haze 位移） |
| `fs_noalpha` / `fs_forcealpha` | 忽略/强制 alpha |
| `fs_fill` | 纯色填充 |
| `fs_text` | 字形（R8，可带描边偏移与 4×4 盒式滤波） |
| `fs_rim_text` | 描边文字（打包 RGB24，uniform 精确传递） |
| `fs_mono` / `fs_nega` | 单色化 / 负片 |
| `fs_transition` | 转场 |
| `fs_rule` | 规则遮罩（擦除等） |
| `fs_blur` / `fs_alter` | 模糊 / 调色 |
| `fs_ps` | 24 种 PS 混合 |
| `fs_present` / `fs_present_srgb` | 画布到扫描输出 |
| `fs_group_composite` | 组像素面 × 遮罩面合成 |
| `fs_sprite_mask` | 预留（未启用，见已放弃方案） |

着色器编译：`tools/compile-vita-effects.ps1`，每个入口编译两次比对 SHA-256，确保可重复。
