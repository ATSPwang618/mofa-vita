# r5 slot completion 修正

r4 的旧槽复用判断使用 `slot.last_sequence`，实际却等待最新全局 `g->sequence`。本轮保留两个槽，为每个槽分别使用 GXM notification region 的一个 word。每个实际成功提交的 EndScene 仍写全局单调序号，但只写当前槽自己的 word；复用槽只等待该槽最后已提交序号，不再被另一个槽的新完成值越过或扩大为全局 drain。没有增加每 scene CPU 等待，也不改变 RAW/WAR/WAW flags、scanout sync 或 display queue。

每槽记录当前批次的 `first_sequence/last_sequence`。全局退休／资源依赖继续使用保守连续完成 frontier：所有未完成槽区间中最早未完成序号减一；不直接取两 word 的 max。因此较新槽先完成也不会提前释放较旧 reader/owner。重置槽区间前必须完成旧槽，空批次没有虚构提交序号；capacity reopen 保留当前区间，显式 drain 仍覆盖两个槽。序号 rollover 在真实完成和退休后清空两区间／word。End／wait／分配失败仍保留 native owner，并由既有 Finish＋display detach 的 teardown 负责最后释放。

同槽多次 fragment notification 的顺序约束沿用原来同 context 单 word 协议；参考本地 vitaGL `source/gxm.c:574` 的单调 query fence。跨槽连续 frontier 不额外依赖“较新槽完成就代表旧槽完成”。[VitaSDK GXM API](https://docs.vitasdk.org/group__SceGxmUser.html) 提供 region 与 fragment notification wait；host模拟仅检查上述内存所有权边界，不能证明GPU时序或像素。

仅真正发生槽复用等待时，三项同次数聚合进入控制台 audit：`slot_wait_needed/latest/covered`，`us=bytes=0`，`aux`分别累计目标槽最后提交序号、当时全局最新序号、等待前连续完成序号。`latest.aux-needed.aux`表示未再强等的较新提交跨度，不是已测省下的微秒；`batch_slot_wait.us`仍是实际等待时间，另含显式全 drain。

生产native lifetime的ASAN/UBSAN定向检查：169 checks通过，12 alloc=12 free；覆盖独立槽等待、乱序完成不提前退休、empty/nested batch、capacity reopen、rollover、End/wait失败保owner和counter一致。最终ARM `-O3 -Wall -Wextra -Werror`由父代理整包统一编译。本轮没有真机新收益数据。
