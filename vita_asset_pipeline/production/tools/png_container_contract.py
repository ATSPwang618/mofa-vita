"""Restore PNG container choices without changing filtered scanline bytes.
No pixel data is retained in this contract. A bounded historical deflate suffix
is accepted only with identical prefix and exact decompressed scanline SHA.
"""
import hashlib,struct,zlib

def sha(d):return hashlib.sha256(d).hexdigest()
def chunks(png):
 if png[:8]!=b'\x89PNG\r\n\x1a\n':raise ValueError('Not PNG')
 p=8;out=[]
 while p<len(png):
  n=struct.unpack_from('>I',png,p)[0];tag=png[p+4:p+8];d=png[p+8:p+8+n]
  if len(d)!=n or zlib.crc32(tag+d)!=struct.unpack_from('>I',png,p+8+n)[0]:raise ValueError('PNG CRC mismatch')
  out.append((tag,d));p+=n+12
 if p!=len(png):raise ValueError('PNG trailing bytes')
 return out

def contract(png):
 c=chunks(png);stream=b''.join(d for t,d in c if t==b'IDAT')
 return dict(sha256=sha(png),chunks=[[t.decode(),len(d)] for t,d in c],stream_bytes=len(stream),
  stream_prefix_sha256=sha(stream[:-16]),stream_tail=stream[-16:].hex(),filtered_sha256=sha(zlib.decompress(stream)))

def restore(png,r):
 if sha(png)==r['sha256']:return png
 c=chunks(png);stream=b''.join(d for t,d in c if t==b'IDAT')
 if len(stream)!=r['stream_bytes'] or sha(stream[:-16])!=r['stream_prefix_sha256'] or sha(zlib.decompress(stream))!=r['filtered_sha256']:
  raise ValueError('PNG encoding differs beyond historical container boundary')
 stream=stream[:-16]+bytes.fromhex(r['stream_tail'])
 if sha(zlib.decompress(stream))!=r['filtered_sha256']:raise ValueError('PNG suffix would change image')
 others=iter((t,d) for t,d in c if t!=b'IDAT');p=0;out=bytearray(png[:8])
 for tag,n in r['chunks']:
  tag=tag.encode()
  if tag==b'IDAT':d=stream[p:p+n];p+=n
  else:
   t,d=next(others)
   if t!=tag or len(d)!=n:raise ValueError('PNG non-IDAT changed')
  out+=struct.pack('>I',len(d))+tag+d+struct.pack('>I',zlib.crc32(tag+d))
 if p!=len(stream) or sha(out)!=r['sha256']:raise ValueError('PNG final identity mismatch')
 return bytes(out)
