"""Verify retained 540p assets and editable images without the deleted HD inputs."""
from __future__ import annotations

from collections import Counter
from datetime import datetime
import hashlib
import json
from pathlib import Path
import time

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]


def read(path: Path):
    return json.loads(path.read_text(encoding='utf-8'))


def digest(path: Path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(4 * 1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def main():
    start = time.monotonic()
    validation = read(ROOT / 'doc/compact-assets-validation.json')
    assert validation['complete']
    lossless = validation.get('lossless_optimization')
    bilingual = validation.get('bilingual')
    lossless_end = read(ROOT / 'doc/compact-assets-validation-before-bilingual.json') if bilingual else validation
    if lossless:
        assert lossless['complete']
        historical_path = ROOT / 'doc/compact-assets-validation-before-lossless.json'
        assert digest(historical_path) == lossless['previous_validation_sha256']
        historical = read(historical_path)
        assert historical['complete']
        assert lossless_end['audio'] == historical['audio']
        assert lossless_end['movies'] == historical['movies']
        assert lossless_end['retained_files'] == historical['retained_files']
        previous_archives = {a['name']: a for a in historical['archives']}
        receipts = {a['name']: a for a in lossless['archives']}
        assert set(previous_archives) == set(receipts) == {a['name'] for a in validation['archives']}
        for archive in lossless_end['archives']:
            receipt = receipts[archive['name']]
            assert receipt['original_sha256'] == previous_archives[archive['name']]['sha256']
            assert receipt['before_bytes'] == previous_archives[archive['name']]['output_bytes']
            assert receipt['sha256'] == archive['sha256']
            assert receipt['after_bytes'] == archive['output_bytes']
            assert receipt['entries_verified'] == archive['entries']
            assert receipt['saved_bytes'] == receipt['before_bytes'] - receipt['after_bytes']
            assert all(receipt[key] for key in ('rgba_pixels_exact', 'jpeg_bytes_unchanged',
                       'labels_indices_logical_coordinates_preserved', 'every_output_entry_hash_verified'))
            assert archive['lossless_optimization'] == {k:v for k,v in receipt.items() if k != 'name'}
    runtime = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64'
    source = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-image-source'
    assets = runtime / 'assets'
    if bilingual:
        from slim_languages import unpack_mzp
        previous_path = ROOT / 'doc/compact-assets-validation-before-bilingual.json'
        assert digest(previous_path) == bilingual['previous_validation_sha256']
        assert bilingual['complete'] and bilingual['original_save_ids_preserved']
        assert [x['id'] for x in bilingual['languages']] == [1, 2]
        assert [x['resource_slot'] for x in bilingual['languages']] == [0, 1]
        assert (assets / 'language-profile.txt').read_text(encoding='utf-8').splitlines() == ['knife-languages-v1', '1\ten\tEnglish', '2\tzc\t中文（澄空）']
        assert all(not (assets / rel).exists() for rel in bilingual['removed_runtime_files'])
        current_archives = {a['name']: a for a in validation['archives']}
        previous_archives = {a['name']: a for a in lossless_end['archives']}
        for receipt in bilingual['archives']:
            assert receipt['original_sha256'] == previous_archives[receipt['name']]['sha256']
            assert receipt['sha256'] == current_archives[receipt['name']]['sha256']
            assert receipt['stable_archive_indices'] and receipt['complete']
            assert receipt['entries_verified'] == current_archives[receipt['name']]['entries']
        script = bilingual['script']
        assert digest(assets / script['path']) == script['sha256']
        blocks = unpack_mzp((assets / script['path']).read_bytes())
        assert len(blocks) == 4
        assert [hashlib.sha256(b).hexdigest() for b in blocks] == script['selected_block_sha256']
    expected = {}
    formats = Counter()
    for archive in validation['archives']:
        path = assets / (archive['name'] + '.mrg')
        assert path.stat().st_size == archive['output_bytes']
        assert digest(path) == archive['sha256'], path
        manifest = read(source / archive['name'] / 'manifest.json')
        assert manifest['sha256'] == archive['sha256']
        assert manifest == read(assets / (archive['name'] + '.compact.json'))
        for record in manifest['records']:
            for texture in record.get('textures', [record]):
                for image in texture.get('source_files', []):
                    rel = image['path']
                    assert rel not in expected, rel
                    path = (source / rel).resolve()
                    assert path.is_relative_to(source.resolve())
                    assert path.stat().st_size == image['bytes']
                    assert digest(path) == image['sha256'], rel
                    with Image.open(path) as picture:
                        assert list(picture.size) == image['physical'], rel
                        assert 0 < picture.width <= 960 and 0 < picture.height <= 540, rel
                        assert picture.format in ('PNG', 'JPEG'), rel
                        if picture.format == 'JPEG':
                            assert not picture.info.get('progressive'), rel
                        formats[picture.format] += 1
                    expected[rel] = image['sha256']
    actual_images = {p.relative_to(source).as_posix() for p in source.rglob('*')
                     if p.is_file() and p.suffix.lower() != '.json'}
    assert actual_images == set(expected)
    assert formats == (bilingual['image_formats'] if bilingual else {'PNG': 32531, 'JPEG': 3480})
    print(f'All {len(expected)} PNG/JPEG source images retain verified hashes and <=960x540 dimensions', flush=True)
    for record in read(runtime / 'compact-audio.json') + validation['movies'] + validation['retained_files']:
        path = assets / record['path']
        assert path.stat().st_size == record.get('output_bytes', record.get('bytes'))
        assert digest(path) == record['sha256'], path
    inventory = [{'path': p.relative_to(assets).as_posix(), 'bytes': p.stat().st_size, 'sha256': digest(p)}
                 for p in sorted(assets.rglob('*')) if p.is_file()]
    assert len(inventory) == validation['after']['files']
    assert sum(r['bytes'] for r in inventory) == validation['after']['total_bytes']
    assert not any(Path(r['path']).suffix.lower() in ('.webp', '.bntx', '.nca', '.nsp') for r in inventory)
    report = dict(as_of=datetime.now().astimezone().isoformat(timespec='seconds'), complete=True,
                  profile='png-jpeg-540p-v1', original_inputs_required=False,
                  scope='Rechecks all retained asset hashes and editable image hashes/dimensions; verifies the lossless archive receipts and hash-bound historical comparison when present. Full original-input comparison is not repeated.',
                  lossless_proof_chain_verified=bool(lossless),
                  bilingual_proof_chain_verified=bool(bilingual),
                  image_formats=dict(formats), editable_image_files=len(expected), maximum_per_image=[960, 540],
                  runtime_assets=inventory, total_bytes=sum(r['bytes'] for r in inventory),
                  elapsed_seconds=round(time.monotonic() - start, 2))
    (ROOT / 'doc/compact-current-integrity.json').write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps({k:v for k,v in report.items() if k != 'runtime_assets'}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
