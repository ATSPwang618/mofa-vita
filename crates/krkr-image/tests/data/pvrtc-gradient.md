The 32x16 fixture is generated from our own synthetic BGRA pattern:
`B=x*7, G=y*15, R=(x*5+y*9)%256, A=(x*7+y*13)%256`.
Input: uncompressed 32-bit TGA, top-left origin, eight alpha bits.

`psp2pvrt -i source.tga -o encoded.pvr -d decoded.dds -f SCE_GXM_TEXTURE_FORMAT_PVRT4BPP_ABGR -m 0 -th 1 -wa`

The `.pvr` is the generated single-level PVRTC-I payload and header. The `.rgba`
is the reference decoder's BGRA8 DDS payload reordered to RGBA, without its DDS
header. No SDK executable or source is included. This exercises rectangular
Morton ordering, wraparound interpolation, translucent endpoints and modulation.
