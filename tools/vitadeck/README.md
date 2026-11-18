# VitaDeck 日志与程序更新

## 多连接日志补丁

`logging_multiclient_fix.py` 用 selector 同时处理最多 64 个客户端，
每个连接独立保留 UTF-8 解码器和未完成行，避免空闲长连接阻塞其他 CatLog 日志。
接收仍使用原 Logging 页的 Qt 信号；单客户端行缓冲有上限。

放入 VitaDeck 安装目录的 `components/custom/`，完全退出并重启。
启动日志包含 `vitadeck-multiclient-20261102`。
补丁加载独立于可选状态页开关；移出该文件并重启即可停用。

PC 日志保存在 VitaDeck 目录的 `logs/vita-console.log`，
单文件最多 8 MiB，另保留一份 `.1` 轮转备份。
设备端仍使用原异步控制台通道。连接与观测口径见
[诊断页](../../doc/06-性能采集与分析.md)。

```powershell
python tools/vitadeck/test_logging_multiclient.py -v
```

测试覆盖空闲连接并存、同 IP 半行隔离、UTF-8 拆分和断线末行、文件保存。
`--baseline` 和 `--baseline-marshal` 用于旧接收器对照，具体参数见帮助。
历史实机接收记录见[18 号收据](../../doc/evidence/18-vitadeck-log-reception.json)。
上游为 [vita-ultra-parse](https://github.com/Rocroverss/vita-ultra-parse)。

## 更新 EBOOT

关闭游戏，开启设备 FTP，使用当前设备 IP：

```powershell
python tools/vitadeck/push_eboot.py --host <设备IP> --eboot release/eboot.bin --backup .work/vita/old-eboot.bin
```

脚本默认更新 `ux0:/app/KNIF00540/eboot.bin`，
上传后 RETR 读回并比较字节数与 SHA，不上传或删除游戏资源。
设备配置是否覆盖程序默认值，应查看启动日志，不能沿用旧 r8 的 mask 推断当前配置。

`--replay` 参数仅为历史测试包保留。
当前游戏已移除固定 GPU 回放入口，写入 `gpu-replay.txt` 不会触发回放。
旧日志可由 `tools/vita_gpu_replay.py` 分析，缺少完整 ABBA 或像素证据时不判通过。
