"""Local, checkpointed NSP -> 70% mixed texture pack with explicit private inputs.

Usage after configuring your own keys/tools/recovery kit:
  python tools/rebuild_from_nsp.py --nsp GAME.nsp --config LOCAL.json --allow-recovery-candidate

This does not pretend translated scripts, firmware fonts or project artwork are
contained in the retail NSP. They must be present in the declared recovery kit.
No network upload, key discovery, in-place resource update or source modification.
"""
import argparse
import atexit
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import zipfile
import uuid

from recovery_kit import file_sha
from asset_reproducibility import recovery_scope, python_sources

ROOT=Path(__file__).resolve().parents[1]
PROGRAM='c134dd53826646afa027df1833254e0f.nca'
PROGRAM_SHA='c134dd53826646afa027df1833254e0f999e6a6fc1c64bcf470944219f43da86'
NSP_BYTES=21380558096
UPDATE_PROGRAM='db83e0cdb33b5eb5a0868f3ddf15204b.nca'
UPDATE_SHA='db83e0cdb33b5eb5a0868f3ddf15204b2173986b4402dad5bf65373731ab5ed4'
def write_json(path,data):
    temp=path.with_suffix(path.suffix+'.tmp')
    temp.write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n','utf8')
    os.replace(temp,path)


def member(path,name):
    """Read only bounded PFS0 metadata. Return the requested member's range."""
    with path.open('rb') as f:
        header=f.read(16)
        if len(header)!=16:raise ValueError('Truncated NSP')
        magic,count,size,_=struct.unpack('<4sIII',header)
        if magic!=b'PFS0' or not 0<count<=4096 or size>1024*1024:
            raise ValueError('Unsupported NSP directory')
        table=f.read(count*24);strings=f.read(size);start=f.tell()
    if len(table)!=count*24 or len(strings)!=size:raise ValueError('Truncated PFS0')
    found=[]
    for off,length,n,_ in struct.iter_unpack('<QQII',table):
        if n>=size or b'\0' not in strings[n:]:raise ValueError('Invalid PFS0 name')
        filename=strings[n:].split(b'\0',1)[0].decode('ascii')
        if off+length>path.stat().st_size-start:raise ValueError('PFS0 member outside file')
        if filename==name:found.append((start+off,length))
    if len(found)!=1:raise ValueError('Expected exact supported BASE program NCA')
    return found[0]


def tree_receipt(root):
    return {p.relative_to(root).as_posix():dict(bytes=p.stat().st_size,sha256=file_sha(p))
            for p in sorted(root.rglob('*')) if p.is_file()}


def check_tree(root,receipt):
    actual={p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file()}
    if actual!=set(receipt):raise ValueError('Checkpoint file set changed: '+str(root))
    for rel,row in receipt.items():
        p=root/rel
        if p.stat().st_size!=row['bytes'] or file_sha(p)!=row['sha256']:
            raise ValueError('Checkpoint content changed: '+rel)


def free_space(path):
    while not path.exists():path=path.parent
    return shutil.disk_usage(path).free


def input_identity(config):
    sys.path.insert(0,str(config['python_deps']))
    import PIL, numpy, ispc_texcomp
    encoders={n:file_sha(Path(config[n])) for n in ('nstool','encoder','packer')}
    encoders['ffmpeg']=file_sha(Path(shutil.which(config.get('ffmpeg','ffmpeg'))))
    return dict(schema='knife-nsp-rebuild-job-v1',
        kit_sha256=file_sha(Path(config['kit'])/'kit.json'),program_sha256=PROGRAM_SHA,
        upstream=dict(update=file_sha(Path(config['update'])),
                      logo=file_sha(Path(config['logo'])),
                      mod=tree_receipt(Path(config['mod'])),
                      audio_contract=file_sha(Path(config['audio_contract']))),
        generators=python_sources(ROOT/'tools'),encoders=encoders,
        invocation=dict(workers=config.get('workers',4), build_tag=config.get('build_tag'),
                        wsl=config.get('wsl'), recipe_policy='recovery-candidate-only'),
        python_executable_sha256=file_sha(Path(sys.executable)),
        native_modules={str(p.resolve()):file_sha(p) for module in (PIL,numpy,ispc_texcomp)
                        for p in Path(module.__file__).parent.rglob('*')
                        if p.is_file() and p.suffix.lower() in ('.pyd','.dll','.so')},
        host=dict(python=sys.version.split()[0],pillow=PIL.__version__,numpy=numpy.__version__,
                  ispc_texcomp=ispc_texcomp.__version__),
        output=str(Path(config['output']).resolve()),work=str(Path(config['work']).resolve()))


def run_tool(name,args,logdir,*,root=ROOT,env=None):
    print('RUN',name,flush=True)
    command=[sys.executable,str(root/'tools'/name),*map(str,args)]
    script_hash=file_sha(root/'tools'/name)
    execution=dict(command=command,cwd=str(root),script_sha256=script_hash,status='started')
    receipt_path=logdir/(name+'.execution.json')
    write_json(receipt_path,execution)
    with (logdir/(name+'.log')).open('wb') as log:
        process=subprocess.Popen(command,
                                 cwd=root,stdout=log,stderr=subprocess.STDOUT,env=env)
        while True:
            try:
                code=process.wait(timeout=30)
                break
            except subprocess.TimeoutExpired:
                with (logdir/(name+'.log')).open('rb') as tail:
                    tail.seek(max(0,(logdir/(name+'.log')).stat().st_size-800))
                    lines=tail.read().decode('utf8','replace').splitlines()
                print(name+': '+(lines[-1] if lines else 'running'),flush=True)
    execution.update(returncode=code,status='completed' if code==0 else 'failed')
    write_json(receipt_path,execution)
    if file_sha(root/'tools'/name)!=script_hash:
        execution['status']='source-changed-during-execution';write_json(receipt_path,execution)
        raise RuntimeError('Generator changed during execution: '+name)
    if code:
        raise RuntimeError(f'{name} failed; see {logdir/(name+".log")}')


def safe_owned_remove(path,work):
    """Delete only a named stage inside the marked job, never inputs/repository."""
    path=path.resolve();work=work.resolve()
    if not (work/'.rebuild-owner.json').is_file() or path.parent!=work:
        raise ValueError('Refusing unowned cleanup: '+str(path))
    if path.name not in ('program.nca','update.nca','romfs','rebuilt-100','resized-70','pvrtc-cache','engine-copy','arm-cache'):
        raise ValueError('Refusing unrecognized cleanup target')
    candidates=[path,*path.rglob('*')] if path.is_dir() else [path]
    for item in candidates:
        if item.is_symlink() or (hasattr(item,'is_junction') and item.is_junction()):
            raise ValueError('Reparse point in cleanup target')
    if path.is_dir():shutil.rmtree(path)
    elif path.exists():path.unlink()


def compile_pair(final,work,config,logs):
    """Build in a small copy of current Git-visible source; never edit live source."""
    stage=work/'engine-copy'
    if stage.exists():safe_owned_remove(stage,work)
    stage.mkdir()
    names=subprocess.check_output(['git','ls-files','-z','--cached','--others','--exclude-standard'],cwd=ROOT)
    for raw in names.split(b'\0'):
        if not raw:continue
        rel=Path(os.fsdecode(raw));source=ROOT/rel
        if not source.is_file():continue
        if rel.is_absolute() or '..' in rel.parts:raise ValueError('Unsafe Git path')
        target=stage/rel;target.parent.mkdir(parents=True,exist_ok=True);shutil.copyfile(source,target)
    # The final pack itself is the source for byte-verified coverage locators.
    run_tool('rebind_story_coverage.py',['--source',final,'--candidate',final],logs,root=stage)
    run_tool('generate_vita_asset_profile_540.py',['--assets',final],logs,root=stage)
    expected=json.loads((stage/'doc/evidence/48-large-image-integration.json').read_bytes())
    env=os.environ.copy()
    env.update(KNIFE_BUILD_TAG=config['build_tag'],KNIFE_PERF_CONSOLE='1',KNIFE_VITA_QUIET='0',
               KNIFE_VITA_OPTIMIZATIONS=expected['optimization_profile'],KNIFE_VITA_OFFLINE='1',
               KNIFE_VITA_CACHE=str(work/'arm-cache'))
    command=['game','release']
    if config.get('wsl'):command+=['--wsl',config['wsl']]
    run_tool('build-vita.py',command,logs,root=stage,env=env)
    receipt=json.loads((stage/'.work/vita/game-release-540p/vita-game-release-540p-build.json').read_bytes())
    actual=receipt['optimization_profile']
    if (actual['default']!=expected['optimization_profile']
        or actual['default_mask']!=int(expected['optimization_mask'],16)
        or sorted(actual['default_features'])!=sorted(expected['optimization_features'])
        or receipt['build_tag']!=config['build_tag']
        or not receipt['performance_console_default'] or receipt['quiet_build']):
        raise ValueError('Paired build differs from repository R10/logging contract')
    built=stage/'release'
    with zipfile.ZipFile(built/'mofa-vita.vpk') as z:
        if z.read('eboot.bin')!=(built/'eboot.bin').read_bytes():raise ValueError('EBOOT/VPK mismatch')
    for name in ('eboot.bin','mofa-vita.vpk'):shutil.copyfile(built/name,final.parent/name)
    for name in ('knife.elf','knife.map','vita-game-release-540p-build.json'):
        path=stage/'.work/vita/game-release-540p'/name
        if path.is_file():shutil.copyfile(path,final.parent/name)
    evidence=final.parent/'rebuild-evidence';evidence.mkdir(exist_ok=True)
    for rel in ('knife/knife-platform-vita/src/asset_profile_540p.rs',
                'knife/knife-render/src/vita_live_alpha_data.rs',
                'knife/knife-render/src/vita_sparse_coverage.rs'):
        source=stage/rel;shutil.copyfile(source,evidence/source.name)


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--nsp',type=Path,required=True)
    p.add_argument('--config',type=Path,default=ROOT/'.work/nsp-rebuild-config.json')
    p.add_argument('--check',action='store_true',help='Read-only inputs, dependencies and free space check')
    p.add_argument('--assets-only',action='store_true',help='Do not build paired VPK; existing EBOOT will reject new manifest')
    p.add_argument('--keep-intermediates',action='store_true')
    p.add_argument('--allow-recovery-candidate',action='store_true',
                   help='Explicitly accept independent recovery output, never a reproducible release')
    a=p.parse_args();config=json.loads(a.config.read_bytes())
    for key in ('kit','keys','nstool','encoder','packer','python_deps','work','output',
                'update','mod','logo','audio_contract'):
        if key not in config:raise ValueError('Missing explicit source input: '+key)
        config[key]=Path(config[key]).resolve()
    work,output,kit=config['work'],config['output'],config['kit']
    if work==output or work.is_relative_to(output) or output.is_relative_to(work):
        raise ValueError('Work and final output must be separate directories')
    if kit.is_relative_to(work) or a.nsp.resolve().is_relative_to(work):
        raise ValueError('Inputs must not be inside temporary stage directories')
    contract=json.loads((kit/'kit.json').read_bytes())
    scope=recovery_scope(contract,a.allow_recovery_candidate)
    if a.nsp.stat().st_size!=NSP_BYTES:raise ValueError('This recipe requires the exact supported BASE NSP')
    start,length=member(a.nsp,PROGRAM)
    update_start,update_length=member(config['update'],UPDATE_PROGRAM)
    from source_inputs import checked
    checked(config['logo'].read_bytes(),contract['files']['movie/logo.mp4'],'custom logo')
    for key in ('keys','nstool','encoder','packer'):
        if not config[key].is_file():raise ValueError('Missing local input/tool: '+key)
    if not contract.get('contains_game_data'):raise ValueError('Recovery kit must disclose private game data')
    if contract.get('grade_source_sha256')!=file_sha(ROOT/'knife/knife/src/grade.rs'):
        raise ValueError('Production color-grade code changed or kit provenance missing; regenerate kit')
    for name,key in [('recipe.json','recipe_sha256'),('grade-luts.bin','luts_sha256')]:
        if file_sha(kit/name)!=contract[key]:raise ValueError('Kit identity changed: '+name)
    for rel,row in contract['files'].items():
        if row['source']=='private-supplement' and file_sha(kit/'private'/rel)!=row['sha256']:
            raise ValueError('Private input changed: '+rel)
    proof_hashes={r['sha256'] for rows in contract['proofs'].values() for r in rows}
    proof_hashes.update(r['private_sha256'] for r in contract['previews'] if 'private_sha256' in r)
    for digest in proof_hashes:
        if file_sha(kit/'private/proofs'/digest)!=digest:raise ValueError('Private proof/capture changed')
    if not shutil.which(config.get('ffmpeg','ffmpeg')):raise ValueError('ffmpeg missing')
    sys.path.insert(0,str(config['python_deps']))
    import PIL, numpy, ispc_texcomp  # dependency check before disk-intensive extraction
    free=free_space(work)
    print('Private supplement:',contract['private_bytes'],'bytes. Not pure NSP-only.',flush=True)
    print('Free space:',round(free/1024**3,2),'GiB; fresh run requires 48 GiB scratch headroom.',flush=True)
    if a.check:
        if not a.assets_only:
            command=[sys.executable,str(ROOT/'tools/build-vita.py'),'game','release','--check']
            if config.get('wsl'):command+=['--wsl',config['wsl']]
            subprocess.run(command,check=True)
        return
    work.mkdir(parents=True,exist_ok=True)
    owner=input_identity(config)
    ownerpath=work/'.rebuild-owner.json'
    if ownerpath.exists():
        if json.loads(ownerpath.read_bytes())!=owner:raise ValueError('Job inputs changed; select a new work directory')
    else:
        if any(work.iterdir()):raise ValueError('Fresh work directory must be empty')
        if output.exists():raise ValueError('Final output already exists; choose a new directory')
        if free<48*1024**3:raise ValueError('Insufficient scratch space for fresh extraction')
        if free_space(output)<8*1024**3:raise ValueError('Insufficient final output disk space')
        write_json(ownerpath,owner)
    lock=work/'build.lock'
    token=json.dumps(dict(pid=os.getpid(),token=uuid.uuid4().hex)).encode('ascii')
    try:
        with lock.open('xb') as f:f.write(token)
    except FileExistsError:
        raise RuntimeError(f'A rebuild lock exists at {lock}; inspect that process before retrying')
    def unlock():
        if lock.is_file() and lock.read_bytes()==token:lock.unlink()
    atexit.register(unlock)
    logs=work/'logs';logs.mkdir(exist_ok=True)
    statepath=work/'state.json';state=json.loads(statepath.read_bytes()) if statepath.exists() else {}
    romfs=work/'romfs';full=work/'rebuilt-100/assets';small=work/'resized-70/assets';final=output/'assets'
    if state.get('intermediates_cleaned'):
        check_tree(final,state['final'])
        receipt=json.loads((output/'nsp-rebuild-receipt.json').read_bytes())
        for name,digest in receipt.get('programs',{}).items():
            if file_sha(output/name)!=digest:raise ValueError('Completed program changed: '+name)
        if not a.assets_only and not receipt['paired_executable']:
            compile_pair(final,work,config,logs)
            receipt['paired_executable']=True
            receipt['programs']={n:file_sha(output/n) for n in ('eboot.bin','mofa-vita.vpk')}
            write_json(output/'nsp-rebuild-receipt.json',receipt)
            if not a.keep_intermediates:
                for name in ('engine-copy','arm-cache'):safe_owned_remove(work/name,work)
        print('Previously completed pack verified:',output,flush=True)
        return
    def done(stage,root):
        state[stage]=tree_receipt(root);write_json(statepath,state)
    if 'romfs' not in state:
        if romfs.exists():raise ValueError('Incomplete extraction retained; inspect logs before removing its owned directory')
        nca=work/'program.nca'
        if nca.exists():
            if file_sha(nca)!=PROGRAM_SHA:raise ValueError('Incomplete or wrong NCA; retained for inspection')
        else:
            digest=hashlib.sha256()
            with a.nsp.open('rb') as source,nca.open('xb') as target:
                source.seek(start);left=length
                while left:
                    data=source.read(min(4*1024*1024,left))
                    if not data:raise ValueError('Truncated NSP member')
                    target.write(data);digest.update(data);left-=len(data)
            if digest.hexdigest()!=PROGRAM_SHA:raise ValueError('Program NCA hash differs; no extraction attempted')
        update_nca=work/'update.nca'
        if not update_nca.exists():
            with config['update'].open('rb') as source,update_nca.open('xb') as target:
                source.seek(update_start);left=update_length
                while left:
                    block=source.read(min(4*1024*1024,left))
                    if not block:raise ValueError('Truncated update NSP')
                    target.write(block);left-=len(block)
        if file_sha(update_nca)!=UPDATE_SHA:raise ValueError('Update NCA differs from supported 1.02')
        with (logs/'extract.log').open('wb') as log:
            subprocess.run([str(config['nstool']),'-k',str(config['keys']),'--basenca',str(nca),
                            '-x','/1',str(romfs),str(update_nca)],
                           stdout=log,stderr=subprocess.STDOUT,check=True)
        # Verify patched script/shaders and explicit MOD/logo before expensive conversion.
        from source_inputs import resolve
        resolve(romfs,config['mod'],config['logo'],contract)
        done('romfs',romfs);safe_owned_remove(nca,work);safe_owned_remove(update_nca,work)
    else:check_tree(romfs,state['romfs'])
    workers=str(config.get('workers',4))
    if 'full' not in state:
        full.parent.mkdir(exist_ok=True)
        run_tool('build_recovered_runtime.py',['--romfs',romfs,'--kit',kit,'--output',full,'--workers',workers,
                 '--python-deps',config['python_deps'],'--ffmpeg',config.get('ffmpeg','ffmpeg'),
                 '--mod',config['mod'],'--logo',config['logo'],'--audio-contract',config['audio_contract'],
                 '--allow-recovery-candidate'],logs)
        run_tool('rebuild_bgm_lead.py',['--assets',full],logs)
        run_tool('verify_recovered_runtime.py',['--assets',full,'--kit',kit],logs)
        # Run production metadata, RGBA budget and file checks without changing the live profile.
        from generate_vita_asset_profile_540 import generate
        (full.parent/'asset_profile_540p.rs').write_text(generate(full),'utf8')
        done('full',full)
    else:check_tree(full,state['full'])
    if 'small' not in state:
        run_tool('resize_story_variants.py',['--source',full,'--output',small,'--scale','0.7','--workers',workers,
                                          '--python-deps',config['python_deps']],logs)
        run_tool('verify_resized_story_pack.py',['--source',full,'--candidate',small],logs)
        done('small',small)
    else:check_tree(small,state['small'])
    if 'final' not in state:
        run_tool('convert_story_pvrtc.py',['--source',small,'--output',final,'--cache',work/'pvrtc-cache',
              '--encoder',config['encoder'],'--packer',config['packer'],'--workers',workers],logs)
        run_tool('verify_story_pvrtc.py',['--source',small,'--candidate',final],logs)
        done('final',final)
    else:check_tree(final,state['final'])
    if not a.assets_only:compile_pair(final,work,config,logs)
    evidence=output/'rebuild-evidence';evidence.mkdir(exist_ok=True)
    for path in (full.parent/'rebuild-report.json',full.parent/'recovered-verification.json',
                 full.parent/'jpeg-repair.json',small.parent/'resize-report.json'):
        if path.is_file():shutil.copyfile(path,evidence/path.name)
    shutil.copyfile(small.parent/'verification.json',evidence/'resize-verification.json')
    write_json(evidence/'inputs.json',owner)
    if input_identity(config)!=owner:
        raise RuntimeError('Recipe/tools/config changed during build; refusing completion receipt and cleanup')
    for path in logs.glob('*.execution.json'):
        shutil.copyfile(path,evidence/path.name)
    write_json(evidence/'reproduction-scope.json',scope)
    write_json(output/'nsp-rebuild-receipt.json',dict(schema='knife-nsp-rebuild-v1',program_sha256=PROGRAM_SHA,
        reproduction=scope,
        kit_sha256=owner['kit_sha256'],private_inputs_bytes=contract['private_bytes'],pure_nsp_only=False,
        reference_identical=False,manifest_sha256=file_sha(final/'vita-540p-assets.json'),
        physical_files=len(state['final']),physical_bytes=sum(r['bytes'] for r in state['final'].values()),
        hardware_tested=False,paired_executable=not a.assets_only,
        programs={n:file_sha(output/n) for n in ('eboot.bin','mofa-vita.vpk')} if not a.assets_only else {}))
    if not a.keep_intermediates:
        for name in ('romfs','rebuilt-100','resized-70','pvrtc-cache','engine-copy','arm-cache'):
            safe_owned_remove(work/name,work)
        state['intermediates_cleaned']=True;write_json(statepath,state)
    print('Completed:',output,flush=True)


if __name__=='__main__':main()
