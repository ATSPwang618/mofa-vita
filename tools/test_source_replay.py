"""Small fail-closed tests; no retail assets, SDK, or conversion run needed."""
import bz2,hashlib,json,struct,tempfile,unittest
from pathlib import Path
from historical_encoding import apply,sha
from rebuild_final_assets import tree,verify_tree,save,source_sha

def fixture():
 source=b'abc';target=b'abd!';control=bz2.compress(struct.pack('<3q',3,1,0));diff=bz2.compress(bytes([0,0,1]));extra=bz2.compress(b'!')
 patch=b'BSDIFF40'+struct.pack('<3q',len(control),len(diff),len(target))+control+diff+extra
 record=dict(identity='test',generated_sha256=sha(source),patch_sha256=sha(patch),output_sha256=sha(target),output_bytes=len(target))
 return source,patch,record

class SourceReplayTests(unittest.TestCase):
 def test_git_line_endings_do_not_change_source_identity(self):
  with tempfile.TemporaryDirectory() as d:
   p=Path(d)/'code.py';p.write_bytes(b'a\nb\n');h=source_sha(p);p.write_bytes(b'a\r\nb\r\n');self.assertEqual(source_sha(p),h);p.write_bytes(b'changed\n');self.assertNotEqual(source_sha(p),h)
 def test_exact_delta(self):
  self.assertEqual(apply(*fixture()),b'abd!')
 def test_wrong_generated_bytes_rejected(self):
  s,p,r=fixture()
  with self.assertRaisesRegex(ValueError,'intermediate changed'):apply(b'xxx',p,r)
 def test_patch_identity_rejected(self):
  s,p,r=fixture()
  with self.assertRaisesRegex(ValueError,'identity changed'):apply(s,p+b'x',r)
 def test_truncation_rejected_even_with_new_patch_hash(self):
  s,p,r=fixture();p=p[:32];r['patch_sha256']=sha(p)
  with self.assertRaisesRegex(ValueError,'Truncated'):apply(s,p,r)
 def test_output_hash_rejected(self):
  s,p,r=fixture();r['output_sha256']='0'*64
  with self.assertRaisesRegex(ValueError,'output identity'):apply(s,p,r)
 def test_oversize_rejected(self):
  s,p,r=fixture();r['output_bytes']=65*1024**2;p=p[:24]+struct.pack('<q',r['output_bytes'])+p[32:];r['patch_sha256']=sha(p)
  with self.assertRaisesRegex(ValueError,'bounds'):apply(s,p,r)
 def test_checkpoint_same_size_corruption(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d);(root/'a').write_bytes(b'abc');saved=tree(root);verify_tree(root,saved);(root/'a').write_bytes(b'xyz')
   with self.assertRaisesRegex(ValueError,'Checkpoint changed'):verify_tree(root,saved)
 def test_checkpoint_extra_file(self):
  with tempfile.TemporaryDirectory() as d:
   root=Path(d);(root/'a').write_bytes(b'abc');saved=tree(root);(root/'extra').write_bytes(b'x')
   with self.assertRaisesRegex(ValueError,'Checkpoint changed'):verify_tree(root,saved)
 def test_atomic_checkpoint_unicode(self):
  with tempfile.TemporaryDirectory() as d:
   p=Path(d)/'state.json';save(p,{'source':'魔法使之夜'});self.assertEqual(json.loads(p.read_bytes())['source'],'魔法使之夜');self.assertFalse(p.with_suffix('.json.tmp').exists())

if __name__=='__main__':unittest.main()
