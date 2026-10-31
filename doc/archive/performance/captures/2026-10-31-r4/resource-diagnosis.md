# r4 硬件资源链诊断（只读）

**结论：r4 的 atlas 真实 need 回收已生效，启动工作集仍把 DISP 挤出，frame523 因同一 DISP 的再次完整解压等待 863.570 ms；PC178–212 的持续慢帧没有资源加载活动。stream-index 和 source-rename 已实际执行；grade-cache 以及 r3 PC425/601 的 0.8 秒疑点没有执行到，收益与根因结论必须留空。**

本轮仅离线解析日志、读取源码和既有素材元数据。没有修改运行时代码，没有 Docker、Vita3K 或设备操作。可复算数据为同目录 `resource-diagnosis.json`、`r4-resource-parsed.json`；提取程序 `analyze-r4-resources.py` / `write-r4-resource-diagnosis.py`。

## 捕获范围与比较口径

- r4 附件 SHA256 `f69760a35c8f17422ed37a5c6c9621c62422d88645196de8030867f74c994809`，310,023 B，header `mofa-perf-optimized-20261031-r4`，mask65535。480×270 scene、720×404 composite、444/222/222 MHz、素材 manifest 均与 r2/r3 相同。
- 捕获到 frame2201 / scene1 PC212 / elapsed115025ms，**没有 PC425、601、825、1142**。不得把旧 r3 后段 grade3.75s 或两次 0.8 秒 update 当作本次 r4 成绩。
- 21 个 audit end group 全部完整，累计 counter 无 reset。chunk seq127/157/185/393 缺失，剩15条完整 sample；另18条 frame_window。缺失的 sample 不补零；worst 的 frame/scope 来自完整 audit。r3/r2 原本没有的新指标保留 null，不等于0。
- 三方统一比较检查点：r2 frame2115、r3 frame2039、r4 frame2008，均 scene1 PC212。三者 GXT descriptor127 /56,002,576B、16 PNG /8,697,280B、CG12GXT+3decoded、worker完成13项一致，是资源工作量对照锚点。不同手动停留与渲染帧数仍禁止直接对比全段墙钟/FPS或归因单一flag。

## 相同进度资源变化

| 指标 | r2 /f2115 | r3 /f2039 | r4 /f2008 | 解释 |
|---|---:|---:|---:|---|
| card bytes |85,213,379|8,166,880|7,303,835|r4 较r3少863,045B（10.57%）；不是FPS提升 |
| card calls / scope |492 /7191.994ms|58 /721.022ms|63 /655.749ms|r4次数多5，源于小头读计划；scope少65.273ms是读取作用域观测，含worker |
| NX full calls /output |27 /57,155,196B|29 /84,338,593B|27 /77,674,724B|r4比r3少2次、6,663,869B |
| NX range calls /full output |146 /1,539,661,334B|无事件|无事件|r3 已消掉的每子图整流解压未恢复 |
| NX full scope |2563.068ms|3655.737ms|3431.255ms|r4比r3少224.482ms；不可当主线程等量节省 |
| whole atlas完成 /evict |旧130个子图/57个子图|11 /10|9 /4|r3与r4均whole单位；明显不再每miss清空 |
| C→Vec |16 /8,697,280B|12 /2,839,680B|12 /2,839,680B|r4没有增加这项复制；剩12次全在启动快照 |
| PNG codec scope |321.598ms|334.741ms|330.973ms|同16次；codec耗时波动，不是grade优化证据 |
| CG首次upload |3 /96.274ms|3 /66.159ms|3 /67.389ms|同5,832,000B；r4没有实质改善这段父scope |
| native GXT upload |20 /14,778,368B|17 /14,417,920B|17 /14,417,920B|r3→r4实际上传量相同，无额外GXT locator事件 |
| native RGBA upload |7 /5,923,144B|7 /5,923,144B|7 /5,923,144B|r4 pool charge5,949,184B，与r3一致 |

少的两次 whole/NX 恰好是 TITLE_EN `6,563,442B` +BORDER `100,427B`。其 stored 合计`764,240+98,917=863,157B`，与 NX aux 减量精确相等；card 少863,045B，比这个少112B，即7个额外16B头读。r4的9个plan头读增加9次小读取，去掉2个旧full read对应的2头+2body读取，净次数`+9-4=+5`。**调用数增加不是又读了更多大素材的证据。**

## atlas：策略修复成立，工作集问题仍在

源码路径：`Game::new`（game.rs:968）先取得 DISP，再 BORDER；`Ui::new` → `Ui::load_textures`（ui.rs:1028/1200）预登记 TITLE/MENU/SAVE/CONF/DIALOG/DISP/MODEPNL。`Assets::ui_textures`（assets.rs:1382）对已缓存 texture handle 直接返回，handle保留locator，不长期pin住整atlas；首次登记仍需whole解压读出每个成员的metadata。

既有已核验素材 `asset-textures.csv` 给出启动8个whole：

| 按源码首次加载顺序 | 解压output B |
|---|---:|
|DISP_PARTS_EN|20,543,452|
|BORDER_PARTS|100,427|
|TITLE_PARTS_EN|6,563,442|
|MENU_PARTS_EN|2,360,111|
|SAVE_PARTS_EN|3,757,651|
|CONF_PARTS_EN|9,737,149|
|DIALOG_PARTS_EN|655,603|
|MODEPNL_PARTS_EN|327,933|
|合计|44,045,768|

前五项合计`33,325,083B`，与日志 `stage_ready_bytes.peak` 精确一致。随后 CONF 需要9,737,149B，总需求43,062,232B超过cap33,554,432B；DISP是最旧条目且未被数据pin，于是LRU先淘汰它。启动日志恰好 `8 complete /1 evict /20,543,452B evicted`，最终ready23,502,316B。r3则同8个whole完成后evict7次、ready只327,933B；因此不是旧 `reclaim(CAP)` 仍在。

运行中 frame523 /scene1 PC50 的完整证据：

- work868.506ms，update0.180ms，render868.321ms；`atlas_wait863.570ms`，占该帧work99.43%；`reload_us=0` 仍不覆盖Shared，但新增atlas wait已闭合。
- audit f561热点直接标注 `allui:6:DISP_PARTS_EN.NXZ`：load863.406ms、wait863.570ms、NX796.311ms /20,543,452B output /783,395B input。
- 启动f0同一key已经 load876.210ms、NX808.130ms。身份来自本次真实标签，**不再依靠r3的尺寸推断**。
- 该最慢帧只新增131,072B GPU upload，却必须等整个20,543,452B atlas output。没有同帧GPU copy、CPU grade或明显GPU wait；这确实是一处读取/解压放大。
- 此窗CPU atlas再evict3项/15,854,911B，当前八项素材中唯一三项大小组合是MENU+SAVE+CONF；ready最终28,190,857B也与算式相等。这是尺寸组合推断，因为eviction仍没有per-key事件，不能伪称日志直接打印了那三个victim。

r4通过修复避免了TITLE/BORDER重复，但没有避免DISP启动→场景的重复。这是**首屏atlas预登记工作集44.05MB与32MiB固定预算/当前LRU热度不匹配**。CPU atlas cap与GPU CDRAM/graph target分配独立；本捕获无GPU deferred eviction事件，native GXT resident misses17次=17次首次上传，不能说graph target挤出了这些CPU atlas。GPU占用变大可能另影响后续驻留，当前未出现该因果链。

后续资源方案应优先处理首次使用工作集：按页面登记/预热，或用离线小metadata表让登记不必立即whole inflate，并保留当前页面及即将进场DISP的热度。TITLE+DISP+BORDER仅27,207,321B，能同驻32MiB；当前提前登记CONF/MENU/SAVE却冲掉DISP。也可评估有界热子图缓存（一次从whole提取实际需要的小成员后保留），避免每次重新为131KiB成员解压20MiB；这会增加一次明确计量的小复制，不能无条件称整链零复制。仅扩大预算会掩盖44MB预登记和真正live set的区别。

## source-rename：已生效，但引入可见whole backing memcpy

相同PC212检查点两代 **native texture_update 都为39次/153,621B**，是较好的局部工作量锚点：

| 局部scope | r3 | r4 |
|---|---:|---:|
|finish_texture_update|16次 /402.834ms|3次 /0.018ms|
|transfer_finish_update|39次 /1.436ms|无事件|
|dirty region memcpy|39次 /0.559ms /153,621B|39次 /0.531ms /153,621B|
|source rename parent|旧路径无此指标|6次 /100.820ms|
|其中whole backing copy（嵌套，不再相加）|旧路径不执行|6次 /91.098ms /6,291,456B|
|retire old owner|旧路径无此指标|6次 /0.048ms|

可比的局部等待+rename+dirty update合计观测404.829→101.369ms，少303.460ms；它不是端到端节省，也不是单开关A/B。先前的GPU等待可能转移到batch slot或其他提交点，其他flag也改变排队关系。

源码 `gpu_lifetime.inc:354` 确认在last_use尚未完成时，先分新backing、`memcpy`整个pitch×height×bpp，再patch descriptor；保证旧GPU读owner活到完成，预算最多8MiB、同logical source最多一个旧版本。**这就是用户怀疑的新增CPU内存复制，日志证实发生了6MiB。**其中91.098ms约15.183ms/MiB，值得针对处理；不能把每个新glyph仅几十/几百字节的dirty write等同该1MiB复制。

每次1MiB与源码1024×1024 R8 font page契合，`FontAtlas::glyph`确实逐新glyph调用write_region（text.rs:410）；这是有代码支持的候选owner识别。native rename日志没有texture label、rect、last_use/completion序号，**还不能唯一锁定6次全是字体页**。下一步可按已知CPU原始内容维护有界可写shadow或先汇总本帧glyph更新再rename一次，比较dirty bytes、full clone bytes及后续wait总量；不能直接删除copy，否则旧像素丢失或覆盖仍在采样的owner。

6次rename只发生在audit1301、1522、1665、1905的过渡窗口。f1905后计数固定。PC212持续每帧~178ms时没有新的rename，不能将这一copy缺点当持续低FPS的主因。

## stream / message：命中索引，但旧0.8秒问题还未复现

完整累计：stream_resolve2次/0.072ms，stream_index_hit2次，无stream_stat事件；11条message共38.328ms，其中text resolve0.072ms、layout24.380ms、history10.712ms、backup1.611ms。voice_schedule1次0.049ms。代码的manifest绑定静态索引binary search确实命中，不再主线程重复is_file；已验证的只是本次两条stream路径。

audio_stream_open2次142.095ms，分布于audit831窗121.919ms与1865窗20.176ms。它在独立mixer worker `StreamDecoder::open`，不能加到主线程message scope或称已消失。成功index hit时Scope Drop有累计但无finish_hot label，故目前无这两条stream的路径热点名。

r3两次0.8s疑点位于PC425/601；r4停在PC212。本次不能用resolver72us证明那两次已经解决，更不能把null改善量写成0ms。需要同一PC/指令触发后的message、stream resolve/stat、audio open分段才可闭合。当前最大新update是frame874 /PC117的183.944ms，和r3同PC117的186.881ms相近，且该窗有两张GXT压缩资源完整NX输出/worker准备；缺失该窗完整sample，不能把已知update全部归voice或精确拆作demand wait。

## grade / 持续慢帧的边界

header声明grade-cache开启，但本次所有完整audit `grade_lut_bytes` /`grade_result_bytes` value及peak均0，没有cpu_grade、grade_lut_build/hit、grade_result_hit/miss事件。没有执行任何CPU Bake的实机热区，**不能评估新kernel精确encoder和完整结果cache在实体机上的耗时收益**。相关实测值/提升在JSON为null；不是宣布grade耗时0或把r3后段成本消掉了。

audit1905后直到2201，atlas/NX/image upload/texture update/rename/stream counter全部不再变化（f1957仅一条message3.490ms）。此区间worst帧仍出现：

- f2032 /PC212：work178.493ms，update0.468ms，render178.021ms；EndScene165.923ms/35次；atlas wait、reload、deferred miss、upload均0；10次PS GPU snapshot合计5,184,000B。
- f2178 /PC212：work178.021ms，update0.465ms，render177.553ms；EndScene165.467ms/35次；同样无资源事件。
- PC178典型f1788：work130.906ms，update0.467ms，EndScene112.222ms/25次；无资源重载。

因此持续瓶颈要交给GXM scene/pass/GPU dependency/失败的fetch回退分析；资源缓存和额外rename copy都解释不了此时反复发生的165ms EndScene。EndScene是CPU提交API作用域，可能包含driver提交或等待，不能仅凭它给出具体GPU硬件单元利用率。

## 资源项的证据优先级

1. **已闭合、可避免的长停顿：DISP整atlas重复冷准备863.570ms。** 先修启动/入场工作集和登记粒度，不再把已修LRU算式当未修。
2. **已发生的新增复制：6次whole rename /6MiB /91.098ms。** 局部wait已明显减少，但仍应识别texture/rect并合并更新或保留CPU shadow；需要把挪到其他提交点的等待一起看。
3. **过渡帧PC117约184ms update：相同旧位置仍在。** 该窗是GXT/worker准备路径，缺该sample导致不能精确拆分；不能冒充grade、metadata或atlas。
4. **尚未覆盖：grade cache实机成本和PC425/601 metadata疑点。** 下一次日志必须到对应进度才给改善数值。本轮没有证据支持继续改这些公式或随意断定根因。

资源项排序不替代全局排序：PC212 steady的EndScene165ms/帧反复出现，累计收益优先级高于这次只发生六次的rename复制。父报告应按持续帧/冷加载两个口径并列展示。
