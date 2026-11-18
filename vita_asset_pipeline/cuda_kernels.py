"""Exact-contract CUDA operations for the optional image accelerator.

Lanczos coefficients and pass rounding follow Pillow 11.3.0 Resample.c.
Copyright for the Pillow reference remains with its authors (HPND license).
"""
from functools import lru_cache
import math
import numpy as np
from numba import cuda, float32, float64, int32
from numba.cuda import libdevice


@lru_cache(maxsize=64)
def coefficients(n, out):
    scale=n/out
    support=3*max(1.,scale)
    width=math.ceil(support)*2+1
    bounds=np.zeros((out,2),np.int32)
    weights=np.zeros((out,width),np.float64)
    if n==out:
        bounds[:,0]=np.arange(n)
        bounds[:,1]=1
        weights[:,0]=1
        return bounds,weights,(weights*(1<<22)).astype(np.int32)
    for i in range(out):
        center=(i+.5)*scale
        start=max(0,int(center-support+.5))
        end=min(n,int(center+support+.5))
        total=0.
        for j in range(end-start):
            x=(j+start-center+.5)*(1/max(1.,scale))
            if -3 <= x < 3:
                def sinc(v):
                    return 1. if v == 0 else math.sin(v*math.pi)/(v*math.pi)
                weights[i,j]=sinc(x)*sinc(x/3)
                total+=weights[i,j]
        if total:
            for j in range(end-start):weights[i,j]/=total
        bounds[i]=(start,end-start)
    fixed=np.where(weights<0,weights*(1<<22)-.5,weights*(1<<22)+.5).astype(np.int32)
    return bounds,weights,fixed


@cuda.jit
def horizontal_alpha(src,bounds,weights,tmp):
    x,y,c=cuda.grid(3)
    if y>=src.shape[0] or x>=tmp.shape[1] or c>=7:return
    total=float64(0)
    for j in range(bounds[x,1]):
        xx=bounds[x,0]+j
        alpha=float32(src[y,xx,3])/float32(255)
        value=alpha
        if c>0 and c<4:value=float32(src[y,xx,c-1])*alpha
        elif c>=4:value=float32(src[y,xx,c-4])
        total=libdevice.dadd_rn(total,libdevice.dmul_rn(float64(value),weights[x,j]))
    tmp[y,x,c]=total


@cuda.jit
def vertical_alpha(tmp,bounds,weights,out):
    x,y=cuda.grid(2)
    if x>=out.shape[1] or y>=out.shape[0]:return
    values=cuda.local.array(7,float32)
    for c in range(7):
        total=float64(0)
        for j in range(bounds[y,1]):
            total=libdevice.dadd_rn(total,libdevice.dmul_rn(float64(tmp[bounds[y,0]+j,x,c]),weights[y,j]))
        values[c]=float32(total)
    a=min(float32(1),max(float32(0),values[0]))
    for c in range(3):
        v=values[c+4]
        if a>float32(1/510):v=values[c+1]/max(values[0],float32(1e-8))
        out[y,x,c]=int(libdevice.nearbyintf(min(float32(255),max(float32(0),v))))
    out[y,x,3]=int(libdevice.nearbyintf(a*float32(255)))


@cuda.jit
def horizontal_u8(src,bounds,weights,tmp):
    x,y,c=cuda.grid(3)
    if y>=src.shape[0] or x>=tmp.shape[1] or c>=4:return
    total=int32(1<<21)
    for j in range(bounds[x,1]):total+=int32(src[y,bounds[x,0]+j,c])*weights[x,j]
    tmp[y,x,c]=min(255,max(0,total>>22))


@cuda.jit
def vertical_u8(tmp,bounds,weights,out):
    x,y,c=cuda.grid(3)
    if y>=out.shape[0] or x>=out.shape[1] or c>=4:return
    total=int32(1<<21)
    for j in range(bounds[y,1]):total+=int32(tmp[bounds[y,0]+j,x,c])*weights[y,j]
    out[y,x,c]=min(255,max(0,total>>22))


@lru_cache(maxsize=64)
def device_coefficients(n,out):
    return tuple(cuda.to_device(a) for a in coefficients(n,out))


def resize_array(src,size,opaque=False):
    h,w=src.shape[:2]
    nw,nh=size
    if (w,h)==size:return src.copy()
    xb,xw,xi=device_coefficients(w,nw)
    yb,yw,yi=device_coefficients(h,nh)
    inp=cuda.to_device(src)
    out=cuda.device_array((nh,nw,4),np.uint8)
    if opaque:
        tmp=cuda.device_array((h,nw,4),np.uint8)
        horizontal_u8[((nw+15)//16,(h+7)//8,1),(16,8,4)](inp,xb,xi,tmp)
        vertical_u8[((nw+15)//16,(nh+7)//8,1),(16,8,4)](tmp,yb,yi,out)
    else:
        tmp=cuda.device_array((h,nw,7),np.float32)
        horizontal_alpha[((nw+15)//16,(h+3)//4,1),(16,4,8)](inp,xb,xw,tmp)
        vertical_alpha[((nw+15)//16,(nh+15)//16),(16,16)](tmp,yb,yw,out)
    return out.copy_to_host()


@cuda.jit
def premultiply_u8(src,out):
    x,y=cuda.grid(2)
    if y>=src.shape[0] or x>=src.shape[1]:return
    alpha=int32(src[y,x,3])
    for c in range(3):
        value=int32(src[y,x,c])*alpha+128
        out[y,x,c]=((value>>8)+value)>>8
    out[y,x,3]=alpha


@cuda.jit
def unpremultiply_u8(src,out):
    x,y=cuda.grid(2)
    if y>=src.shape[0] or x>=src.shape[1]:return
    alpha=int32(src[y,x,3])
    for c in range(3):
        value=int32(src[y,x,c])
        if alpha!=0 and alpha!=255:value=min(255,255*value//alpha)
        out[y,x,c]=value
    out[y,x,3]=alpha


def resize_rgba8(src,size):
    """Pillow RGBA.resize: RGBa u8 conversion, two fixed-point passes, RGBA."""
    h,w=src.shape[:2]
    nw,nh=size
    if (w,h)==size:return src.copy()
    xb,_,xi=device_coefficients(w,nw)
    yb,_,yi=device_coefficients(h,nh)
    inp=cuda.to_device(src)
    pre=cuda.device_array_like(inp)
    tmp=cuda.device_array((h,nw,4),np.uint8)
    scaled=cuda.device_array((nh,nw,4),np.uint8)
    out=cuda.device_array_like(scaled)
    premultiply_u8[((w+15)//16,(h+15)//16),(16,16)](inp,pre)
    horizontal_u8[((nw+15)//16,(h+7)//8,1),(16,8,4)](pre,xb,xi,tmp)
    vertical_u8[((nw+15)//16,(nh+7)//8,1),(16,8,4)](tmp,yb,yi,scaled)
    unpremultiply_u8[((nw+15)//16,(nh+15)//16),(16,16)](scaled,out)
    return out.copy_to_host()


@cuda.jit
def grade_kernel(src,lut,grey,out):
    x,y=cuda.grid(2)
    if y>=src.shape[0] or x>=src.shape[1]:return
    index=int32(src[y,x,0])*54+int32(src[y,x,1])*183+int32(src[y,x,2])*19
    for c in range(3):out[y,x,c]=lut[index if grey else int32(src[y,x,c]),c]
    out[y,x,3]=src[y,x,3]


def grade_array(src,lut,grey):
    inp=cuda.to_device(src)
    out=cuda.device_array_like(inp)
    grade_kernel[((src.shape[1]+15)//16,(src.shape[0]+15)//16),(16,16)](inp,cuda.to_device(lut),grey,out)
    return out.copy_to_host()


@cuda.jit(device=True)
def spread(v):
    v=(v|(v<<8))&0x00ff00ff
    v=(v|(v<<4))&0x0f0f0f0f
    v=(v|(v<<2))&0x33333333
    return (v|(v<<1))&0x55555555


@cuda.jit
def swizzle_kernel(src,w,h,bpe,out):
    i=cuda.grid(1)
    if i>=src.size:return
    pixel=i//bpe
    x=pixel%w
    y=pixel//w
    tile=min(w,h)
    dst=((x//tile)+(y//tile)*(w//tile))*tile*tile+(spread(x%tile)<<1)+spread(y%tile)
    out[dst*bpe+i%bpe]=src[i]


def swizzle(data,w,h,bpe):
    inp=cuda.to_device(np.frombuffer(data,np.uint8))
    out=cuda.device_array_like(inp)
    swizzle_kernel[(len(data)+255)//256,256](inp,w,h,bpe,out)
    return out.copy_to_host().tobytes()


@cuda.jit
def bc3_kernel(src,width,height,out):
    x,y=cuda.grid(2)
    if x>=width or y>=height:return
    block=(y//4)*((width+3)//4)+x//4
    p=block*16
    pixel=(y%4)*4+x%4
    c0=int32(src[p+8])|(int32(src[p+9])<<8)
    c1=int32(src[p+10])|(int32(src[p+11])<<8)
    ci=(int32(src[p+12+pixel//4])>>(2*(pixel%4)))&3
    for c in range(3):
        shift=11 if c==0 else 5 if c==1 else 0
        mask=63 if c==1 else 31
        a=(c0>>shift)&mask
        b=(c1>>shift)&mask
        if ci==0:v=((a*259+33)>>6) if c==1 else ((a*527+23)>>6)
        elif ci==1:v=((b*259+33)>>6) if c==1 else ((b*527+23)>>6)
        elif ci==2:v=(((2*a+b)*2763+1039)>>11) if c==1 else (((2*a+b)*351+61)>>7)
        else:v=(((a+2*b)*2763+1039)>>11) if c==1 else (((a+2*b)*351+61)>>7)
        out[y,x,c]=v
    bit=3*pixel
    offset=p+2+bit//8
    bits=int32(src[offset])
    if offset+1<p+8:bits|=int32(src[offset+1])<<8
    ai=(bits>>(bit%8))&7
    a=int32(src[p])
    b=int32(src[p+1])
    if ai==0:v=a
    elif ai==1:v=b
    elif a>b:v=((8-ai)*a+(ai-1)*b+1)//7
    elif ai<6:v=((6-ai)*a+(ai-1)*b+1)//5
    else:v=0 if ai==6 else 255
    out[y,x,3]=v


def bc3_decode(data,w,h):
    inp=cuda.to_device(np.frombuffer(data,np.uint8))
    out=cuda.device_array((h,w,4),np.uint8)
    bc3_kernel[((w+15)//16,(h+15)//16),(16,16)](inp,w,h,out)
    return out.copy_to_host()
