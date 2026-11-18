# 工具入口

从仓库根目录运行。当前资源制作入口为
[`prepare-mofa-vita.py`](../prepare-mofa-vita.py)，实现和必要依赖集中在
[vita_asset_pipeline/](../vita_asset_pipeline/README.md)。
这里保留引擎构建、专项开发工具及历史制作工具，参数以各脚本 `--help` 为准。

## 常用操作

| 任务 | 入口 |
|---|---|
| 资源样本、全量转换、验收、同步清单 | [资源制作指南](../doc/10-Switch原版资源提取.md) |
| 交付清单与编译索引一致性 | `python vita_asset_pipeline/build_binding.py --check` |
| 配套 VPK | [build-companion.sh](../vita_asset_pipeline/packaging/build-companion.sh)，见[构建指南](../doc/03-构建与工具链.md) |
| 引擎环境与通用构建 | `build-vita.py --check`、`build-vita.py game release` |
| 离线 shader 身份与生成 | `offline_vita_shaders.py`、`generate-runtime-shaders.py`、`generate-material-variants.py` |
| 原生构建收据与符号处理 | `vita-build-receipt.py`、`sanitize-vita-elf.py` |
| 日志接收与 EBOOT 更新 | [vitadeck/](vitadeck/README.md) |
| 脚本静态盘点、历史回放分析 | `script_census.py`、`vita_gpu_replay.py`，见[诊断](../doc/06-性能采集与分析.md) |

`check-vita-package.py` 保留通用引擎包检查；
其 `--release` 策略还要求内置编译器模块，不适用于本次无 Sony 模块的配套 VPK。
后者由 `finish_companion.py` 检查 SELF、SFO、LiveArea、ZIP 和清单绑定。

## 专项开发

- 图片与纹理：`vita_image_metadata.py`、`audit-vita-texture-formats.py`、
  `audit-vita-sparse-coverage.py` 及对应定向测试，规则见[架构](../doc/01-架构总览.md)。
- UI 范围读取与感谢页：`prepare_vita_ui_ranges.py`、
  [credit-intro/](credit-intro/README.md)。
- 素材元数据：`generate_vita_asset_profile_540.py`、
  `generate-vita-atlas-index.py`、`generate-vita-stream-index.py`；
  当前整包通过流水线统一同步，不分别运行后遗漏绑定收据。
- 渲染与平台回归：`test_vita_render_host.py`、`test-vita-gxm-*`、
  `test-vita-image-*`、`test-vita-extra-ui.py` 等，按改动选用。
- 模拟器与捕获：`verify-vita*.mjs`、`verify-vita-*-frame.py` 等，
  需要独立环境；宿主/模拟器通过不能作为实机 GXM 验收。

## 历史制作工具

| 工具组 | 保留用途 |
|---|---|
| `rebuild_final_assets.py`、`replay_*.py`、[source_replay/](source_replay/README.md) | 旧固定 logo 包与历史编码配方；当前生产副本锁在流水线目录 |
| [quality540/](quality540/README.md) | 早期高清基础图及 RGBA 预算制作，不能单独替代当前分块资源 |
| `export-original-switch-png.py`、`finalize-original-switch-png.py`、`repair-original-switch-scaling.py` | BASE 原图母版与语言/采样审计 |
| `prepare_game.py`、`create_540p.py`、`compact_media.py`、`slim_languages.py` | 初制、媒体转换及语言精简 |
| `rebuild_from_nsp.py`、`recover_runtime_recipe.py`、`recovery_kit.py` | 历史调查；部分辅助函数仍为生产依赖，不能整文件按死代码删除 |

旧脚本的默认路径可能指向历史目录，运行前检查输入和输出。
历史数量、画质和性能记录见[证据索引](../doc/evidence/README.md)。
当前不保证每个调查脚本在干净环境都可直接执行，工作目录维护见[构建指南](../doc/03-构建与工具链.md)。
