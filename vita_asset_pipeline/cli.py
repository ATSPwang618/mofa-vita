"""Prepare current-main MOFA assets and synchronize the next VPK build."""
import argparse
import json
import os
from pathlib import Path
import sys
from .dependencies import ensure_python,tool_path
from .switch import BASE,UPDATE,MOD,FIRMWARE,KEYS,prepare_source,sha_file


def main(default_project=None):
    project=default_project or Path(__file__).resolve().parents[1]
    parser=argparse.ArgumentParser(description=__doc__)
    for name,default in (('base',BASE),('update',UPDATE),('mod',MOD),('firmware',FIRMWARE),('keys',KEYS)):
        parser.add_argument('--'+name,type=Path,default=default)
    parser.add_argument('--work-dir',type=Path,default=project/'.work/mofa-asset-build')
    parser.add_argument('--output',type=Path,default=project/'dist/ux0/data/WitchontheHolyNight')
    parser.add_argument('--jobs',type=int,default=min(8,max(2,(os.cpu_count() or 4)-4)))
    parser.add_argument('--hactool');parser.add_argument('--ffmpeg')
    parser.add_argument('--image-backend',choices=('cpu','cuda'),default='cpu')
    parser.add_argument('--setup-cuda',action='store_true',help='install isolated pinned CUDA image accelerator')
    parser.add_argument('--setup',action='store_true');parser.add_argument('--extract-only',action='store_true')
    parser.add_argument('--verify-samples',action='store_true');parser.add_argument('--check',action='store_true')
    parser.add_argument('--sync-project',action='store_true',help='synchronize metadata from an existing validated delivery')
    parser.add_argument('--bind-project',action='store_true',help=argparse.SUPPRESS)
    parser.add_argument('--gate',type=Path);parser.add_argument('--project-dir',type=Path,default=project)
    args=parser.parse_args()
    if not 2<=args.jobs<=24:parser.error('--jobs must be 2..24')
    try:
        ensure_python()
        if args.setup_cuda:
            from .cuda_backend import setup,describe
            setup();print(json.dumps(describe(),indent=2));return 0
        from . import production_driver as current
        from .build_binding import synchronize
        work=args.work_dir.resolve();output=args.output.resolve();assets=output/'assets'
        if args.check or args.sync_project:
            result=current.validate(assets)
            if args.sync_project or args.bind_project:synchronize(assets,args.project_dir.resolve())
            print(json.dumps(result,ensure_ascii=False,indent=2));return 0
        hactool=tool_path(args.hactool,'hactool');ffmpeg=tool_path(args.ffmpeg,'ffmpeg')
        if args.setup:print(f'hactool: {hactool}\nFFmpeg: {ffmpeg}');return 0
        work.mkdir(parents=True,exist_ok=True)
        romfs,stamp=prepare_source(args.base.resolve(strict=True),args.update.resolve(strict=True),
                                  args.mod.resolve(strict=True),work,hactool,args.keys.resolve(strict=True))
        if args.extract_only:print(romfs);return 0
        config=current.prepare_config(romfs,work/'main-production',args.mod.resolve(strict=True),ffmpeg,args.jobs)
        config['image_backend']=args.image_backend
        if args.image_backend=='cuda':
            from .cuda_backend import describe
            config['image_acceleration']=describe()
            print('Image accelerator:',json.dumps(config['image_acceleration']),flush=True)
        from .switch import write_json
        write_json(work/'main-production/config.json',config)
        if args.verify_samples:
            from .current_samples import verify
            verify(config,work/'romfs-source.json',work/'samples-main');return 0
        if not args.gate or not args.gate.is_file():
            raise ValueError('Run --verify-samples, then supply samples-main/verification.json with --gate')
        gate=json.loads(args.gate.read_bytes())
        if not gate.get('passed') or gate.get('conversion_signature')!=current.signature():
            raise ValueError('Current-main differential gate missing or stale; rerun --verify-samples')
        if gate.get('romfs_source_sha256')!=sha_file(work/'romfs-source.json'):
            raise ValueError('Sample gate belongs to other sources')
        if assets.exists():raise ValueError('Complete output exists; use --check or another --output')
        from .fonts import prepare_fonts
        fonts=prepare_fonts(args.firmware.resolve(strict=True),work,hactool,args.keys.resolve(strict=True))
        source=current.build(config,fonts);result=current.finalize(source,output,config,stamp,args.gate)
        synchronize(assets,args.project_dir.resolve())
        from .delivery import write_receipt
        write_receipt(output,args.project_dir.resolve(),result,args.gate)
        print('Vita assets:',assets,flush=True);print(json.dumps(result,ensure_ascii=False,indent=2));return 0
    except (ImportError,OSError,RuntimeError,ValueError,AssertionError) as error:
        print('Conversion failed: '+str(error),file=sys.stderr);return 1
