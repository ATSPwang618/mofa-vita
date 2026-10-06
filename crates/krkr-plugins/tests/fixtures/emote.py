"""Generate a tiny original E-mote animation; no game assets are used."""
from pathlib import Path
import struct
import zlib

frame = lambda src: {'time': 0, 'type': 0, 'content': {'src': src, 'opa': 1.0}}
root = {
    'spec': 'krkr', 'screenSize': {'width': 32, 'height': 32},
    'metadata': {'base': {'chara': 'actor', 'motion': 'idle'},
                 'variableList': [{'label': 'face', 'frameList': [0, 1]}]},
    'source': {'body': {'icon': {'red': {'width': 4, 'height': 4,
               'pixel': bytes([0, 0, 255, 255]) * 16}}}},
    'object': {'actor': {'motion': {'idle': {
        'layer': [{'label': 'image', 'type': 0, 'frameList': [frame('src/body/red')]},
                  {'label': 'touch', 'type': 1, 'frameList': [frame('shape/rect')]}],
        'priority': [{'content': [0, 1]}], 'lastTime': 60, 'loopTime': 0
    }}}}
}
names, strings, chunks = [], [], []
def scan(node):
    if isinstance(node, dict):
        for key, value in node.items():
            if key not in names: names.append(key)
            scan(value)
    elif isinstance(node, list):
        for value in node: scan(value)
    elif isinstance(node, str) and node not in strings: strings.append(node)
    elif isinstance(node, bytes): chunks.append(node)
scan(root)
def array(values):
    return b'\x10' + struct.pack('<I', len(values)) + b'\x10' + b''.join(struct.pack('<I', n) for n in values)
parents, bases, indices, next_base = {0: 0}, {0: 1}, [], 256
for name in names:
    parent = 0
    for byte in name.encode():
        node = bases[parent] + byte
        parents[node] = parent
        if node not in bases: bases[node], next_base = next_base, next_base + 256
        parent = node
    terminal = bases[parent]
    parents[terminal] = parent
    indices.append(terminal)
name_table = array([bases.get(n, 0) for n in range(max(bases)+1)])
name_table += array([parents.get(n, 0) for n in range(max(parents)+1)]) + array(indices)
def blocks(items):
    offsets, data = [], b''
    for item in items:
        offsets.append(len(data))
        data += item
    return offsets, data
def encode(node):
    if isinstance(node, dict):
        offsets, data = blocks([encode(v) for v in node.values()])
        return b'\x21' + array([names.index(n) for n in node]) + array(offsets) + data
    if isinstance(node, list):
        offsets, data = blocks([encode(v) for v in node])
        return b'\x20' + array(offsets) + data
    if isinstance(node, str): return b'\x18' + struct.pack('<I', strings.index(node))
    if isinstance(node, bytes): return b'\x1c' + struct.pack('<I', chunks.index(node))
    if isinstance(node, float): return b'\x1f' + struct.pack('<d', node)
    return b'\x08' + struct.pack('<i', node)
string_offsets, string_data = blocks([s.encode() + b'\0' for s in strings])
chunk_offsets, chunk_data = blocks(chunks)
data, offsets = bytearray(40), []
for block in [name_table, array(string_offsets), string_data, array(chunk_offsets), array(list(map(len, chunks))), chunk_data, encode(root)]:
    offsets.append(len(data))
    data.extend(block)
data[:40] = struct.pack('<4sHH8I', b'PSB\0', 2, 0, offsets[0], *offsets)
Path(__file__).with_name('emote.mdf').write_bytes(b'mdf\0' + struct.pack('<I', len(data)) + zlib.compress(data))
