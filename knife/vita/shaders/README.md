# Basic GXM shaders

Build the original Cg sources with the user's locally installed offline compiler:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools/compile-vita-shaders.ps1
```

Set `KNIFE_VITA_CGC` or pass `-Compiler` to select another installed compiler.
The script never copies the compiler or Sony runtime modules. It fixes the
profile, entry point and precision/optimization flags, compiles each shader
twice, checks GXP magic and size, and rejects differing output digests. The
receipt records the exact compiler, source, script, flags, generated header and
GXP identities. Generated binaries remain under `.work/vita/shaders`; the C11
header used by the native build is `generated/shaders.h`.

The vertex shader takes `aPosition` (`float2`, clip space), `aTexcoord`
(`float2`) and `aColor` (`float4`). It emits a position at depth zero and
passes texture coordinates and color through. The fragment shader samples
`uTexture` at texture unit 0 and multiplies all four channels by the interpolated
color. The caller controls viewport placement and blend configuration. A white
1×1 texture also supports solid colored quads without a separate program.

These programs only cover the basic rendering probe. The game's masks, blur,
color conversion, transitions and other effects are not implemented here. File
checks and repeatable compilation do not establish that `sceGxmProgramCheck`,
program patching, rasterization or actual Vita hardware work; those checks must
be recorded by the runtime verification.
