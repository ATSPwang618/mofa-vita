"""Prewarm the locked PVRTC selection at texture granularity.

The historical SDK encoder, quality gates, source/output hashes and final
archive assembler remain authoritative. Only task scheduling changes.
"""
from concurrent.futures import ProcessPoolExecutor, wait, FIRST_COMPLETED
import json
from pathlib import Path
import time

from .production_driver import activate
from .switch import write_json


def init_tiles(config, base, cache=None):
    from .dependencies import ensure_python
    ensure_python()
    activate()
    import replay_final_textures as replay
    replay.init(config, base)
    if cache is not None:
        import convert_story_pvrtc as converter
        converter.work = Path(cache)
        converter.work.mkdir(parents=True, exist_ok=True)


def encode_tile(index):
    import replay_final_textures as replay
    block, record = replay.encode(index)
    # replay.encode verifies both identities, including on every cache hit.
    return index, replay.p.sha(block[0]), record['reason']


def prewarm(config, base, indices=None, cache=None, workers=None):
    activate()
    from replay_images import load
    selected = load('pvrtc-selection.json.gz').get(base, {})
    indices = sorted(map(int, selected)) if indices is None else list(indices)
    if any(str(i) not in selected for i in indices):
        raise ValueError('Only the fixed upstream PVRTC selection is eligible')
    workers = workers or config.get('workers', 8)
    started = time.perf_counter()
    completed = 0
    identities = {}
    iterator = iter(indices)
    with ProcessPoolExecutor(max_workers=workers, initializer=init_tiles,
                             initargs=(config, base, cache)) as pool:
        pending = set()
        def fill():
            while len(pending) < workers * 2:
                index = next(iterator, None)
                if index is None:
                    break
                pending.add(pool.submit(encode_tile, index))
        fill()
        while pending:
            done, pending = wait(pending, return_when=FIRST_COMPLETED)
            for future in done:
                index, identity, reason = future.result()
                identities[str(index)] = identity
                completed += 1
                if completed % 250 == 0:
                    print('PVRTC tile cache', base, completed, '/', len(indices), flush=True)
            fill()
    report = dict(archive=base, workers=workers, textures=completed,
                  seconds=time.perf_counter()-started, exact=True,
                  stored_sha256=identities)
    print('PVRTC tile cache verified', base, completed,
          'seconds', round(report['seconds'], 2), flush=True)
    return report


def build(config):
    activate()
    import replay_final_textures as replay
    from replay_images import load
    manifest = load('manifest-70.json.gz')
    reports = []
    for base in manifest['story_gxt_variants']['original_entries']:
        reports.append(prewarm(config, base))
        replay.build(config, only=base)
    # The original assembler verifies completed archive hashes, then copies
    # non-story files and publishes its original final manifest.
    replay.build(config)
    write_json(Path(config['work'])/'parallel-pvrtc.json', reports)


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True, type=Path)
    parser.add_argument('--archive', required=True)
    parser.add_argument('--workers', type=int, default=8)
    args = parser.parse_args()
    prewarm(json.loads(args.config.read_bytes()), args.archive, workers=args.workers)
