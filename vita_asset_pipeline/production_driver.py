"""Build the current main-branch contract from fixed local inputs.

Production modules and their historical encoder recipe live inside this package.
The repository tools directory is never imported by this entry point.
"""
from hashlib import sha256
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys

from .switch import sha_file, write_json

ROOT = Path(__file__).resolve().parent
SNAPSHOT = ROOT / 'production'
TOOLS = SNAPSHOT / 'tools'
MARKER = 'vita-540p-assets.json'


def activate():
    sys.path.insert(0, str(TOOLS))
    os.environ['KNIFE_EXACT_REPLAY'] = '1'
    import replay_images as images
    if not hasattr(images,'_mofa_original_init'):
        images._mofa_original_init=images.init
        images.init=init_images
        images.cached=verified_cached
        images.publish=publish_image


def init_images(config):
    # Windows spawn imports a fresh module; install adapters in every worker.
    activate()
    import replay_images as images
    images._mofa_original_init(config)
    if config.get('image_backend')=='cuda':
        from .cuda_backend import install
        install()


def verified_cached(kind,identity):
    import replay_images as images
    from convert_story_pvrtc import read_cache
    path=images.R/kind/identity
    if not path.is_file():return None
    data=read_cache(path)
    if sha256(data).hexdigest()!=identity:
        raise ValueError('Corrupted cache '+str(path))
    return data


def publish_image(kind,data,identity):
    import replay_images as images
    from convert_story_pvrtc import publish_cache
    if sha256(data).hexdigest()!=identity:
        raise ValueError('Output identity mismatch '+identity)
    if verified_cached(kind,identity) is not None:return
    path=images.R/kind/identity
    temp=path.with_suffix('.'+str(os.getpid())+'.tmp')
    temp.write_bytes(data)
    publish_cache(temp,path)


def signature():
    digest = sha256()
    for path in sorted([*[p for p in ROOT.glob('*.py') if not p.name.startswith('test_') and p.name!='vendor_main.py'],
                        *[p for p in SNAPSHOT.rglob('*') if p.is_file() and '__pycache__' not in p.parts]]):
        digest.update(path.relative_to(ROOT).as_posix().encode())
        digest.update(path.read_bytes().replace(b'\r\n', b'\n') if path.suffix in ('.py','.rs','.json') else path.read_bytes())
    return digest.hexdigest()


def load_module(name, filename):
    spec = importlib.util.spec_from_file_location(name, TOOLS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def prepare_config(romfs, work, mod, ffmpeg, workers):
    activate()
    work.mkdir(parents=True, exist_ok=True)
    patched = work / 'patched'
    if not patched.exists():
        patched.mkdir()
        for source in romfs.rglob('*'):
            target = patched / source.relative_to(romfs)
            if source.is_dir():
                target.mkdir(exist_ok=True)
            elif source.is_file():
                try:
                    os.link(source, target)
                except OSError:
                    shutil.copyfile(source, target)
    for base in ('jallpac','jallpachd','allpacml','allpachdml','allui','parts'):
        for suffix in ('hed','nam','mrg'):
            relative = base + '.' + suffix
            if base=='parts' and suffix=='nam' and not (romfs/relative).exists():
                continue
            if sha_file(patched / relative) != sha_file(romfs / relative):
                raise ValueError('Production input cache changed: ' + relative)
    from .dependencies import RUNTIME
    sdk = Path('D:/unity/PSVITA/sdk/host_tools/bin')
    locked = json.loads((TOOLS / 'source_replay/toolchain-lock.json').read_bytes())
    config = dict(work=str(work),mod=str(mod),python_deps=str(Path(__import__('ispc_texcomp').__file__).parent.parent),
                  ffmpeg=str(ffmpeg),workers=min(8,workers),logo=None)
    for key, name in (('encoder','psp2pvrt64.exe'),('packer','psp2gxt.exe')):
        target = RUNTIME / 'bin' / 'vita-texture-tools' / name
        if not target.exists():
            source = sdk / name
            if not source.is_file():
                raise ValueError('Missing local Vita texture converter: ' + str(source))
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
        if sha_file(target) != locked['tools'][key]:
            raise ValueError('Vita texture converter differs from current main: ' + key)
        config[key] = str(target.resolve())
    if sha_file(Path(ffmpeg)) != locked['tools']['ffmpeg']:
        raise ValueError('FFmpeg differs from current main production encoder')
    import PIL, numpy, ispc_texcomp
    from importlib.metadata import version
    for name,value in (('python',sys.version.split()[0]),('pillow',PIL.__version__),
                       ('numpy',numpy.__version__),('ispc_texcomp',version('ispc_texcomp'))):
        if value != locked['versions'][name]:
            raise ValueError('Current main requires ' + name + '==' + locked['versions'][name])
    write_json(work / 'config.json', config)
    return config


def copy_direct(config, fonts):
    import replay_images as q
    from source_inputs import resolve
    root = Path(config['work']); out = root / 'rebuilt-100'
    out.mkdir(exist_ok=True)
    kit = q.load('source-contract.json.gz')
    # Resolve every established direct input except the historical custom logo.
    # The requested inputs use the movie from merged RomFS instead.
    direct = dict(kit,files={name:row for name,row in kit['files'].items() if name!='movie/logo.mp4'})
    resolved = resolve(root/'patched',Path(config['mod']),None,direct)
    for relative,(data,origin) in resolved.items():
        path = out / relative; path.parent.mkdir(parents=True,exist_ok=True); path.write_bytes(data)
    (out/'movie').mkdir(exist_ok=True)
    shutil.copyfile(root/'patched/movie/logo.mp4',out/'movie/logo.mp4')
    (out/'language-profile.txt').write_bytes((q.DATA/'language-profile.txt').read_bytes())
    import gzip
    (out/MARKER).write_bytes(gzip.decompress((q.DATA/'manifest-100.json.gz').read_bytes()))
    for name,row in kit['files'].items():
        if name.endswith('.bfttf'):
            source = fonts / name
            if sha_file(source)!=row['sha256']:
                raise ValueError('Firmware font differs: ' + name)
            shutil.copyfile(source,out/name)


def build(config, fonts):
    activate()
    import replay_images, replay_misc
    from . import resize_acceleration, parallel_textures
    root = Path(config['work'])
    state_path = root / 'pipeline-state.json'
    state = json.loads(state_path.read_bytes()) if state_path.exists() else {}
    for name,action in (
        ('images',lambda:replay_images.build(config)),
        ('misc',lambda:(copy_direct(config,fonts),replay_misc.build_ui(config),
                        replay_misc.build_audio(config),replay_misc.build_movies(config))),
        ('resize70',lambda:resize_acceleration.build(config)),
        ('mixed',lambda:parallel_textures.build(config))):
        # Each production stage validates content-addressed caches on rerun.
        if state.get(name):
            print('Completed production stage:',name,flush=True)
            continue
        print('Production stage:',name,flush=True)
        action();state[name]=True;write_json(state_path,state)
    return root / 'final-assets'


def validate(assets):
    activate()
    from generate_vita_asset_profile_540 import audio_bank_locations, generate
    from vita_image_metadata import check_story_rgba_budget
    manifest = json.loads((assets/MARKER).read_bytes())
    lock = json.loads((SNAPSHOT/'doc/evidence/59-final-assets-lock.json').read_bytes())
    actual = {p.relative_to(assets).as_posix():p for p in assets.rglob('*') if p.is_file()}
    if set(actual)!=set(lock['files']):
        raise ValueError('Physical runtime inventory differs: ' + str(set(actual)^set(lock['files'])))
    matched = []
    for name,row in lock['files'].items():
        if name in (MARKER,'movie/logo.mp4'):
            continue
        path = actual[name]
        if path.stat().st_size!=row['bytes'] or sha_file(path)!=row['sha256']:
            raise ValueError('Current main production output differs: ' + name)
        matched.append(name)
    members = audio_bank_locations(assets)
    expected = {r['path']:r for r in manifest['files']}
    for name,row in expected.items():
        if name in members:
            path,offset,size = members[name]
            with path.open('rb') as source:
                source.seek(offset);data=source.read(size)
            if size!=row['bytes'] or sha256(data).hexdigest()!=row['sha256']:
                raise ValueError('Audio bank member differs: '+name)
        else:
            path=assets/name
            if path.stat().st_size!=row['bytes'] or sha_file(path)!=row['sha256']:
                raise ValueError('Runtime manifest mismatch: '+name)
    generate(assets)
    budget = check_story_rgba_budget(lambda name:assets/name,manifest)
    inventory={name:dict(bytes=path.stat().st_size,
                        sha256=lock['files'][name]['sha256'] if name in matched else sha_file(path))
               for name,path in actual.items()}
    banks={name:dict(bytes=inventory[name]['bytes'],sha256=inventory[name]['sha256'],
                    members=sum(path.name==name for path,offset,size in members.values()))
           for name in ('voice.mrg','bgm.mrg')}
    return dict(schema='mofa-main-production-validation-v2',passed=True,
                upstream_revision=json.loads((SNAPSHOT/'upstream.json').read_bytes())['revision'],
                physical_files=len(actual),physical_bytes=sum(p.stat().st_size for p in actual.values()),
                upstream_identical_files=len(matched),upstream_identical_except=[MARKER,'movie/logo.mp4'],
                audio_members=len(members),audio_streams=sum(n.endswith('.ogg') for n in members),
                audio_banks=banks,physical_inventory=inventory,
                no_loose_stream_directory=not (assets/'stream').exists(),offline_budget=budget,
                hardware_tested=False)


def finalize(source, output, config, provenance, gate):
    activate()
    proof=json.loads(gate.read_bytes())
    if not proof.get('passed') or proof.get('conversion_signature')!=signature():
        raise ValueError('Conversion code changed after sample verification; rerun --verify-samples')
    if proof.get('romfs_source_sha256')!=sha_file(Path(config['work']).parent/'romfs-source.json'):
        raise ValueError('Delivery sample gate belongs to other sources')
    output.mkdir(parents=True,exist_ok=True)
    pending = output / '.assets.partial'
    if pending.exists() or (output/'assets').exists():
        raise ValueError('Delivery output exists; choose a fresh directory')
    shutil.copytree(source,pending)
    from .media import video
    video(Path(config['work'])/'patched/movie/logo.mp4',pending/'movie/logo.mp4',config['ffmpeg'])
    manifest = json.loads((pending/MARKER).read_bytes())
    logo = pending/'movie/logo.mp4'
    next(row for row in manifest['files'] if row['path']=='movie/logo.mp4').update(bytes=logo.stat().st_size,sha256=sha_file(logo))
    manifest['statistics'].update(files=len(manifest['files']),bytes=sum(row['bytes'] for row in manifest['files']))
    manifest['pipeline_source'] = provenance
    manifest['pipeline_build'] = dict(upstream=json.loads((SNAPSHOT/'upstream.json').read_bytes())['revision'],
        conversion_signature=signature(),differential_gate_sha256=sha_file(gate),historical_encoder_corrections=True,
        logo_policy='merged RomFS movie scaled by existing compact_media settings',hardware_tested=False)
    write_json(pending/MARKER,manifest)
    report = validate(pending)
    pending.rename(output/'assets')
    write_json(output/'asset-validation.json',report)
    return report
