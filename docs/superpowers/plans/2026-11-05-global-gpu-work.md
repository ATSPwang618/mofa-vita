# 全游戏 GPU 工作量优化实施计划

基线：`perf/render`，`24b0467`。保持 960x540、现有纹理/混合顺序/存档兼容性。

## 交付顺序

1. 将优化掩码和 shader 能力扩为独立的 `u64`，增加 `ps-plain-material`、`group-bounds`、`half-color`、`tiled-targets`、`blur-weights`、`rt-scenes-2`、`rt-scenes-4`，其中 2/4 互斥，默认容量为 1，稀疏覆盖默认关闭。
2. 增加成功执行边界上的 OP、材质功能、pass 角色和 `estimated_covered_pixels` 观测；使用固定容量表和 4 KiB/窗口摘要。
3. 首轮实现 OP1 Add、OP6 Dodge、OP13 Overlay 的有限 plain Sprite/Fill/NoAlpha 专用路径，保持 112 字节顶点、4 纹理槽、8 参数和原始 RGBA8 公式；未知组合回退原路径。
4. 只裁最终 Group Composite 的保守物理矩形，加入 `Empty/Rect/Full/Unknown` 覆盖状态并随 owner、版本、cache、ring 传递；producer、clear、copy、tail write 继续完整执行。
5. 加入独立但默认关闭的 plain Half、RGBA8 tiled 离屏目标、Blur 权重缓存和每目标 Scene 容量 2/4 实验；每项失败都清理后回退 1 或原程序。
6. 使用生产 MZX/运动表生成全游戏材质盘点及固定回放描述，覆盖 scene38/PC1246 与代表性组合；不复制游戏资源，不伪造实机结果。
7. 运行 host/native/ARM/VPK 检查和固定回放；只有 RGBA8 一致、命中样本中位数至少降低 5%、p95/p99 不恶化、无新增分配失败时才将功能默认开启。未有实机证据的实验保留独立开关并明确标注。

## 约束与裁决

- `estimated_covered_pixels` 是按最终裁剪几何和物理目标尺寸计算的重复累计估计，不是硬件 fragment 计数或 GPU 时间。
- `Unknown` 永远回退全屏；不把未知当作空。
- 不为 Haze、rule、soft、mask 的排列批量生成 shader；不扩大多层融合或删除同步等待。
- 每轮使用版本标签 `stage-effect-r5-material-20261105`，日志启动打印版本、配置、shader 能力和素材身份。
- 所有修改留在本地，不推送远端。


## r6 coverage extension requested by user

Add independent bit37 `ps-full-material` and full-source OP1/6/13 entries54/55/56, retaining ps_source and removing only OP dispatch. Cover ordinary and Group routes; keep plain programs first. Do not create material permutations or lower resolution. The candidate package explicitly enables this option; `all` remains r5 until hardware acceptance. Replay adds isolated full-option ABBA and actual successful program execution gates.

## r7：用户要求全游戏所有OP离线专用化

将完整源处理入口推广至全部24种PS操作，新增程序64–87及独立位38 `ps-all-operations`；保留简单材质优先及逐项旧路回退。能力集合扩为两个u64 bank，与优化选项独立。PC离线编译63个实际GXP并嵌入EBOOT，普通构建检查身份，设备先验GXP元数据再注册patch。新增Vita3K-Shacc可选命令适配，本轮实际使用已安装的psp2cgc编译。回放优先单独切换位38，检查64–87成功执行和RGBA8比较。此扩展不以章节/PC/资源名选路，不扩大未经证明的多层融合，不把静态指令减少当作实机收益。

## r8：用户明确要求系列优化默认开启以实机测试

构建默认hardware-test，包含all、全部OP、Group bounds、Half、Tiled、Blur weights、RT scenes2。2/4互斥选择2，不恢复历史sparse撤回项。接通实际Half绘制与探针通过后的Tiled使用；保留逐项输入/能力/分配/图案失败回退。只对精确旧标准配置做内存内展开，保留显式关闭的自定义配置，启动日志记录来源与有效mask。此测试启用授权覆盖先前“等实机验收再默认”的交付约定，不构成已取得实机收益的声明。
