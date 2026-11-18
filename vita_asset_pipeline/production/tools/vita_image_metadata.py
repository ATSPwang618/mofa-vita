"""Validate production KIMG/KATL/GXT metadata without decoding image pixels."""
import re
import struct
import zlib
from contextlib import ExitStack

CAP = 32 * 1024 * 1024
GRANULE = 256 * 1024
VOLUME_BYTES = 1024 * 1024 * 1024
STORY = ('jallpac', 'jallpachd', 'allpacml', 'allpachdml')
STORY_KEY = re.compile(r'KQ([0-9A-F]{5})([0-9A-F]{2})([012])([0-9A-F]{8})([0-9A-F]{8})\.NXZ')


def volume_name(base, index):
    return f'{base}.mrg.{index:03}' if index else f'{base}.mrg'


class ArchiveVolumes:
    """Python verification counterpart of Archive's bounded Vita seeks."""
    def __init__(self, asset, base, count):
        self.stack = ExitStack()
        self.paths = [asset(volume_name(base, i)) for i in range(count)]
        self.size = sum(p.stat().st_size for p in self.paths)
        self.position = 0

    def __enter__(self):
        with ExitStack() as stack:
            self.files = [stack.enter_context(p.open('rb')) for p in self.paths]
            self.stack = stack.pop_all()
        return self

    def __exit__(self, *args):
        self.stack.__exit__(*args)

    def seek(self, pos, whence=0):
        if whence == 2:
            pos += self.size
        elif whence != 0:
            raise ValueError('unsupported archive seek')
        self.position = pos
        return pos

    def read(self, size):
        if len(self.files) == 1:
            file = self.files[0]
            file.seek(self.position)
            data = file.read(size)
            self.position += len(data)
            return data
        end = self.position + size
        blocks = []
        while self.position < end:
            index, offset = divmod(self.position, VOLUME_BYTES)
            self.files[index].seek(offset)
            count = min(VOLUME_BYTES - offset, end - self.position)
            block = self.files[index].read(count)
            if len(block) != count:
                raise ValueError('short read in numbered story volume')
            blocks.append(block)
            self.position += count
        return b''.join(blocks)


def story_extension_rows(archive, rows, names, source, extent, original_count):
    """Validate every added KIMG and that a tiled variant owns its exact grid."""
    require(0 < original_count < len(rows) - 2, 'story variant ordinal boundary')
    require(rows[-1] == (0xffffffff, 0xffff, 0xffff), 'story variant HED terminator')
    require(len(names) == len(rows) * 32, 'story variant NAM length')

    def name(i):
        return names[i * 32:(i + 1) * 32].split(b'\x00')[0].split(b'\r')[0].decode('ascii')

    def entry(i):
        sector, count, _ = rows[i]
        require(count and count * 2048 <= CAP and (sector + count) * 2048 <= extent,
                f'{archive}:{i}: story data outside MRG')
        source.seek(sector * 2048)
        return source.read(count * 2048)

    def checked_gxt(data, logical, descriptor=None):
        info = image_info(data)
        require(info['codec'] == 9 and info['format'] in ('dxt1', 'dxt5', 'pvrtc2_4')
                and info['logical'] == list(logical)
                and info['physical'][0] * info['physical'][1] <= 960 * 1080
                and info['gpu_payload_bytes'] <= 2 * 1024 * 1024
                and struct.unpack_from('<I', data, 80)[0] == 0xa0000000
                and not any(data[info['envelope_bytes']:]), 'story GXT format or padding')
        if descriptor is not None:
            size, fmt, payload = descriptor
            require(info['physical'] == size
                    and fmt == {'dxt1': 0x85000000, 'dxt5': 0x87000000, 'pvrtc2_4': 0x83000000}[info['format']]
                    and payload == info['gpu_payload_bytes'], 'story tile descriptor mismatch')

    def raw_nx(data):
        if data[:4] not in (b'NXCX', b'NXGX'):
            return data
        n, compressed = struct.unpack_from('<II', data, 4)
        require(0 < n <= CAP and compressed + 16 <= len(data), 'story NX bounds')
        decoder = zlib.decompressobj(31 if data[:4] == b'NXGX' else 15)
        raw = decoder.decompress(data[16:16 + compressed], n + 1)
        require(len(raw) == n and decoder.eof and not decoder.unused_data
                and not decoder.unconsumed_tail, 'story NX stream')
        return raw

    i = original_count
    end = len(rows) - 2
    require(name(end) == 'KQ_VARIANTS.NXZ' and rows[end][1:] == (0, 0), 'story variant directory marker')
    seen = set()
    while i < end:
        key = name(i)
        match = STORY_KEY.fullmatch(key)
        require(match is not None and key not in seen, f'{archive}:{i}: invalid/duplicate story variant')
        seen.add(key)
        base, grade, language = (int(match[j], 16) for j in range(1, 4))
        require(base < original_count and grade <= 82 and not name(base).startswith('KQ'), 'story variant base ordinal')
        original = image_info(raw_nx(entry(base)))
        logical = original['logical']
        if language:
            logical[1] //= 2

        data = raw_nx(entry(i))
        if data[:4] == b'KIMG':
            checked_gxt(data, logical)
            i += 1
            continue
        require(data[:4] == b'KQTL' and len(data) >= 52, 'story grid header')
        version, lw, lh, w, h, columns, lines = struct.unpack_from('<7I', data, 4)
        count = columns * lines
        length = 32 + (count + 1) * 20
        valid_length = len(data) == length or (
            len(data) == (length + 2047) // 2048 * 2048 and not any(data[length:]))
        require(version == 1 and [lw, lh] == logical and 0 < w <= lw and 0 < h <= lh
                and columns == (w + 503) // 504 and lines == (h + 503) // 504
                and count <= 256 and valid_length
                and i + count + 1 < end, f'{archive}:{i}: story grid dimensions or entry range')
        for slot in range(count + 1):
            ref, pw, ph, fmt, size = struct.unpack_from('<5I', data, 32 + 20 * slot)
            require(ref == i + 1 + slot and name(ref) == f'KQT{i:07X}{slot:03X}.NXZ', 'story grid payload ordinal')
            if slot == 0:
                require(fmt in (0x85000000, 0x87000000), 'story fallback must remain CPU-decodable DXT')
                require(pw <= w and ph <= h, 'story grid fallback dimensions')
                expected_logical = logical
            else:
                x, y = ((slot - 1) % columns) * 504, ((slot - 1) // columns) * 504
                expected_logical = [min(w, x + 508) - max(0, x - 4),
                                    min(h, y + 508) - max(0, y - 4)]
                require([pw, ph] == expected_logical, 'story grid guard dimensions')
            checked_gxt(raw_nx(entry(ref)), expected_logical, ([pw, ph], fmt, size))
        i += count + 2
    require(i == end and seen, 'incomplete story variant table')


def check_story_rgba_budget(asset, manifest):
    """Host packaging guard; no checks are introduced on the Vita frame path."""
    policy = manifest.get('offline_story_memory_budget')
    if policy is None:
        return None  # Historical packs predate this explicit contract.
    require(policy.get('schema') == 'knife-offline-story-memory-budget-v1'
            and policy.get('rgba_allocation_cap') == 1024 * 1024, 'unknown offline RGBA budget policy')
    checked = protected = maximum = 0
    extensions = manifest.get('story_gxt_variants')
    volumes = manifest.get('story_archive_volumes', {})
    require(set(volumes) <= set(STORY) and all(type(n) is int and 2 <= n <= 128 for n in volumes.values()),
            'invalid story volume directory')
    if extensions is not None:
        require(extensions.get('schema') == 'knife-story-gxt-variants-v1', 'unknown story variant policy')
        extensions = extensions['original_entries']
        require(extensions and set(extensions) <= set(STORY), 'invalid story variant archives')
    else:
        extensions = {}
    for archive in manifest['archives']:
        base = archive['name']
        if base not in STORY:
            continue
        names = asset(base + '.nam').read_bytes()
        cache = {}
        count_volumes = volumes.get(base, 1)
        with ArchiveVolumes(asset, base, count_volumes) as source:
            if count_volumes > 1:
                sizes = [p.stat().st_size for p in source.paths]
                require(sizes[:-1] == [VOLUME_BYTES] * (count_volumes - 1)
                        and 0 < sizes[-1] <= VOLUME_BYTES, 'invalid story volume size')
            extent = source.seek(0, 2)
            rows = list(struct.iter_unpack('<IHH', asset(base + '.hed').read_bytes()))
            if base in extensions:
                story_extension_rows(base, rows, names, source, extent, extensions[base])
                rows = rows[:extensions[base]]
            for index, row in enumerate(rows):
                sector, count, _ = row
                if sector == 0xffffffff:
                    break
                if not count:
                    continue
                name = names[index*32:(index+1)*32].split(b'\0')[0].split(b'\r')[0].decode()
                key = (*row, name.upper().startswith('RULE_'))
                if key not in cache:
                    require(count * 2048 <= CAP and (sector+count)*2048 <= extent, 'offline archive bounds')
                    source.seek(sector*2048)
                    raw = source.read(count*2048)
                    if raw[:4] in (b'NXCX', b'NXGX'):
                        n, compressed = struct.unpack_from('<II', raw, 4)
                        require(0 < n <= CAP and compressed+16 <= len(raw), 'offline NX bounds')
                        decoder = zlib.decompressobj(31 if raw[:4] == b'NXGX' else 15)
                        raw = decoder.decompress(raw[16:16+compressed], n+1)
                        require(len(raw) == n and decoder.eof and not decoder.unused_data
                                and not decoder.unconsumed_tail, 'offline NX stream')
                    result = []
                    for _, info in images_in(raw):
                        w, h = info['physical']
                        if min(w,h) <= 16 or info.get('png_color_type') in (0,4) or key[-1]:
                            result.append(None)
                            continue
                        payload = align(w,8)*h*4
                        charge = align(payload,64 if payload < GRANULE else GRANULE)
                        require(charge <= policy['rgba_allocation_cap'],
                            f'offline RGBA image budget exceeded: {base}:{index}:{name} '
                            f'{w}x{h} allocation={charge} > {policy["rgba_allocation_cap"]}; '
                            'preprocess this pack with tools/quality540/limit_runtime_art.py')
                        result.append(charge)
                    cache[key] = result
                for charge in cache[key]:
                    if charge is None:
                        protected += 1
                    else:
                        checked += 1
                        maximum = max(maximum, charge)
    return dict(checked=checked, protected=protected, maximum_rgba_allocation=maximum)


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
    formats = {0x81000000: "pvrtc4", 0x83000000: "pvrtc2_4", 0x85000000: "dxt1", 0x87000000: "dxt5"}
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
    require(p == len(data) or (len(data) == align(p, 2048) and not any(data[p:])), "KATL trailing bytes")
    return out
