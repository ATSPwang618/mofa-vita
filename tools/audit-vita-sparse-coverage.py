"""Verify the admitted sparse mesh against the user's final encoded DXT asset.

Read only with respect to assets. Produces a compact JSON receipt, no game pixels.
The source domain proof includes every nonzero decoded alpha plus two texels.
It is not a GPU timing or hardware rasterization test.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import struct

import numpy as np
from PIL import Image, ImageFilter


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--assets", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--generate-rectangles", type=Path, help="write an exact-palette plan for review instead of checking the Rust table")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    assets = args.assets or root / "dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets"
    output = args.output or root / ".work/vita/sparse-coverage-proof.json"
    source = (root / "knife/knife/src/sparse_source.rs").read_text(encoding="utf-8")
    expected_manifest = re.search(r'MANIFEST: &str = "([a-f0-9]{64})"', source)[1]
    expected_envelope = re.search(r'ENVELOPE_SHA256:.*b"([a-f0-9]{64})"', source)[1]
    manifest_sha = hashlib.sha256((assets / "vita-540p-assets.json").read_bytes()).hexdigest()
    assert manifest_sha == expected_manifest, "coverage plan pack identity mismatch"
    index = 1093
    hed = (assets / "jallpac.hed").read_bytes()
    nam = (assets / "jallpac.nam").read_bytes()
    name = nam[index * 32:(index + 1) * 32].split(b"\0")[0].split(b"\r")[0].decode()
    assert name == "IMG0436.NXZ", name
    offset, stored, _ = struct.unpack_from("<IHH", hed, index * 8)
    with (assets / "jallpac.mrg").open("rb") as file:
        file.seek(offset * 2048)
        encoded = file.read(stored * 2048)
    if encoded[:4] in (b"NXCX", b"NXGX"):
        encoded = encoded[:16 + struct.unpack_from("<I", encoded, 8)[0]]
    spec = importlib.util.spec_from_file_location("sparse_runtime_gxt", root / "tools/migrate-vita-native-color.py")
    gxt = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(gxt)
    raw = gxt.unpack(encoded)
    assert raw[:4] == b"KIMG"
    version, lw, lh, w, h, codec, length = struct.unpack_from("<7I", raw, 4)
    raw = raw[:32 + length]
    envelope_sha = hashlib.sha256(raw).hexdigest()
    assert (version, lw, lh, w, h, codec, len(raw)) == (1, 1750, 1165, 624, 415, 9, 524384)
    assert envelope_sha == expected_envelope, "coverage plan encoded content mismatch"
    image, physical, backing, fmt = gxt.gxt_decode(raw[32:])
    assert tuple(physical) == (w, h) and fmt == 0x87000000
    alpha = np.asarray(image.crop((0, 0, w, h)))[:, :, 3]
    # Use exact palette positivity before u8 rounding. An interpolated DXT alpha
    # can be >0 while a software decoder rounds it to zero (e.g. 1/7 of 1/255).
    bw, bh = backing
    payload_offset = struct.unpack_from("<I", raw, 32 + 12)[0]
    blocks = gxt.unswizzle(raw[32 + payload_offset:], bw // 4, bh // 4, 16)
    exact = np.zeros((bh, bw), dtype=bool)
    for block in range(len(blocks) // 16):
        data = blocks[block * 16:(block + 1) * 16]
        a0, a1 = data[0], data[1]
        interpolated = bool(a0 or a1)
        palette = ([a0 > 0, a1 > 0] + ([interpolated] * 6 if a0 > a1
            else [interpolated] * 4 + [False, True]))
        indices = int.from_bytes(data[2:8], "little")
        x, y = block % (bw // 4) * 4, block // (bw // 4) * 4
        for pixel in range(16):
            exact[y + pixel // 4, x + pixel % 4] = palette[(indices >> (pixel * 3)) & 7]
    support = exact[:h, :w]
    halo = np.asarray(Image.fromarray((support * 255).astype(np.uint8)).filter(ImageFilter.MaxFilter(5))) > 0
    if args.generate_rectangles:
        cells = np.pad(halo, ((0, (-h) % 16), (0, (-w) % 16))).reshape(
            (h + 15) // 16, 16, (w + 15) // 16, 16).any(axis=(1, 3))
        rectangles, pending = [], {}
        for y, row in enumerate(cells):
            spans, start = [], None
            for x, on in enumerate([*row, False]):
                if on and start is None:
                    start = x
                elif not on and start is not None:
                    spans.append((start, x))
                    start = None
            following = {}
            for span in spans:
                rect = pending.pop(span, [span[0], y, span[1], y + 1])
                rect[3] = y + 1
                following[span] = rect
            rectangles.extend(pending.values())
            pending = following
        rectangles.extend(pending.values())
        rectangles = [[x0 * 16, y0 * 16, min(w, x1 * 16), min(h, y1 * 16)]
            for x0, y0, x1, y1 in rectangles]
        plan = dict(kimg_sha256=envelope_sha, physical=[w, h], tile=16, halo_texels=2,
            alpha_contract="exact DXT5 palette positivity before u8 rounding", rectangles=rectangles,
            covered_pixels=sum((c-a)*(d-b) for a,b,c,d in rectangles))
        args.generate_rectangles.parent.mkdir(parents=True, exist_ok=True)
        args.generate_rectangles.write_text(json.dumps(plan, indent=2) + "\n", encoding="utf-8")
        print(f"Generated {len(rectangles)} disjoint rectangles, {plan['covered_pixels']} pixels")
        return
    mesh_source = (root / "knife/knife-render/src/vita_sparse_coverage.rs").read_text(encoding="utf-8")
    block = mesh_source.split("IMG0436_RECTS:", 1)[1].split("];", 1)[0]
    rects = [tuple(map(int, r)) for r in re.findall(r"\[(\d+),\s*(\d+),\s*(\d+),\s*(\d+)\]", block)]
    coverage = np.zeros((h, w), dtype=np.uint8)
    for x0, y0, x1, y1 in rects:
        assert 0 <= x0 < x1 <= w and 0 <= y0 < y1 <= h
        coverage[y0:y1, x0:x1] += 1
    assert len(rects) <= 96 and coverage.max() == 1, "overlap would apply the blend twice"
    assert not np.any(halo & (coverage == 0)), "mesh omitted nonzero filtered alpha support"
    receipt = dict(schema="vita-sparse-coverage-proof-v1", result="pass",
        asset_manifest_sha256=manifest_sha, archive="jallpac", entry=index, name=name,
        kimg_sha256=envelope_sha, logical=[lw, lh], physical=[w, h], format="dxt5",
        backing=list(backing), rects=len(rects), vertices=len(rects) * 6,
        source_pixels=w * h, exact_palette_nonzero_pixels=int(support.sum()),
        decoded_u8_nonzero_pixels=int((alpha > 0).sum()), halo_texels=2,
        halo_pixels=int(halo.sum()), mesh_pixels=int(coverage.sum()),
        mesh_fraction=float(coverage.mean()), omitted_halo_pixels=0, overlaps=0,
        limitations=["Source alpha proof, not actual hardware fragments or FPS.",
            "Only the renderer's plain Sprite/op7, unmodified complete UV affine contract is admitted."])
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
