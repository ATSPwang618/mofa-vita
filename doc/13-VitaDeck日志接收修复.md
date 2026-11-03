# VitaDeck 无日志的定位与恢复

日期：2026-11-02。游戏仍为 `540p-d734b38-poolfix-20261102`。本次恢复由运行时显式开关和 PC 端接收补丁完成，没有改动游戏源码、EBOOT 或 VPK。

## 确认的原因

- 从实机 FTP 读取 `ux0:app/KNIF00540/eboot.bin` 的字节数与 SHA-256，确认为本地交付的 9,802,491 B / `5b21e711279c8b2ca6d2ce62550436e30f16f0e4ab1c27229088cf343e3b0a7a`。
- 构建明确为 `QUIET=0 / KNIFE_PERF_CONSOLE=1`，匹配 ELF 的 `kv_perf_console` 仍调用 `sceClibPrintf`。最初实机数据根和 assets 下都没有日志开关，因此不存在内容 `0` 覆盖默认值的情况。
- PC 的 `10.195.41.1:8080` 已监听，并与 `10.195.41.100` 相连，排除该时刻没有监听或 IP 不通。端口同时积累 128 个 CloseWait。
- 上游源码与本机打包模块都保留了“接受一个客户端后，一直读取到它断开才接受下一个”的循环。两个真实 TCP 客户端的回归用例复现：第一个空闲长连接存在时，第二个游戏流不能及时接收。

因此，本次无日志的已确认问题在 PC 接收循环，重编同样开关的 EBOOT 无法解决该阻塞。运行程序与实机进度在接收器之外，不把这次日志恢复视为渲染或崩溃修复的整体验证。

## 实际修改

1. 在实机 `ux0:data/WitchontheHolyNight/perf-enable.txt` 写入 ASCII `1\n` 并回读核验；交付目录也保留同名文件，供以后同步。开关放在数据根，与 `assets/` 同级，不改变素材清单。
2. 在本机 VitaDeck 的 `components/custom/` 安装 [接收补丁](../tools/vitadeck/logging_multiclient_fix.py)，通过原生自定义组件启动机制生效。只替换接收线程，不修改 `VitaDeck.exe` 本体。VitaDeck 重启后已经加载 `vitadeck-multiclient-20261102`。
3. 接收线程可同时读取多个连接，按连接分别缓存半行和 UTF-8 状态；断线后立即关闭相应 socket。最多 64 个连接，每连接未完成行有界。
4. PC 自动把收到的内容保存到 `D:/AI-PSVITA/PSV-ARTEMIS/VitaDeck/logs/vita-console.log`，8 MiB 轮转，保留一个 `.1` 备份。游戏仍走原 `sceClibPrintf` 异步通道，没有新增实机逐帧写盘。

该打包版未包含标准库 `logging.handlers`，第一份外部补丁因此未加载；改为普通文件轮转后已实际启动验证。对安装版的包结构/字节码核对通过普通本地工具进行，REA 可用性诊断不充当运行证据。最终程序本体 SHA 仍为 `d643406dd3a067b010bd3bc4396cddf30e6e1b807b0c005de863a478fc08fdc0`。

## 验证结果与边界

- 原始接收器的空闲连接回归失败，补丁 4 个真实 TCP 测试通过。
- 运行中的 VitaDeck 在一个半行连接保持打开时，立即接收另一个连接；同 IP 中文和断线末行保存正常，Python stderr 为零字节。
- 恢复后实机已经发来 `540p-d734b38-poolfix-20261102` 的 `build_identity`、`frame_window`、资源/显存统计及分块审计行；8080 没有继续累积 CloseWait。
- 主动发送的 PC 接收自测使用 `kind=receiver_probe / source=PC self-test`，与真正的游戏日志分开识别。抓取边界处的不完整分块不算完整性能记录；接收成功不证明 CatLog 上游绝无丢包。

机器可读记录见 [18](evidence/18-vitadeck-log-reception.json)。原版复现、补丁测试和实际接收摘要在 `.work/vita/logging-20261102/`，详细说明见 [工具 README](../tools/vitadeck/README.md)。

以后先启动 VitaDeck，再运行游戏。确认界面启动行包含接收补丁版本，游戏窗口里版本是 `540p-d734b38-poolfix-20261102`；分享日志时使用 PC 上的 `vita-console.log`，轮转过则同时保留 `.1`。游戏的 `perf-enable.txt` 在启动时读取，修改后需重启游戏。
