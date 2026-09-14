# mofa-vita-krkr

**《魔法使いの夜》（魔法使之夜krkr版本）的 PS Vita 启动器。**



引擎侧是 [Yuri](https://github.com/YuriSizuku/Kirikiroid2Yuri)（Android 版 KiriKiri2 /
Kirikiroid2 一脉）加上 VitaGL 呈现层：游戏保持原样，继续使用它自己的脚本、XP3 数据包、
存档和插件，不做任何数据上传。

| 项目 | 说明 |
| --- | --- |
| 平台 | PS Vita / PS TV（需 HENkaku 或 enso），也可用 Vita3K 测试 |
| 标题 ID | `MOFA00001` |
| 数据根目录 | `ux0:data/mofa-vita/`（游戏固定在 `game/`，补丁在 `patch/`） |
| 实机需要 | `libshacccg.suprx`|

## 当前状态

| | |
| --- | --- |
| Vita3K | **已实测可进入界面，目前仅仅模拟器测试**|
| 真机 PS Vita | 构建通过全部契约，但未实机验证 |
| 已知缺口 | 若干特效插件为空实现；`stringUtil.dll` 的 `isNumber/initSpline` 未实现；Vita3K 上建议加 `tvpgl-scalar` 标记（见下） |

## 功能说明

**解密过滤器。** 本作 XP3 里的正文是加密的，必须由过滤器读出。启动时程序按固定顺序确定：

1. `ux0:data/mofa-vita/patch/xp3filter.tjs`（把游戏「解密补丁」里的那份复制过来即可）；
2. 游戏目录自带的 `xp3filter.tjs`；
3. **Phase 1 归档推断**：直接采样真实 XP3 数据、合成规则，并且**用归档数据验证通过后**才使用（结果缓存在 `ux0:data/mofa-vita/heuristics/<指纹>/`）；
4. 以上都不成立时，启动会带着确切原因停下，而不是猜一个会把数据解坏的过滤器。

**兼容补丁。** 同一目录下的 `patch.tjs`（Kirikiroid2 社区补丁）会在启动时执行，它把本作
用到的 Windows 插件入口换成 TJS 实现——这是这作能在非 Windows 平台跑起来的关键。

**引擎覆盖范围。** KAG 3.29 / KiriKiri 2.32 脚本、存档、历史记录、视频（FFmpeg）、音乐与
音效（OpenAL）、通过 `msgothic.ttc` 的字体，以及文末列出的插件清单。

## 尚未支持

- **若干插件只做到"可链接"**：`LayerExFilter.dll`、`LayerExParticle.dll`、`LayerExYaDraw.dll`、
  `filter.dll`、`clipboardEx.dll`、`windowEx.dll`，以及 `stringUtil.dll`、`Equations.dll`、
  `json.dll` 等长尾名字已按发布名登记，`Plugins.link()` 不会再让启动中断，但它们的**滤镜像素、
  粒子、集中线、剪贴板与 Win32 窗口行为没有实现**。本作靠 `patch.tjs` 把这些入口换成 TJS
  实现，所以正常游玩不受影响；缺的是纯视觉效果。


- **链接和效果是两件事**：登记一个模块名只保证"链接成功"。真机上遇到某个特效没出现，先查
  `game/savedata/krkr.console.log`，那多半是某个空实现被调到，而不是启动失败。


- **`stringUtil.dll` 的 `isNumber/initSpline` 尚未实现**：关键帧动画若走到样条插值，可能报缺少错误。


## 在 PS Vita 上安装

1. 用 VitaShell 安装 `mofa-vita-krkr.vpk`。
2. 准备数据目录（**目录名不能改**）：

   ```text
   ux0:data/mofa-vita/
   ├── game/                     复制整个游戏目录的内容
   │   ├── data.xp3              游戏启动
   │   ├── bg.xp3  bgm.xp3  fg.xp3  image.xp3  rule.xp3  sound.xp3     游戏资源包
   │   ├── mahoyo.xp3  mahoyo.dll  *.exe
   │   ├── plugin/               游戏附带插件（原样保留）
   │   └── savedata/             存档（可留空，首次运行自动建）
   ├── patch/                    
   │   ├── xp3filter.tjs         XP3 解密过滤器（必需，否则读不出正文）
   │   └── patch.tjs             兼容补丁（必需，插件入口靠它接管）
   ├── msgothic.ttc              字体
   ├── mofa.ini                  可选：按键映射（格式见下）
   └── boot-status.txt / error.txt / engine.log   日志文件
   ```

   注意：`game/` 里的 `*.sig` / `.index` 之类的附带文件留着无妨，启动器只按名字找需要的存储。
3. 启动游戏

## 在 Vita3K 上安装

本仓库只保留源码，VPK 需要自己构建（见文末「构建」）。构建出 `build-release-vita/mofa-vita-krkr.vpk`
之后，位置参数的含义就是"安装并运行"，不需要手动点图标：

```powershell
& '<vita3k>\Vita3K.exe' '<仓库>\build-release-vita\mofa-vita-krkr.vpk'
```

已经装过的版本可以直接启动（`-r` / `--installed-path` 要的是**标题 ID**，不是路径）：

```powershell
& '<vita3k>\Vita3K.exe' -r MOFA00001
```

Vita3K 自己不会实现着色器编译器：把真机导出的 `libshacccg.suprx`（约 3 MB，`SCE\0` 开头）放到
模拟器存储卡的 `ur0\data\libshacccg.suprx`。**放 0 字节占位文件会在游戏现场编译着色器时把模拟器
打成崩溃**——本构建会拒绝这种占位文件并给出中文提示。

数据目录与真机一致，都是 `<存储卡>\ux0\data\mofa-vita\`。

## 按键映射（可选）

**按键映射**放在 `ux0:data/mofa-vita/mofa.ini`，

```ini
# ux0:data/mofa-vita/mofa.ini —— 只有输入部分会被读取
input_mapping_version=4
analog_deadzone=0.18
cursor_speed=780
touch_enabled=true
bind.circle=key_enter
bind.cross=mouse_right
bind.square=disabled
bind.triangle=mouse_wheel_up
bind.ltrigger=mouse_left
bind.rtrigger=key_control
bind.dpad_up=key_up
bind.dpad_down=key_down
bind.dpad_left=key_left
bind.dpad_right=key_right
bind.start=disabled
bind.select=disabled
bind.left_stick=mouse_cursor
bind.front_touch=mouse_absolute
```

每个字段都有默认值，删掉某项就回到默认。改完重启应用生效。

可绑定的**来源**：`cross circle square triangle ltrigger rtrigger dpad_up dpad_down dpad_left dpad_right start select left_stick front_touch`。
可绑定的**动作**：`mouse_left mouse_right mouse_wheel_up mouse_wheel_down mouse_cursor mouse_absolute`；按键 `key_space key_enter key_escape key_pageup key_pagedown key_up key_down key_left key_right key_control key_shift key_tab key_backspace`；`menu`（等同 Escape）以及 `disabled`。

## 默认按键说明

| 输入 | 动作 |
| --- | --- |
| 左摇杆 | 移动鼠标指针 |
| 前面触摸屏 | 直接把鼠标指针放到该位置 |
| L | 鼠标左键 |
| ×（叉） | 鼠标右键 |
| ○（圈） | 回车 |
| △（三角） | 滚轮上滚 |
| R | 按住 Ctrl（快进） |
| 方向键 | 上下左右方向键 |

## 诊断文件

| 文件 | 内容 |
| --- | --- |
| `ux0:data/mofa-vita/error.txt` | 本次启动失败的原因（每次运行都重写） |
| `ux0:data/mofa-vita/boot-status.txt` | 只追加的启动追踪；最后一行就是停下的阶段 |
| `ux0:data/mofa-vita/engine.log` | 引擎日志：过滤器、补丁、插件与存储的决定 |
| `game/savedata/krkr.console.log` | KiriKiri 控制台输出，包含脚本异常 |

**说明：** `error.txt` 会先用中文说明失败原因和处理方向，再附上引擎/游戏脚本给的原文以便对照；`boot-status.txt` 里每个英文阶段标记的**下一行**就是它的中文说明（以 `#` 开头）。英文标记保留原样，因为构建校验脚本按整行精确匹配它们。

`boot-status.txt` 里几个有用的里程碑（按顺序）：
`vita-threading-self-test-passed` → `retail-xp3filter-selected` →
`retail-launch-resolved` → `vitagl-initialized` → `yuri-platform-ready` →
`yuri-storage-preflight-complete` → `yuri-startup-script-entered` →
`retail-layereximage-ready`（此处开始注册插件）→ `yuri-startup-script-complete` →
`vitagl-first-game-frame-presented` → `retail-runtime-30s-stable-with-video`。

## 常见问题

| `error.txt` 里的报错 | 原因 | 处理 |
| --- | --- | --- |
| `没有找到精确匹配的 xp3filter.tjs…` | `patch/` 里没有过滤器，且只靠归档数据解不出来 | 把本作的 `xp3filter.tjs` 放进 `ux0:data/mofa-vita/patch/` |
| `vitaGL 需要真实的 ur0:/data/libshacccg.suprx 着色器编译器` | 没装着色器编译器，或只放了一个 0 字节占位文件 | 安装/复制真的 `libshacccg.suprx`（Vita3K 见上文） |
| `Member "isExistentDirectory" does not exist` | 用的是补齐 Z 版 API 之前的旧构建 | 用本仓库重新编译 |
| `Cannot load Plugin <插件名>.dll` | 作品链接了尚未登记的 KAGEX 插件（Windows 专用 DLL，Vita 端不会加载它） | 把模块名登记进 `include/mofa/yuri_plugin_capabilities.hpp`，并在 `src/engine/retail/` 下新增同名静态注册模块，见 [尚未支持](#尚未支持) |
| `找不到游戏目录：ux0:data/mofa-vita/game` | 游戏没放到固定目录 | 把《魔法使之夜》的文件放进 `ux0:data/mofa-vita/game/` |
| `Yuri 没有跑完游戏的启动脚本（启动脚本中途中止）` | 游戏自己的启动脚本抛异常了 | 看 `game/savedata/krkr.console.log` 里的脚本异常 |
| 画面正常但某些特效没有 | 对应插件只做到"可链接" | 见 [尚未支持](#尚未支持) |

### 在 Vita3K 里偏慢 / 模拟器日志暴涨

Vita3K 的 CPU 模拟（Dynarmic）不是每条 ARM NEON 指令都能翻译。Yuri 的图像内核在 ARM 上默认安装一整套手写 NEON 版本（`tvpgl_arm.cpp`），其中混合内核里的 `vsubhn` 序列会让 Dynarmic 退回解释器——于是**每个像素的混合**都要付出一次 JIT→解释器的切换，日志里会刷出大量

```text
[ArmDynarmicCallback::InterpreterFallback]: Unimplemented instruction at address 0x...
[ArmDynarmicCallback::ExceptionRaised]: Undefined instruction ... (vsubhn.i16 ...)
```

这时在数据目录放一个空标记文件即可让引擎改用通用 C++ 内核（真机上不要放，NEON 更快）：

```text
ux0:data/mofa-vita/tvpgl-scalar
```

放好后 `boot-status.txt` 里会出现 `yuri-tvpgl-scalar-kernels`（没有标记则是 `yuri-tvpgl-neon-kernels-installed`）。

## 构建

### 在 WSL2 里构建

VitaSDK 装在 WSL 里时，一条命令即可完成"主机测试 → 编译 Vita 版 FFmpeg → 编译 Vita 后端 → 打包 VPK"：

```bash
wsl -d <发行版> -u root bash /mnt/<盘符>/<路径>/mofa-vita-krkr/scripts/build-vita-wsl.sh
```

该脚本会自己导出 `VITASDK`（非交互登录 shell 不会读 `~/.bashrc`），检查依赖，并在缺东西时打印出确切的安装命令：

- 主机端软件包：`libcurl4-openssl-dev zlib1g-dev libpng-dev libsqlite3-dev libssl-dev libfreetype-dev nasm`
- VitaSDK 软件包（`vdpm install ...`）：`boost libarchive vitaGL openal-soft opusfile`

产物是 `build-release-vita/mofa-vita-krkr.vpk`。本仓库**只保留源码**：`.cache/`、
`build-release-*/` 属于本地产物、已被 `.gitignore` 忽略，需要时由构建脚本重新生成。
注意首次构建要重新下载并编译 FFmpeg，约需半小时。

### 只构建主机端

主机端（含测试）不需要 VitaSDK：

```bash
cmake -S . -B build-release-host -DCMAKE_BUILD_TYPE=Release -DMOFA_BUILD_TESTS=ON
cmake --build build-release-host --parallel
ctest --test-dir build-release-host --output-on-failure
```

主机端只用来做过滤/补丁/签名/存储等逻辑的回归测试（19 个用例），不产出 Vita 可执行文件。

## 仓库结构

```text
CMakeLists.txt          主机端工具/测试，以及 Vita 构建入口
cmake/                  Yuri 后端生成，以及各种 fail-closed 构建校验
docs/                   设计/优化笔记（非构建输入）
include/mofa/           可移植部分（mofa）的公开头文件
resources/vita/         气泡资源、默认过滤器、启动后脚本
scripts/                构建、兼容性审计与验证脚本
src/common/             XP3 读取、过滤器启发式、补丁解析
src/engine/retail/      零售运行时用到的插件实现
src/engine/vita/        启动解析、VitaGL 呈现、早期追踪
src/platform/vita/      存储、输入、音频、字体、线程等平台桥接
src/yuri/               TJS 平台适配与过滤器虚拟机
tests/                  主机端测试套件与零售兼容性样本
vita/booter/            气泡/引导模板
```

## 插件支持清单

本作脚本会链接的模块都在下面。链接命中任何一项时，都不需要原来的 Windows DLL。

*Yuri 内核内置：* 
`menu.dll`、
`kagparser.dll`、
`wuvorbis.dll`、
`krmovie.dll`、
`motionplayer.dll`、
`squirrel.dll`。

*由 mofa-yuri-plugins 提供：* 
`addfont.dll`、
`csvparser.dll`、
`dirlist.dll`、
`fftgraph.dll`、
`fstat.dll`、
`getabout.dll`、
`getsample.dll`、
`perspective.dll`、
`savestruct.dll`、
`varfile.dll`、
`win32dialog.dll`、
`wutcwf.dll`、
`xp3filter.dll`、
`extrans.dll`、
`extnagano.dll`、
`krflash.dll`、
`gfxeffect.dll`、
`layerexdraw.dll`、
`layerexbtoa.dll`、
`scriptsex.dll`、
`layerexsave.dll`、
`layereximage.dll`、
`layerexareaaverage.dll`、
`shrinkcopy.dll`、
`sigcheck.dll`、
`psd.dll`、
`psbfile.dll`、
`sqlite3.dll`。

*已经实现、而且不是空壳的 KAGEX 插件：*

| 插件 | 状态 |
| --- | --- |
| `layerExAreaAverage.dll` | **真实实现**：上游 wamsoft 的区域平均缩小 `Layer.stretchCopyAA`，按原算法逐像素计算 |
| `layerExImage.dll` | **真实实现**：light / colorize / modulate / noise 等 |
| `layerExBTOA.dll` | **真实实现**：透明通道与选区（province）搬运 |
| `layerExSave.dll` | **真实实现**：PNG / TLG5 截图与存档图 |
| `shrinkCopy.dll` | **真实实现**：`Layer.shrinkCopy` / `shrinkCopyFast` |
| `fstat.dll`、`savestruct.dll`、`sqlite3.dll`、`psbfile.dll`、`psd.dll`、`extrans.dll` | **真实实现**（含补齐的吉里吉里Z 存储 API） |
| `gfxEffect.dll`、`motionplayer.dll`、`layerexdraw.dll`、`krflash.dll`、`extnagano.dll` | **可链接 + 控制流兼容**：状态与调用面保留，像素/渲染是空实现或替代方案 |
| `layerExFilter.dll`、`layerExParticle.dll`、`layerExYaDraw.dll`、`filter.dll`、`clipboardEx.dll`、`windowEx.dll` | **仅可链接**：名字已登记，链接不会中断启动；真实行为未实现 |
| `stringUtil.dll`、`Equations.dll`、`json.dll`、`snow3d.dll`、`messenger.dll`、`SystemExTouchImage.dll`、`base64.dll`、`qrcode.dll`、`registory.dll`、`win32ole.dll` | **仅可链接**：本作脚本会链接的 Windows 专用插件长尾，名字可解析，功能未实现 |


