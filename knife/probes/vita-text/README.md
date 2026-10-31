# 原始 FontAtlas / Vita 文字探针

这个独立 crate 直接编译 `../../knife/src/text.rs`，复用原来的 `ab_glyph`、`ttf-parser`、CFF 修正、BFFNT 外字表、英文换行表和消息排版。它有自己的 workspace 与 Cargo.lock，不增加 platform → game 的依赖，也不改游戏入口或生产字体引擎。

Vita bin 名为 `vita-text-probe`，DEV title ID 为 **KNIF00008**。VPK 打包与模拟器运行由主任务接线；这个目录只提供编译、字体/排版测试和真实原生调用入口。

## 真实输入

主机测试默认从当前发布目录 `../../../dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets` 加载这 6 个现有文件；也可显式设置 `KNIFE_TEXT_ASSETS`。任何必需字体缺失都会失败。

- `en/HelveticaNeueLTGEO-55Roman.otf`
- `zc/arshanghaisonggbpro_lt.otf`
- `nintendo_udsg-r_std_003.bfttf`
- `nintendo_udsg-r_org_zh-cn_003.bfttf`
- `nintendo_udsg-r_ext_zh-cn_003.bfttf`
- `exfont_nx01.bffnt`

每个输入限制为 1 到 8 MiB，并检查读取长度。英文消息使用 Helvetica，澄空消息使用原有 `arshanghaisonggbpro_lt.otf`；系统/气泡字体保持原项目的优先顺序：英文 standard，中文 original Chinese → extension Chinese → standard。Witch 的 variant-3 排版 metrics 取第一优先字体，字形查找仍保留完整 fallback 链。外字表来自真实 BFFNT；英文断词表直接 include 原项目的 `knife/data/exe_tables_witch.bin`。

Vita 上这 6 个文件的根目录为 `ux0:data/knife-woh-dev/text-assets`，保留上面的相对路径。不把字体复制进本探针源码或嵌入 ELF/VPK；只有原项目已有的换行表以原有 bundle 嵌入。具体输入大小与 SHA-256 在 `doc/vita-text-verification.json`。

## 测试与编译

在 Linux、macOS 或任意已配置的 WSL 发行版中执行（需 Rust；ARM 编译还需 VitaSDK）：

```sh
bash ./test-host.sh green
bash ./build-arm.sh
python3 ./record-verification.py
```

缓存默认为 `${XDG_CACHE_HOME:-$HOME/.cache}/knife-vita-text-agent`，可通过 `CARGO_TARGET_DIR` 覆盖，不使用主任务的 Vita cache。ARM 编译用 nightly-2026-05-28、`build-std=std,panic_abort`、VitaSDK GCC，并保持 `--locked`。默认允许 Cargo 获取缺失依赖；设置 `KNIFE_VITA_OFFLINE=1` 才启用 `--offline`。脚本保留现有 PATH，添加 `${CARGO_HOME:-$HOME/.cargo}/bin`，并复用生产入口发现 VitaSDK；也可显式设置 `VITASDK`。ELF 默认位于缓存的 `armv7-sony-vita-newlibeabihf/vita/vita-text-probe.elf`。

主机测试编译当前 `knife-render/src/vita.rs` 的独立测试快照。`build.rs` 只调整 recorder 模块路径、原上游测试模块注册并追加测试查询接口，功能代码不改。它显式排除依赖 desktop backend 的上游 facade 测试模块；主工程仍拥有那些回归。原 `text.rs` 的一个依赖旧 romfs 的 legacy test 被 `--skip` 显式过滤，本探针新增的 10 个测试强制使用上述真实字体，不能因缺少输入而跳过。

10 个测试覆盖：

- 两种语言的真实 message/system FontAtlas、原始 markup、ruby、斜体、颜色、连破折号，以及 Text / RimText / Alpha 的原生绘制命令。
- 非 sRGB、线性采样的 2048×2048 **R8** 字形缓存；逐像素比较实际 atlas 覆盖值、区域外零值、局部 packed 上传与重复字形缓存复用。修改一个实际 atlas texel 的负控制必须被拒绝。
- 真实英文 CFF 的 59 处修正，以及重音字符、引号、破折号的有效 glyph/outline。
- 真实 shared font 的顺序、中文 fallback 的实际字形覆盖、Witch 的 first-font metrics。
- 真实 BFFNT 外字 sheet 的 upright R8 内容及长破折号复用。
- 原英文断词表的单词换行、中文行首禁则、ruby 的大小/基线/reveal order、斜体顶点。
- 选定坏字体、截断头、非法 atlas 区域在原生上传前报错；这只是明确列出的负用例，不代表 BFFNT/TTF 全格式安全审计。
- deferred encoder 在命令退休前保活实际 atlas/exfont 资源，释放后每个纹理仅销毁一次。

现有 atlas 的 2048×2048 是运行时 glyph cache，并非 1080p 游戏图片包。场景沿用原游戏 1920×1080 的逻辑坐标，由实际 Vita facade 创建 **960×540 物理渲染目标**。

## DEV 运行契约与验证边界

运行日志为 `ux0:data/knife-woh-dev/text-probe.log`。按 EN → ZC 顺序记录 `STAGE ... BEGIN`、`STAGE ... FRAME READY`、`STAGE ... HOLD COMPLETE`；每次 `FRAME READY` 刷新日志后，Vita bin 保留 page、target 和字体纹理至少 **4 秒**，供主任务分别抓帧。最终 `RESULT PASS` 只表示字体读取、原始布局、纹理上传和原生命令提交成功。

主机 recorder 实际检查 R8 字节及 GXM 调用参数，但不执行 GPU shader。ARM ELF 编译/链接成功也不能证明文字已经在屏幕可见、Text/RimText 的最终混合或边缘效果正确。截图/像素 oracle、Vita3K/真机实际显示、帧率与峰值内存仍由后续运行证据确认。当前 probe 没有接入游戏完整 UI、脚本或音频。
