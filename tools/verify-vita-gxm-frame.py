"""Compare retained DEV GXM pixels with a CPU composition reference.

The emulator's screen.capture exports opaque PNGs. Its RGB result establishes
displayed composition only; raw readback separately checks stored RGBA bytes.
Neither establishes hardware correctness or complete game-renderer coverage.
"""
import argparse
import datetime
import hashlib
import json
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
WIDTH, HEIGHT = 960, 544


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def reference(version):
    frame = Image.new("RGBA", (WIDTH, HEIGHT), (0, 0, 0, 255))
    draw = ImageDraw.Draw(frame)
    draw.rectangle((0, 2, 959, 541), fill=(32, 64, 96, 255))
    draw.rectangle((40, 42, 239, 201), fill=(255, 0, 0, 255))
    colors = [(255, 0, 0, 255), (0, 255, 0, 255),
              (0, 0, 255, 255), (255, 255, 255, 255)]
    for index, color in enumerate(colors):
        x, y = 300 + index % 2 * 64, 42 + index // 2 * 64
        draw.rectangle((x, y, x + 63, y + 63), fill=color)
    # Straight-alpha green over opaque red: half coverage, independent of GPU.
    frame.alpha_composite(Image.new("RGBA", (64, 64), (0, 255, 0, 128)), (80, 82))
    for index, alpha in enumerate([0, 128, 255, 64]):
        x, y = 500 + index % 2 * 64, 42 + index // 2 * 64
        frame.alpha_composite(Image.new("RGBA", (64, 64), (*colors[index][:3], alpha)), (x, y))
    inputs = []
    if version == 2:
        fixture_root = ROOT / "knife/knife-platform-vita/src/image_fixtures"
        for name, size, position in [("alpha.png", (128, 128), (300, 242)),
                                     ("colors.jpg", (128, 64), (500, 242))]:
            path = fixture_root / name
            with Image.open(path) as source:
                image = source.convert("RGBA").resize(size, Image.Resampling.NEAREST)
            frame.alpha_composite(image, position)
            inputs.append({"path": str(path), "sha256": digest(path)})
    return frame, inputs


def compare(actual, expected, channels, tolerance):
    if len(actual) != len(expected):
        raise ValueError("pixel byte lengths differ")
    per_channel = [0] * channels
    mismatched_pixels = 0
    maximum = 0
    first = None
    for offset in range(0, len(actual), channels):
        deltas = [abs(actual[offset + c] - expected[offset + c]) for c in range(channels)]
        maximum = max(maximum, *deltas)
        for channel, delta in enumerate(deltas):
            per_channel[channel] += delta > tolerance
        if max(deltas) > tolerance:
            mismatched_pixels += 1
            if first is None:
                index = offset // channels
                first = {"x": index % WIDTH, "y": index // WIDTH,
                         "actual": list(actual[offset:offset + channels]),
                         "expected": list(expected[offset:offset + channels])}
    return {"status": "pass" if not mismatched_pixels else "fail",
            "compared_pixels": WIDTH * HEIGHT,
            "channels": "RGB" if channels == 3 else "RGBA",
            "tolerance_per_channel": tolerance,
            "mismatched_pixels": mismatched_pixels,
            "mismatches_per_channel": per_channel,
            "maximum_channel_delta": maximum, "first_mismatch": first}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--screenshot", type=Path)
    parser.add_argument("--readback", type=Path)
    parser.add_argument("--reference-version", type=int, choices=[1, 2], default=2)
    parser.add_argument("--tolerance", type=int, default=2)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not args.screenshot and not args.readback:
        parser.error("request at least one pixel artifact")
    if not 0 <= args.tolerance <= 255:
        parser.error("tolerance must be in 0..255")
    expected, inputs = reference(args.reference_version)
    receipt = {"tested_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
               "scope": "finite direct GXM DEV composition reference",
               "reference_version": args.reference_version, "reference_inputs": inputs,
               "reference_rgba_sha256": hashlib.sha256(expected.tobytes()).hexdigest(),
               "dimensions": [WIDTH, HEIGHT], "content_rect": [0, 2, 960, 540],
               "verifier_sha256": digest(Path(__file__)), "observations": {},
               "hardware_verified": False,
               "limitations": ["Screenshot RGB comparison does not check stored framebuffer alpha.",
                               "Pixel artifacts do not establish teardown, memory, full shader or game behavior."]}
    for name, path in [("displayed_rgb", args.screenshot), ("cpu_readback_rgba", args.readback)]:
        if path is None:
            continue
        item = {"path": str(path.resolve()), "sha256": digest(path), "bytes": path.stat().st_size}
        try:
            if name == "displayed_rgb":
                with Image.open(path) as source:
                    if source.size != (WIDTH, HEIGHT):
                        raise ValueError(f"unexpected dimensions {source.size}")
                    actual = source.convert("RGB").tobytes()
                item.update(compare(actual, expected.convert("RGB").tobytes(), 3, args.tolerance))
            else:
                actual = path.read_bytes()
                if len(actual) != WIDTH * HEIGHT * 4:
                    raise ValueError(f"unexpected readback length {len(actual)}")
                item.update(compare(actual, expected.tobytes(), 4, args.tolerance))
        except (ValueError, OSError) as error:
            item.update(status="fail", error=str(error))
        receipt["observations"][name] = item
    receipt["status"] = "pass" if all(o["status"] == "pass" for o in receipt["observations"].values()) else "fail"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"status": receipt["status"], "observations": {
        k: {field: value for field, value in v.items() if field not in ("path", "sha256")}
        for k, v in receipt["observations"].items()}, "receipt": str(args.output)}, ensure_ascii=False))
    return 0 if receipt["status"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
