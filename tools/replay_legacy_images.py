"""Bounded replay of the retained 2026-10 PNG/JPEG/Alter conversion path."""
import io,struct,subprocess,ctypes,importlib.util
from pathlib import Path
import numpy as np
from PIL import Image
import build_recovered_runtime as b
from compact_media import encode_portable
from optimize_compact import optimize_portable,optimize_png
from slim_languages import slim_kimg
ROOT=Path(__file__).resolve().parents[1]
proc=None

def compact(image,scale,strips,jpeg,source=None):
 global proc
 if proc is None:
  exe=Path(__import__('os').environ.get('LEGACY_EXE',str(ROOT/'tools/source_replay/legacy-compact-replay.exe')))
  if not exe.is_file():raise ValueError('Missing locked legacy image helper: '+str(exe))
  cmd=[str(exe)]
  proc=subprocess.Popen(cmd,stdin=subprocess.PIPE,stdout=subprocess.PIPE)
 proc.stdin.write(struct.pack('<II f II',len(source),0,scale,strips,2)+source if source else struct.pack('<II f II',*image.size,scale,strips,jpeg)+image.tobytes());proc.stdin.flush()
 w,h,n=struct.unpack('<3I',proc.stdout.read(12));data=proc.stdout.read(n);assert len(data)==n
 if jpeg:return Image.open(io.BytesIO(data)).convert('RGBA')
 hdr=b.texture.gxt.dds(data,w,h,'DX10')[:128]+struct.pack('<5I',98,3,0,1,0)
 return Image.open(io.BytesIO(hdr+data)).convert('RGBA')

def init():
 global g,vg
 g=b.texture.gxt;lib=ctypes.CDLL(str(ROOT/'tools/source_replay/native-color-blocks.dll'));fn=lib.native_color_refine;fn.restype=ctypes.c_size_t;fn.argtypes=[ctypes.c_void_p,ctypes.c_void_p,ctypes.c_void_p,ctypes.c_size_t,ctypes.c_uint,ctypes.c_uint,ctypes.c_void_p];g.load_refiner=lambda:fn
 spec=importlib.util.spec_from_file_location('vg',ROOT/'tools/vita-game-gxt.py');vg=importlib.util.module_from_spec(spec);spec.loader.exec_module(vg)

def migrate(image,logical,ui=False):
 im=vg.offline_alter(image);w,h=im.size;bw,bh=max(4,g.pow2(w)),max(4,g.pow2(h));opaque=im.getextrema()[3]==(255,255);bpe=8 if opaque else 16
 out=io.BytesIO();im.save(out,'DDS',pixel_format='DXT1' if opaque else 'DXT5');linear=out.getvalue()[128:];bb=np.zeros((bh//4,bw//4,bpe),dtype=np.uint8);bb[:(h+3)//4,:(w+3)//4]=np.frombuffer(linear,dtype=np.uint8).reshape((h+3)//4,(w+3)//4,bpe);swizzled=g.swizzle(bb.tobytes(),bw//4,bh//4,bpe)
 hdr=struct.pack('<8I',0x00545847,0x10000003,1,64,len(swizzled),0,0,0)+struct.pack('<6IHHB3s',64,len(swizzled),0xffffffff,0,0xa0000000,0x85000000 if opaque else 0x87000000,w,h,1,b'\0'*3)
 kimg=struct.pack('<4s7I',b'KIMG',1,*logical,w,h,8,len(hdr+swizzled))+hdr+swizzled
 g.STRICT_UI=ui
 result=g.convert_image(kimg)[0]
 for _ in range(int(__import__("os").environ.get("REFINE_AGAIN","0"))):result=g.convert_image(kimg,existing=result)[0]
 return result

def build(base,index,contract):
 raw=b.original.unpack(b.originals.readers[base].read(index));items=[x for _,leaf in b.original.leaves(raw) for x in b.original.textures(leaf,production_bc3=True)];assert len(items)==1
 _,_,image,fmt=items[0];logical=image.size;strips=4 if base.endswith('ml') and index<b.original.thumbnail_start(b.originals.root,base) else 1
 if index<b.original.thumbnail_start(b.originals.root,base) and min(image.size)>16 and fmt not in ('0x201','0x1d01'):
  image=compact(image,.5 if 'hd' in base else .75,strips,raw[:2]==b'\xff\xd8',raw if raw[:2]==b'\xff\xd8' else None)
 portable,_=encode_portable(image,logical,srgb=fmt not in ('0x201','0x1d01','0x2001','0x1a01','0x1c01'),strips=strips,force_png=contract['name'].startswith('RULE_'))
 portable,_=optimize_portable(portable)
 if strips==4:portable=slim_kimg(portable)
 if contract['codec'] in (9,7) and struct.unpack_from('<I',portable,24)[0] in (1,2) and strips==1:
  image=Image.open(io.BytesIO(portable[32:])).convert('RGBA');portable=migrate(image,struct.unpack_from('<II',portable,8))
 old=struct.unpack_from('<II',portable,16)
 if old!=tuple(contract['physical']):
  image=b.texture.decode(portable) if struct.unpack_from('<I',portable,24)[0]==9 else Image.open(io.BytesIO(portable[32:])).convert('RGBA')
  # budget crop resizes each retained language separately
  w,h=contract['physical'];out=Image.new('RGBA',(w,h));cnt=4 if Path(contract['name']).stem in b.original.strip_names() else 1
  for k in range(cnt):out.paste(image.crop((0,k*image.height//cnt,image.width,(k+1)*image.height//cnt)).resize((w,h//cnt),Image.Resampling.LANCZOS),(0,k*h//cnt))
  data,_,_=b.encode_image(out,contract,contract['raw_storage']);portable=b.texture.unpack(data)
 return portable[:32+int.from_bytes(portable[28:32],'little')]

