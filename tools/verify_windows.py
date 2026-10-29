"""Run the built Windows engine against the local WoH assets and keep evidence."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import time

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / 'dist/Witch-on-the-Holy-Night-Windows-x64'
QA = ROOT / '.work/qa'
BASE_QA = QA
QA_EXE = QA / 'knife-qa.exe'
LOGS = ROOT / 'doc/logs'
SHOTS = ROOT / 'doc/screenshots'
REPORT = ROOT / 'doc/verification.json'
VARIANT = 'original'


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(4 * 1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def run_case(name: str, args: list[str], *, timeout: int = 120,
             exe: Path | None = None, cwd: Path = ROOT) -> dict:
    exe = exe or QA_EXE
    command = [str(exe), *args]
    start = time.monotonic()
    env = dict(os.environ, RUST_LOG='info,wgpu=warn,naga=warn', NO_COLOR='1')
    print(f'Running {name}', flush=True)
    with (LOGS / f'{name}.log').open('wb') as output:
        process = subprocess.Popen(command, cwd=cwd, stdout=output,
                                   stderr=subprocess.STDOUT, env=env)
        (QA / 'active-case.json').write_text(json.dumps({
            'name': name, 'pid': process.pid, 'exe': str(exe)
        }, ensure_ascii=False), encoding='utf-8')
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            code = 'timeout'
    result = {'name': name, 'command': command, 'exit_code': code,
              'elapsed_seconds': round(time.monotonic() - start, 2),
              'log': str(LOGS / f'{name}.log')}
    print(f'{name}: exit={code}, {result["elapsed_seconds"]}s', flush=True)
    return result


def production_smoke() -> dict:
    """Exercise the shipped EXE and its relative ini paths from an unrelated cwd."""
    working = QA / 'production-cwd'
    working.mkdir(parents=True, exist_ok=True)
    save = QA / 'production-save/savedata000'
    command = [str(DIST / 'knife.exe'), '--save-file', str(save)]
    log = LOGS / 'production-portable.log'
    start = time.monotonic()
    # A real-time production boot includes the first-visit splash animation.
    # Wait for its actual scene transition instead of assuming a 12-second boot.
    observation_limit = 60
    first_scene_seconds = None
    with log.open('wb') as output:
        process = subprocess.Popen(command, cwd=working, stdout=output,
                                   stderr=subprocess.STDOUT,
                                   env=dict(os.environ, RUST_LOG='info,wgpu=warn,naga=warn', NO_COLOR='1'))
        while process.poll() is None and time.monotonic() - start < observation_limit:
            observed = log.read_text(encoding='utf-8', errors='replace')
            elapsed = time.monotonic() - start
            if first_scene_seconds is None and 'scene 1 ' in observed:
                first_scene_seconds = round(elapsed, 2)
            if first_scene_seconds is not None and elapsed >= 12:
                break
            time.sleep(0.25)
        remained_running = process.poll() is None
        if remained_running:
            process.kill()
        code = process.wait()
    text = log.read_text(encoding='utf-8', errors='replace')
    checks = {
        'remained_running_during_observation': remained_running,
        'loaded_executable_side_ini': 'Using ' in text and 'knife.ini' in text,
        'physical_window_960x540': 'width=960 height=540' in text,
        'graphics_adapter_initialized': 'AdapterInfo' in text,
        'opening_story_reached': 'scene 1 ' in text,
        'no_logged_error_or_panic': ' ERROR ' not in text and 'panicked at' not in text,
    }
    if VARIANT == 'compact':
        checks['actual_gpu_canvas_960x540'] = 'physical_width=960 physical_height=540' in text
    return {'name': 'production-portable', 'command': command, 'cwd': str(working),
            'exit_code_after_test_cleanup': code,
            'termination': 'Test terminated its own process after the observation window' if remained_running else 'Exited before observation completed',
            'elapsed_seconds': round(time.monotonic() - start, 2),
            'maximum_observation_seconds': observation_limit,
            'first_opening_scene_observed_seconds': first_scene_seconds,
            'log': str(log), 'checks': checks, 'passed': all(checks.values()),
            'scope': 'Startup, ini resolution, window size and opening-script logs; no desktop input or screenshot capture'}


def main() -> None:
    global DIST, QA, LOGS, SHOTS, REPORT, QA_EXE, VARIANT
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--unit-tests', action='store_true')
    parser.add_argument('--production-smoke', action='store_true')
    parser.add_argument('--variant', choices=['original', '540p', 'compact'], default='compact')
    parser.add_argument('--language', choices=['en', 'zc'], default='zc')
    parser.add_argument('--scenario', choices=['boot', 'story', 'load', 'config', 'backlog', 'title-prime', 'title', 'movie'], default='story')
    args = parser.parse_args()
    VARIANT = args.variant
    if args.variant == '540p':
        DIST = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-x64'
        QA_EXE = BASE_QA / 'knife-540p-qa.exe'
        QA = BASE_QA / '540p'
        LOGS = LOGS / '540p'
        SHOTS = SHOTS / '540p'
        REPORT = ROOT / 'doc/540p-verification.json'
    elif args.variant == 'compact':
        DIST = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64'
        QA_EXE = BASE_QA / 'knife-compact-qa.exe'
        QA = BASE_QA / 'compact'
        LOGS = LOGS / 'compact'
        SHOTS = SHOTS / 'compact'
        REPORT = ROOT / 'doc/compact-verification.json'
        if args.language == 'en':
            QA = BASE_QA / 'compact-en'
            LOGS = ROOT / 'doc/logs/compact-en'
            SHOTS = ROOT / 'doc/screenshots/compact-en'
            REPORT = ROOT / 'doc/compact-en-verification.json'
    for directory in [QA, LOGS, SHOTS]:
        directory.mkdir(parents=True, exist_ok=True)
    data = json.loads(REPORT.read_text(encoding='utf-8')) if REPORT.exists() else {'cases': []}
    exe = DIST / 'knife.exe'
    raw = exe.read_bytes()
    pe = struct.unpack_from('<I', raw, 0x3c)[0]
    assert raw[:2] == b'MZ' and raw[pe:pe + 4] == b'PE\0\0'
    machine = struct.unpack_from('<H', raw, pe + 4)[0]
    magic = struct.unpack_from('<H', raw, pe + 24)[0]
    assert (machine, magic) == (0x8664, 0x20b), (machine, magic)
    data['production_executable'] = {'path': str(exe), 'bytes': len(raw),
        'sha256': sha256(exe), 'machine': hex(machine), 'optional_header_magic': hex(magic)}
    if args.production_smoke:
        result = production_smoke()
        data['cases'].append(result)
        REPORT.write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
        print(json.dumps(result, ensure_ascii=True, indent=2))
        if not result['passed']:
            raise SystemExit('Failed: production-portable')
        return
    if args.unit_tests:
        tests = json.loads((BASE_QA / 'tests/manifest.json').read_text())
        for test in tests:
            result = run_case('unit-' + test['target'], ['--nocapture'],
                              exe=BASE_QA / 'tests' / test['exe'])
            data['cases'].append(result)
            REPORT.write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
            if result['exit_code'] != 0:
                raise SystemExit(f'Failed: {result["name"]}')
        return
    if args.scenario in ['title-prime', 'title']:
        save = QA / 'title-save/savedata000'
        if args.scenario == 'title' and not save.exists():
            raise SystemExit('Run --scenario title-prime first to prepare the reached-title test profile')
    else:
        save = QA / ('boot-save/savedata000' if args.scenario == 'boot' else 'scenario-save/savedata000')
    options = ['--assets-dir', str(DIST / 'assets'), '--language', args.language,
               '--save-file', str(save), '--shots', str(SHOTS / args.scenario),
               '--shot-from', '1000', '--shot-every', '2000', '--exit-after', '10000']
    if args.variant in ('540p', 'compact'):
        options.extend(['--window-size', '960x540'])
    extra = {
        'boot': ['--boot', '--exit-after', '30000'],
        'story': ['--scene', '1', '--save-at', '6000', '--save-slot', '0'],
        'load': ['--load-slot', '0'],
        # WoH opens these through its own pad routing; --screen is the Tsukihime UI path.
        'config': ['--scene', '1', '--keys', '3000:y,5000:down,5500:down,6000:a', '--exit-after', '14000'],
        'backlog': ['--scene', '1', '--keys', '2500:a,3500:a,4500:x'],
        # A fresh WoH profile plays its first splash and enters the story. The title-menu case
        # needs G010 set, so prepare it explicitly in an isolated QA save, never a player save.
        'title-prime': ['--scene', '1', '--var', 'G010=1', '--save-at', '1500', '--save-slot', '0', '--exit-after', '3000'],
        'title': ['--title', '--skip-intro', '--exit-after', '15000'],
        'movie': ['--scene', '1', '--play-movie', f'opening_{args.language}', '--exit-after', '14000'],
    }[args.scenario]
    # Replace options with explicit per-scenario values, keeping clap's no-duplicates rule.
    for key in ['--exit-after']:
        if key in extra:
            i = options.index(key)
            del options[i:i + 2]
    options.extend(extra)
    before = sha256(save) if save.exists() else None
    result = run_case(args.scenario, options, timeout=180)
    result['language'] = args.language
    result['executable_sha256'] = sha256(QA_EXE)
    result['save_sha256_before'] = before
    result['save_bytes'] = save.stat().st_size if save.exists() else None
    result['save_sha256_after'] = sha256(save) if save.exists() else None
    result['screenshots'] = [str(p) for p in sorted((SHOTS / args.scenario).glob('*.png'))]
    log_text = Path(result['log']).read_text(encoding='utf-8', errors='replace')
    result['no_logged_error_or_panic'] = ' ERROR ' not in log_text and 'panicked at' not in log_text
    from PIL import Image
    frames = []
    for path in result['screenshots']:
        with Image.open(path) as image:
            extrema = image.convert('RGB').getextrema()
            frames.append({'size': list(image.size), 'extrema': extrema,
                           'visible': any(high - low >= 8 for low, high in extrema)})
    result['frames_with_visible_content'] = sum(f['visible'] for f in frames)
    if frames:
        result['last_frame_size'] = frames[-1]['size']
        result['last_frame_rgb_extrema'] = frames[-1]['extrema']
        result['last_frame_has_visible_content'] = frames[-1]['visible']
    # The first-visit opening contains intentional solid-color transitions.
    # Check the captured sequence and scene transition, not its arbitrary final instant.
    if args.scenario == 'boot':
        result['opening_story_reached'] = 'scene 1 ' in log_text
        result['opening_sequence_has_visible_content'] = result['frames_with_visible_content'] >= 3
        result['frame_check_scope'] = 'Opening sequence contains visible frames and reaches scene 1; a solid-color final transition is allowed'
    if args.variant == 'compact':
        result['actual_gpu_canvas_960x540'] = 'physical_width=960 physical_height=540' in log_text
        result['all_frames_960x540'] = bool(frames) and all(f['size'] == [960, 540] for f in frames)
    if args.scenario == 'movie':
        result['native_media_foundation'] = 'Media Foundation' in log_text and 'trying ffmpeg' not in log_text
        result['movie_frame_uploaded'] = 'Movie first frame uploaded' in log_text
    if args.scenario == 'title-prime':
        result['fixture_note'] = 'Isolated QA profile; debug variable G010=1 enables the ordinary title menu'
    visible = (result.get('opening_story_reached') and result.get('opening_sequence_has_visible_content')
               if args.scenario == 'boot' else result.get('last_frame_has_visible_content'))
    result['passed'] = bool(result['exit_code'] == 0 and frames and visible
        and result['no_logged_error_or_panic']
        and (args.variant != 'compact' or (result.get('actual_gpu_canvas_960x540') and result.get('all_frames_960x540')))
        and (args.scenario != 'movie' or (result.get('native_media_foundation') and result.get('movie_frame_uploaded'))))
    data['cases'].append(result)
    REPORT.write_text(json.dumps(data, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    if not result['passed']:
        raise SystemExit(f'Failed: {args.scenario}')


if __name__ == '__main__':
    main()
