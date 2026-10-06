"""Create tiny AMV streams with known DC/AC values and both alpha encodings."""
from pathlib import Path
import struct
import zlib
root=Path('target/alpha-movie')
root.mkdir(parents=True,exist_ok=True)

def codes(counts):
    result={};code=0;symbol=0
    for width,count in enumerate(counts,1):
        for _ in range(count):
            result[symbol]=format(code,f'0{width}b');code+=1;symbol+=1
        code <<= 1
    return result

luma=codes([0,1,5,1,1,1,1,1,1])
chroma=codes([0,3,1,1,1,1,1,1,1,1])

def block(delta, color=False, ac=False):
    width=abs(delta).bit_length()
    bits=(chroma if color else luma)[width]
    if width:
        bits+=format(delta if delta>=0 else delta+(1<<width)-1,f'0{width}b')
    if ac:
        bits+='1011'+'1000' # luma AC run 0, width 4, value +8
    return bits+('00' if color else '1010')

def frame(index, compressed, ac=False):
    # U=160, V=100; shared chroma predictor. Y=100 or 128.
    y=0 if ac else -28
    bits=block(32,True)+block(-60,True)
    bits+=block(y,ac=ac)+block(0)+block(0)+block(0)
    if not compressed:
        # Alpha=160, shares the luma predictor with the preceding Y blocks.
        bits+=block(32-y)+block(0)+block(0)+block(0)
    bits+='0'*(-len(bits)%32)
    payload=int(bits,2).to_bytes(len(bits)//8,'big')
    alpha=zlib.compress(bytes((x+y*16)%256 for y in range(16) for x in range(16))) if compressed else b''
    header=struct.pack('<4sII4H',b'FRAM',len(payload)+len(alpha)+(16 if compressed else 12),index,16,16,16,16)
    if compressed: header+=struct.pack('<I',len(alpha))
    return header+alpha+payload

def movie(compressed):
    quant=bytes([8])*(128 if compressed else 192)
    frames=frame(0,compressed)+frame(1,compressed,True)
    header=struct.pack('<4s7I2HI',b'AJPM',40+len(quant)+len(frames),1,40+len(quant),0,2,0,24,32,32,2 if compressed else 1)
    return header+quant+frames

(root/'zlib.amv').write_bytes(movie(True))
(root/'yuva.amv').write_bytes(movie(False))
(root/'short.amv').write_bytes(b'AJPM')
(root/'bad.amv').write_bytes(movie(True)[:-8])

data=movie(False)
def chunk(tag,value):return tag+struct.pack('<Q',len(value))+value
name='inside.amv'.encode('utf-16-le')
info=struct.pack('<IQQH',0,len(data),len(data),len(name)//2)+name
segment=struct.pack('<IQQQ',0,19,len(data),len(data))
index=chunk(b'File',chunk(b'info',info)+chunk(b'segm',segment)+chunk(b'adlr',struct.pack('<I',zlib.adler32(data))))
(root/'movie.xp3').write_bytes(b'XP3\r\n \n\x1a\x8bg\x01'+struct.pack('<Q',19+len(data))+data+b'\x00'+struct.pack('<Q',len(index))+index)
