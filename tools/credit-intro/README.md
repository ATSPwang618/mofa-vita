# 开机感谢页

启动时在 logo 影像与原版注意画面之间显示两页项目感谢图。每页淡入、停留、淡出合计约 5 秒，任意键跳过当前页；`skip-intro` 跳过整段。

| 文件 | 用途 |
|---|---|
| `credit_1_supporters.png` | 第一页感谢图，960×544、8-bit RGBA、非交错 |
| `credit_2_special_thanks.png` | 第二页感谢图，规格同上 |
| `apply_credit_intro.py` | 历史工作包的感谢页写入与只读检查工具 |

图片以 `KIMG` codec 7 `PngUnorm` 存入 `parts.mrg` 的第 2、3 项；第 0 项 `logo_fix` 与第 1 项 `caution` 保持原字节。启动状态机位于 `knife/knife/src/ui/witch/title.rs`，每页纹理显示结束后释放。

## 历史工具的使用边界

当前生产流水线使用 `vita_asset_pipeline/production/` 中锁定的实现与输入，感谢页也受整包校验约束。此处工具保留供美术候选包验证，默认路径仍是历史交付目录；运行时必须显式指定独立工作副本，例如：

```powershell
python tools/credit-intro/apply_credit_intro.py --assets .work/credit-candidate/assets --check
```

去掉 `--check` 会改写该副本的 `parts.hed`、`parts.mrg` 和清单；首次追加时建立备份，替换已有四项归档时保留前两项。脚本剥除 PNG 元数据块，不改变像素数据。

替换这里的 PNG 不会自动更新当前生产输入。新的美术需要先更新并核验生产输入与资源契约，再按[资源构建流程](../../doc/10-Switch原版资源提取.md)执行样本验证、整包验收、项目清单同步和 VPK 构建。当前固定校验不会接受直接修改过的交付归档。

设备端资源与 VPK 内嵌清单必须配套，安装方法见[素材与安装](../../doc/08-素材与安装.md)。不要将两项的原始 `parts` 归档覆盖到需要四项的当前引擎。临时工作副本与备份按[工作目录维护规则](../../doc/03-构建与工具链.md#工作目录与维护)清理。

脚本自身回归检查：

```powershell
python -B -m unittest discover -s tools/credit-intro -p test_apply_credit_intro.py -v
```
