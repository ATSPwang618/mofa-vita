# krkr-render-gles2

OpenGL ES 2.0 image operations for the Vita host. The host owns EGL and keeps it
current until the renderer and every image have been dropped. Neither `Gpu`
nor `Image` is `Send` or `Sync`.

Images use tiled GPU storage, copy-on-write, bounded texture/staging budgets,
and logical dimensions for converted assets. Drawing covers legacy blend modes,
affine and separable stretch filters, glyph masks, indexed lines, meshes,
perspective, warps, image adjustments and all `Filter` variants. Scene trees and
transitions run through the Vita host's ordered window protocol. CPU pixel
storage is used only for uploads and explicit readback. Display composition and
transition surfaces use physical resolution while retaining script coordinates.
The Vita hardware-video path remains integration work.

The Linux tests require EGL/GLES libraries and a Mesa surfaceless driver. They
compile GLSL ES 1.00 and compare actual rendered pixels; missing libraries fail
the tests. Run from the workspace root:

```sh
EGL_PLATFORM=surfaceless LIBGL_ALWAYS_SOFTWARE=1 MESA_GLES_VERSION_OVERRIDE=2.0 \
  cargo test -p krkr-render-gles2 -- --test-threads=1
```

The text and scene-cache pixel tests also run on Windows with ANGLE. Add the
directory containing `libEGL.dll`, `libGLESv2.dll` and their dependencies to
`PATH`, then run:

```powershell
cargo test -p krkr-render-gles2 --features windows-gles-tests --test text --test scene_damage
```

These tests request an ES2 context and serialize access to ANGLE's shared
display. They are disabled by default on Windows.

The Vita package uses the repository's `cargo vita` configuration:

```sh
cargo vita build vpk --release -p krkr-host-vita
```

The software Mesa checks and successful Vita linking do not establish native
PowerVR shader execution or frame rate; those require the device.
