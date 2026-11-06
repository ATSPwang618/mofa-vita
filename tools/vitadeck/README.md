# VitaDeck 多连接日志接收补丁

`logging_multiclient_fix.py` 修复 VitaDeck 的单客户端接收阻塞。原接收线程在 `accept()` 后持续读取同一个客户端；若它保持连接而不发数据，其他进程的 CatLog 连接会排在后面，甚至积满 listen backlog。实机排查时 PC 的 8080 端口有 128 个 CloseWait，游戏 EBOOT 则确实已开启 `sceClibPrintf` 输出。

补丁使用一个 selector 同时接收最多 64 个客户端，逐连接保留 UTF-8 解码器和未完成行，不再按 IP 合并缓冲。停止/断开会清理 socket 并保留末尾未换行文字；单客户端行缓冲有上限。继续使用原 Logging 页的 Qt 信号和显示函数。

每行先保存在 PC 的 `logs/vita-console.log`，再发送给界面。单文件上限 8 MiB，保留一份 `.1` 轮转备份，总计约 16 MiB。只有 PC 写盘，游戏仍走既有异步控制台通道。

## 安装

把 `logging_multiclient_fix.py` 放到 **VitaDeck.exe 同目录下的 `components/custom/`**，完全退出并重新打开 VitaDeck。已在本机以下位置安装并实测：

```text
D:/AI-PSVITA/PSV-ARTEMIS/VitaDeck/components/custom/logging_multiclient_fix.py
```

该版 VitaDeck 的启动流程先导入自定义组件，再创建内置 Logging 页。补丁在导入时只替换 `LogServerThread.run`；自定义组件开关仅控制可选状态页，状态页默认隐藏，不影响补丁加载。要停用补丁，将此文件移出 `components/` 后重启。`VitaDeck.exe` 本体和工作区端口配置保持原值。

启动日志应包含 `vitadeck-multiclient-20261102`。PC 日志在上述 VitaDeck 目录的 `logs/vita-console.log`。原文件复制、推送、崩溃分析等功能不由该补丁接管。

## 验证

```powershell
python tools/vitadeck/test_logging_multiclient.py -v
```

四个真实 TCP 用例覆盖：空闲长连接与游戏流并存、同 IP 两连接的半行隔离、拆分 UTF-8 与断线末行、PC 文件保存。测试不需要 Qt，也不启动 VitaDeck。

`--baseline <原始 components/logging.py>` 可对照复现原接收器。2026-11-02 原版的空闲连接用例失败，补丁四项通过；安装后的真实 VitaDeck 也通过双连接与中文接收测试，并收到 `540p-d734b38-poolfix-20261102` 实机日志。详见 [日志恢复记录](../../doc/06-性能采集与分析.md)。

`--baseline-marshal <已提取模块>` 可在匹配的稳定 Python 版本下运行安装版原循环；它同样复现了空闲长连接阻塞。

自定义组件接口与接收器背景可在 [上游项目](https://github.com/Rocroverss/vita-ultra-parse)、[原接收器源码](https://github.com/Rocroverss/vita-ultra-parse/blob/main/components/logging.py) 核对。本机使用的打包版为 Python 3.11，未包含 `logging.handlers`；补丁采用普通文件轮转，不引入额外依赖。

## 推送当前 EBOOT 与固定 GPU 回放

设备恢复FTP后，可以用 `push_eboot.py` 更新引擎，并在显式选择时写入回放请求。脚本覆盖 `ux0:/app/KNIF00540/eboot.bin`，不会上传或删除素材；上传后用RETR重新读取EBOOT并比较字节数与SHA-256，读回不一致时返回失败。

当前r8 `stage-effect-r8-hardware-test-20261106` 默认开启GPU测试组合。设备没有 `optimization-profile.txt`，或其内容恰为旧标准值 `all`、`all ps-all-operations`、`all ps-full-material` 时，只更新EBOOT即可启用；启动日志 `optimization_profile` 会报告实际来源与mask `0x4fdfffffff`。其他自定义文件继续覆盖默认值。若需明确设置，将交付目录的配置复制到 `ux0:data/WitchontheHolyNight/optimization-profile.txt`，内容为 `hardware-test`。脚本本身不会修改该文件，迁移在引擎启动时完成，也不会重写设备文件。独立关闭方法与Half/Tiled实际运行规则见 [r8默认实机测试](../../doc/05-优化记录与实测数据.md)。

```powershell
python tools/vitadeck/push_eboot.py `
  --host 10.195.41.100 `
  --backup .work/vita/old-eboot.bin `
  --replay 38:1246
```

`--replay` 是显式开关，接受 `current` 或 `scene:pc`，对应写入 `ux0:/data/WitchontheHolyNight/gpu-replay.txt`。不带该参数时不会创建回放请求。请求每次启动读取一次，但不会自动删除；测试结束后移走以恢复正常游玩。设备或VitaShell FTP未运行时脚本会在连接超时后返回exit code 3，不会宣称已更新。上面IP是历史示例，使用设备当前IP。

回放结束后，将 `logs/vita-console.log` 复制到仓库，执行：

```powershell
python tools/vita_gpu_replay.py .work/vita/vita-console.log --environment hardware
```

只有包含对应build identity、完整ABBA四个区块、每区块120个完成样本、零RGBA8差异、实际专用程序执行及性能门槛通过的日志才会报告pass。当前回放默认单独翻转位38并检查64–87的执行计数；r8其余默认测试项保持相同，结果只归因于被翻转的项，不能当作整套组合已验收。旧版本日志、模拟器日志、丢失分块或没有gpu_replay区块均保持inconclusive。
