"""Vectorized BC3 decode with the project's bcdec_rs 0.2 rounding rules.

Pillow's endpoint expansion is not byte-equivalent. Use only where the historical
PNG exporter used production kdu, not for earlier Pillow-based conversion stages.
"""
import numpy as np
from PIL import Image


def decode(data, width, height):
    wb, hb = (width+3)//4, (height+3)//4
    blocks = np.frombuffer(data, dtype=np.uint8).reshape(-1, 16)
    if len(blocks) != wb*hb:
        raise ValueError('BC3 block count mismatch')
    words = blocks[:, 8:].copy().view('<u2').reshape(-1, 4).astype(np.uint32)
    c0, c1 = words[:, 0], words[:, 1]
    r0, g0, b0 = c0 >> 11, (c0 >> 5) & 63, c0 & 31
    r1, g1, b1 = c1 >> 11, (c1 >> 5) & 63, c1 & 31
    colors = np.empty((len(blocks), 4, 3), dtype=np.uint8)
    for c, a, b in [(0,r0,r1), (1,g0,g1), (2,b0,b1)]:
        colors[:,0,c] = ((a*259+33)>>6) if c == 1 else ((a*527+23)>>6)
        colors[:,1,c] = ((b*259+33)>>6) if c == 1 else ((b*527+23)>>6)
        colors[:,2,c] = (((2*a+b)*2763+1039)>>11) if c == 1 else (((2*a+b)*351+61)>>7)
        colors[:,3,c] = (((a+2*b)*2763+1039)>>11) if c == 1 else (((a+2*b)*351+61)>>7)
    selectors = blocks[:,12:].copy().view('<u4').reshape(-1)
    ci = (selectors[:,None] >> (2*np.arange(16, dtype=np.uint32))) & 3
    a0, a1 = blocks[:,0].astype(np.uint32), blocks[:,1].astype(np.uint32)
    alpha = np.empty((len(blocks),8), dtype=np.uint8)
    alpha[:,0], alpha[:,1] = a0, a1
    for i in range(2,8):
        alpha[:,i] = np.where(a0>a1, ((8-i)*a0+(i-1)*a1+1)//7,
                             ((6-i)*a0+(i-1)*a1+1)//5 if i<6 else 0 if i==6 else 255)
    selectors = blocks[:,:8].copy().view('<u8').reshape(-1) >> 16
    ai = (selectors[:,None] >> (3*np.arange(16,dtype=np.uint64))) & 7
    rgba = np.empty((len(blocks),16,4), dtype=np.uint8)
    rows = np.arange(len(blocks))[:,None]
    rgba[:,:,:3] = colors[rows,ci]
    rgba[:,:,3] = alpha[rows,ai]
    image = rgba.reshape(hb,wb,4,4,4).transpose(0,2,1,3,4).reshape(hb*4,wb*4,4)
    return Image.fromarray(image[:height,:width].copy())
