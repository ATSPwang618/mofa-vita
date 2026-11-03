# BGM 预读缓冲调整

2026-11-03，本地交付版本 `540p-d734b38-bgmbuf-20261103`。按项目所有者选择，继续使用现有 Ogg Opus，单独增加 BGM 的有界 PCM 预读。当前渲染、图片和此前 targetfix 保持现有实现。构建与验证证据见 [21-bgm-pcm-buffer.json](evidence/21-bgm-pcm-buffer.json)。

## 触发证据

用户提供的 `log.txt` 对应 `540p-d734b38-targetfix-20261103`，SHA 为 `28b3f174095376ce6c92dd926fd442b7fc7f653a8cf0840357bda437cadfca16`。第二章 `2_1` / `2_2` 覆盖区间新增 47 次单流断供、缺失 PCM 合计约 11.1 秒。其中 M27 带曲名热点可归属 28 次、7.36 秒，M29 为 17 次、3.70 秒；其余两次缺少完整曲名证据。

M27 最大已记录单次补充为 724.251 ms，M29 为 681.763 ms；旧队列约 200 ms，补充停顿时会耗尽。`audio_mix_late=0`，这些后续窗口没有持续新增 AudioOut 整体缺块。音频补充 scope 包含读入、解码、循环跳转和调度等待，仍未拆分底层原因。

完整分析在本机 `.work/vita/chapter2-bgm-analysis-20261103/`，包括压缩原日志、统计和两首音频完整解码检查。此处的缺失时长是单流采样帧合计，不能当作整段游戏的墙钟停顿比例。

## 调整

`knife/knife-audio/src/stream_pump.rs` 增加两个固定预读档，`vita.rs` 根据已经明确设置的 `Curve::Bgm` 类别选择音乐档；文件名、扩展名和是否循环不参与判断。语音和其他流继续使用原档位。每块仍为 960 帧、48 kHz、双声道 f32，约 20 ms、7,680 B。

| 项目 | 原 BGM／当前普通流 | 当前 BGM |
|---|---:|---:|
| 队列块数 | 10 | 50 |
| 队列 PCM 时长 | 200 ms | 1,000 ms |
| 固定池块数（含两个周转块） | 12 | 52 |
| PCM 池载荷 | 92,160 B / 90 KiB | 399,360 B / 390 KiB |
| 单路 BGM 增量 | — | 307,200 B / 300 KiB |

PCM 池由音频工作线程在 MAIN/newlib 内存中一次分配，只按活跃流存在，不使用 CDRAM。全局流通道上限仍为 8；保守地将 8 路均按 BGM 档计算，PCM 池载荷上限为 3,194,880 B，约 3.05 MiB。正常的一路 BGM 加一路语音池载荷为 491,520 B。这里不包含 codec 对象、线程栈、队列元数据和其他 scratch；日志实际占用仍由 gauge 记录。

没有新增等待一秒才播放的门槛：第一块可用时即可开始，后台继续填充有界队列。较长的预读只有在已产生足够 PCM 时才能抵御补充停顿，不能解决首次打开长期等待。

预读 PCM 尚未应用播放音量。暂停、停止、增益和淡出仍在现有混音控制处作用，所以不必把队列里的一秒声音播完才能响应。AudioOut 的四块输出队列、20 ms 混音块、原编解码与循环定位逻辑沿用现状。

本次不增加压缩源文件缓存，也没有接入 ATRAC9/NGS。后续若仍断供，应从新的同进度实机数据判断是否需要音频专用 I/O 计时、压缩源驻留或调度调整。

## 日志核对

启动与窗口身份应为 `540p-d734b38-bgmbuf-20261103`。header 新增：

```json
{
  "audio_stream_queue_blocks": 10,
  "audio_stream_pool_blocks": 12,
  "audio_bgm_queue_blocks": 50,
  "audio_bgm_pool_blocks": 52,
  "audio_bgm_prefetch_ms": 1000,
  "audio_bgm_pcm_pool_bytes": 399360,
  "audio_stream_pool_ceiling_bytes": 3194880
}
```

`audio_stream_queue_blocks=10` 继续表示普通流；需要同时查看 BGM 的新字段。每路启动还会输出 `Audio stream PCM buffering`，含实例名、队列块数、预读毫秒及池字节。实际开始输出后，`audio_stream_starved`、`audio_stream_starve_wall`、`audio_stream_missing_frames` 仍按原定义统计。

`audio_stream_buffer_bytes` 是当时活跃池的载荷量，不能当成“此刻已有多少毫秒声音排队”。预读配置也不能证明运行时队列始终是满的。

## 验证和限制

- 新回归首先在旧队列失败：读入工作线程被 barrier 持续阻塞时，只能提供 9,600 帧，而 700 ms 的混音需求需要 33,600 帧。
- 调整后，同一个真实 Pump 的用例可以连续供给 700 ms；继续消费至一秒后明确返回 Pending，解除 barrier 后下一个样本位置正确，没有跳样或重复。该测试使用确定性 barrier，不依赖 PC 计时模拟 Vita 解码速度。
- 音频单元测试 28 项、集成测试 1 项，共 **29 项通过**，其中新增 4 项。实际 `StreamSound::process_frames` 在 host 测试中覆盖暂停/恢复、增益、立即停止和淡出；两个档位均覆盖回收池、背压及部分 EOF 样本顺序。
- Vita ARM release 构建通过，VPK 21 项内容、CRC、包内 EBOOT 与交付 EBOOT 一致。308 个源码输入和 10 个构建产物 SHA 核验通过。
- 素材清单及全部 9,470 个 stream 文件 SHA/大小一致，没有重编码、替换或降音质。

本次未部署到实机。测试证明固定缓存、样本顺序、控制边界和构建契约，尚不证明第二章实际断供已全部消失。超过预读余量的连续等待、开流前的阻塞、长期供给速率不足等仍可能造成缺音。

## 更新

交付文件在 `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/`，同名 `eboot.bin` 可通过 VitaDeck 更新到 `ux0:app/KNIF00540/eboot.bin`，也可安装配套 VPK。日志开启，TITLE ID、扩展内存标记、heap、960×540 画布和 60 FPS pacer 沿用现有配置。

设备已经使用匹配的 prebudget 素材时，只更新 EBOOT/VPK，无需重传音频或图片。素材清单 SHA 仍为 `5481d818422dd8a7fe37fa9cd45dbd7a6d805bd9a14749c69ab5f27bd831e7eb`。

复测重点是第二章 M27/M29 播放、连续语音触发、暂停恢复、读档和换曲。比较相同进度的单流断供增量，而不是只看整体 `audio_underruns`。旧 targetfix 的符号和修改前源码压缩保存在 `.work/vita/bgm-buffer-20261103/targetfix-sources-symbols.zip`，用于继续解析旧日志/core。
