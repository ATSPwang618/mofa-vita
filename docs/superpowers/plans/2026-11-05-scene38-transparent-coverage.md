# Scene38 transparent coverage implementation record

Goal: reduce proven transparent fragment coverage in the scene38/PC1246 IMG0436 Sprite/op7 layer without changing 960x540 resolution, original texture, blend order or shader math.

Final architecture: an exact manifest/archive/index/layout/KIMG-SHA identity check attaches an immutable marker to the GpuTexture before sharing. The renderer converts one admitted original Quad into one vertex stream of 68 disjoint source-support rectangles (408 vertices). It remains one Draw with shader29, generic shader17, or one background Copy plus one Draw without framebuffer fetch. Do not split the stage layer into separate Quads: that would create repeated background copies on the fallback path.

Completed:

1. Reviewed original MZX, current runtime assets, source support, batching and non-fetch boundaries; see doc/23.
2. Implemented source identity, immutable marker, exact full-UV affine material gate, one sparse vertex stream and complete-quad fallback.
3. Rebuilt support using exact DXT5 palette alpha positivity before u8 rounding, then a two-source-texel halo and 16px tiles. Independent read-only verification: 68 rectangles, 67,696 source pixels, no overlap or omitted halo.
4. Passed full game/renderer/perf host tests, C/Rust image tests, native effect input checks and Vita ARM/VPK build.
5. Delivered versioned EBOOT/VPK locally with default VitaDeck logs. No resource conversion or remote write.

Not shipped: layer35 Normal -> layer34 Multiply fusion, group0 mask changes, transform approximations, UV mirroring/cropping support or resolution reduction. Numeric equivalence across F16/fixed-blend/U8 boundaries was not established for fusion.

Hardware validation remains pending: same scene/animation phase frame timings, actual optimization hits and visible star-edge regression. Source coverage is not a measured FPS gain.

Implementation and artifact evidence: doc/24-scene38-透明覆盖优化.md and doc/evidence/29-stage-effect-r4-sparse.json.
