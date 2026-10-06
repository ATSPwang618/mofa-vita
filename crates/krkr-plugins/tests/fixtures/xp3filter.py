"""Generate encrypted XP3 resources and a real portable script filter."""
from pathlib import Path
import struct,zlib
root=Path('target/xp3filter');root.mkdir(parents=True,exist_ok=True)
data=b'/*'+b'x'*140000+b"*/'XP3_DECODED';"
def chunk(name,data):return name+struct.pack('<Q',len(data))+data
payload=bytearray();entries=[]
for name,compressed in [('stream.tjs',False),('full.tjs',True)]:
    encrypted=bytes(b^0xa5 for b in data)
    blob=zlib.compress(encrypted) if compressed else encrypted
    encoded=name.encode('utf-16-le')
    info=struct.pack('<IQQH',0x80000000,len(data),len(blob),len(encoded)//2)+encoded
    seg=struct.pack('<IQQQ',int(compressed),19+len(payload),len(data),len(blob))
    entries.append(chunk(b'File',chunk(b'info',info)+chunk(b'segm',seg)+chunk(b'adlr',struct.pack('<I',0x12345678))))
    payload+=blob
index=b''.join(entries)
(root/'encrypted.xp3').write_bytes(b'XP3\r\n \n\x1a\x8bg\x01'+struct.pack('<Q',19+len(payload))+payload+b'\0'+struct.pack('<Q',len(index))+index)
(root/'xp3filter.tjs').write_text(r'''
var decoderPrivate=123, counter=0;
function check(v,m){if(!v)throw m;}
class Content {
    var name, length;
    function Content(n,s){name=n;length=s;}
    property flags {getter{return name=='full.tjs'?1:0;}}
    property context {getter{return %[name:name,size:length,calls:0];}}
}
Storages.setXP3ArchiveContentFilter(function(file,archive,size){
    check(archive.indexOf('encrypted.xp3')>=0 && size>140000,'content metadata');
    counter++;var result=new Content(file,size);
    &result['0']=&result.flags;&result['1']=&result.context;return result;
});
Storages.setXP3ArchiveExtractionFilter(function(hash,offset,buf,size,file,ctx){
    check(hash==0x12345678 && file==ctx.name && size==buf.count && offset>=0,'extraction metadata');
    check(decoderPrivate==123 && counter>=1,'isolated persistent globals');ctx.calls++;
    var original=buf[0];buf[0]=255;buf[0]++;check(buf[0]==0,'byte increment wraps');
    buf[0]=240;buf[0]/=254;check(buf[0]==136,'signed byte division');
    buf[0]=255;buf[0]>>=8;check(buf[0]==0,'promoted byte shift');
    buf[0]=original;buf[0]+=256;check(buf[0]==original,'byte operand truncation');
    if(size>1){var second=buf[1];buf.ptr++;check(buf[0]==second && buf.count==size,'relative pointer');buf.ptr=0;}
    check(buf['0']===void && buf.unknown===void,'named read differs from numeric index');
    buf.xor(0,size,0xa5);
});
''',encoding='utf-8')
