"""Validate production KIMG/KATL/GXT metadata without decoding image pixels."""
import struct

CAP = 32 * 1024 * 1024
GRANULE = 256 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def align(value, alignment):
    return (value + alignment - 1) & -alignment


def gxt_info(data, width, height):
    require(len(data) >= 64, "short GXT")
    h = struct.unpack_from("<8I", data)
    r = struct.unpack_from("<6IHHB", data, 32)
    require(h[:3] == (0x00545847, 0x10000003, 1) and h[5:] == (0, 0, 0), "GXT header")
    offset, size = h[3:5]
    require(offset >= 64 and offset % 16 == 0 and offset + size == len(data), "GXT range")
    require(r[:4] == (offset, size, 0xffffffff, 0), "GXT record")
    require(r[4] in (0, 0xa0000000) and r[6:9] == (width, height, 1), "GXT layout")
    require(not any(data[61:64]) and not any(data[64:offset]), "GXT reserved bytes")
    formats = {0x81000000: "pvrtc4", 0x85000000: "dxt1", 0x87000000: "dxt5"}
    fmt = formats.get(r[5])
    require(fmt is not None, "GXT format")
    minimum = 8 if fmt == "pvrtc4" else 4
    bw = max(minimum, 1 << (width - 1).bit_length())
    bh = max(minimum, 1 << (height - 1).bit_length())
    require(r[4] != 0 or (width == bw and height == bh), "GXT PoT layout")
    payload = bw * bh // (1 if fmt == "dxt5" else 2)
    require(size == align(payload, 16) and not any(data[offset + payload:]), "GXT payload")
    return dict(format=fmt, backing=[bw, bh], gpu_payload_bytes=payload,
                cdram_allocation_bytes=align(payload, GRANULE))


def image_info(data):
    require(len(data) >= 32 and data[:4] == b"KIMG", "not KIMG")
    version, lw, lh, w, h, codec, length = struct.unpack_from("<7I", data, 4)
    require(version == 1 and 0 < w <= lw <= 32768 and 0 < h <= lh <= 32768, "KIMG shape")
    require(codec in (1, 2, 3, 4, 7, 8, 9) and 0 < length <= len(data) - 32, "KIMG payload")
    info = dict(logical=[lw, lh], physical=[w, h], codec=codec,
                envelope_bytes=32 + length, encoded_payload_bytes=length, rgba_bytes=w * h * 4)
    if codec in (8, 9):
        require(w <= 2048 and h <= 2048, "GXT dimension bound")
        info.update(gxt_info(data[32:32 + length], w, h))
    else:
        # Ordinary compact PNG/JPEG route returns RGBA8, including codec 7.
        # Language cropping / grades may change the actual runtime upload shape.
        info.update(format="cpu_decode", gpu_payload_bytes=w * h * 4,
                    cdram_allocation_bytes=align(align(w, 8) * h * 4, GRANULE))
        if codec in (2, 7):
            png = data[32:32 + length]
            require(png[:8] == b"\x89PNG\r\n\x1a\n" and png[12:16] == b"IHDR", "PNG header")
            require(struct.unpack_from(">II", png, 16) == (w, h), "PNG dimensions")
            info.update(png_color_type=png[25], png_interlace=png[28])
    return info


def images_in(data):
    if data[:4] == b"KIMG":
        return [("", image_info(data))]
    if data[:4] != b"KATL":
        return []
    require(len(data) >= 12, "short KATL")
    version, count = struct.unpack_from("<II", data, 4)
    require(version == 1 and count <= 4096, "KATL header")
    p, out = 12, []
    for _ in range(count):
        require(p + 8 <= len(data), "KATL member header")
        nl, il = struct.unpack_from("<II", data, p)
        p += 8
        require(0 < nl <= 1024 and p + nl + il <= len(data), "KATL member bounds")
        name = data[p:p + nl].decode("utf-8")
        p += nl
        out.append((name, image_info(data[p:p + il])))
        p += il
    require(p == len(data), "KATL trailing bytes")
    return out
