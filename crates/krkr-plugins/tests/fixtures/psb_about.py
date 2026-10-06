"""Generate PSB/MDF/XP3 fixtures without borrowing game assets."""
from pathlib import Path
import struct
import zlib

root = Path('target/psb-about')
root.mkdir(parents=True, exist_ok=True)

def array(values):
    return b'\x10' + struct.pack('<I', len(values)) + b'\x10' + b''.join(struct.pack('<I', n) for n in values)

names = ['title', 'number', 'real', 'values', 'Payload', 'inner', 'data']
parents = {0: 0}
bases = {0: 1}
indices = []
next_base = 256
for name in names:
    parent = 0
    for byte in name.encode():
        node = bases[parent] + byte
        parents[node] = parent
        if node not in bases:
            bases[node] = next_base
            next_base += 256
        parent = node
    terminal = bases[parent]
    parents[terminal] = parent
    indices.append(terminal)
name_table = array([bases.get(n, 0) for n in range(max(bases) + 1)])
name_table += array([parents.get(n, 0) for n in range(max(parents) + 1)]) + array(indices)

def listing(nodes):
    offsets, body = [], b''
    for node in nodes:
        offsets.append(len(body))
        body += node
    return b'\x20' + array(offsets) + body

def mapping(items):
    offsets, body = [], b''
    for _, node in items:
        offsets.append(len(body))
        body += node
    return b'\x21' + array([names.index(key) for key, _ in items]) + array(offsets) + body

payload = b'global.psbExecuted = 73;'
entries = mapping([
    ('title', b'\x15\x00'),
    ('number', b'\x05\xfe'),
    ('real', b'\x1f' + struct.pack('<d', 1.25)),
    ('values', listing([b'\x03', b'\x01', b'\x05\x07', b'\x19\x00'])),
    ('Payload', b'\x19\x00'),
    ('inner', mapping([('data', b'\x19\x00')])),
])

def psb(version):
    output = bytearray(44 if version == 3 else 40)
    offsets = []
    for block in [name_table, array([0]), '参照PSB'.encode() + b'\x00', array([0]), array([len(payload)]), payload, entries]:
        offsets.append(len(output))
        output.extend(block)
    output[:40] = struct.pack('<4sHH8I', b'PSB\x00', version, 0, offsets[0], *offsets)
    return bytes(output)

data = psb(3)
(root / 'data.psb').write_bytes(data)
(root / 'second.psb').write_bytes(psb(2))
(root / 'data.mdf').write_bytes(b'mdf\x00' + struct.pack('<I', len(data)) + zlib.compress(data))
(root / 'bad.psb').write_bytes(b'PSB\x00' + b'\xff' * 40)
tiny = struct.pack('<4sHH8I', b'PSB\x00', 2, 0, 40, 40, 58, 64, 64, 70, 76, 76)
tiny += array([]) * 6 + b'\x05\x2a'
# Tables: names = three empty arrays, strings/chunk offsets/chunk lengths = empty.
(root / 'octet.tjs').write_text('global.psbOctet=<% ' + tiny.hex(' ') + ' %>;', encoding='utf-8')

def chunk(tag, content):
    return tag + struct.pack('<Q', len(content)) + content

filename = 'packed.psb'.encode('utf-16-le')
info = struct.pack('<IQQH', 0, len(data), len(data), len(filename) // 2) + filename
segment = struct.pack('<IQQQ', 0, 19, len(data), len(data))
index = chunk(b'File', chunk(b'info', info) + chunk(b'segm', segment) + chunk(b'adlr', struct.pack('<I', zlib.adler32(data))))
(root / 'data.xp3').write_bytes(b'XP3\r\n \n\x1a\x8bg\x01' + struct.pack('<Q', 19 + len(data)) + data + b'\x00' + struct.pack('<Q', len(index)) + index)
