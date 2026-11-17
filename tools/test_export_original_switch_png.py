"""Small invariants for the original-only PNG export, independent of game data."""
import importlib.util
import io
from pathlib import Path
import struct
import unittest
import zlib

from PIL import Image

spec = importlib.util.spec_from_file_location('exporter', Path(__file__).with_name('export-original-switch-png.py'))
exporter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(exporter)


class ExportTests(unittest.TestCase):
    def test_constant_opaque(self):
        image = Image.new('RGBA', (1920,1080), (120,30,200,255))
        result = exporter.resize(image,(960,540))
        self.assertEqual(result.size,(960,540))
        self.assertEqual(result.getextrema(),image.getextrema())

    def test_transparent_rgb_preserved(self):
        image = Image.new('RGBA',(8,8),(210,40,100,0))
        self.assertEqual(exporter.resize(image,(4,4)).getextrema(),image.getextrema())

    def test_transparent_edge_not_black(self):
        image = Image.new('RGBA',(32,32),(0,0,0,0))
        image.paste((255,100,20,255),(8,8,24,24))
        for r,g,b,a in exporter.resize(image,(16,16)).getdata():
            if a > 4:
                self.assertEqual(r,255)
                self.assertLessEqual(abs(g-100),1)
                self.assertLessEqual(abs(b-20),1)

    def test_no_resize_pixel_identity(self):
        image = Image.frombytes('RGBA',(2,1),bytes([1,2,3,0,200,30,10,127]))
        self.assertEqual(exporter.resize(image,image.size).tobytes(),image.tobytes())

    def test_nx_and_mzp(self):
        payload=b'payload';compressed=zlib.compress(payload)
        nx=b'NXCX'+struct.pack('<III',len(payload),len(compressed),0)+compressed
        mzp=b'mrgd00'+struct.pack('<H4H',1,0,0,0,len(nx))+nx
        self.assertEqual(list(exporter.leaves(mzp)),[('_m00',payload)])

    def test_truncated_mzp_rejected(self):
        with self.assertRaises(AssertionError):
            list(exporter.leaves(b'mrgd00'+struct.pack('<H4H',1,0,0,0,20)))

    def test_png_decode_preserves_alpha(self):
        image=Image.new('RGBA',(7,5),(120,33,44,17));buf=io.BytesIO();image.save(buf,format='PNG')
        decoded=list(exporter.textures(buf.getvalue()))[0][2]
        self.assertEqual(decoded.tobytes(),image.tobytes())

    def test_bc4_interpolation_and_crop(self):
        # Every texel selects interpolation slot 2; production rounds 6/7*255 to 219.
        bits=sum(2 << (3*i) for i in range(16))
        image=exporter.decode_bc4(bytes([255,0])+bits.to_bytes(6,'little'),3,2)
        self.assertEqual(image.size,(3,2))
        self.assertEqual(image.getpixel((2,1)),(219,219,219,255))

    def test_language_crop_matches_production_floor_rules(self):
        plans=exporter.image_plan('jallpac',11418,'IMG0000.NXZ',(84,155),'0x2006',11408,{'IMG0000'})
        self.assertEqual([p['crop'] for p in plans],[[0,0,84,38],[0,38,84,76],[0,77,84,115],[0,116,84,154]])
        self.assertEqual([p['size'] for p in plans],[[84,38]]*4)

    def test_language_split_is_table_driven(self):
        self.assertEqual(len(exporter.image_plan('allpacml',1,'OTHER.NXZ',(1280,2880),'0x2006',999,set())),1)
        self.assertEqual(len(exporter.image_plan('jallpachd',671,'IMG0000.NXZ',(750,1392),'0x2006',999,{'IMG0000'})),4)

    def test_fullscreen_densities_match(self):
        for archive,size in [('jallpac',(1280,720)),('jallpachd',(1920,1080))]:
            self.assertEqual(exporter.image_plan(archive,1,'IMG1',size,'raster',999,set())[0]['size'],[960,540])

    def test_long_pan_image_not_squashed_to_canvas(self):
        p=exporter.image_plan('jallpachd',1,'IMG2331.JPG',(2100,2940),'raster',999,set())[0]
        self.assertEqual(p['size'],[1050,1470])
        self.assertEqual(p['crop'],[0,0,2100,2940])

    def test_soft_clip_lookup_preserved(self):
        p=exporter.image_plan('allui',46,'BORDER_PARTS.NXZ',(512,512),'0x2006',999,set())[0]
        self.assertEqual(p['size'],[512,512])
        self.assertEqual(p['rule'],'native-soft-clip-lookup')

    def test_tiny_control_not_downsampled(self):
        p=exporter.image_plan('allui',48,'EFFECT_PARTS.NXZ',(960,2),'0x2006',999,set())[0]
        self.assertEqual(p['size'],[960,2])

    def test_adjacent_languages_never_filter_together(self):
        im=Image.new('RGBA',(64,32))
        colors=[(255,0,0,255),(0,255,0,255),(0,0,255,255),(255,255,255,255)]
        for k,c in enumerate(colors):im.paste(c,(0,k*8,64,(k+1)*8))
        plans=exporter.image_plan('jallpachd',1,'TXT.NXZ',im.size,'0x2006',999,{'TXT'})
        for c,p in zip(colors,plans):
            out=exporter.resize(im.crop(p['crop']),tuple(p['size']))
            self.assertEqual(out.getextrema(),tuple((x,x) for x in c))


if __name__ == '__main__':
    unittest.main()
