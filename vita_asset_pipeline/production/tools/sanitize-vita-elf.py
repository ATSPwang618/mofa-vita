#!/usr/bin/env python3
"""Remove inherited SDK source filenames from a linked, unsigned Vita ELF.

The build strips DWARF first. Prebuilt SDKs can also put their builder's source
filenames in C assertion strings. Only such strings may be replaced, in read-only
data; file size, section layout, symbols and executable bytes stay unchanged.
This tool never edits the separately bundled shader compiler module.
"""
from pathlib import Path
import re
import struct
import sys


HOST_PATH = re.compile(rb"(?:/(?:Users|home)/[^/\x00]+|/root)/")
SOURCE_SUFFIXES = (b".c", b".h", b".cc", b".cpp", b".hpp", b".rs")


def sanitize(data):
    if data[:7] != b"\x7fELF\x01\x01\x01" or len(data) < 52:
        raise ValueError("expected an unsigned ELF32 little-endian file")
    if struct.unpack_from("<H", data, 18)[0] != 40:
        raise ValueError("expected the ARM Vita executable")
    offset = struct.unpack_from("<I", data, 32)[0]
    stride, count, names_index = struct.unpack_from("<HHH", data, 46)
    if stride != 40 or names_index >= count or offset + stride * count > len(data):
        raise ValueError("invalid ELF section table")
    sections = [struct.unpack_from("<10I", data, offset + stride * i) for i in range(count)]
    names_section = sections[names_index]
    names = data[names_section[4]:names_section[4] + names_section[5]]
    result = bytearray(data)
    replaced = 0
    for section in sections:
        name = names[section[0]:].split(b"\0", 1)[0]
        kind, flags, begin, length = section[1], section[2], section[4], section[5]
        if kind == 8:  # SHT_NOBITS has no payload in the file.
            continue
        if begin + length > len(data):
            raise ValueError("ELF section outside file")
        payload = data[begin:begin + length]
        if name.startswith((b".debug", b".zdebug")):
            raise ValueError("strip SDK debug sections before sanitizing the package ELF")
        if flags & 4:  # SHF_EXECINSTR: never modify machine code or literal pools.
            continue
        if not name.startswith(b".rodata"):
            continue
        cursor = 0
        for value in payload.split(b"\0"):
            host = HOST_PATH.search(value)
            if host and not value.startswith((b"/home/builder/", b"/home/runner/")):
                if host.start() != 0 or not value.endswith(SOURCE_SUFFIXES):
                    raise ValueError("unexpected host pathname in data; refusing to alter runtime input")
                filename = value.rsplit(b"/", 1)[-1]
                prefix = b"/source/sdk/"
                padding = len(value) - len(prefix) - len(filename) - 1
                if padding < 0:
                    raise ValueError("source filename too short for a same-length virtual path")
                replacement = prefix + b"_" * padding + b"/" + filename
                result[begin + cursor:begin + cursor + len(value)] = replacement
                replaced += 1
            cursor += len(value) + 1
    if len(result) != len(data):
        raise AssertionError("ELF layout changed")
    for section in sections:
        if section[2] & 4:
            begin, length = section[4], section[5]
            if result[begin:begin + length] != data[begin:begin + length]:
                raise AssertionError("executable section changed")
    for match in HOST_PATH.finditer(result):
        if not result.startswith((b"/home/builder/", b"/home/runner/"), match.start()):
            raise ValueError("host pathname remains outside supported source-filename metadata")
    return bytes(result), replaced


if __name__ == "__main__":
    try:
        path = Path(sys.argv[1])
        output, count = sanitize(path.read_bytes())
        path.write_bytes(output)
        print("Package ELF: %d SDK source filenames sanitized; executable bytes unchanged" % count)
    except (OSError, ValueError, IndexError) as exc:
        raise SystemExit("Package ELF sanitization failed: " + str(exc))
