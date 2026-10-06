# krkr-profiler

在 PC 上使用 Vita 主机代码和 GLES2 渲染路径运行游戏，录制耗时、内存预算和图形调用。窗口在后台绘制，可用操作序列推进剧情，也可注入一段 TJS 进入指定场景。

## 录制

Windows 需要 ANGLE 的 `libEGL.dll`、`libGLESv2.dll` 及其依赖。使用 release 构建分析性能：

```powershell
cargo run --release -p krkr-profiler -- run `
  --game "D:/Games/魔法使之夜" `
  --gles-dir "D:/.ENV/Android/SDK/emulator/lib64/gles_angle" `
  --startup "启动游戏.xp3>startup.tjs" `
  --out target/profiles/mahoyo-before `
  --seconds 45 --at9-clock
```

游戏存档写入输出目录的 `savedata`。重复使用该输出目录会沿用这些存档；做冷启动对照时使用新的输出目录。游戏路径和脚本原文件不作修改。

`--canvas-size 480x272` 缩小脚本画布，最终画面仍按屏幕分辨率合成。加上 `--compact-scene` 可同时缩小最终画面，用于比较带宽与画质。`--effect-interval-ms 33` 将魔夜 ActionManager 的动画更新间隔设为至少 33 ms，保留按时间计算的动画时长。放大缩小后的特效画布时默认启用轻度锐化，`--no-effect-sharpen` 可关闭以作对照。批次清单支持 `canvas_size`、`compact_scene`、`effect_interval_ms` 和 `effect_sharpen` 字段。

`--startup` 指定入口，默认 `startup.tjs`。`--script replay.tjs` 在入口执行后追加 UTF-8 TJS，可用于设置测试场景。`--actions actions.json` 按启动后的毫秒数发送输入，坐标采用游戏逻辑坐标：

```json
[
  {"at_ms": 15000, "action": "mark", "label": "开始剧情"},
  {"at_ms": 16000, "action": "click", "x": 500, "y": 400},
  {"at_ms": 18000, "action": "key_down", "key": 13},
  {"at_ms": 18050, "action": "key_up", "key": 13},
  {"at_ms": 20000, "action": "screenshot"}
]
```

支持 `click`、`move`、`key_down`、`key_up`、`mark`、`screenshot`。截图会等待绘制并编码 PNG，耗时单独标记为 `capture.screenshot`。

`--video-output scene.mp4` 将画面直接交给 FFmpeg 编码为静音 H.264 视频，不生成逐帧图片。可用 `--ffmpeg` 指定程序、`--video-fps` 指定帧率；编码器默认 `libx264`，也可指定 `--video-encoder libopenh264`。

离线素材录制可用 `--video-encoder ffv1 --video-output scene.mkv` 保存无损画面。原始素材较大时，`--memory-scale 8` 将录制工具的图形、位图和上传预算提高至默认值的 8 倍；默认值为 1。提高预算后的录制用于生成素材，不用于判断 Vita 是否会耗尽内存。

`--video-offline` 使用固定脚本时钟，在每一帧的脚本和资源操作完成后录制，避免 PC 渲染速度改变动画时长。`--video-fps` 决定步长，`--video-duration-ms` 指定录制时长，默认采用 `--seconds`。可用 `--video-ready-global ready` 等待全局整数 `ready` 非零后开始录制；操作序列的时间也从这里计起。`--native-canvas-storage` 保留素材原始分辨率，最终输出仍为 960×544；这些离线模式的耗时不能用于判断实机性能。

预制静态画面时，`--capture-global frame` 会在全局整数 `frame` 变为新的正值后保存稳定画面，`--capture-count N` 可在保存 N 张后结束。`--video-bitrate` 设置 `libopenh264` 等编码器的目标码率，单位为 bit/s，默认 6000000；`libx264` 仍使用 CRF 18。

没有 AT9 桌面解码器时使用 `--at9-clock`：保留音轨时长和播放进度，以静音样本推进，不测量 AT9 解码性能。其他音频使用引擎解码器，输出同样静音。

## 查看结果

单独测量 BC 图片的准备耗时，无需启动游戏或图形上下文：

```powershell
cargo run --release -p krkr-profiler -- images `
  --game "D:/Games/魔法使之夜" --out target/profiles/images `
  --image "image.xp3>background.kbct" --samples 5
```

可重复指定 `--image`。每张图片预热一次，再记录指定次数，`images.json` 给出中位数、编码大小、解包大小和输出摘要。时间线分别记录熵解码与纹理布局重排，`image.workspace_bytes` 记录图片工作区预算占用。`--budget-mib` 可调整工作区上限，默认 32 MiB。

| 文件 | 内容 |
| --- | --- |
| `report.txt` | 热点、最长调用和计数器峰值 |
| `summary.json` | 可供脚本处理的同一份报告 |
| `trace.json` | 时间线，可拖入 [Perfetto](https://ui.perfetto.dev/) |
| `events.jsonl` | 原始采样，逐条写入并定期刷新 |
| `run.json` | 运行参数、退出错误和丢事件数 |
| `last-frame.png` | 结束时画面 |

时间线包含 VM 调度、GC、资源查找、图片读取、BC 解包、图形命令、模糊、画布回收和图形接口等待。脚本控制台消息作为时间线标记保留，不刷终端。

`total_ms` 包含子阶段；`self_ms` 扣除同线程内已记录的子阶段。等待和队列延迟单独看，不能当成 CPU 执行时间。计数器包括纹理预算、临时缓冲、CPU 暂存、进程内存和绘制/复制次数；VM 对象计数和 GC 分配债务用于观察变化，不代表堆的实际字节数。

`gl.viewport_calls` 记录视口设置次数，`gl.viewport_changes` 记录实际变化次数，可用于检查内部拷贝是否反复切换视口。

`process.rust_heap_bytes` 和 `process.rust_heap_peak_bytes` 统计 Rust 分配器申请的存活字节及峰值，包含录制器自身，不包含原生库分配、分配器元数据和碎片。它们用于估算引擎堆用量；`process.rss_bytes` 还包含桌面图形驱动，不能直接作为 Vita 的 newlib 堆需求。

只查看某段时间，或比较两次录制：

```powershell
cargo run --release -p krkr-profiler -- report target/profiles/mahoyo-before --from-ms 15000 --to-ms 25000
cargo run --release -p krkr-profiler -- compare target/profiles/mahoyo-before target/profiles/mahoyo-after
```

区间报告写入 `report-range.txt` 和 `summary-range.json`。对照运行应使用相同剧情、存档和操作序列。

PC 录制使用 960×544 显示区域和 Vita 的引擎内存预算，默认限制为每秒 60 次画面提交。`--fps 0` 可取消限制，`host.frame_wait` 单独记录帧间等待。前后对照应使用相同的帧率限制；旧版未限帧的报告不能直接比较动画拷贝总量。

时间反映当前 PC 与 GLES 驱动；预算计数包含引擎预留，不包含 Vita 驱动内部副本、内存对齐和碎片。判断优化时先对比工作量与峰值，再在实机确认帧耗时和物理内存。

## 批量场景

`batch.py` 运行一组回放，每个案例使用独立进程和存档目录。需要 Python 3。先构建 `krkr-profiler`，再运行：

```powershell
python tools/krkr-profiler/scripts/batch.py run cases.json target/profiles/batch
python tools/krkr-profiler/scripts/batch.py run cases.json target/profiles/parallel --jobs 3
python tools/krkr-profiler/scripts/batch.py report cases.json target/profiles/batch
```

`cases.json` 示例：

```json
{
  "game": "D:/Games/MyGame",
  "gles_dir": "D:/ANGLE",
  "cases": [{
    "name": "menu",
    "seconds": 30,
    "script": "Debug.message(\"PERF: title\");",
    "actions": [],
    "expect": ["PERF: title"]
  }]
}
```

顶层还可设置 `startup`、`savedata`、`at9_clock` 和 `fps`；单个案例的 `fps` 可覆盖顶层设置。`script` 是启动后注入的 TJS；`actions` 与单次录制相同。脚本输出 `PERF: 阶段名` 可划分打开菜单、切页、关闭等阶段；`expect` 中的消息必须在录制中出现，失败案例会标为 `incomplete`。案例设置 `pages: true` 时，另外按 KAG 的剧情页日志分段。

结果在 `batch.md` 和 `batch.json`。`rankings.json` 分别按拷贝量、回读、等待、内存和 VM 工作量排序；`frame-work.md` 给出两次画面提交之间的工作量分位数与峰值。运行参数保存在输出目录的 `cases.json`，可以据此重建报告。用 `--case 名称` 只重跑某个案例，重复该选项可选多个。

`hotspots.md` 合并各场景的命令热点，`hotspots.json` 提供不同指标的排序。`resources.json` 按图片准备耗时列出资源名称、加载次数和编码字节数，便于查找重复解包。剧情案例只从 `PERF: story` 开始计入排名，避免重复启动的 Logo 淹没剧情热点。`--jobs` 默认是 1；并行运行适合筛选工作量热点，耗时对照使用单进程。

长批次可加 `--compact`，每个案例结束后保留分段统计、脚本标记和截图，删除原始时间线。仍可重新生成排名和覆盖率报告；需要查看逐次调用时使用普通录制。

批次尚未结束时，用 `batch.py report cases.json 输出目录 --completed-only` 查看已完成案例的热点。

已分别运行的剧情与界面批次可以合并排序：

```powershell
python tools/krkr-profiler/scripts/batch.py merge target/profiles/combined target/profiles/story target/profiles/menus
```

案例名称须唯一；优化前后的同一案例使用 `compare` 对照。

`scan 脚本目录 candidates.json` 会列出 KS 命令、页面和 TJS 界面入口，筛选特效密集的候选段落。多个补丁中的同名脚本保留各自路径；这份静态清单不代表实际执行或耗时。

### 魔法使之夜

`mahoyo.py` 生成菜单和剧情回放，包含设置四页、字体选择、普通菜单、历史、存读档、自动、快进、CG 翻页与大图、音乐播放与切曲、TeaTime 和确认窗口。剧情通过游戏的回想流程进入，保留章节返回栈。需要本地游戏和用于回放的存档：

```powershell
python tools/krkr-profiler/scripts/mahoyo.py --game "D:/Games/魔法使之夜" --savedata "D:/Saves/mahoyo" --gles-dir "D:/ANGLE" --out target/profiles/mahoyo.json
python tools/krkr-profiler/scripts/batch.py run target/profiles/mahoyo.json target/profiles/mahoyo
```

默认选择序章、钟楼和几个特效密集章节。`--chapters 5b-14 5a-8` 可指定章节，`--seconds 90` 设置每段剧情的录制时长；界面案例有独立的操作时间表。每个案例复制一份存档，存读档测试也只写这份副本。换汉化或游戏版本后，应检查阶段确认消息和截图，确认脚本入口仍有效。

全量回放先用 `--catalog-only` 生成目录采集案例并运行，再把得到的 `catalog/events.jsonl` 传给 `--catalog`。生成的案例按书库入口逐段运行到返回，`--full-timeout` 指定每段的最长秒数，默认 900。文字即时显示，工具只自动点击阅读等待，保留动画时序。

`--catalog … --extra-blocks` 另外生成书库以外的流程图分支，包括番外和片头。它们独立进入，沿用流程图的脚本分组，不重建之前的分支选择；需要完整故事状态的分支应另用存档回放。

将提取的游戏脚本与实际执行页对照：

```powershell
python tools/krkr-profiler/scripts/coverage.py target/profiles/mahoyo-full extracted-scripts
```

`coverage.md` 列出完成、错误、未完成的案例和未进入的页面，`coverage.json` 保留具体清单。页面标记代表进入该页，不保证条件分支、循环动画和阅读期间的所有变化均已执行。

传入 `--also 另一批次目录` 可合并分支覆盖率，同一页面只计一次；`--out 目录` 指定合并报告的位置。中止录制保留已进入的页面，但不计为完成，也不计入性能排名。

## 控制台日志

`--log-level` 支持 `off`、`error`、`warn`、`info`、`debug`、`trace`，默认 `warn`。普通引擎命令也支持该参数；`--debug` 启用调试消息，逐次慢调用只在 `trace` 下打印。Vita 启动器的脚本日志与引擎日志开关仍分别控制。

录制由 `krkr-protocol/profiling` 功能启用，工具会自动启用它。普通引擎构建不包含录制器。

## 自测

`fixtures` 是一个不依赖商业游戏素材的小场景：

```powershell
cargo run -p krkr-profiler -- run --game tools/krkr-profiler/fixtures --out target/profiles/fixture --seconds 3 --actions tools/krkr-profiler/fixtures/actions.json --gles-dir "D:/.ENV/Android/SDK/emulator/lib64/gles_angle"
```

## 目录

- `src/`：录制器与报告命令。
- `scripts/`：批量回放、魔夜场景生成和覆盖率统计。
- `tests/`：Python 工具测试，运行 `python -B -m unittest discover -s tools/krkr-profiler/tests`。
- `fixtures/`：独立的小场景。

录制结果和生成的回放清单放在 `target/profiles/`；测试临时文件放在 `target/krkr-profiler-tests/`。
