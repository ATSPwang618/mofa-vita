"""Compare existing capture receipts without rerunning or changing the game."""
import argparse
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path

from vita_capture_check import inspect_captures


ROOT = Path(__file__).resolve().parents[1]


def identity(path):
    path = Path(path)
    data = path.read_bytes()
    return dict(path=str(path), bytes=len(data), sha256=hashlib.sha256(data).hexdigest())


def verify(record):
    current = identity(record['path'])
    if any(current[key] != record[key] for key in ('bytes', 'sha256')):
        raise ValueError(f'Artifact changed: {record["path"]}')
    return current


def inspect_receipt(path):
    source = json.loads(path.read_text(encoding='utf-8'))
    log = verify(source['host_log'])
    text = Path(log['path']).read_text(encoding='utf-8', errors='replace')
    if '[renderer::gl::create]: GL_VERSION' in text:
        backend = 'OPENGL'
    elif '[renderer::vulkan::VKState::create]: Vulkan device:' in text:
        backend = 'VULKAN'
    else:
        backend = 'unknown'
    evidence = [dict(line=index, text=line) for index, line in enumerate(text.splitlines(), 1)
                if any(term in line for term in ('[main]: Vita3K', 'Created OpenGL',
                    'GL_VERSION =', 'Vulkan device:', 'Running -> Stopping',
                    'Exception EXCEPTION_', 'Failed to initialise color surface'))]
    images = inspect_captures([dict(label=frame['label'], path=frame['artifact']['path'],
                                   expected_sha256=frame['artifact']['sha256'])
                               for frame in source['captures']])
    same_fields = ('vpk', 'emulator', 'debugger', 'asset_manifest')
    verified_inputs = {key: verify(source[key]) for key in same_fields}
    return dict(receipt=identity(path), host_backend_observed=backend,
                inputs=verified_inputs, log=log, log_observations=evidence,
                capture_check=images, original_runtime_verified=source['runtime_verified'],
                control_sequence_completed=source.get('control_sequence_completed',
                                                     source.get('visible_run_completed', False)),
                original_visible_run_completed_was_only_process_state='visible_run_completed' in source,
                error=source.get('error'), exit=source.get('exit'),
                visual_correctness_verified=False, hardware_verified=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('receipts', type=Path, nargs='+')
    args = parser.parse_args()
    runs = [inspect_receipt(path.resolve()) for path in args.receipts]
    same = {key: len({row['inputs'][key]['sha256'] for row in runs}) == 1
            for key in ('vpk', 'emulator', 'debugger', 'asset_manifest')}
    maps = []
    for relative in ('.work/vita/game-release/knife.map',
                     '.work/vita/game-investigation/270p-release-before-asset-guard/'
                     '.work_vita_game-release/knife.map'):
        path = ROOT / relative
        text = path.read_text(encoding='utf-8')
        required = ('sceGxmInitialize', 'sceGxmBeginScene', 'sceGxmDraw', 'sceDisplaySetFrameBuf')
        maps.append(dict(artifact=identity(path),
                         gxm_symbols_present={name: name in text for name in required},
                         unexpected_backend_markers={name: name in text for name in
                             ('libvitaGL', 'vglInit', 'vkCreateInstance', 'vkCreateDevice', 'libvulkan')}))
    report = dict(schema='knife-vita-render-backend-audit-v1',
                  generated_at=datetime.now(timezone.utc).isoformat(),
                  tool=identity(Path(__file__)), image_check_tool=identity(ROOT / 'tools/vita_capture_check.py'),
                  guest_renderer='direct GXM', all_selected_runs_same_inputs=same,
                  runs=runs, link_maps=maps,
                  dist_vpk=identity(ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-270p-Vita/knife-vita-270p.vpk'),
                  root_cause_established=False, hardware_verified=False,
                  limitations=[
                      'Controlled observations apply to the recorded fork, GPU/driver and inputs only.',
                      'The compared .work VPK includes a newer startup resource guard; dist is a separate older VPK.',
                      'Nonblack pixels do not prove correct shaders, layout, blending or complete menus.',
                      'The historical visible_run_completed field tested process state, not RGB content.',
                      'OpenGL black frames and stop-time host crash are separate observed symptoms; causality is unknown.',
                      'No Vita hardware run or hardware performance conclusion is supplied.'])
    with args.output.open('x', encoding='utf-8') as handle:
        json.dump(report, handle, ensure_ascii=False, indent=2)
        handle.write('\n')
    print(json.dumps(dict(output=str(args.output), same_inputs=same,
        runs=[dict(backend=row['host_backend_observed'], nonblack=row['capture_check']['required_views_nonblack'],
                   exit=row['exit']) for row in runs]), ensure_ascii=True, indent=2))


if __name__ == '__main__':
    main()
