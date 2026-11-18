"""MOFA base + BKTR update extraction, followed by a private MOD overlay."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

from .container import entries

INPUT = Path('C:/Users/26241/Desktop/switch魔夜原数据包')
BASE = INPUT / 'WITCH ON THE HOLY NIGHT v0 (010012A017F18000) (BASE).nsp'
UPDATE = INPUT / '魔夜重制版-switch版/魔法使之夜1.2升级档/1.02补丁/WITCH ON THE HOLY NIGHT v131072 (010012A017F18800) (UPD).nsp'
MOD = INPUT / '魔夜重制版-switch版/魔夜澄空汉化mod(12月27日更新)/010012A017F18000/romfs'
FIRMWARE = Path(os.environ.get('APPDATA', str(Path.home()))) / 'Ryujinx/bis/system/Contents/registered'
KEYS = Path(os.environ.get('APPDATA', str(Path.home()))) / 'Ryujinx/system/prod.keys'
PROGRAM_ID = '010012a017f18000'


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + '.writing')
    temp.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    temp.replace(path)


def sha_file(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def identity(path):
    status = path.stat()
    with path.open('rb') as source:
        first = source.read(1024**2)
        source.seek(max(0, status.st_size - 1024**2))
        last = source.read()
    return dict(path=str(path.resolve()), bytes=status.st_size,
                mtime_ns=status.st_mtime_ns, edges=hashlib.sha256(first + last).hexdigest())


def hactool(executable, keys, work, log, *arguments):
    work.mkdir(parents=True, exist_ok=True)
    command = [str(executable), '-k', str(keys), '--disablekeywarns', '--suppresskeys', *map(str, arguments)]
    with (work / log).open('w', encoding='utf-8') as dest:
        with subprocess.Popen(command, cwd=work, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                              text=True, encoding='utf-8', errors='replace') as process:
            for line in process.stdout:
                if not re.search(r'(?i)key|signature', line):
                    dest.write(line)
            if process.wait():
                raise RuntimeError(f'hactool failed: {work / log}')
    return (work / log).read_text(encoding='utf-8')


def program_nca(nsp, work, executable, keys, kind):
    stamp = identity(nsp)
    item = max((r for r in entries(nsp) if r.name.endswith('.nca')), key=lambda r: r.size)
    target = work / (kind + '.nca')
    marker = work / (kind + '-nca.json')
    if marker.is_file():
        previous = json.loads(marker.read_bytes())
        if previous['source'] != stamp or not target.is_file() or sha_file(target) != previous['sha256']:
            raise ValueError(f'{kind} NCA cache does not match source; use another --work-dir')
    else:
        temp = target.with_suffix('.partial')
        digest = hashlib.sha256()
        with nsp.open('rb') as source, temp.open('wb') as dest:
            source.seek(item.offset)
            remaining = item.size
            while remaining:
                block = source.read(min(16 * 1024**2, remaining))
                if not block:
                    raise ValueError('truncated NSP')
                dest.write(block)
                digest.update(block)
                remaining -= len(block)
        temp.replace(target)
        write_json(marker, dict(source=stamp, member=item.name, sha256=digest.hexdigest()))
    info = hactool(executable, keys, work, kind + '-info.log', '-i', target)
    if not re.search(r'Content Type:\s*Program', info, re.I) or not re.search(
            r'(?:Title ID|Program I[dD])\s*:\s*' + PROGRAM_ID, info, re.I):
        raise ValueError(f'{kind} is not the MOFA Program NCA')
    return target


def prepare_source(base, update, mod, work, executable, keys):
    mod_files = sorted(p for p in mod.rglob('*') if p.is_file())
    if not mod_files or not (mod / 'allui.mrg').is_file() or not (mod / 'script_text.mrg').is_file():
        raise ValueError('select the Chengkong inner romfs directory')
    stamp = dict(base=identity(base), update=identity(update),
                 overlay=[dict(path=p.relative_to(mod).as_posix(), sha256=sha_file(p)) for p in mod_files])
    marker, romfs = work / 'romfs-source.json', work / 'romfs'
    if marker.is_file():
        if json.loads(marker.read_bytes()) != stamp:
            raise ValueError('RomFS cache belongs to other inputs; use another --work-dir')
        check_romfs(romfs)
        return romfs, stamp
    work.mkdir(parents=True, exist_ok=True)
    base_nca = program_nca(base, work, executable, keys, 'base')
    update_nca = program_nca(update, work, executable, keys, 'update')
    print('Extracting merged RomFS: update BKTR + base; no images are converted yet.', flush=True)
    hactool(executable, keys, work, 'romfs.log', '--basenca=' + str(base_nca),
            '--romfsdir=' + str(romfs), update_nca)
    for source in mod_files:
        target = romfs / source.relative_to(mod)
        target.parent.mkdir(parents=True, exist_ok=True)
        temp = target.with_name(target.name + '.overlay')
        shutil.copyfile(source, temp)
        temp.replace(target)
    check_romfs(romfs)
    write_json(marker, stamp)
    return romfs, stamp


def check_romfs(romfs):
    ARCHIVES = ('jallpac', 'jallpachd', 'allpacml', 'allpachdml', 'allui', 'parts')
    required = ['allscr.mrg', 'script_text.mrg', 'witch.bfsar', 'exfont_nx01.bffnt',
                'zc/arshanghaisonggbpro_lt.otf']
    required += [a + '.' + ext for a in ARCHIVES for ext in ('hed', 'mrg')]
    for name in required:
        if not (romfs / name).is_file():
            raise ValueError(f'MOFA RomFS missing {name}')
