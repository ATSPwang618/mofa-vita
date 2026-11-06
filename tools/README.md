# 工具索引

从项目根目录运行脚本。当前正式目标只有 **KNIF00540 / 原生 GXM / 960×540**。工具生成的日志、暂存文件放在 `.work/`；资源长期只保留 `dist/` 中的一份运行包和一份可编辑工程，见 [仓库维护](../doc/09-仓库维护与清理.md)。

## 构建与当前包检查

| 入口 | 用途 |
|---|---|
| [build-vita.py](build-vita.py) | 统一构建入口；`--check` 检查环境，`game release` 生成正式 VPK |
| `build-vita.ps1` / `build-vita.sh` | Windows 包装与 WSL/Linux 构建实现 |
| [check-vita-package.py](check-vita-package.py) | VPK 的引擎文件、SFO、LiveArea 和模块身份检查 |
| `vita-build-receipt.py` / `sanitize-vita-elf.py` | 构建收据与 ELF 路径脱敏 |
| `generate-runtime-shaders.py` / `compile-vita-*.ps1` | 当前运行时 shader 源生成及基础 shader 离线编译 |
| `offline_vita_shaders.py` / `generate-material-variants.py` | 85个离线GXP身份校验、编译与有限材质入口生成 |
| `script_census.py` / `script-census/` | 生产MZX/motion解析器全场景盘点；完整输出默认放`.work/reports/script-census.json` |
| `vita_gpu_replay.py` / `test_vita_gpu_replay.py` | 有限GPU回放的ABBA、实际命中、像素差分与性能门槛分析 |

完整依赖和输出路径见 [构建文档](../doc/03-构建与工具链.md)。编译引擎不需要游戏素材。

## 素材与可编辑工程

| 入口 | 用途与边界 |
|---|---|
| [quality540](quality540/README.md) | 从自备原图重做剧情图库；一次 DXT1/DXT5 编码，保护长图、逻辑坐标、语言条带和蒙版 |
| [limit_runtime_art.py](quality540/limit_runtime_art.py) / [verify_runtime_art.py](quality540/verify_runtime_art.py) | 全库离线缩小潜在 RGBA 大图，生成只含变化归档的暂存输出；单图 1 MiB 分配限额及逐条校验，见 [14](../doc/04-资源调度与缓存.md) |
| [credit-intro](credit-intro/README.md) | 感谢页源图、更新/检查脚本与回归测试 |
| `generate_vita_asset_profile_540.py` | 清单与启动 profile；`--check` 核对关键文件和已生成源码 |
| `generate-vita-atlas-index.py` / `generate-vita-stream-index.py` | 图集成员索引与音频文件名索引 |
| [prepare_vita_ui_ranges.py](prepare_vita_ui_ranges.py) | 将选中的公共菜单/存读档及图录/书库等UI外层 NX 离线去除，逐张校验 KIMG 原字节，另存暂存结果；`--check` 校验直接读取布局，见 [17](../doc/04-资源调度与缓存.md) |
| `prepare_game.py` / `create_540p.py` / `create_compact.py` | 历史初制链路；需要自备原始输入、字体、汉化和对应宿主工具 |
| `make-vita-assets.py` / `pack_dxt_540.py` / `create_native540.py` / `migrate-vita-native-color.py` | 历史基包的纹理与色彩契约转换；它们的旧 codec8 中间输出不能直接替代当前包 |
| `compact_media.py` / `slim_languages.py` / `optimize_compact.py` | 初制媒体、双语精简与旧 PNG/JPEG Compact 优化；输入契约见各脚本帮助 |
| `inspect_textures.py` / `analyze_compact_storage.py` | 纹理和存储分析，需匹配相应输入 |
| [audit-vita-texture-formats.py](audit-vita-texture-formats.py) | 只读核验可编辑图片 SHA，扫描完整 RGBA 颜色/透明度，对应运行包 DXT，检查精确保留的 P4/P8 候选；不改图，说明见 [调色板排查](../doc/04-资源调度与缓存.md) |
| [audit-vita-sparse-coverage.py](audit-vita-sparse-coverage.py) | 只读核验 IMG0436 的归档身份和精确 DXT5 非零 alpha，检查覆盖表没有遗漏过滤邻域、没有重叠；`--generate-rectangles` 可另存计划，不修改资源；见 [24](../doc/05-优化记录与实测数据.md) |

可编辑图片从当前 Vita 数据解码得到，不能把 PNG 编辑目录直接当作运行时归档。素材改动后要更新清单、profile、atlas/stream 索引并重编 EBOOT，见 [素材文档](../doc/08-素材与安装.md)。

`audit-vita-sparse-coverage.py` 需要 Python、NumPy 和 Pillow。本机使用 Windows Python 运行；WSL 的默认 Python 未安装这两个图像分析依赖。该工具只生成几何覆盖证据，不是 GXM 性能测试。

## 开发验证

- `test_vita_render_host.py`、`test-vita-effects-inputs.sh`、`test-vita-gxm-*`、`test-vita-image-*`、`test-vita-gxt.sh`、`test-vita-compressed.sh`：渲染/图像/原生边界的定向测试。
- `test-vita-extra-ui.py --assets <配套资源目录> --output <不存在的暂存目录>`：在宿主直接引入生产 ByteStage、AtlasCache 和静态索引，必测全部 675 张 Extra 成员、零 NX 解压、分配上界、真实错误重试和活跃引用回收；不运行 GXM，也不代表实机速度。`test_prepare_vita_ui_ranges.py` 覆盖离线封装、字节一致性及损坏拒绝。
- `verify-vita-*-frame.py`、`check-vita-*-pixels.py`、`vita_capture_check.py`：像素与捕获校验。
- `vita-*-evidence.mjs` 及对应测试：开发探针证据协议；`stage-vita-gxt-*`：独立暂存与转换验证。
- `verify-vita.mjs`、`verify-vita-release-integration.mjs`、`stage-vita3k-memory-fix.py`、`vita3k-owned-debugger.cpp`：可选模拟器工具，需另外提供 Vita3K/固件/控制接口。
- `read-vita-performance.py`、`test-vita-performance.py`、`summarize-vita-artemis-performance.py`、`audit-vita-*`：观测分析；部分依赖本地未入库模块和历史输入，不能视为干净检出即可运行，详见 [性能文档](../doc/06-性能采集与分析.md)。
- [vitadeck](vitadeck/README.md)：PC 端多连接日志接收补丁、真实 TCP 回归和有界文件保存；使用 VitaDeck 自定义组件入口，不改游戏程序。

探针、模拟器和 host 通过只证明对应测试边界，不代表新包已完成实机回归。

## 桌面对照与历史来源

`build-windows.sh`、`build-540p.ps1`、`build-compact.*` 和相应 `verify_*` / `test_*` 保留给桌面对照及历史初制；需要其自己的输入与依赖，不是当前 Vita 包验证入口。

已删除依赖失效文档或已移除 ZIP 格式的 `package_project.py`、`verify_compact_package.py`、`write_compact_delivery.py`、`cleanup-obsolete-assets.ps1`、`verify_current_compact.py`。历史内容通过 Git 查看，不再维护多个相互冲突的交付入口。
