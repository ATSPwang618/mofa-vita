"""Stage one hash-locked DEV emulator copy with corrected free-memory accounting.

This is a local diagnostic patch for one verified PE, not a general Vita3K updater.
The original executable, game VPK, SDK query and game admission checks stay intact.
"""

import argparse
import ctypes
import hashlib
import json
from pathlib import Path
import random
import shutil
import struct
import sys


SOURCE_SHA256 = '8262786a3ea8007a3addb7b38e1e9863a7fa9299a862c30505ec41d750eeb242'
IMAGE_BASE = 0x140000000
FUNCTION_RVA = (0x8D3100, 0x8D33D7)  # PE .pdata uses an exclusive end.
PATCH_RVA = 0x8D31DE
ORIGINAL = bytes.fromhex(
    '8b4114 3d60d0200c 7f17 740e 3d60804009 7507 '
    '8b01 294378 eb1a 8b01 294374 eb13 '
    '3d60d0800c 7407 3d6080800d 75eb 8b01 29437c'
)
# RCX = SceKernelMemBlockInfo*, RBX = SysmemState*. RDX is volatile and is
# overwritten by LEA at the unchanged continuation (RVA 0x8d3215).
# The original compiler merged every non-CDRAM/non-PHYCONT case into USER.
# Keep that same routing, load mappedSize (+8) once, and preserve block length,
# function extents, call sites, stack state and PE unwind metadata.
REPLACEMENT = bytes.fromhex(
    '8b4114 8b5108 3d60804009 7413 3d60d0800c 7411 '
    '3d6080800d 740a 295374 eb14 295378 eb0f 29537c'
) + b'\x90' * 12
assert len(ORIGINAL) == len(REPLACEMENT) == 52


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def file_offset(data, rva):
    pe = struct.unpack_from('<I', data, 0x3C)[0]
    if data[:2] != b'MZ' or data[pe:pe + 4] != b'PE\0\0':
        raise ValueError('Not a PE executable')
    machine, sections, _, _, _, optional_size, _ = struct.unpack_from('<HHIIIHH', data, pe + 4)
    optional = pe + 24
    if machine != 0x8664 or struct.unpack_from('<H', data, optional)[0] != 0x20B:
        raise ValueError('Only the verified AMD64 PE32+ image is supported')
    if struct.unpack_from('<Q', data, optional + 24)[0] != IMAGE_BASE:
        raise ValueError('Unexpected PE image base')
    for index in range(sections):
        offset = optional + optional_size + 40 * index
        _, virtual, address, raw_size, raw_offset = struct.unpack_from('<8sIIII', data, offset)
        if address <= rva < address + min(virtual, raw_size):
            return raw_offset + rva - address
    raise ValueError(f'RVA {rva:x} has no file-backed section')


def patch_image(data):
    if sha256(data) != SOURCE_SHA256:
        raise ValueError('Source SHA-256 mismatch; no generic or already-patched input is accepted')
    offset = file_offset(data, PATCH_RVA)
    if data[offset:offset + len(ORIGINAL)] != ORIGINAL:
        raise ValueError('Original instruction bytes do not match')
    pe = struct.unpack_from('<I', data, 0x3C)[0]
    # PE32+ exception directory: confirm the observed complete function extent.
    directory_rva, directory_size = struct.unpack_from('<II', data, pe + 24 + 112 + 3 * 8)
    directory = file_offset(data, directory_rva)
    functions = [struct.unpack_from('<III', data, row)[:2]
                 for row in range(directory, directory + directory_size, 12)]
    if FUNCTION_RVA not in functions:
        raise ValueError('Verified function unwind entry is missing')
    result = data[:offset] + REPLACEMENT + data[offset + len(ORIGINAL):]
    if result[:offset] != data[:offset] or result[offset + 52:] != data[offset + 52:]:
        raise AssertionError('Patch escaped the selected 52-byte instruction region')
    return result, offset


def native_self_test():
    """Execute both exact instruction blocks against guarded synthetic structs."""
    if sys.platform != 'win32' or ctypes.sizeof(ctypes.c_void_p) != 8:
        raise RuntimeError('Native instruction validation requires 64-bit Windows')
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.VirtualAlloc.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.c_ulong, ctypes.c_ulong)
    kernel.VirtualAlloc.restype = ctypes.c_void_p
    kernel.VirtualProtect.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.c_ulong,
                                     ctypes.POINTER(ctypes.c_ulong))
    kernel.VirtualFree.argtypes = (ctypes.c_void_p, ctypes.c_size_t, ctypes.c_ulong)
    kernel.GetCurrentProcess.restype = ctypes.c_void_p
    kernel.FlushInstructionCache.argtypes = (ctypes.c_void_p, ctypes.c_void_p, ctypes.c_size_t)
    tested = 0
    rng = random.Random(20261027)
    types = [0x09408060, 0x0C80D060, 0x0D808060, 0x0C20D060, 0x0C208060,
             0x0C20D050, 0, 0xFFFFFFFF] + [rng.getrandbits(32) for _ in range(128)]
    sizes = [24, 4096, 65536, 1048576, 33554432]
    for code, fixed in ((ORIGINAL, False), (REPLACEMENT, True)):
        # Win64 arguments: RCX=block, RDX=state. Preserve nonvolatile RBX.
        wrapper = bytes.fromhex('53 488bda') + code + bytes.fromhex('5b c3')
        address = kernel.VirtualAlloc(None, len(wrapper), 0x3000, 0x04)
        if not address:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            ctypes.memmove(address, wrapper, len(wrapper))
            previous = ctypes.c_ulong()
            if not kernel.VirtualProtect(address, len(wrapper), 0x20, ctypes.byref(previous)):
                raise ctypes.WinError(ctypes.get_last_error())
            if not kernel.FlushInstructionCache(kernel.GetCurrentProcess(), address, len(wrapper)):
                raise ctypes.WinError(ctypes.get_last_error())
            execute = ctypes.WINFUNCTYPE(None, ctypes.c_void_p, ctypes.c_void_p)(address)
            for block_type in types:
                pool_offset = {0x09408060: 0x78, 0x0C80D060: 0x7C,
                               0x0D808060: 0x7C}.get(block_type, 0x74)
                for size in sizes:
                    block_bytes = struct.pack('<6I', 24, 0x81000000, size, 0x4000, 0, block_type)
                    block = ctypes.create_string_buffer(block_bytes)
                    before = bytearray(b'\xA5' * 160)
                    struct.pack_into('<III', before, 0x74, 201326592, 117440512, 27262976)
                    state = ctypes.create_string_buffer(bytes(before))
                    expected = bytearray(before)
                    old = struct.unpack_from('<I', before, pool_offset)[0]
                    struct.pack_into('<I', expected, pool_offset, (old - (size if fixed else 24)) & 0xFFFFFFFF)
                    execute(block, state)
                    if state.raw[:-1] != expected or block.raw[:-1] != block_bytes:
                        raise AssertionError(f'Native route/write mismatch type={block_type:x} size={size} fixed={fixed}')
                    tested += 1
        finally:
            if not kernel.VirtualFree(address, 0, 0x8000):
                raise ctypes.WinError(ctypes.get_last_error())
    return {'executed_native_instruction_cases': tested, 'original_reclaimed_bytes': 24,
            'fixed_reclaimed_bytes': 'mappedSize', 'untouched_bytes_verified': True,
            'scope': 'Synthetic counter instructions only; full emulator SDK round trip is separate'}


def stage(source, output):
    source, output = source.resolve(strict=True), output.resolve()
    root = Path(__file__).resolve().parents[1]
    if not output.is_relative_to(root / '.work/vita') or output.exists():
        raise ValueError('Output must be an absent directory under this project .work/vita')
    data = source.read_bytes()
    patched, offset = patch_image(data)
    test = native_self_test()
    # Both changed input and already-patched input must fail before any writing.
    for invalid in (data[:-1] + bytes([data[-1] ^ 1]), patched):
        try:
            patch_image(invalid)
        except ValueError:
            pass
        else:
            raise AssertionError('Hash-lock negative control failed')
    output.mkdir()
    (output / 'Vita3K.exe').write_bytes(patched)
    # Independent copies avoid sharing mutable settings/logs/cache with the source.
    dependencies = []
    for name in sorted(source.parent.glob('*.dll')):
        shutil.copy2(name, output / name.name)
        dependencies.append({'path': name.name, 'bytes': name.stat().st_size,
                             'sha256': sha256((output / name.name).read_bytes())})
    for name in ('config.yml', 'COPYING.txt'):
        shutil.copy2(source.parent / name, output / name)
    for name in ('plugins', 'shaders-builtin', 'translations', 'icons'):
        shutil.copytree(source.parent / name, output / name)
    # The runner only imports the bridge client and its local errors module.
    bridge = output / 'mcp/server/dist'
    bridge.mkdir(parents=True)
    for name in ('bridge-client.js', 'errors.js'):
        shutil.copy2(source.parent / 'mcp/server/dist' / name, bridge / name)
    (output / 'mcp/server/package.json').write_text('{"type":"module","private":true}\n', encoding='utf-8')
    if source.read_bytes() != data:
        raise RuntimeError('Source changed during staging')
    manifest = {
        'schema': 'knife-vita3k-local-memory-accounting-fix-v1',
        'purpose': 'DEV diagnosis only; not an upstream emulator build or final game requirement',
        'source': {'path': str(source), 'bytes': len(data), 'sha256': sha256(data)},
        'output': {'path': str(output / 'Vita3K.exe'), 'bytes': len(patched), 'sha256': sha256(patched)},
        'source_reference': 'https://github.com/DeQxJ00/Vita3K-mcp/blob/1a9ad6aa1c73468012cd1e3c23741b6810b67614/vita3k/modules/SceSysmem/SceSysmemForDriver.cpp',
        'function_rva': [hex(value) for value in FUNCTION_RVA],
        'patch': {'rva': hex(PATCH_RVA), 'file_offset': offset, 'length': len(ORIGINAL),
                  'before_hex': ORIGINAL.hex(), 'after_hex': REPLACEMENT.hex(),
                  'changed_bytes': sum(a != b for a, b in zip(ORIGINAL, REPLACEMENT)),
                  'only_selected_region_changed': True, 'stack_unwind_metadata_unchanged': True},
        'native_self_test': test, 'hash_lock_negative_controls': 2, 'runtime_dll_copies': dependencies,
        'limits': ['Only regular ksceKernelFreeMemBlock accounting is fixed.',
                   'FreeMemBlockForVM remains unmodified and unverified.',
                   'The full emulator and guest SDK round trip must still be run.',
                   'No original executable or game memory admission check is changed.']}
    (output / 'memory-fix-manifest.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({'manifest': str(output / 'memory-fix-manifest.json'),
                      'sha256': sha256(patched), 'native_self_test': test}, ensure_ascii=False))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--source-executable', type=Path)
    parser.add_argument('--output-directory', type=Path)
    arguments = parser.parse_args()
    if arguments.self_test:
        print(json.dumps(native_self_test()))
    elif arguments.source_executable and arguments.output_directory:
        stage(arguments.source_executable, arguments.output_directory)
    else:
        parser.error('Select --self-test or both --source-executable and --output-directory')
