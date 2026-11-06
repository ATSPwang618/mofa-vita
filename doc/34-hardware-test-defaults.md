# r8：默认开启GPU实机测试路径

版本：`stage-effect-r8-hardware-test-20261106`。按用户本轮要求，将上一轮已实现的GPU候选默认用于实机测试。r7“默认关闭、等待验证”的交付策略由此测试包替代；测试启用不等于已经取得实机收益。

## 实际默认行为

默认配置名为 `hardware-test`，有效mask为 `0x4fdfffffff`。保留已有all功能，并加入以下路径：

| 功能 | 默认实际行为 | 回退 |
|---|---|---|
| 全24种PS专用程序 | 64–87按实际OP和材质全局选路；保留已有简单路径 | 单项能力失败回原程序 |
| Group最终合成范围裁剪 | 使用owner/版本/历史覆盖限制最终Composite几何，保留producer及clear | 未知覆盖或不满足RGBA恒等的混合模式全屏 |
| Half颜色候选 | 合格plain Sprite/Fill/NoAlpha的OP1/6/7/13实际使用49/50/51/52 | Haze、rule、soft、Group源及缺能力使用完整FP32路径 |
| Tiled离屏目标 | 启动图案探针通过后，960×540、无CPU初始数据的RGBA8离屏目标使用Tiled | API、分配或逐字节图案比较失败使用Linear |
| Blur权重 | 有53能力且半核n≤16时使用预计算权重与有界缓存 | 大核或能力缺失使用原程序，不截核 |
| 每目标Scene容量 | 离屏RenderTarget请求2，记录查询及选择结果；顶点环槽仍为2 | 预算或创建失败清理后回1 |

容量2和4互斥，本包选择2；4保留为独立对照。已撤回的sparse-coverage和被全24OP取代的ps-full-material不默认启用。底层 `all` 仍保留原集合，最新测试包的构建默认与启动fallback为hardware-test。

## 修复“选项开启但不执行”

上一轮Half在patch成功后仍清空能力，而且Rust选路没有实际使用49–52。本轮连接真实选路并保留成功能力；native入口同时检查half-color开关，关闭时在顶点申请前拒绝直接调用。日志中的admitted表示已能进入测试绘制，`rgba8_equivalence_verified=false`明确没有替实机做等价结论。

Half与blend-chain同时启用时，保留原有Half绘制，不进入FP32交集合批；双图层合批的交集使用FP32，不能把同一图层分成Half外侧和FP32交集。回归用例覆盖双侧Half、单侧Half拒绝合批，以及原FP32仍可合批。

上一轮Tiled不论探针结果都销毁并恢复Linear。本轮在图案逐字节匹配后记录就绪状态，为随后合格目标建立配套Tiled纹理和颜色表面。逻辑尺寸保持960×540，后备按960×544计费；不改变采样域。其他尺寸、R8、压缩图片、电影和CPU上传图片沿用原存储方式。

Tiled探针从独立Linear图案源采样到Tiled，再采样到独立Linear staging；图案跨x/y、31/32像素边界及最后一行变化，比较全部RGBA字节。任一差异均回Linear，不能靠单色块宣布布局正确。两份临时GPU纹理与CPU图案缓冲在探针后释放；探针写入不会作为游戏初始画面。

涉及Tiled的复制通过原有GPU四边形完成；读回通过强制Linear staging；CPU区域更新拒绝Tiled。seal只在完成现有GPU/transfer等待后释放附件，保留纹理布局和采样描述符。无法确定完成状态时保留closing owner，不提前释放或复用。

## 只更新EBOOT时的旧配置处理

当编译默认恰为hardware-test时，设备根目录中以下**完整标准旧值**会在启动时展开为hardware-test，磁盘文件不被改写：

```text
all
all ps-all-operations
all ps-full-material
```

去掉文件首尾空白后精确匹配。任何其他自定义配置，尤其显式 `-功能名`，继续完整覆盖默认值，不擅自重新开启。没有配置文件也采用hardware-test。交付目录配套配置写hardware-test，可复制到 `ux0:data/WitchontheHolyNight/optimization-profile.txt`。

独立关闭或对照示例：

```text
hardware-test -half-color
hardware-test -tiled-targets
hardware-test -group-bounds
hardware-test -rt-scenes-2 rt-scenes-4
```

要保持r7组合而不被标准旧值迁移，可用 `all ps-all-operations -group-bounds`（冗余关闭项也表示明确的自定义配置）。重启生效。

## 日志与验证

VitaDeck默认开启；版本为stage-effect-r8-hardware-test-20261106。`optimization_profile`记录compiled_profile、source、legacy_expanded、实际mask及功能名。`legacy-stock-expanded`明确表示旧标准值已展开。该行在标准配置下小于900字节。

实际执行核对：

- Half：`half_color_candidate`的admitted及 `gpu_work.program_counts` 49–52增量。
- Group裁剪：`gpu_work`中Composite角色的estimated_covered_pixels；保守回退全屏仍属于正确执行，不能要求每个组必裁剪。
- Tiled：`tiled_target_probe`的production_enabled、pixel_status和max_byte_delta；API拒绝或图案失败会明确报告Linear回退。
- Blur：成功执行程序53增量。
- Scene容量：`target_scene_capacity`的requested、selected及queried_driver_bytes。

本轮host渲染166项、游戏121项、配置7项、配置收据4项、render-api实验8项通过；native GXM 66 cases/8,422 checks通过并启用ASan/UBSan，覆盖真实后端的Tiled双向复制、Linear staging、seal、清理/所有权失败和Half逐项能力。探针对(32,32)、(959,539)单字节故障会拒绝Tiled。native缓存38 cases/2,660 checks及完整PS输入11,144项通过。

这些host夹具不执行实机GPU。ARM/VPK身份见本轮交付收据；尚无本轮实机帧率或全场景RGBA8对照结果，尤其Half不能宣称与FP32逐字节相同。固定ABBA工具仍可使用；其默认翻转位38，其余测试项保持不变，不把这次组合包的结果冒充单项归因。

交付目录仍为 `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/`，包含同名eboot.bin、VPK、配置与版本说明。只保留上一轮小体积程序回退到 `.work/vita/rollback/stage-effect-r7-all-ops-offline-20261106/`；游戏assets不修改或复制。所有源码留在本地perf/render，不自动提交或推送。
