"""Check 39 finite GXM effects tile centers against the original WGSL mathematics.

This oracle never reads Cg, native shader output, or emulator readback to derive
its reference. Text/RimText pixels, raw target alpha, edges, and real hardware
remain outside the measured claim. Exit 0 means the finite RGB claim passed;
exit 1 means a mismatch/error; exit 2 means source identity is unknown.
"""
import argparse
import datetime
import hashlib
import io
import json
import math
from pathlib import Path
import struct
import sys
import unittest

from PIL import Image, __version__ as PILLOW_VERSION

ROOT = Path(__file__).resolve().parent.parent
WIDTH, HEIGHT, TARGET_HEIGHT, CONTENT_Y = 960, 544, 540, 2
ORIGINAL_WGSL_SHA256 = "56d2d1048a80d0762714f52b629b6f55eb6479b355a54439ffd0c6d56853836f"
ORIGINAL_BLEND_SHA256 = "26cf075241ce93902721df12965eaf64ed9ecf1c95da56edced7ec9a055257ae"
SOURCE_TEXTURE_BYTES = (128, 64, 200, 128)
QUAD_COLOR = (0.8, 0.7, 0.6, 0.75)
BASE_WORKING = (0.2, 0.4, 0.6, 0.7)
SHADE_NAMES = ("Sprite", "Fill", "Text", "RimText", "Mono", "Nega", "NoAlpha", "ForceAlpha")
BLEND_NAMES = ("Alpha", "Add", "Sub", "Multiply", "Screen", "Opaque", "Over", "Footprint", "MaskAlpha")
PS_NAMES = ("Add", "Darken", "Lighten", "LinearBurn", "Burn", "Dodge", "DodgeAlpha",
            "Difference", "DifferenceAlpha", "Exclusion", "HardLight", "HardLightEx",
            "Overlay", "SoftLight", "Sub", "AddNoAlpha", "SubNoAlpha", "NoAlpha",
            "Normal", "DarkenColorAlpha", "LightenColorAlpha", "ScreenColorAlpha", "MultiplyDst", "Copy")


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def clamp(value):
    return min(1.0, max(0.0, value))


def stage_f32(vector):
    """Varyings, sampler results, and fragment outputs have binary32 storage."""
    return tuple(struct.unpack("<f", struct.pack("<f", value))[0] for value in vector)


def quantize_unorm8(value):
    if not math.isfinite(value):
        raise ValueError("non-finite color cannot establish a reference")
    return math.floor(clamp(value) * 255 + 0.5)


def srgb_encode(value):
    # shader.wgsl:299-301. Clamp is the UNORM attachment boundary, not the shader.
    return value * 12.92 if value <= 0.0031308 else 1.055 * value ** (1 / 2.4) - 0.055


def srgb_decode(value):
    # The inverse IEC sRGB sampler transfer, also original target_level().
    return value / 12.92 if value <= 0.04045 else ((value + 0.055) / 1.055) ** 2.4


def store_target(value):
    return (*[quantize_unorm8(srgb_encode(clamp(v))) for v in value[:3]], quantize_unorm8(value[3]))


def load_target(value):
    return (*[srgb_decode(v / 255) for v in value[:3]], value[3] / 255)


def present(value):
    # fs_present samples the sRGB target, raises decoded RGB to 2.2, explicitly
    # re-encodes, then writes the NON-sRGB display. Alpha is forced to one.
    linear = stage_f32(load_target(value))
    fragment = stage_f32([srgb_encode(v ** 2.2) for v in linear[:3]])
    return (*[quantize_unorm8(v) for v in fragment], 255)


def shade(name, texture, color):
    alpha = texture[3] * color[3]
    rgb = tuple(t * c for t, c in zip(texture[:3], color[:3]))
    if name == "Sprite":
        return (*[v * alpha for v in rgb], alpha)
    if name == "Fill":
        return (*[v * alpha for v in color[:3]], alpha)
    if name == "Mono":
        luminance = sum(t * k for t, k in zip(texture[:3], (0.299, 0.587, 0.114)))
        return (*[luminance * v * alpha for v in color[:3]], alpha)
    if name == "Nega":
        return (*[(1 - t) * c * alpha for t, c in zip(texture[:3], color[:3])], alpha)
    if name == "NoAlpha":
        return (*rgb, 1)
    if name == "ForceAlpha":
        return (*[v * color[3] for v in rgb], color[3])
    raise ValueError(f"shade {name!r} has no agreed pixel fixture")


def blend(name, source, destination):
    s, d, alpha = source[:3], destination[:3], source[3]
    if name == "Opaque":
        return tuple(source)
    if name in ("Footprint", "MaskAlpha"):
        out_alpha = destination[3] + alpha if name == "Footprint" else destination[3] * alpha
        return (*d, out_alpha)
    if name in ("Alpha", "Over"):
        rgb = [s[i] + d[i] * (1 - alpha) for i in range(3)]
    elif name == "Add":
        rgb = [s[i] + d[i] for i in range(3)]
    elif name == "Sub":
        rgb = [d[i] - s[i] for i in range(3)]
    elif name == "Multiply":
        rgb = [s[i] * d[i] + d[i] * (1 - alpha) for i in range(3)]
    elif name == "Screen":
        rgb = [s[i] + d[i] * (1 - s[i]) for i in range(3)]
    else:
        raise ValueError(f"unsupported blend {name!r}")
    return (*rgb, destination[3] + (alpha if name == "Over" else 0))


def ps(operation, texture, color, destination):
    # Original fs_ps: no fill, rule mask, or noalpha prepass in this probe.
    if not 1 <= operation <= 24:
        raise ValueError(f"PS operation {operation} is outside the probe contract")
    ca, alpha = color[3], texture[3] * color[3]
    if alpha <= 0 and (operation <= 15 or operation in (19, 20, 23, 24)):
        return tuple(destination)  # preserve BOTH RGB and alpha; do not force 1.
    if operation == 21 and ca <= 0:
        return tuple(destination)
    output = []
    for tex, tint, d in zip(texture[:3], color[:3], destination[:3]):
        s = tex * tint
        mix = lambda r, weight=alpha, base=d: base + weight * (r - base)
        if operation == 1:
            o = mix(min(d + s, 1))
        elif operation == 2:
            o = mix(min(d, s))
        elif operation == 3:
            o = mix(max(d, s))
        elif operation == 4:
            o = mix(max(d + s - 1, 0))
        elif operation == 5:
            o = mix(max(1 - (1 - d) / s, 0) if s > 0 else 0)
        elif operation == 6:
            k = max(1 - s, 0)
            o = mix(min(d / k, 1) if k > 0 else 1)
        elif operation == 7:
            k = max(1 - alpha * s, 0)
            o = (min(d / k, 1) if k > 0 else 1) if d > 0 else 0
        elif operation == 8:
            o = mix(abs(d - s))
        elif operation == 9:
            o = abs(d - alpha * s)
        elif operation == 10:
            o = mix(d + s - 2 * d * s)
        elif operation in (11, 12):
            h = min(s / texture[3], 1) if operation == 12 else s
            o = mix(2 * d * h if h < 0.5 else 1 - 2 * (1 - h) * (1 - d))
        elif operation == 13:
            o = mix(2 * d * s if d < 0.5 else 1 - 2 * (1 - s) * (1 - d))
        elif operation == 14:
            o = mix(d ** (2 - 2 * s) if s < 0.5 else d ** (1 / (2 * s)))
        elif operation == 15:
            o = max(d - alpha * (1 - s), 0)
        elif operation == 16:
            o = min(d + s * ca, 1)
        elif operation == 17:
            o = max(d - ca * (1 - s), 0)
        elif operation == 18:
            o = s
        elif operation == 19:
            o = mix(s, base=d * destination[3])
        elif operation == 20:
            o = mix(min(d, s), ca)
        elif operation == 21:
            o = mix(max(d, s), ca)
        elif operation == 22:
            o = mix(1 - (1 - d) * (1 - s * ca), ca)
        elif operation == 23:
            o = min(d * s + (1 - alpha) * d, 1)
        else:  # PS24 Copy only reaches here when a > 0.
            o = s
        output.append(o)
    return (*output, 1)


def build_reference():
    texture = stage_f32(tuple(v / 255 for v in SOURCE_TEXTURE_BYTES))
    color = stage_f32(QUAD_COLOR)
    base_color = stage_f32((*[v / BASE_WORKING[3] for v in BASE_WORKING[:3]], BASE_WORKING[3]))
    base_fragment = stage_f32(shade("Fill", (1, 1, 1, 1), base_color))
    base_stored = store_target(base_fragment)
    destination = stage_f32(load_target(base_stored))
    items, unknowns = [], []

    def add_tile(family, name, rectangle, output, source=None, operation=None):
        x, y, w, h = rectangle
        fragment = stage_f32(output)
        stored = store_target(fragment)
        item = {"family": family, "name": name, "target_rect": rectangle,
                "sample_rect_physical": [x + (w - 32) // 2, y + CONTENT_Y + (h - 24) // 2, 32, 24],
                "working_rgba_after_blend": fragment, "stored_target_rgba8": stored,
                "decoded_target_rgba": load_target(stored), "display_rgba8": present(stored)}
        if source is not None:
            item["source_fragment_rgba"] = source
        if operation is not None:
            item["operation"] = operation
        items.append(item)

    for index, name in enumerate(SHADE_NAMES):
        rectangle = [24 + 112 * index, 20, 88, 48]
        if name in ("Text", "RimText"):
            unknowns.append({"name": name, "target_rect": rectangle, "status": "not_pixel_verified",
                             "reason": "R8 texture input/UV contract is pending; program/link checks are external."})
            continue
        fragment = stage_f32(shade(name, texture, color))
        add_tile("shade", name, rectangle, blend("Alpha", fragment, destination), fragment)
    fragment = stage_f32(shade("Sprite", texture, color))
    for index, name in enumerate(BLEND_NAMES):
        add_tile("blend", name, [24 + 100 * index, 96, 80, 48], blend(name, fragment, destination), fragment)
    for index, name in enumerate(PS_NAMES):
        operation = index + 1
        add_tile("ps", name, [24 + 76 * (index % 12), 172 + 72 * (index // 12), 60, 48],
                 ps(operation, texture, color, destination), operation=operation)
    return {
        "schema": "knife-vita-effects-original-wgsl-reference-v1", "reference_authority": "original WGSL and desktop fixed-function blend state",
        "source_wgsl": {"path": "knife/knife-render/src/shader.wgsl", "sha256": ORIGINAL_WGSL_SHA256,
                        "shade_lines": [81, 195], "present_lines": [299, 307], "ps_lines": [315, 396]},
        "blend_source": {"path": "knife/knife-render/src/desktop.rs", "symbol": "blend_state", "line": 1207},
        "dimensions": [WIDTH, HEIGHT], "content_rect": [0, CONTENT_Y, WIDTH, TARGET_HEIGHT],
        "contract": {"target_format": "RGBA8_UNORM_SRGB", "display_format": "RGBA8_UNORM (non-sRGB)",
                     "source_texture_format": "RGBA8_UNORM (non-sRGB)", "source_texture_rgba8": SOURCE_TEXTURE_BYTES,
                     "quad_color": QUAD_COLOR, "base_intended_working_rgba": BASE_WORKING,
                     "base_fill_color": base_color, "base_fill_fragment_rgba": base_fragment,
                     "base_stored_target_rgba8": base_stored, "base_sampled_working_rgba": destination,
                     "shade_row_blend": "Alpha", "blend_row_shade": "Sprite", "ps_fill": False,
                     "ps_rule_progress": -1, "ps_noalpha_prepass": False, "haze": [0, 0, 0, 0], "soft_radius": 0},
        "color_path": ["non-sRGB UNORM source sample", "original WGSL shade/premultiply or PS math",
                       "blend with IEC-sRGB-decode of the quantized destination target",
                       "IEC-sRGB encode RGB and round/clamp RGBA to UNORM8", "IEC-sRGB decode stored RGB",
                       "fs_present: pow(RGB, 2.2), IEC-sRGB encode, non-sRGB UNORM8 display"],
        "arithmetic": {"math": "binary64 scalar evaluation of original equations, binary32 inputs/stage outputs",
                       "target_conversion": "IEC sRGB, clamp and round-to-nearest UNORM8 (half upward)",
                       "hardware_approximation": "not substituted; non-IEC gamma deviations remain measurable failures"},
        "sample_pixels": len(items) * 32 * 24, "tiles": items, "pixel_unknowns": unknowns,
        "limitations": ["Text/RimText program/link checks do not verify their pixels.",
                        "Screenshot RGB cannot establish raw stored target alpha; alpha is modeled because PS19 uses it.",
                        "Only uniform center rectangles are compared; edges, interpolation and surrounding display pixels are unmeasured.",
                        "Float transcendental instructions and UNORM tie rounding may differ by a byte; the caller's fixed tolerance is never enlarged.",
                        "This finite constant-input probe does not establish full game shader or physical Vita correctness."]}


def fixture_frame(reference):
    """Synthetic matcher fixture, never evidence of GPU correctness."""
    frame = Image.new("RGB", (WIDTH, HEIGHT))
    for item in reference["tiles"]:
        x, y, w, h = item["sample_rect_physical"]
        frame.paste(tuple(item["display_rgba8"][:3]), (x, y, x + w, y + h))
    return frame


def compare_frame(frame, reference, tolerance):
    if frame.size != (WIDTH, HEIGHT):
        raise ValueError(f"expected {WIDTH}x{HEIGHT} screenshot, got {frame.size}")
    if not 0 <= tolerance <= 255:
        raise ValueError("tolerance must be in 0..255")
    pixels = frame.convert("RGB").load()
    total, mismatched, maximum, first, items = 0, 0, 0, None, []
    for tile in reference["tiles"]:
        expected = tile["display_rgba8"][:3]
        x, y, width, height = tile["sample_rect_physical"]
        count, tile_max, sums = 0, 0, [0, 0, 0]
        minimum, greatest = [255, 255, 255], [0, 0, 0]
        for row in range(y, y + height):
            for column in range(x, x + width):
                actual = pixels[column, row]
                differences = [abs(a - e) for a, e in zip(actual, expected)]
                delta = max(differences)
                total += 1
                tile_max = max(tile_max, delta)
                for channel in range(3):
                    sums[channel] += actual[channel]
                    minimum[channel] = min(minimum[channel], actual[channel])
                    greatest[channel] = max(greatest[channel], actual[channel])
                if delta > tolerance:
                    count += 1
                    if first is None:
                        first = {"family": tile["family"], "name": tile["name"], "x": column, "y": row,
                                 "actual_rgb": actual, "expected_rgb": expected, "deltas": differences}
        maximum = max(maximum, tile_max)
        mismatched += count
        items.append({"family": tile["family"], "name": tile["name"], "operation": tile.get("operation"),
                      "status": "fail" if count else "pass", "sample_rect_physical": tile["sample_rect_physical"],
                      "compared_pixels": width * height, "mismatched_pixels": count,
                      "expected_rgb": expected, "actual_rgb_min": minimum, "actual_rgb_max": greatest,
                      "actual_rgb_mean": [v / (width * height) for v in sums], "maximum_channel_delta": tile_max})
    if total != 39 * 32 * 24:
        raise ValueError(f"reference must enumerate all 39 pixel tiles, got {total} pixels")
    return {"status": "fail" if mismatched else "pass", "compared_pixels": total,
            "mismatched_pixels": mismatched, "maximum_channel_delta": maximum, "first_mismatch": first,
            "channels": "RGB", "tolerance_per_channel": tolerance, "tiles": items}


class OracleTests(unittest.TestCase):
    def assertVector(self, actual, expected):
        self.assertEqual(len(actual), len(expected))
        for value, reference in zip(actual, expected):
            self.assertAlmostEqual(value, reference, places=12)

    def test_iec_srgb_not_gamma_22_approximation(self):
        self.assertAlmostEqual(srgb_encode(0.0031308), 0.040449936, places=12)
        self.assertAlmostEqual(srgb_decode(0.04045), 0.0031308049535603713, places=12)
        self.assertAlmostEqual(srgb_encode(0.5), 0.7353569830524495, places=12)
        self.assertAlmostEqual(srgb_decode(0.5), 0.21404114048223255, places=12)

    def test_quantized_target_and_final_output_chain(self):
        self.assertEqual(store_target((0.2, 0.4, 0.6, 0.7)), (124, 170, 203, 179))
        stored = store_target((0.2, 0.4, 0.6, 0.7))
        self.assertNotEqual(load_target(stored), (0.2, 0.4, 0.6, 0.7))
        self.assertEqual(present((0, 0, 0, 0)), (0, 0, 0, 255))
        self.assertEqual(present((255, 255, 255, 0)), (255, 255, 255, 255))
        self.assertEqual(present((124, 170, 203, 179)), (48, 103, 154, 255))
        self.assertEqual(present((12, 21, 63, 255)), (0, 0, 4, 255))
        for level in range(256):
            encoded = level / 255
            decoded = srgb_decode(encoded)
            self.assertAlmostEqual(srgb_encode(decoded), encoded, places=12)

    def test_shade_hand_calculated_vectors(self):
        texture, color = (0.5, 0.25, 0.75, 0.5), (0.8, 0.7, 0.6, 0.75)
        values = {
            "Sprite": (0.15, 0.065625, 0.16875, 0.375),
            "Fill": (0.3, 0.2625, 0.225, 0.375),
            "Mono": (0.114525, 0.100209375, 0.08589375, 0.375),
            "Nega": (0.15, 0.196875, 0.05625, 0.375),
            "NoAlpha": (0.4, 0.175, 0.45, 1),
            "ForceAlpha": (0.3, 0.13125, 0.3375, 0.75),
        }
        for name, expected in values.items():
            with self.subTest(shade=name):
                self.assertVector(shade(name, texture, color), expected)

    def test_hardware_blend_hand_calculated_vectors(self):
        source, destination = (0.1, 0.2, 0.3, 0.25), (0.2, 0.4, 0.6, 0.7)
        values = {
            "Alpha": (0.25, 0.5, 0.75, 0.7),
            "Add": (0.3, 0.6, 0.9, 0.7),
            "Sub": (0.1, 0.2, 0.3, 0.7),
            "Multiply": (0.17, 0.38, 0.63, 0.7),
            "Screen": (0.28, 0.52, 0.72, 0.7),
            "Opaque": (0.1, 0.2, 0.3, 0.25),
            "Over": (0.25, 0.5, 0.75, 0.95),
            "Footprint": (0.2, 0.4, 0.6, 0.95),
            "MaskAlpha": (0.2, 0.4, 0.6, 0.175),
        }
        for name, expected in values.items():
            with self.subTest(blend=name):
                self.assertVector(blend(name, source, destination), expected)

    def test_every_ps_operation_against_hand_calculated_vectors(self):
        texture, color, destination = (0.5, 0.25, 0.75, 0.5), (1, 1, 1, 1), (0.25, 0.5, 0.75, 0.5)
        # Analytical examples use different values from the DEV probe contract.
        expected = [
            (0.5, 0.625, 0.875), (0.25, 0.375, 0.75), (0.375, 0.5, 0.75),
            (0.125, 0.25, 0.625), (0.125, 0.25, 17 / 24), (0.375, 7 / 12, 0.875),
            (1 / 3, 4 / 7, 1), (0.25, 0.375, 0.375), (0, 0.375, 0.375),
            (0.375, 0.5, 0.5625), (0.25, 0.375, 0.8125), (0.625, 0.5, 0.875),
            (0.25, 0.375, 0.8125), (0.25, (0.5 + 0.5 ** 1.5) / 2, (0.75 + 0.75 ** (2 / 3)) / 2),
            (0, 0.125, 0.625), (0.75, 0.75, 1), (0, 0, 0.5), (0.5, 0.25, 0.75),
            (0.3125, 0.25, 0.5625), (0.25, 0.25, 0.75), (0.5, 0.5, 0.75),
            (0.625, 0.625, 0.9375), (0.25, 0.375, 0.9375), (0.5, 0.25, 0.75),
        ]
        for operation, rgb in enumerate(expected, 1):
            with self.subTest(operation=operation):
                self.assertVector(ps(operation, texture, color, destination), (*rgb, 1))

    def test_zero_alpha_keeps_destination_rgba_only_in_defined_branches(self):
        destination, color = (0.2, 0.4, 0.6, 0.7), (0.8, 0.7, 0.6, 0.75)
        texture = (0.5, 0.25, 0.75, 0)
        for operation in (*range(1, 16), 19, 20, 23, 24):
            with self.subTest(operation=operation):
                self.assertEqual(ps(operation, texture, color, destination), destination)
        self.assertVector(ps(18, texture, color, destination), (0.4, 0.175, 0.45, 1))
        transparent_color = (*color[:3], 0)
        self.assertEqual(ps(21, texture, transparent_color, destination), destination)
        # ScreenColorAlpha with ca=0 rewrites destination RGB with alpha=1.
        self.assertVector(ps(22, texture, transparent_color, destination), (*destination[:3], 1))

    def test_uncovered_text_shades_are_explicit(self):
        reference = build_reference()
        self.assertEqual(len(reference["tiles"]), 39)
        self.assertEqual({item["name"] for item in reference["pixel_unknowns"]}, {"Text", "RimText"})
        self.assertEqual(reference["contract"]["base_stored_target_rgba8"], (124, 170, 203, 178))

    def test_dimensions_and_tolerance_are_strict(self):
        reference = build_reference()
        with self.assertRaises(ValueError):
            compare_frame(Image.new("RGB", (960, 540)), reference, 4)
        positive = fixture_frame(reference)
        x, y = reference["tiles"][0]["sample_rect_physical"][:2]
        expected = positive.getpixel((x, y))
        direction = 1 if expected[0] <= 250 else -1
        positive.putpixel((x, y), (expected[0] + direction * 4, *expected[1:]))
        self.assertEqual(compare_frame(positive, reference, 4)["status"], "pass")
        positive.putpixel((x, y), (expected[0] + direction * 5, *expected[1:]))
        self.assertEqual(compare_frame(positive, reference, 4)["status"], "fail")

    def test_zero_divisor_and_zero_color_alpha_branches(self):
        destination = (0, 0.5, 1, 0.5)
        for operation in (5, 6, 7, 14):
            for texture in ((0, 0, 0, 1), (1, 1, 1, 1)):
                with self.subTest(operation=operation, texture=texture):
                    self.assertTrue(all(math.isfinite(v) for v in ps(operation, texture, (1, 1, 1, 1), destination)))
        for operation in (*range(1, 16), 19, 20, 21, 23, 24):
            self.assertEqual(ps(operation, (1, 0.5, 0, 0.5), (1, 1, 1, 0), destination), destination)

    def test_center_comparison_and_one_pixel_negative_control(self):
        reference = build_reference()
        positive = fixture_frame(reference)
        self.assertEqual(compare_frame(positive, reference, 4)["status"], "pass")
        changed = positive.copy()
        rectangle = reference["tiles"][0]["sample_rect_physical"]
        x, y = rectangle[:2]
        rgb = list(changed.getpixel((x, y)))
        rgb[0] = 0 if rgb[0] > 127 else 255
        changed.putpixel((x, y), tuple(rgb))
        result = compare_frame(changed, reference, 4)
        self.assertEqual(result["status"], "fail")
        self.assertEqual(result["mismatched_pixels"], 1)
        self.assertEqual(result["compared_pixels"], 39 * 32 * 24)


def source_identity():
    wgsl = ROOT / "knife/knife-render/src/shader.wgsl"
    identity = {"wgsl": {"path": str(wgsl), "expected_sha256": ORIGINAL_WGSL_SHA256},
                "blend_state": {"expected_normalized_function_sha256": ORIGINAL_BLEND_SHA256}}
    try:
        identity["wgsl"]["actual_sha256"] = digest(wgsl)
        # The desktop facade moved this original code from lib.rs to desktop.rs.
        # Authenticate only the fixed-function state, so unrelated refactoring
        # cannot change the reference authority or spuriously invalidate it.
        for relative in ("knife/knife-render/src/desktop.rs", "knife/knife-render/src/lib.rs"):
            path = ROOT / relative
            text = path.read_text(encoding="utf-8")
            if "fn blend_state(" not in text:
                continue
            start = text.index("fn blend_state(")
            end = text.index("{", start) + 1
            level = 1
            while level:
                level += int(text[end] == "{") - int(text[end] == "}")
                end += 1
            identity["blend_state"].update(path=str(path),
                                           actual_normalized_function_sha256=hashlib.sha256(text[start:end].encode("utf-8")).hexdigest())
            break
        identity["status"] = "verified" if (
            identity["wgsl"]["actual_sha256"] == ORIGINAL_WGSL_SHA256 and
            identity["blend_state"].get("actual_normalized_function_sha256") == ORIGINAL_BLEND_SHA256) else "unknown"
    except (OSError, ValueError, IndexError) as error:
        identity.update(status="unknown", error=str(error))
    return identity


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--screenshot", type=Path)
    parser.add_argument("--tolerance", type=int, default=4)
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--write-reference", type=Path, help="write the derived reference/stages JSON without GPU execution")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not (args.screenshot or args.self_test or args.write_reference):
        parser.error("request --screenshot, --self-test or --write-reference")
    if not 0 <= args.tolerance <= 255:
        parser.error("tolerance must be in 0..255")
    reference = build_reference()
    reference["blend_source"]["normalized_function_sha256"] = ORIGINAL_BLEND_SHA256
    reference_bytes = json.dumps(reference, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
    identity = source_identity()
    receipt = {"schema": "knife-vita-effects-oracle-receipt-v1", "tested_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
               "command": [sys.executable, *sys.argv], "python": sys.version, "pillow": PILLOW_VERSION,
               "verifier_sha256": digest(Path(__file__)), "source_identity": identity,
               "reference_canonical_sha256": hashlib.sha256(reference_bytes).hexdigest(), "tolerance_per_channel": args.tolerance,
               "claim": ("all 39 defined uniform RGB tile centers match the original equations within the requested fixed tolerance"
                         if args.screenshot else "CPU oracle self-tests and/or reference generation only; no GPU pixel observation"),
               "gpu_pixel_validation_status": "not_run",
               "overall_effects_coverage": "partial", "hardware_verified": False,
               "pixel_unknowns": reference["pixel_unknowns"], "limitations": reference["limitations"], "observations": {}}
    exit_code = 0
    if identity["status"] != "verified":
        receipt["status"] = "unknown"
        exit_code = 2
    else:
        if args.write_reference:
            write_json(args.write_reference, reference)
            receipt["reference_artifact"] = {"path": str(args.write_reference.resolve()), "sha256": digest(args.write_reference)}
        if args.self_test:
            stream = io.StringIO()
            result = unittest.TextTestRunner(stream=stream, verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(OracleTests))
            positive = fixture_frame(reference)
            changed = positive.copy()
            x, y = reference["tiles"][0]["sample_rect_physical"][:2]
            rgb = list(changed.getpixel((x, y)))
            rgb[0] = 0 if rgb[0] > 127 else 255
            changed.putpixel((x, y), tuple(rgb))
            negative = compare_frame(changed, reference, 4)
            receipt["self_test"] = {"status": "pass" if result.wasSuccessful() else "fail", "tests_run": result.testsRun,
                                    "failures": len(result.failures), "errors": len(result.errors), "test_output": stream.getvalue(),
                                    "analytical_ps_cases": 24, "analytical_blend_cases": 9, "analytical_shade_cases": 6,
                                    "positive_fixture": {"kind": "synthetic CPU-only", "rgb_sha256": hashlib.sha256(positive.tobytes()).hexdigest(),
                                                         "result": compare_frame(positive, reference, 4)},
                                    "negative_control": {"kind": "same synthetic fixture with one changed center pixel",
                                                         "rgb_sha256": hashlib.sha256(changed.tobytes()).hexdigest(),
                                                         "expected_verifier_exit_code": 1, "result": negative}}
            exit_code = 0 if result.wasSuccessful() else 1
            print(stream.getvalue())
        if args.screenshot:
            observation = {"path": str(args.screenshot.resolve())}
            try:
                observation.update(sha256=digest(args.screenshot), bytes=args.screenshot.stat().st_size)
                with Image.open(args.screenshot) as image:
                    if image.format != "PNG":
                        raise ValueError("pixel evidence must be lossless PNG")
                    observation.update(compare_frame(image, reference, args.tolerance))
            except (OSError, ValueError, Image.DecompressionBombError) as error:
                observation.update(status="fail", error=str(error))
            receipt["observations"]["displayed_rgb"] = observation
            receipt["gpu_pixel_validation_status"] = observation["status"]
            if observation["status"] != "pass":
                exit_code = 1
        receipt["status"] = "pass" if exit_code == 0 else "fail"
    receipt["exit_code"] = exit_code
    write_json(args.output, receipt)
    print(json.dumps({"status": receipt["status"], "exit_code": exit_code, "overall_effects_coverage": "partial",
                      "screenshot": {k: v for k, v in receipt["observations"].get("displayed_rgb", {}).items()
                                     if k in ("status", "compared_pixels", "mismatched_pixels", "maximum_channel_delta", "first_mismatch", "error")},
                      "output": str(args.output.resolve())}, ensure_ascii=False))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
