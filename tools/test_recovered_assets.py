"""Regression tests for the recovered production-format writers."""
import json
import io
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

from pack_audio_bank import pack
from generate_vita_asset_profile_540 import audio_bank_locations
from recover_runtime_recipe import image_contract
from rebuild_story_images import apply_lut, read_luts, Originals, original
from rebuild_from_nsp import member, safe_owned_remove
from rebuild_bgm_lead import make_lead
from recovery_kit import opus_contract
import numpy as np
from PIL import Image


class RecoveredTests(unittest.TestCase):
    def test_ogg_duration_uses_page_boundaries_not_payload_markers(self):
        head=b'OpusHead'+bytes([1,2])+struct.pack('<HIhB',312,48000,0,0)
        def page(payload,granule,flags):
            return struct.pack('<4sBBQIIIB',b'OggS',0,flags,granule,9,0,0,1)+bytes([len(payload)])+payload
        data=page(head,0,2)+page(b'encodedOggSpayload',48312,4)
        self.assertEqual(opus_contract(data),(2,48000))
        with self.assertRaises(ValueError):opus_contract(data[:-1])

    def test_recovered_jpeg_keeps_codec_sampling_and_tables(self):
        from build_recovered_runtime import encode_image
        from resize_story_variants import unpack
        image=Image.new('RGB',(19,13),(24,90,170))
        source=io.BytesIO();image.save(source,'JPEG',quality=87,subsampling=2)
        raw=struct.pack('<4s7I',b'KIMG',1,38,26,19,13,1,len(source.getvalue()))+source.getvalue()
        contract=image_contract(raw)
        encoded,_,_=encode_image(image.convert('RGBA'),contract)
        self.assertEqual(image_contract(unpack(encoded)),contract)

    def test_bgm_lead_rejects_non_ogg(self):
        with self.assertRaises(ValueError):make_lead(b'not an ogg file'*20)

    def test_duplicate_thumbnail_keeps_ordinal_instead_of_hd_name(self):
        class FakeReader:
            def __init__(self):self.calls=[]
            def read(self,index):self.calls.append(index);return b'fake'
        obj=Originals.__new__(Originals);obj.root=Path('unused')
        low,high=FakeReader(),FakeReader()
        obj.readers={'jallpac':low,'jallpachd':high}
        obj.names={'jallpac':{'SAME':2},'jallpachd':{'SAME':5}}
        image=Image.new('RGBA',(2,4),(20,30,40,255))
        with patch.object(original,'thumbnail_start',return_value=10), \
             patch.object(original,'leaves',return_value=[('',b'fake')]), \
             patch.object(original,'textures',return_value=[(0,'',image,'test')]):
            _,source=obj.image('jallpac',dict(source_name='SAME.NXZ',source_index=13,language=0))
        self.assertEqual(low.calls,[13]);self.assertEqual(high.calls,[])
        self.assertEqual(source['index'],13)

    def test_pfs_member_bounds(self):
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'game.nsp';name=b'program.nca\0'
            p.write_bytes(struct.pack('<4sIII',b'PFS0',1,len(name),0)+
                          struct.pack('<QQII',0,4,0,0)+name+b'data')
            self.assertEqual(member(p,'program.nca'),(40+len(name),4))
            with self.assertRaises(ValueError):member(p,'other.nca')
            p.write_bytes(p.read_bytes()[:-1])
            with self.assertRaises(ValueError):member(p,'program.nca')

    def test_cleanup_requires_owner_and_exact_stage(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);stage=root/'romfs';stage.mkdir();(stage/'x').write_bytes(b'x')
            with self.assertRaises(ValueError):safe_owned_remove(stage,root)
            (root/'.rebuild-owner.json').write_bytes(b'{}')
            with self.assertRaises(ValueError):safe_owned_remove(root,root)
            with self.assertRaises(ValueError):safe_owned_remove(root/'private-kit',root)
            safe_owned_remove(stage,root);self.assertFalse(stage.exists())

    def test_bank_real_reader_and_offsets(self):
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp)
            source = p/'source'
            source.write_bytes(b'ignoredOggSabcOggSdefg')
            pack({'B.ogg': (source,14,8), 'A.ogg': (source,7,7)}, p/'bgm.mrg')
            locations = audio_bank_locations(p)
            for name, payload in [('A.ogg',b'OggSabc'),('B.ogg',b'OggSdefg')]:
                path,offset,size = locations['stream/'+name]
                with path.open('rb') as f:
                    f.seek(offset)
                    self.assertEqual(f.read(size),payload)

    def test_bank_rejects_unsafe_name_before_creation(self):
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp); src=p/'source';src.write_bytes(b'a')
            for name in ['../a','x/y','x\\y','x\0y','a'*32]:
                with self.assertRaises(ValueError):
                    pack({name:(src,0,1)},p/'bgm.mrg')
                self.assertFalse((p/'bgm.mrg').exists())

    def test_kimg_contract(self):
        raw=struct.pack('<4s7I',b'KIMG',1,1920,1080,480,270,2,4)+b'data'
        self.assertEqual(image_contract(raw),dict(logical=[1920,1080],physical=[480,270],codec=2))
        with self.assertRaises(ValueError):image_contract(raw[:-1])

    def test_lut_keeps_alpha_and_integer_grey_weights(self):
        key=dict(grade=2,bright_bits='3F800000',contrast_bits='3F800000')
        lut=np.repeat(np.minimum(np.arange(65281)//256,255).astype(np.uint8)[:,None],3,axis=1)
        im=Image.fromarray(np.array([[[255,0,0,17],[0,255,0,200]]],dtype=np.uint8))
        out=np.asarray(apply_lut(im,key,{(2,0x3f800000,0x3f800000):(True,lut)}))
        self.assertEqual(out.tolist(),[[[53,53,53,17],[182,182,182,200]]])


if __name__=='__main__':unittest.main()
