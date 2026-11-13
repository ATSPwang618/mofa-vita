# mofa-vita · Witch on the Holy Night

**A native GXM engine port for PS Vita · 960×540**

[![简体中文](doc/language-zh.svg)](README.md)

[Project website](https://atspwang618.github.io/myvita.github.io/mofa-vita/) · [Device showcase](https://www.bilibili.com/video/BV1vUxn6nE59) · [Technical documentation (Chinese)](doc/README.md) · [Optimization inventory (Chinese)](doc/05-优化记录与实测数据.md)

Based on [AuroraWright/knife](https://github.com/AuroraWright/knife) 0.9.4, this Rust engine runs user-supplied data from the Switch release of *Witch on the Holy Night*. Vita rendering uses native **GXM**, with **960×540** scenes and composition centered on the 960×544 display. The desktop WGPU backend remains available for development comparisons.

The repository contains engine sources, build tools, offline shaders and resource metadata. Supply game assets, fonts, audio, video and translations separately. **Building the engine does not require the full game pack.** See the [Switch asset extraction guide](doc/10-switch-asset-extraction.en.md).

## Device screenshots

| Title | Voice settings |
|---|---|
| ![Title](doc/screenshots/01-title.jpg) | ![Voice settings](doc/screenshots/02-voices.jpg) |
| **Archive** | **Story** |
| ![Archive](doc/screenshots/03-archive.jpg) | ![Story](doc/screenshots/04-story.jpg) |

Historical device screenshots demonstrate the interface, not performance acceptance of the latest candidate.

## Current version

The local candidate is `perf-render-20261109-213051-210a811-story-font-local`, title ID `KNIF00540`. Its timestamp identifies the local `perf/render` base commit; `local` indicates uncommitted changes. Use the build receipt and SHA256 to distinguish individual builds. See [installation and validation](doc/08-素材与安装.md).

- **Material routing across the game:** 85 offline GXP programs cover 24 PS blend operations and bounded specialized material combinations. Unsupported combinations retain the complete fallback.
- **Reduced GPU and resource work:** native DXT1/DXT5 uploads, destination fetch, Group composition and conservative bounds, member reads, shared caches and safe retirement.
- **Stability measures:** direct UI atlas member reads, bounded allocation recovery, protection of the last valid progress point, save/load focus fixes and approximately one second of BGM PCM buffering.
- **[Reading and input settings](doc/11-可选交互更新.md#english):** L+Square opens a menu with 50%–150% story font size in 5% steps; Circle saves and Cross cancels. Enlarged long passages scroll with revealed text. Rear-touch L2/R2 simulation is off by default. The package includes PlayStation prompts. Story rewind, fast-forward and scene skipping remain disabled; automatic playback remains available.
- **Local build workflow:** a GitHub Actions workflow has been added but has not run remotely. The stale GPU-headroom rejection fix is also integrated; see [integration scope](doc/11-可选交互更新.md#english).

See the [optimization inventory](doc/05-优化记录与实测数据.md) for switches, historical results and remaining validation. Enabled experiments are not proof of improvement. Neither 20 FPS throughout the game nor elimination of every resource failure is guaranteed.

## Build and install

Run in the repository root on Linux/WSL, with VitaSDK and pinned Rust `nightly-2026-05-28`:

```bash
python3 tools/build-vita.py --check
KNIFE_BUILD_TAG=<build-version> python3 tools/build-vita.py game release
```

See the [build guide](doc/03-构建与工具链.md) for Windows setup and dependencies. Successful builds always produce:

| Local file | Purpose |
|---|---|
| `release/mofa-vita.vpk` | Installable package; full game assets are not included |
| `release/eboot.bin` | Stop the game, then replace `ux0:app/KNIF00540/eboot.bin` through VitaDeck |
| `dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets/` | Matching resources; copy to `ux0:data/WitchontheHolyNight/assets/` |

With the current r11 resource pack installed, this update requires only the executable/package. The Vita UI is fixed to Chengkong Chinese ID2; resources retain English and Chinese. See [installation](doc/08-素材与安装.md) for manifest validation, older packs and recovery.

VitaDeck logging is enabled by default. An explicit `0` in `perf-enable.txt` under the data root disables it; `1` enables it. Check `build_tag` at startup and receiver settings after network changes. See [logging](doc/06-性能采集与分析.md).

## Repository navigation

[Engine](knife/README.md) · [Tools](tools/README.md) · [Technical documentation](doc/README.md) · [Maintenance policy](doc/09-仓库维护与清理.md)

`release/` holds the current program; `dist/` holds one runtime asset pack and one editable PNG project. `.work/` holds build staging, symbols and verification records. These directories are ignored by Git. Changes remain local unless pushing is explicitly requested.

## Credits and licensing

Engine: [AuroraWright/knife](https://github.com/AuroraWright/knife), [MPL 2.0](knife/LICENSE), [upstream provenance](knife/UPSTREAM.txt). Third-party dependencies retain their licenses under `knife/vita/vendor/`. Game content belongs to its respective rights holders and is not distributed with the source. Special thanks to @SpriteLisenLisen @DeQxJ00 for technical support.