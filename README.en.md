# mofa-vita · Witch on the Holy Night

**Native PS Vita GXM engine · 960×540 scenes and compositing**

[中文](README.md) · [Project website](https://atspwang618.github.io/myvita.github.io/mofa-vita/) · [Hardware demonstration](https://www.bilibili.com/video/BV1vUxn6nE59) · [Documentation](doc/README.md)

Based on [AuroraWright/knife](https://github.com/AuroraWright/knife) 0.9.4.
The Rust engine reads Switch game data and renders through native GXM, centered on the Vita's
960×544 display. The desktop WGPU backend remains available for development.
Supply your own game data, translation, keys and system fonts.

## Installation

1. Install the matching `dist/mofa-vita.vpk` with VitaShell.
2. Close the game and copy the contents of `dist/ux0/` into the device's `ux0:`.
   Assets belong at `ux0:data/WitchontheHolyNight/assets/`.
3. Replace the complete `assets` directory while keeping saves and settings in its parent.
   Use the executable and assets from the same delivery.

The current pack preserves the **70% variant scaling and PVRTC2 4bpp / DXT mixed format**.
Audio uses `voice.mrg` and `bgm.mrg`; no loose `stream/` directory is needed.
The actual manifest and compiled indexes are bound and checked before subsequent builds.

L+Square opens reading/rear-touch settings. The story font supports 50–150%.
See [installation and settings](doc/08-素材与安装.md) for the complete input policy.

## Building

- [Asset guide](doc/10-switch-asset-extraction.en.md): `prepare-mofa-vita.py` merges BASE and update,
  then overlays matching Chengkong MOD files.
- [VPK build guide](doc/03-构建与工具链.md): VitaSDK, the pinned Rust toolchain and synchronized metadata.
- [Diagnostics](doc/06-性能采集与分析.md): running build identity, effective settings and device logs.

Conversion also requires external keys, firmware fonts, the locked FFmpeg build and Vita SDK encoders.
Stage checkpoints support resuming. A complete rebuild after deleting every conversion cache has not
been independently accepted. The delivery passed host asset and VPK checks; fresh Vita gameplay,
image-quality, performance and long-session validation remain pending.

Historical hardware screenshots are shown in the [Chinese README](README.md#实机截图).

## Layout and licensing

[knife/](knife/README.md) contains the engine,
[vita_asset_pipeline/](vita_asset_pipeline/README.md) the current resource pipeline,
[tools/](tools/README.md) development utilities, and [doc/](doc/README.md) the documentation.
`dist/`, `release/` and `.work/` are local outputs, not repository inputs.

Engine license: [MPL 2.0](knife/LICENSE); provenance: [UPSTREAM.txt](knife/UPSTREAM.txt).
Third-party dependencies retain their own notices. The locked recipe contains explicitly identified
historical game-derived encoding deltas; see its
[scope and licensing notes](vita_asset_pipeline/production/tools/source_replay/README.md).

Special thanks to [SpriteLisen](https://github.com/SpriteLisen) and [DeQxJ00](https://github.com/DeQxJ00) for the technical support they provided, which made an important contribution to this project.
Thanks also to @今何求 for technical support.
