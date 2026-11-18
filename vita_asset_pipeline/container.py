"""Stream local Nintendo container entries without altering their NCA bytes."""
from dataclasses import dataclass
import re
import struct
import time

CHUNK_BYTES = 16 * 1024 * 1024
MAX_ENTRIES = 10_000
HEADER = struct.Struct("<4sIII")
ENTRY = struct.Struct("<QQII")
SAFE_NAME = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,95}")


@dataclass(frozen=True)
class Entry:
    name: str
    offset: int
    size: int


def read_exact(stream, count):
    data = stream.read(count)
    if len(data) != count:
        raise ValueError("truncated container")
    return data


def table(stream, start, size, magic, stride):
    if start < 0 or size < HEADER.size:
        raise ValueError("invalid container extent")
    stream.seek(start)
    found, count, name_bytes, _ = HEADER.unpack(read_exact(stream, HEADER.size))
    header_bytes = HEADER.size + count * stride + name_bytes
    if (found != magic or count > MAX_ENTRIES or name_bytes > 16 * 1024 * 1024
            or header_bytes > size):
        raise ValueError(f"invalid {magic.decode()} table")
    records = read_exact(stream, count * stride)
    names = read_exact(stream, name_bytes)
    result = []
    seen = set()
    for i in range(count):
        offset, length, at, _ = ENTRY.unpack_from(records, i * stride)
        end = names.find(b"\0", at)
        name = names[at:end].decode("ascii") if end >= at else ""
        if not SAFE_NAME.fullmatch(name) or name.casefold() in seen:
            raise ValueError(f"invalid or duplicate container entry {name!r}")
        if offset + length > size - header_bytes:
            raise ValueError(f"entry outside container: {name}")
        seen.add(name.casefold())
        result.append(Entry(name, start + header_bytes + offset, length))
    ordered = sorted((entry.offset, entry.offset + entry.size) for entry in result if entry.size)
    if any(end > next_start for (_, end), (next_start, _) in zip(ordered, ordered[1:])):
        raise ValueError("overlapping container entries")
    return result


def entries(path):
    with path.open("rb") as stream:
        return table(stream, 0, path.stat().st_size, b"PFS0", ENTRY.size)


def secure_entries(path):
    with path.open("rb") as stream:
        stream.seek(0x100)
        header = read_exact(stream, 0x38)
        if header[:4] != b"HEAD":
            raise ValueError("not an XCI gamecard")
        start, = struct.unpack_from("<Q", header, 0x30)
        partitions = table(stream, start, path.stat().st_size - start, b"HFS0", 64)
        secure = next((part for part in partitions if part.name == "secure"), None)
        if secure is None:
            raise ValueError("XCI has no secure partition")
        return table(stream, secure.offset, secure.size, b"HFS0", 64)


def copy_entry(source, destination, entry):
    source.seek(entry.offset)
    left = entry.size
    start = time.monotonic()
    last = start
    while left:
        data = read_exact(source, min(CHUNK_BYTES, left))
        destination.write(data)
        left -= len(data)
        now = time.monotonic()
        if now - last >= 10 and left:
            print(f"{entry.name}: {(entry.size - left) / entry.size:.0%}", flush=True)
            last = now


def remux_xci(source, destination):
    items = secure_entries(source)
    if not items:
        raise ValueError("empty XCI secure partition")
    names = bytearray()
    records = bytearray()
    offset = 0
    for item in items:
        records.extend(ENTRY.pack(offset, item.size, len(names), 0))
        names.extend(item.name.encode("ascii") + b"\0")
        offset += item.size
    names.extend(bytes((-(HEADER.size + len(records) + len(names))) % 16))
    header = HEADER.pack(b"PFS0", len(items), len(names), 0) + records + names
    write_entries(source, destination, items, header)


def write_entries(source, destination, items, header=b""):
    if destination.exists():
        raise FileExistsError(destination)
    pending = destination.with_name(destination.name + ".partial")
    with pending.open("xb") as output:
        try:
            output.write(header)
            with source.open("rb") as stream:
                for item in items:
                    copy_entry(stream, output, item)
        except BaseException:
            output.close()
            pending.unlink()
            raise
    pending.rename(destination)
