"""Compare displayed old/fused GXM group draws, never CPU target backing.

The caller supplies the exact runtime receipt and its retained PNGs. This is a
finite Vita3K differential, not a complete mathematical or hardware proof.
"""
import argparse
from collections import Counter
import datetime
import hashlib
import io
import json
from pathlib import Path
import sys

from PIL import Image, __version__ as PILLOW_VERSION

PAGES = ("cartesian-037", "cartesian-100", "alpha-exhaustive")
BLENDS = ("Alpha", "Add", "Sub", "Multiply", "Screen", "Opaque", "Over", "Footprint", "MaskAlpha")


def sha(data):
    return hashlib.sha256(data).hexdigest()


def compare_page(image, name, tolerance=0):
    if image.size != (960, 544) or name not in PAGES:
        raise ValueError("Require an exact 960x544 display PNG and known fixture page")
    rgb = image.convert("RGB")
    # These primary color sentinels survive every relevant color transform.
    # Missing/unrendered/all-black frames must not pass pair equality.
    sentinels = [{"xy": [936, 30], "expected": [255, 0, 0]},
                 {"xy": [936, 62], "expected": [0, 255, 0]}]
    for item in sentinels:
        item["observed"] = list(rgb.getpixel(tuple(item["xy"])))
        item["pass"] = item["observed"] == item["expected"]
    exhaustive = name == PAGES[2]
    size, count = (256, 1) if exhaustive else (64, 33)
    cases = []
    for case in range(count):
        x, y = (32, 32) if exhaustive else (16 + 132 * (case % 7), 16 + 68 * (case // 7))
        y += 2  # 960x540 content is centered in the 960x544 scanout.
        old = rgb.crop((x, y, x + size, y + size))
        fused = rgb.crop((x + size, y, x + 2 * size, y + size))
        a, b = old.tobytes(), fused.tobytes()
        histogram, first, mismatch = Counter(), None, 0
        for index in range(size * size):
            offset = index * 3
            delta = max(abs(a[offset + channel] - b[offset + channel]) for channel in range(3))
            histogram[delta] += 1
            if delta > tolerance:
                mismatch += 1
                if first is None:
                    first = {"local_xy": [index % size, index // size],
                             "old_rgb": list(a[offset:offset + 3]), "fused_rgb": list(b[offset:offset + 3])}
        old_colors = len(set(old.getdata()))
        fused_colors = len(set(fused.getdata()))
        # Every fixture has alpha variation, including alpha-only native blends.
        variation = old_colors >= 16 and fused_colors >= 16
        cases.append({"case": case, "blend": "Opaque" if exhaustive else BLENDS[case] if case < 9 else f"Ps({case - 8})",
                      "compared_pixels": size * size, "mismatched_pixels": mismatch,
                      "maximum_channel_delta": max(histogram), "maximum_delta_histogram": dict(sorted(histogram.items())),
                      "first_mismatch": first, "old_distinct_rgb": old_colors, "fused_distinct_rgb": fused_colors,
                      "old_rgb_sha256": sha(a), "fused_rgb_sha256": sha(b),
                      "spatial_variation": variation, "pass": mismatch == 0 and variation})
    return {"page": name, "status": "pass" if all(c["pass"] for c in cases) and all(s["pass"] for s in sentinels) else "fail",
            "compared_pixels": sum(c["compared_pixels"] for c in cases),
            "mismatched_pixels": sum(c["mismatched_pixels"] for c in cases),
            "maximum_channel_delta": max(c["maximum_channel_delta"] for c in cases),
            "sentinels": sentinels, "cases": cases}


def verify(runtime_path, tolerance):
    runtime_bytes = runtime_path.read_bytes()
    runtime = json.loads(runtime_bytes)
    result = {"schema": "knife-vita-group-display-differential-v1",
              "tested_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "runtime_receipt": {"path": str(runtime_path.resolve()), "sha256": sha(runtime_bytes)},
              "vpk_sha256": runtime.get("vpk_sha256"), "verifier_sha256": sha(Path(__file__).read_bytes()),
              "python": sys.version, "pillow": PILLOW_VERSION, "tolerance_per_rgb_channel": tolerance,
              "claim": "paired displayed RGB for 9 native and 24 Ps blends at opacity .37/1 plus all 65536 alpha pairs at opacity 1",
              "limitations": ["Vita3K Vulkan only; no real hardware performance or whole-game equivalence",
                              "Target alpha observed through a subsequent premultiplying draw; raw alpha is not read",
                              "No rule, haze or soft paths in this bounded group API",
                              "Both paths share native production shaders; original WGSL oracle remains separate"],
              "hardware_verified": False, "pages": [], "negative_controls": [], "errors": []}
    if not runtime.get("group_validation", {}).get("valid") or not runtime.get("probe_verified"):
        result["errors"].append("Missing finite guest/capture validation")
    if runtime.get("renderer_observation", {}).get("backends") != ["Vulkan"]:
        result["errors"].append("Missing observed Vulkan identity")
    identities = set()
    for page in PAGES:
        capture = runtime.get("group_captures", {}).get(page, {})
        artifact = capture.get("artifact", {})
        filename = Path(artifact.get("path", ""))
        try:
            data = filename.read_bytes()
            if len(data) != artifact.get("bytes") or sha(data) != artifact.get("sha256"):
                raise ValueError("Retained PNG digest/size mismatch")
            if filename.resolve() in identities or not capture.get("captured") or not capture.get("page_valid_during_capture"):
                raise ValueError("Duplicate or unverified capture")
            identities.add(filename.resolve())
            with Image.open(io.BytesIO(data)) as image:
                if image.format != "PNG":
                    raise ValueError("Require lossless PNG")
                observation = compare_page(image, page, tolerance)
                observation["artifact"] = artifact
                result["pages"].append(observation)
                # Negative control uses the same actual frame and changes one
                # pixel in memory. This is analysis, never an evidence rewrite.
                changed = image.convert("RGB")
                position = (288, 34) if page == PAGES[2] else (80, 18)
                old = changed.getpixel(position)
                changed.putpixel(position, tuple(0 if value > 127 else 255 for value in old))
                negative = compare_page(changed, page, tolerance)
                result["negative_controls"].append({"page": page, "kind": "one fused pixel changed in memory only",
                                                     "detected": negative["status"] == "fail" and negative["mismatched_pixels"] > observation["mismatched_pixels"]})
        except (OSError, ValueError) as error:
            result["errors"].append(f"{page}: {error}")
    result["compared_pixels"] = sum(p["compared_pixels"] for p in result["pages"])
    result["mismatched_pixels"] = sum(p["mismatched_pixels"] for p in result["pages"])
    result["maximum_channel_delta"] = max((p["maximum_channel_delta"] for p in result["pages"]), default=None)
    result["status"] = "pass" if not result["errors"] and len(result["pages"]) == 3 and all(p["status"] == "pass" for p in result["pages"]) and all(n["detected"] for n in result["negative_controls"]) else "fail"
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--tolerance", type=int, default=0, choices=range(0, 5))
    args = parser.parse_args()
    result = verify(args.runtime, args.tolerance)
    args.output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({key: result[key] for key in ("status", "compared_pixels", "mismatched_pixels", "maximum_channel_delta", "errors")}, ensure_ascii=False))
    for page in result["pages"]:
        print(page["page"], page["status"], [(c["blend"], c["mismatched_pixels"], c["maximum_channel_delta"], c["first_mismatch"]) for c in page["cases"] if not c["pass"]])
    return 0 if result["status"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
