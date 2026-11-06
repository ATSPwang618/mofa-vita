# r12：存读档选择框焦点修复

2026-11-06，版本 `stage-effect-r12-save-focus-20261106`，在本地 r11 上增加存读档焦点修复。分支 `perf/render`，基线提交 `ac5863b`；本轮未提交、未推送。

## 原因与边界

原实现中，第一行按上、最后一行按下只设置 `tabrow=true`，没有同时把共享对象焦点移到页码。格子边框要求 `!tabrow` 才使用高亮帧，因此选择状态与实际焦点可能分离。页码分支还把“没有命中格子”的布尔值反向使用；无触屏命中的手柄路径不能稳定进入页码处理。

`WObjs.hit` 也可能保留翻页动画 lock 阶段合成的对象编号，不能无条件用作新的触屏选择。重新打开存读档时，原实现没有统一恢复格子/页码焦点。

输入日志 SHA256：`9bf34da0eeb938e428c72d11388a4ea08fecf686fb8a905c1ddd030e60633e47`。该日志有56次资源 pending、56次 ready，没有记录到资源 failed；存在日志丢批，且未记录缺框时的具体焦点，故不能证明该次实机现象仅有这一种原因。截图 No.018 仍可见青色框，也不能作为缺框瞬间的证据。

## 实现

- 格子与页码焦点统一由 `focus_slot` / `focus_tab` 更新，格子同时维护 slot/col/row；边界进入当前显示页的页码。
- 无实际触屏事件时，按照保存的格子/页码状态导航，不接受旧 hit 强行覆盖。真实触屏命中可以在两区之间切换；松手 tap 虽然 active=false，仍保留确认语义。
- 重新打开时保留原 page/slot，恢复格子焦点。页码候选同步到当前页。
- 原上下左右移动顺序、边界重复按键规则、复制源标记与存档数据保持原语义。没有改动着色器、纹理、画布、音频、内存预算或资源清单。
- Vita 控制台新增 `kind=save_focus`，仅当状态变化时输出。字段含 state/saving/page/slot/col/row/tabrow/focus/hit/frame_texture_ready，离开界面时 selection=null；page/slot/col/row 均为0起始。
- 日志复用现有 perf 开关与异步控制台，默认开启。数据根 `perf-enable.txt=0` 仍显式关闭。无变化帧不创建日志字符串。

## 验证

6组焦点回归通过：20格全部方向和重复按键边界、6页×5列的上下边界转移、真实 WObjs 更新后的页码可见焦点、翻页合成 hit、真实 WIn 松手 tap、触屏覆盖/无效 hit、存读档与复制状态的重新聚焦。游戏侧全部130项 host 测试通过。

这些测试执行生产焦点函数及 WObjs/WIn 输入更新；没有驱动完整资源加载、实际存档写入或 GXM 像素输出，不能替代实机界面验收。ARM构建、VPK及成品身份见同名 evidence 收据。

## 使用与回退

交付仍在 `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/`，同名 `eboot.bin` 与 `knife-vita-540p.vpk`。

已有 r11 资源的设备只需关闭游戏并替换 `ux0:app/KNIF00540/eboot.bin`，或安装配套 VPK。本轮没有资源更新。尚未更新 r11 三个 UI 资源文件的设备仍需按 [r11说明](38-r11-ui-group.md) 安装，不能省略其资源更新。

启动核对 `build_tag=stage-effect-r12-save-focus-20261106`。先检查第一行按上、最后一行按下后页码有选中状态，再按上下返回格子；检查翻页后继续移动、关闭重开、存档/读档两种入口。后续若仍缺框，日志可区分实际焦点、是否仍在页码区、边框纹理是否存在。

上一轮程序、ELF/map和收据位于 `.work/vita/rollback/stage-effect-r11-ui-group-20261106/`。程序回退不需复制整套资源或回退存档。

## 成品身份

- `eboot.bin`：10,163,135 B，SHA256 `a5b77356977cbe9ae3290672fbf13d70accf419fbc34f258aa99696242d17dfb`。
- `knife-vita-540p.vpk`：7,271,033 B，SHA256 `15a9f527028ab7c4eaa11d090316da306044bec9bff363276c38c636ed616a75`。
- `build-receipt-r12.json`：86,551 B，SHA256 `eb44f73f98aaed07e26191a908f88edea6674a023fd02c317db309518d14f631`。

验证收据：[39-r12-save-focus.json](evidence/39-r12-save-focus.json)。
