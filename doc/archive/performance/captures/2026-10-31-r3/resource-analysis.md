# r3 资源/加载链硬件对照（供主报告整合）

结论：同 scene=1 / PC=246 进度下，重复 atlas range 解压、重复 GXT locator 重读、Arc/Vec 往返和受影响的 C→Vec 拷贝确实明显减少。**atlas 冷读仍会造成约 0.85 秒卡顿；新后段还出现 CPU grade 与尚未闭合的 message/voice update 卡顿，不能把所有长帧归给 atlas。**

本轮只读源码和用户日志；未改运行时代码，未启动 Docker、Vita3K 或设备。

## 证据与公平口径

- r2：固定基线 `doc/performance/baselines/2026-10-30-r2/capture.txt`，324,358 B，SHA256 `c16d1ac5ae7ad766802fd475db7d1939438056cd914823fec0650857fbd14d3d`。
- r3：附件 `e36939bc-e307-46bc-beb2-71e0da5d9e9a/已粘贴的文本.txt`，635,841 B，SHA256 `a5be07fc3aecc3f435cc1061425b703c9c076db6c81c230a5a085bf975e7fa0c`。header=`mofa-perf-optimized-20261031-r3`，mask=4095，十二个开关均声明开启。
- profile、manifest SHA、480×270 scene、720×404 composite、444/222/222 MHz 和四项设频返回值与 r2 相同。
- r2 到 frame2467 / scene1 PC246；r3 完整捕获到 frame8993 / scene2 PC186。**不能把两段全捕获累计量直接相减作为节省量。**
- 本报告主要截取 r3 **frame≤2216**（scene1 PC246）对比 r2 frame≤2467；两者 GXT descriptor 均127次、56,002,576 B；常规 CG 均12个GXT+3个decoded，image worker均完成13项；PNG均16次、8,697,280 B。因此该检查点对比资源工作量比全捕获更公平，但仍是不同手动轨迹/驻留时长，不能声称单开关 A/B 或端到端节省时间。
- r2 11条 sample缺片；r3多个分片sample缺失，但有frame_window小摘要补充。r3 frame7715 audit丢bank，禁止解释为该窗口零成本；下一完整累计bank恢复区间总量。两个检查点和最终累计求和均已独立与原始raw bank逐项相等核对。
- 机器可复查数据：`.work/performance/r3-resource-analysis.json`、`r3-resource-checkpoints.json`。提取程序 `r3-resource-analysis.py`；不是模拟器测量。

## 同进度资源对照表

| 项目 | r2 至frame2467 | r3 至frame2216 | 可作的判断 |
| --- | ---: | ---: | --- |
| card read调用/字节 | 492 / 85,213,379 B | 58 / 8,166,880 B | 同进度读取次数少434，读取字节少77,046,499 B（90.42%）；不是FPS提升90.42% |
| card read累计scope | 7,191.994 ms | 721.022 ms | 观测读取作用域明显下降；含后台，不能当主线程节省6.47s |
| NX full次数/输出 | 27 / 57,155,196 B | 29 / 84,338,593 B | 单独看full上升会误判；新实现把旧子范围重复解压改为whole owner |
| NX range次数/完整输出 | 146 / 1,539,661,334 B | 0 / 0 B | 全部旧UI/CG range重复链在该检查点消失 |
| NX full+range输出处理量 | 1,596,816,530 B | 84,338,593 B | 同进度输出处理量少94.72%；这是解压工作量，不是实测内存总线流量 |
| NX累计scope | 56,691.029 ms | 3,655.737 ms | 后台作用域可重叠，不得写成53.0s墙钟时间提速 |
| archive lock累计wait | 724,253.705 ms | 0.186 ms | 旧130个并发任务的累计等待基本消失；不能说游戏减少了12分钟卡顿 |
| Stage任务启动/完成 | 130 / 130 | 11 / 11 | 旧值是每子图任务，新值是whole atlas任务；新StageStart仍计job，实际OS worker只有1个 |
| Stage在途峰值 | 63 | 1 | 有界调度生效 |
| Stage eviction | 57个子范围结果 | 10个整atlas | 单位不同，不能宣称淘汰率降低某百分比；新10次还暴露过度淘汰bug（下文） |
| GXT descriptor | 127 / 56,002,576 B | 127 / 56,002,576 B | 相同登记工作量，是此对比的强锚点 |
| GXT locator reload | 20次 / 4,178.143 ms / 14,780,288 B | 0 | 首次worker payload或shared cache被复用；共享冷读没有被这个计时器覆盖，不能说所有GXT准备零读盘 |
| GXT实际native upload | 20次 / 14,778,368 B | 17次 / 14,417,920 B | 存在3次UI实际采样差异，不能以总upload时间差推导单张GXT快了多少 |
| 无用UI GXT retain | 115次 / 43,942,800 B / 647.706 ms | 0 | shared path不再做该拷贝 |
| UI其他retain | 12次 / 100,091 B | 0 | 小PNG descriptor也借用shared owner |
| Stage Vec→Arc / Arc→Vec | 44,943,892 B / 1,376,640 B | 均0 | 不再做这两种payload拷贝；不能将新whole atlas完成字节与旧子图完成字节直接同比 |
| C→Rust Vec | 16次 / 8,697,280 B / 76.525 ms | 12次 / 2,839,680 B / 17.098 ms | PNG数量与输出总量完全相同，确实少4次、5,857,600 B（67.35%）中间拷贝；余下12次均在启动快照，符合soft-LUT兼容接口 |
| PNG解码本体 | 16次 / 321.598 ms | 16次 / 334.741 ms | codec本体没有被C-owner优化“加速”；scope微幅波动 |
| 3张CG首次upload scope | 96.274 ms / 5,832,000 B | 66.159 ms / 5,832,000 B | 同量父作用域减少30.115ms，但还受提交/等待/池化影响，不能只归因C-owner |
| RGBA upload charged bytes | 7次同payload5,923,144 B；charge7,340,032 B | 7次同payload5,923,144 B；charge5,949,184 B | 该组纹理收费少1,390,848 B（18.95%）；pool减圆整有效，但aux是texture收费而非总page映射 |
| GXT upload前zero | 20次 / 15,466,496 B / 24.531 ms | 0 | 新正式payload覆盖路径不再先清整分配 |
| sampled R8 upload | 0 | 0 | 本对照没有执行R8上传，不能报告gray-r8实机收益 |
| GPU deferred eviction | 0 | 0 | PC246前没有GPU驻留抖动；不能把ByteStage eviction当GPU eviction |

同资源热点也可对照：IMG0408 900×540首次上传32.734→23.233ms，IMG2376 31.522→23.527ms；各仅一次，且是包含其他变动的上传父作用域，不能直接宣布稳定的逐张提速比例。

## 确认的 atlas 缓存策略错误与未覆盖计时

源码 `knife/knife/src/atlas_cache.rs:35` 在每个miss都调用 `reclaim(&mut state, CAP)`；`reclaim` 在 `state.bytes + need > CAP` 时持续淘汰未pin块。代入 `need=CAP` 后，只要仍有任何可淘汰字节就继续淘汰，**并非按下一entry的实际长度回收**。正常串行需求没有外部pin时，miss会清光现存块，再读新块，实际退化为一块缓存。pin并没有漏计，但替换策略错误。

r3整段恰好14次whole atlas完成、13次whole eviction，ready峰值仅20,543,452 B，低于32MiB；与这个分支相符。不能仅凭此等式单独证明bug，源码条件和以下具体替换序列共同闭合：

| 窗口末frame | 日志实际whole替换 | 大小关系与慢帧 |
| --- | --- | --- |
| 120 | MODEPNL 327,933 B被TITLE 6,563,442 B替换 | 两者同时远小于32MiB；frame6仍有362.022ms冷准备 |
| 383 | TITLE 6,563,442 B被BORDER 100,427 B替换 | 没有预算必要 |
| 630 | BORDER 100,427 B被DISP 20,543,452 B替换 | `nx_full`单次775.222ms；frame431 scene1 PC50 work846.248/render846.067ms |
| 5720 | DISP 20,543,452 B被NOISE 2,097,608 B替换 | 两者合计22,641,060 B，可以同驻32MiB，当前却淘汰DISP |
| 8181 | NOISE 2,097,608 B→BORDER100,427 B→DISP20,543,452 B | 两次full合计780.390ms；frame8138 scene2 PC146 work845.670/render845.401ms |

**atlas身份没有靠猜语言：** r2的热点直接标注`nx_range:allui:6:DISP_PARTS_EN.NXZ`。r3 `nx_full`当前没有hotspot标签（测量缺口），但frame630单次 `(decoded=20,543,452, stored=783,395)` 在既有实包审计的32个atlas中唯一匹配 `allui:6 DISP_PARTS_EN.NXZ`；ZC虽decoded相同，stored为784,539，可排除。frame8181两次总 `(20,643,879 / 882,312)` 在atlas两两组合中唯一匹配 DISP_EN `(20,543,452 / 783,395)` + BORDER `(100,427 / 98,917)`。这是日志计数与已校验素材身份的组合推断，**不能伪称r3输出了并不存在的full-NX热点标签**。

因此两处长读都指向EN DISP，不支持“语言切换”解释。`scene=1→2`是脚本 `1_0→1_1`，并非语言字段；当前日志也没有language-change事件。BORDER/NOISE的读取来自现有UI/效果消费者。冷块被不必要淘汰后，纹理descriptor首次采样或后续需要该atlas的子图，才触发冷读。

测量还有一个明确缺口：`DeferredBytes::Shared::with_bytes`调用`load()`没有包围`GxtReload/ImageReload` scope；故上述846ms帧 `reload_us=0`，即使其中发生775ms atlas读取等待。这是未计入该分类，不是没有等待。

下一步应先通过NX元数据取得下一entry的真实decoded大小，只淘汰所需额度，保留pin/输入预算与同key合并；也要核对初始UI eagerly加载七个atlas时，热DISP是否应进入当前页工作集，避免正确LRU仍把马上要用的大atlas淘汰。添加带 `archive:index:entry` 标签的整atlas load/demand-wait计时，并把主线程等待纳入SlowFrame；不要仅扩预算或将计时0当成修复完成。此处只读，没有擅自修改实现。

## r3后段新卡顿：与旧基线区分

后段r2没有覆盖，以下是r3内部归因，不能称r3回归或与r2相减。

### Frame6925 / scene1 PC1142：CPU grade占主要已知部分

work1898.855ms，update1892.893ms，render5.957ms，native waits0.034ms；upload3,095,040 B，deferred_miss=0、reload_us=0。窗口末frame7017的累计增量：

- `cpu_grade` 2次，**1505.112ms**，3,095,040 B；
- PNG decode 2次，220.144ms，3,095,040 B；
- NX full 3次，66.212ms，1,281,932 B；
- `image_first_upload` 2次，13.680ms；hotspot为 **IMG1155 960×403 bpp4 alter=false，2次**；
- 没有此窗硬件JPEG解码，也没有atlas Stage请求。

用户allscr的scene1=`1_0`，PC1125–1134明确为IMG2311、IMG1155 + STEC2/SNX3=58、IMG0387 + STEC3；PC1137–1141为STCP、PC1142为FADS。主循环虽先`pc += 1`再执行，但FADS handler在仍有状态时会`pc -= 1`（game.rs:3235），所以PC1142可表示停留在该FADS，而非必然“FADS尚未执行”。FADS会触发stage commit / image需求，应以具体handler与加载作用域对齐，不能机械以PC-1定位所有指令。

`2 * 960 * 403 * 4 = 3,095,040 B`与grade、PNG、首次upload字节完全相等，且窗口热点直接给出IMG1155重复两次。这强烈锁定同尺寸有grade的图像需求。仍是窗口累计，不是每调用独立时间线；当前还没有grade key/亮度/sRGB/来源线程标签，不能断言两次Bake参数完全相同或把各scope机械相加成帧总量。

`grade.rs::Bake::apply`每次重建pre[256]；灰度grade分支重建 **65,281项`Vec<[f32;3]>`** LUT，再逐像素插值和`srgb_byte`。应补LUT-build/像素循环分计时与Bake完整key，在语义一致前提下考虑有界复用。当前还不能从总grade scope证明LUT构建独占了1505ms。

### Frame4327 / scene1 PC825：首次同步grade与graded reload叠加

work1825.040ms = update1221.272 + render603.763ms；deferred_miss1，reload31.776ms，upload6,216,480 B，native waits0.034ms。窗口末frame4327：

- 主需求`demand_decodes`新增3次，`demand_us`约1146.259ms；
- `cpu_grade`4次1077.102ms / 6,216,480 B，PNG7次661.313ms；
- IMG1763 locator重载31.776ms，随后`deferred_decode`558.737ms /1,164,240 B；
- 9次GPU deferred eviction /7,278,080 B，同窗首次upload6次；IMG1763、IMG1895均有淘汰热点。

素材脚本PC808–815明确：IMG1763 + STEC2，IMG1895 + STEC4/SNX3=62；PC823还有IMG0408。日志`IMG1763`重载热点与脚本可对上。render603.763ms中单次558.737ms deferred decode是主要已测父作用域；grade在decode内，**不能再把4次grade1077ms全部加到它上面**。此窗没有硬件JPEG，也没有atlas重读，不能归因“又解压了DISP”。

### Frame2645 / PC425、frame3248 / PC601：消息/语音路径，尚未闭合

两帧update分别806.007ms、818.426ms，render仅1.561ms、3.214ms；所在完整audit窗口archive、NX、PNG/JPEG、cpu_grade、image first/reload计数均无增量。不是本轮已计时图片加载项。

按素材脚本实际解析：

- PC425刚执行完PC424 `ZM0151e($000030)`，前有PC419 `VPLY(F70_1_0_0001_A,...)`；
- PC601刚执行完PC600 `ZM0152b($000044)`，前有PC595 `VPLY(F70_1_0_0003_A,...)`。

相似未分类慢帧PC267、498、704、855、916、995也位于消息入口，前方都有VPLY。Vita主线程 `Op::Message→play_voice→voice_stream_path→Assets::stream_path` 会先`ogg.is_file()`，再`path.is_file()`；音频worker随后自己打开/解码。当前stream目录有9470个文件，上述两个OGG仅9146 B和23210 B。**文件metadata解析/重复stat是具体未计时的候选，尚无其耗时探针，不能断言800ms就是FAT目录查找。**

下一轮可先在保持缺失资源回退语义的前提下，去掉同一voice路径的重复stat，并将已知`.ogg/.opus`存在关系做成一次构建、可失效的文件名索引；或用预生成素材索引直接解析路径，避免消息入口反复在9470文件目录内查询。该改动的收益仍须真机探针确认，不能预填800ms回收值。

还应将message的backup/history snapshot、resolve/layout、voice path resolution、voice schedule和audio worker stream-open分别计时。音频mix包含解码，整段`audio_mix_max_us=711488`、late/underrun增加证明音频链也有长任务，但不证明主线程这两帧阻塞在audio decode；不可从相关性直接定责。

解析证据保存在 `.work/performance/r3-script-context.json/.stdout/.py`，直接移植仓库MZP/MZX/Scene::parse字节规则读取用户allscr，无游戏执行、无新大规模逆向。`r3-voice-path-evidence.json`只记录现有目录文件数与引用文件大小，非设备文件系统基准。

## 全r3资源剩余项

- GXT：30次实际native upload /21,495,808 B；locator `gxt_reload=0`。这证明first payload/shared缓存复用在新覆盖区间仍生效；不等于shared atlas没有冷读。
- C owner：整段PNG38次40,257,520 B，软件JPEG1次1,944,000 B，硬件JPEG3次；C→Vec仍只有启动12次2,839,680 B，后段没有再增长。语言strip2次，copy bytes=0，aux=4,119,120 B；view裁剪生效。
- R8：整段没有native.upload_r8，仍未观察到该项硬件收益。
- GPU驻留：后段共34次eviction /35,669,952 B /53.523ms；resident peak31,542,400 B接近32MiB策略上限。后段6次image locator reload共80.215ms，range输出/选中均505,946 B，**比例1.0**，标签是IMG1763、IMG2311、IMG0387、IMG2309、IMG0668、IMG1765等具体CG，不再是整atlas范围放大。
- graded图像被淘汰后重载会重新CPU grade；例如窗口6919的IMG0387.JPG/IMG2311，2次deferred decode704.657ms，其中CPU grade583.708ms，JPEG HW decode41.052ms。这里真正贵的是重新decode/grade父链，不是仅24.632ms locator读。
- 三次硬件JPEG成功，native decode123.251ms、CSC2.100ms、RGBA→heap31.769ms /2,909,520 B；编码stream copy333us /206,286 B。r2无JPEG，不能给JPEG提升比例，也不能把所有秒级卡顿叫硬解问题。
- pool整段命中94次 /54,265,728 B，创建page48次 /65,798,144 B。它们是累计分配事件，不能用累计page字节当当前CDRAM。GXT全部payload与charge相等，zero_gxt=0；pool/条件清零结构生效，但GPU eviction仍实际发生，后续需按可见/近期图像的工作集和graded重算代价调查，不能只加总预算。

## 可汇报／不可汇报

可汇报：相同PC进度、相同登记/PNG工作量下读取/解压/显式复制工作量减少；有界worker；GXT首次locator重载消失；same-payload RGBA收费下降；明确剩余atlas策略bug、graded重载和消息入口盲区。

不可汇报：全捕获累计相减得到“节省多少毫秒”；把后台累计scope当墙钟；按Stage job/eviction不同单位算命中率提升；把shared cold读的reload_us=0当加载零成本；对未发生R8/JPEG基线覆盖的项捏造提升；将CPUgrade窗口内所有嵌套scope相加归给单帧；在没有language事件时声称跨语言造成重载。
