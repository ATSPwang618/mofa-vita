# 渲染与GPU后端

Vita直接调用GXM，桌面WGPU与Vita3K宿主图形API不决定实机使用的API。场景/合成960×540，最终居中显示至960×544。不得将历史480×270/720×404结果标注为当前原生540p表现。

## 提交与生命周期

- Rust录制Begin/Draw/End/Copy，资源准备成功后再执行；兼容材质连续绘制可合批。
- 两个批次顶点槽，按容量增长；scene切换不随意重置游标。112字节完整顶点可在条件成立时使用32字节流+80字节常量流。
- 每个表面跟踪读写依赖；通知序号和逐槽完成前沿保护RAW/WAR/WAW、资源退休、槽复用。不能用“少一次等待”代替正确依赖。
- CPU更新可以有界rename，旧owner在GPU完成后退休。电影、读回和transfer等路径保留必要同步。
- 无变化的Stage/Scene/画布与UI可复用；日志区分重绘与复用帧，不能用复用帧FPS冒充重演出速度。

## 专用Shader

PC离线编译工具：[offline_vita_shaders.py](../tools/offline_vita_shaders.py)，生成源和GXP均有身份校验；当前源码嵌入85个GXP。按准备后的材质、混合OP、目标和能力选路，不按章节/PC/图片名写特例。

| 程序 | 合同 |
|---|---|
| 14 / 17 | snapshot PS回退 / native destination fetch通用处理 |
| 29、30/31、21 | 完整OP7；plain OP1/6；扩展OP13 |
| 48 / 57 | FP32 plain OP7；有限相邻双OP7交集融合 |
| 49–52 / 53 | 局部Half候选；n≤16的Blur预计算权重 |
| 64–87 | 全24种PS混合，各自保留完整材质处理 |
| 88–95 | OP1/6/7/13的Fill与NoAlpha精简入口 |
| 96–99 | OP1/6/7/13仅Haze及预乘标志的入口 |
| 100–104 | OP3/4/5/11/14 plain材质 |
| 105–107 | OP6/8/14 Group源：Pixel/Mask采样与原量化 |

Fill仍读取纹理alpha，NoAlpha只强制源alpha；坐标、规则、soft clip、mask等不能因不命中专用程序而删除。native目标读取使用raw UCHAR4 ABI，按原RGBA8量化与混合顺序计算。单入口编译、反射或patch失败，逐级回退到完整程序/native fetch/snapshot。

OP7双层融合仅允许相邻、同目标、合格Sprite及足够交集，保留第一次混合的RGBA8存取。它不是任意N层合成；交集切分还可能增加draw数。Half路径不能与FP32局部交集随意混用。

## Group与离屏

Pixel/Mask生产表面仍具有跨写入生命周期。融合Composite采样二者可省scratch；组缓存/producer复用要求内容和版本一致。Group bounds只限制最终Composite，保留完整clear及producer；未知覆盖、非RGBA恒等混合回全屏。254不等于255。

Tiled仅用于探针通过的合格RGBA8离屏目标：逻辑960×540、存储padding960×544，图案采样/读回逐字节校验失败则Linear。图片、R8、压缩纹理和电影不改变布局；CPU更新不能把Tiled当行数组memcpy。

每目标Scene容量默认测试2，失败清理后回1；4为独立互斥实验。Blur只缓存原权重，保持tap顺序与边界，n>16原路回退。

默认配置是`hardware-test`，含待实机单项验收实验。开关、状态、实测和撤回记录统一见[优化清单](05-优化记录与实测数据.md)；完整回归入口见[构建](03-构建与工具链.md)。
