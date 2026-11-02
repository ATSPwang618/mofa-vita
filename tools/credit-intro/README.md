# 开机感谢页（credit-intro）

游戏启动时，在 TYPE-MOON 影像与原版注意画面之间显示两页「感谢支持者」全屏图，向参与本项目的人员致谢。全新存档的首次启动同样会先看这两页（感谢页 → title1st → 故事）。

- 每页：黑底淡入 60 帧 + 停留 180 帧 + 淡出 60 帧 = **300 帧 ≈ 5.0 秒**（60 Hz）；任意键跳过当前页，连按两次跳过整段；命令行/配置里的 `skip-intro` 整段略过。
- 图片：`assets/parts.mrg` 的条目 **#2 / #3**（`KIMG` codec 7 `PngUnorm`，960×544，全屏 1:1 绘制），条目 #0 `logo_fix`、#1 `caution` 保持逐字节不变。
- 引擎：`knife/knife/src/ui/witch/title.rs` 启动 splash 状态机的 `69..=72` 状态（jp_8E200 原版状态之外新增）；每页纹理只在该页显示期间常驻（Vita 上约 2.09 MB），翻页即释放。

```text
启动：logo 动画 → 感谢页 1 → 感谢页 2 → 原版注意画面 → 标题
```

## 文件

| 文件 | 内容 |
|---|---|
| `apply_credit_intro.py` | 把两张 PNG 追加进 `parts` 归档（改写 `parts.hed`/`parts.mrg`），并同步更新 `vita-540p-assets.json` 里 parts 相关数字；默认带备份与自检 |
| `credit_1_supporters.png` | 第一页「感谢每一位支持者」（960×544，8-bit RGBA，非交错） |
| `credit_2_special_thanks.png` | 第二页「特别感谢」（同上） |

这两页是本移植自己的美术资源，不属于游戏素材；替换图片不需要改 Rust 代码，但必须重新生成素材元数据并重编 EBOOT（见下）。

## 更换美术后重建

1. 用同规格 PNG 覆盖 `credit_*.png`（**960×544、8-bit RGBA、非交错**；任何尺寸都能被全屏拉伸，但 960×544 是像素 1:1 的原生尺寸）。
2. 更新归档并同步清单。首次由两条目归档追加时，会建立 `.work/credit-intro/backup-original/`；已有四条目时直接替换感谢页，保留前两条原始内容。旧备份已清理时也可运行：

   ```bash
   python3 tools/credit-intro/apply_credit_intro.py          # 写入 dist/.../assets 的 parts 归档与清单
   python3 tools/credit-intro/apply_credit_intro.py --check  # 只核对，不写
   ```

3. 重生成三件套内嵌元数据（清单 SHA-256 变了，必须一起重生成）：

   ```bash
   python3 tools/generate_vita_asset_profile_540.py
   python3 tools/generate-vita-atlas-index.py dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets knife/knife/data/vita_atlas_index_540p.rs
   python3 tools/generate-vita-stream-index.py dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets/vita-540p-assets.json knife/knife/data/vita_stream_index_540p.txt
   ```

4. 重建 VPK（元数据在编译期嵌入 EBOOT）：

   ```bash
   KNIFE_BUILD_TAG=540p-credit-update-20261102 python3 tools/build-vita.py game release
   ```

   构建机与前置条件见 [doc/03-构建与工具链.md](../../doc/03-构建与工具链.md)；素材变更与元数据的关系见 [doc/08-素材与安装.md](../../doc/08-素材与安装.md) §1、§6。

## 安装

新 EBOOT 与**新素材三件套**必须一起更新，否则启动校验会拒绝（`WRONG FILE SIZE` / `WRONG ASSET VERSION`）：

1. 安装新 VPK（或把新 `eboot.bin` 推到 VitaDeck 对应路径）。
2. 覆盖设备端 `ux0:data/WitchontheHolyNight/assets/` 下的
   `parts.hed`、`parts.mrg`、`vita-540p-assets.json`。

只重编 EBOOT、不换这三个文件，或只换素材、不重编 EBOOT，都会在启动校验处失败——两者是同一套契约。

## 临时备份与回退美术

临时备份不需要长期保留：现有四条目归档本身包含前两条原始内容，`--check` 不会重建备份或写入素材。首次追加产生的备份在验证完成后可清理，遵循 [仓库维护规则](../../doc/09-仓库维护与清理.md)。

回退感谢页美术时，从 Git 历史取回本项目自己的旧 PNG，重新运行更新脚本、生成三件套并构建配套 EBOOT。不要直接把两条目的原始 `parts` 覆盖到需要四条目的当前引擎。

脚本回归检查：

```bash
python3 -B -m unittest discover -s tools/credit-intro -p test_apply_credit_intro.py -v
```

覆盖清理备份后的只读检查，以及替换已有感谢页时原始条目和清单的完整性。

## 备注

- 当前可编辑工程的 `parts/manifest.json` 已同步为 **4 条目**，包含两张感谢页；运行时归档的权威内容仍以 `dist/.../assets/parts.*` 为准。运行脚本修改素材后，可编辑导出图也应同步更新。
- 图片以 PNG 原字节存入 `KIMG` codec 7（UNORM 直通），与归档里原版 `caution` 条目同口径；`apply_credit_intro.py` 只会剥掉 `eXIf`/`tEXt` 等元数据块，不改像素数据。
