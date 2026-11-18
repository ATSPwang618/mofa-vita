"""No game assets or SDK required: fail-closed recovery and cache isolation tests."""
import json
from pathlib import Path
import tempfile
import unittest

from asset_reproducibility import recovery_scope, locked_cache, python_sources, verify_cached_payload


class ReproducibilityTests(unittest.TestCase):
    def test_exact_delivery_requires_all_bytes_not_just_manifest(self):
        from asset_reproducibility import digest
        from verify_final_delivery import verify
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);(root/'vita-540p-assets.json').write_bytes(b'manifest')
            (root/'bank.mrg').write_bytes(b'correct')
            lock={'schema':'knife-final-assets-lock-v1','files':{
                p.name:{'bytes':p.stat().st_size,'sha256':digest(p)} for p in root.iterdir()}}
            expected=digest(root/'vita-540p-assets.json')
            self.assertTrue(verify(root,lock,expected_manifest=expected,expected_lock=None)['exact_delivery_match'])
            (root/'bank.mrg').write_bytes(b'corrupt')
            with self.assertRaisesRegex(ValueError,'bank.mrg'):verify(root,lock,expected_manifest=expected,expected_lock=None)

    def test_extra_files_and_replaced_lock_rejected(self):
        from asset_reproducibility import digest
        from verify_final_delivery import verify
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);p=root/'vita-540p-assets.json';p.write_bytes(b'manifest');expected=digest(p)
            lock={'schema':'knife-final-assets-lock-v1','files':{
                p.name:{'bytes':p.stat().st_size,'sha256':expected}}}
            (root/'extra').write_bytes(b'junk')
            with self.assertRaisesRegex(ValueError,'file set'):verify(root,lock,expected_manifest=expected,expected_lock=None)
            lock['files'][p.name]['sha256']='fake'
            with self.assertRaisesRegex(ValueError,'pinned'):verify(root,lock,expected_manifest=expected,expected_lock=None)
            with self.assertRaisesRegex(ValueError,'lock changed'):verify(root,lock)

    def test_legacy_kit_cannot_claim_release(self):
        with self.assertRaisesRegex(ValueError,'SOURCE_REPRODUCTION_UNPROVEN'):
            recovery_scope({'contains_game_data':True})

    def test_json_claim_cannot_bypass_generator_capability(self):
        with self.assertRaises(ValueError):
            recovery_scope({'source_reproducible_release':True,'provenance_complete':True})

    def test_explicit_experiment_never_becomes_release(self):
        result=recovery_scope({'source_reproducible_release':True},True)
        self.assertFalse(result['source_reproducible_release'])
        self.assertFalse(result['historical_visual_equivalence_verified'])

    def test_encoder_or_parameters_change_cache_namespace(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);base={'encoder':'v1','threshold':4,'sources':{'helper':'a'}}
            a,_=locked_cache(root,base);(a/'image.gxt').write_bytes(b'old')
            for change in ({'encoder':'v2'},{'threshold':5},{'sources':{'helper':'b'}}):
                b,_=locked_cache(root,{**base,**change})
                self.assertNotEqual(a,b);self.assertFalse((b/'image.gxt').exists())
            self.assertEqual(locked_cache(root,base)[0],a)

    def test_unbound_legacy_entries_not_consumed_or_deleted(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);old=root/'image.gxt';old.write_bytes(b'legacy')
            directory,_=locked_cache(root,{'version':1})
            self.assertFalse((directory/'image.gxt').exists());self.assertEqual(old.read_bytes(),b'legacy')

    def test_cache_marker_tampering_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            directory,_=locked_cache(Path(temp),{'version':1})
            (directory/'recipe.json').write_text('{}')
            with self.assertRaises(ValueError):locked_cache(Path(temp),{'version':1})

    def test_missing_marker_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            directory,_=locked_cache(Path(temp),{'version':1})
            (directory/'recipe.json').unlink()
            with self.assertRaises(ValueError):locked_cache(Path(temp),{'version':1})

    def test_imported_helper_change_is_recorded(self):
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp);(root/'main.py').write_text('import helper')
            (root/'helper.py').write_text('value=1');a=python_sources(root)
            (root/'helper.py').write_text('value=2');b=python_sources(root)
            self.assertNotEqual(a,b);self.assertEqual(a['main.py'],b['main.py'])

    def test_corrupted_or_unbound_payload_rejected(self):
        import hashlib
        record=dict(payload_bytes=4,payload_sha256=hashlib.sha256(b'good').hexdigest())
        verify_cached_payload(b'good',record)
        for value,metadata in ((b'evil',record),(b'bad',record),(b'good',{})):
            with self.assertRaises(ValueError):verify_cached_payload(value,metadata)


if __name__=='__main__':unittest.main()
