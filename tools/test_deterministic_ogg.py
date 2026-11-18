import hashlib
import random
import struct
import unittest

from compact_media import ogg_crc
from deterministic_ogg import crc, contract, restore, pages


def stream(serial, payload=b'encoded audio'):
    result = bytearray()
    for sequence, data in enumerate((b'OpusHead' + bytes(11), payload)):
        p = bytearray(struct.pack('<4sBBQIIIB',b'OggS',0,2 if sequence==0 else 4,
                                  sequence*960,serial,sequence,0,1)+bytes([len(data)])+data)
        struct.pack_into('<I',p,22,ogg_crc(p))
        result.extend(p)
    return bytes(result)


class OggTests(unittest.TestCase):
    def test_accelerated_crc_matches_reference(self):
        for n in (0,1,27,255,4096,65536):
            data=random.Random(n).randbytes(n)
            self.assertEqual(crc(data),ogg_crc(data))

    def test_random_serial_reproduces_exact_original(self):
        target=stream(700)
        self.assertEqual(restore(stream(800),contract(target)),target)

    def test_packet_change_cannot_be_hidden(self):
        with self.assertRaisesRegex(ValueError,'encoding/page'):
            restore(stream(800,b'altered audio'),contract(stream(700)))

    def test_changed_granule_cannot_be_hidden(self):
        altered=bytearray(stream(800));struct.pack_into('<Q',altered,6,1)
        with self.assertRaisesRegex(ValueError,'encoding/page'):
            restore(altered,contract(stream(700)))

    def test_bad_reference_crc_rejected(self):
        altered=bytearray(stream(700));altered[-1]^=1
        with self.assertRaisesRegex(ValueError,'CRC'):
            contract(altered)

    def test_truncation_and_chaining_rejected(self):
        for data in (stream(1)[:-1],stream(1)+stream(2),b''):
            with self.assertRaises(ValueError):list(pages(data))

    def test_noop_reuses_exact_stream(self):
        data=stream(500)
        self.assertIs(restore(data,contract(data)),data)


if __name__=='__main__':unittest.main()
