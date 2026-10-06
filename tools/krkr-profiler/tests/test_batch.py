import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))

import batch
import mahoyo


def temporary_directory():
    root = Path(__file__).resolve().parents[3] / 'target' / 'krkr-profiler-tests'
    root.mkdir(parents=True, exist_ok=True)
    return tempfile.TemporaryDirectory(dir=root)


class Reports(unittest.TestCase):
    def test_resource_ranking_and_merged_coverage_keep_original_recordings(self):
        import coverage
        with temporary_directory() as name:
            root = Path(name)
            sources = []
            for index in range(2):
                source = root / str(index)
                directory = source / f'case-{index}'
                directory.mkdir(parents=True)
                case = dict(name=f'case-{index}', pages=True, expect=['PERF: complete'])
                batch.save(source/'cases.json', dict(cases=[case]))
                events = [dict(type='marker', at_ns=0, detail=f'{index}.ks : *page0|'),
                          dict(type='marker', at_ns=10, detail='PERF: story'),
                          dict(type='span', thread=1, name='image.prepare', start_ns=20,
                               duration_ns=10, detail='name=archive>image with spaces.kbct bytes=37'),
                          dict(type='marker', at_ns=40, detail='PERF: complete')]
                (directory/'events.jsonl').write_text(
                    '\n'.join(json.dumps(e) for e in events), encoding='utf-8')
                batch.save(directory/'run.json', dict(elapsed_ms=.0001, dropped_events=0, error=None))
                row = batch.analyze(directory, case, compact=True)
                batch.write_report(source, [row])
                sources.append(source)
            merged = root/'merged'
            batch.merge(merged, sources)
            manifest = json.loads((merged/'cases.json').read_text(encoding='utf-8'))
            rows = coverage.collect(merged, manifest['cases'])
            self.assertEqual([r['status'] for r in rows], ['complete', 'complete'])
            self.assertEqual([r['pages'] for r in rows], [['0.ks*page0'], ['1.ks*page0']])
            resources = json.loads((merged/'resources.json').read_text(encoding='utf-8'))
            self.assertEqual(len(resources), 1)
            self.assertEqual(resources[0]['count'], 2)
            self.assertEqual(resources[0]['encoded_bytes'], 74)

    def test_analysis_includes_gc_io_admission_and_gpu_preparation(self):
        with temporary_directory() as name:
            root = Path(name)
            names = ['gc', 'io-work', 'graphics.make_room', 'gpu.affine.gather', 'image.prepare']
            events = [dict(type='span', thread=1, name=stage, start_ns=i * 100,
                           duration_ns=10, detail='kind=script-read')
                      for i, stage in enumerate(names)]
            (root/'events.jsonl').write_text('\n'.join(json.dumps(e) for e in events), encoding='utf-8')
            batch.save(root/'run.json', dict(elapsed_ms=.001, dropped_events=0, error=None))
            result = batch.analyze(root, dict(name='whole-engine'))
            stages = result['segments'][0]['stages']
            self.assertEqual(set(stages), {*names[2:], 'gc', 'io-work kind=script-read'})

    def test_hotspot_ranking_includes_unpack_and_nested_effect_work(self):
        with temporary_directory() as name:
            root = Path(name)
            segment = dict(name='story', from_ms=1, to_ms=100, counters={},
                           copy_bytes=0, copy_calls=0, finishes=0, readbacks=0,
                           allocations=0, presents=1, stages={
                               'graphics-work command=Adjust': dict(count=1, pc_total_ms=20, pc_max_ms=20),
                               'gpu.blur.stream': dict(count=1, pc_total_ms=15, pc_max_ms=15),
                               'image.bc_unpack': dict(count=2, pc_total_ms=40, pc_max_ms=30)})
            batch.write_report(root, [dict(case='effects', segments=[segment])])
            stages = json.loads((root/'hotspots.json').read_text(encoding='utf-8'))['pc_total_ms']
            self.assertEqual([s['stage'] for s in stages],
                             ['image.bc_unpack', 'graphics-work command=Adjust', 'gpu.blur.stream'])
            self.assertIn('do not add parent', (root/'hotspots.md').read_text(encoding='utf-8'))

    def test_compact_analysis_keeps_coverage_and_detects_changed_cases(self):
        import coverage
        with temporary_directory() as name:
            root = Path(name)
            case = dict(name='compact', pages=True, expect=['PERF: complete'])
            directory = root / case['name']
            directory.mkdir()
            events = [dict(type='marker', at_ns=10, detail='5b-12.ks : *page0|'),
                      dict(type='marker', at_ns=20, detail='PERF: complete')]
            (directory/'events.jsonl').write_text(
                '\n'.join(json.dumps(e) for e in events), encoding='utf-8')
            (directory/'trace.json').write_text('{}', encoding='utf-8')
            batch.save(directory/'run.json', dict(elapsed_ms=.0001, dropped_events=0, error=None))
            result = batch.analyze(directory, case, compact=True)
            self.assertFalse((directory/'events.jsonl').exists())
            self.assertFalse((directory/'trace.json').exists())
            self.assertEqual(batch.analyze(directory, case), result)
            rows = coverage.collect(root, [case])
            self.assertEqual(rows[0]['status'], 'complete')
            self.assertEqual(rows[0]['pages'], ['5b-12.ks*page0'])
            with self.assertRaises(ValueError):
                batch.analyze(directory, {**case, 'expect': ['another marker']})

    def test_interrupted_capture_keeps_pages_without_claiming_completion(self):
        with temporary_directory() as name:
            root=Path(name)
            events=[dict(type='marker', at_ns=10, detail='wik_q-a.ks : *page16|'),
                    dict(type='counter', name='vm.work', at_ns=20, value=100)]
            (root/'events.jsonl').write_text(
                '\n'.join(json.dumps(e) for e in events)+'\n{"type"', encoding='utf-8')
            batch.save(root/'process.json', dict(exit_code=1))
            result=batch.analyze(root, dict(name='interrupted', pages=True, expect=['PERF: complete']))
            self.assertTrue(result['partial_recording'])
            self.assertEqual(result['missing'], ['PERF: complete'])
            self.assertIsNone(result['dropped_events'])
            self.assertEqual(result['segments'][-1]['name'], 'wik_q-a.ks*page16')

    def test_phase_attribution_excludes_capture_and_carries_memory_but_not_frame_samples(self):
        with temporary_directory() as name:
            root=Path(name)
            events=[
                dict(type='counter',name='memory.graphics_bytes',at_ns=1,value=100),
                dict(type='counter',name='frame.copy_bytes',at_ns=2,value=1000),
                dict(type='marker',at_ns=10,detail='PERF: menu'),
                dict(type='span',thread=1,name='capture.screenshot',start_ns=20,duration_ns=20),
                dict(type='span',thread=1,name='gl.read_pixels',start_ns=22,duration_ns=1),
                dict(type='span',thread=1,name='gl.copy_tex_sub_image',start_ns=50,duration_ns=1,detail='size=4x3'),
                dict(type='span',thread=1,name='gl.read_pixels',start_ns=51,duration_ns=1),
                dict(type='counter',name='frame.copy_bytes',at_ns=70,value=48),
                dict(type='marker',at_ns=80,detail='CONFIRMED:menu'),
            ]
            (root/'events.jsonl').write_text('\n'.join(json.dumps(e) for e in events),encoding='utf-8')
            batch.save(root/'run.json',dict(elapsed_ms=.0001,dropped_events=0,error=None))
            result=batch.analyze(root,dict(name='case',expect=['CONFIRMED:menu']))
            self.assertEqual(result['missing'],[])
            segment=result['segments'][-1]
            self.assertEqual(segment['copy_bytes'],48)
            self.assertEqual(segment['readbacks'],1)
            self.assertEqual(segment['counters']['memory.graphics_bytes']['peak'],100)
            self.assertEqual(segment['counters']['frame.copy_bytes']['p95'],48)
            batch.save(root/'process.json',dict(exit_code=1))
            self.assertIn('process failed',batch.analyze(root,dict(name='case'))['error'])

    def test_generated_cases_include_ui_and_chapter_call_return_setup(self):
        cases=list(mahoyo.cases(['5b-14'],60))
        self.assertEqual([c['name'] for c in cases],['title-ui','ingame-ui','extra-ui','story-5b-14'])
        story=cases[-1]['script']
        self.assertIn('getArchiveList();',story)
        self.assertIn('kag.process("call.ks","*archive")',story)
        self.assertTrue(all(c['expect'] for c in cases))
        with self.assertRaises(ValueError):
            mahoyo.chapter_script('"; dangerous();')


if __name__=='__main__':
    unittest.main()
