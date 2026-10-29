These are original synthetic decoder fixtures, with no game pixels or metadata.

- `alpha.png` (83 bytes): a 2 by 2 RGBA PNG. Row-major pixels are
  `(255,17,0,0)`, `(0,255,31,64)`, `(7,11,255,255)`, `(89,45,12,127)`.
- `colors.jpg` (294 bytes): a 16 by 8 baseline JPEG, quality 100, no chroma
  subsampling, optimized Huffman tables. The left 8 columns are `(240,20,10)`
  and the right 8 are `(15,225,35)`; the decoder probe allows an RGB error of 3.

The PNG was encoded with Python `struct`/`zlib`. The JPEG was encoded with
Pillow. `image_probe.rs` embeds only these 377 bytes in the development probe.
