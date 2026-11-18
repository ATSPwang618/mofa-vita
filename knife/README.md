# knife · 引擎工作区

基于 [AuroraWright/knife](https://github.com/AuroraWright/knife) 0.9.4。
Vita 后端使用原生 GXM，960×540 画布居中显示到 960×544；
Vita 固定澄空中文，桌面 WGPU 后端保留作开发对照。

| 目录 | 职责 |
|---|---|
| `knife/` | 脚本、Stage、UI、文字、输入和存档 |
| `knife-core/` | 归档、图片、脚本及字体解析 |
| `knife-render/`、`knife-render-api/` | 绘制命令、材质与渲染后端 |
| `knife-platform-vita/`、`vita/` | 平台接口、原生 C 后端、shader 和依赖 |
| `knife-audio/`、`knife-perf/` | 混音、计时、预算与优化配置 |
| `kdu/`、`probes/` | 格式转换及开发探针 |

引擎构建从外层仓库根目录执行，见[构建指南](../doc/03-构建与工具链.md)。
资源与程序需成对使用，见[安装](../doc/08-素材与安装.md)；
模块合同、缓存和 GPU 生命周期见[架构](../doc/01-架构总览.md)。
工具入口见 [tools](../tools/README.md) 和[资源流水线](../vita_asset_pipeline/README.md)。

## 桌面开发

在本目录运行，使用匹配的桌面资源：

```bash
cargo +1.99.0 build --locked --release -p knife
cargo +1.99.0 run --locked --release -p knife -- -a /path/to/desktop-assets -l zc
```

桌面资源与 Vita 专用 GXT 包不能直接混用。Linux 需要 GStreamer 开发包和解码插件；
macOS 使用 VideoToolbox，Windows 使用 Media Foundation。
配置示例见 [knife.ini.example](knife.ini.example)，Vita 入口不读取此配置。

来源见 [UPSTREAM.txt](UPSTREAM.txt)，上游机制研究见 [engine-writeup.md](engine-writeup.md)，
引擎许可为 [MPL 2.0](LICENSE)。原生依赖保留各自许可，
游戏数据由使用者自行提供。[English project README](../README.en.md)。
