"""Resolve tools and install pinned fallbacks into the portable package cache."""
from dataclasses import dataclass
from hashlib import file_digest, sha256
from importlib import invalidate_caches
from importlib.util import find_spec
from importlib.metadata import version as installed_version, PackageNotFoundError
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import subprocess
import sys
import sysconfig
from tempfile import TemporaryDirectory
from urllib.request import Request, urlopen
import venv
from zipfile import ZipFile

RUNTIME = Path(__file__).resolve().with_name('.runtime')


@dataclass(frozen=True)
class Release:
    directory: str
    url: str
    sha256: str
    executable: str


# The hactool digest is pinned to the upstream 1.4.0 Windows release ZIP.
# FFmpeg comes from the SHA-locked local 8.0 installation or package cache.
WINDOWS_RELEASES = {
    'hactool': Release(
        'hactool-1.4.0-windows-x64',
        'https://github.com/SciresM/hactool/releases/download/1.4.0/hactool-1.4.0-win.zip',
        '36e9a221c8a7949c86ada9388eb703c90663aedfe9f65b6032429614c5e1abe8',
        'hactool.exe',
    ),
}

PYTHON_PACKAGES = {
    'PIL': 'Pillow==11.3.0',
    'numpy': 'numpy==2.3.3',
    'ispc_texcomp': 'ispc_texcomp==1.0.1',
}


def release_for(command):
    if sys.platform != 'win32' or platform.machine().lower() not in {'amd64', 'x86_64'}:
        raise ValueError(f'{command} is required. Automatic downloads are available on Windows x64; '
                         f'install {command} or pass --{command} on this platform')
    return WINDOWS_RELEASES[command]


def install_release(command, release, cache):
    target = cache / release.directory / PurePosixPath(release.executable).name
    if target.is_file():
        return target

    target.parent.mkdir(parents=True, exist_ok=True)
    with TemporaryDirectory(prefix='.download-', dir=target.parent) as directory:
        archive = Path(directory) / 'release.zip'
        request = Request(release.url, headers={'User-Agent': 'mofa-vita-asset-pipeline'})
        print(f'Downloading {command}: {release.url}', flush=True)
        try:
            with urlopen(request, timeout=60) as response, archive.open('wb') as destination:
                shutil.copyfileobj(response, destination, 1024 * 1024)
        except OSError as error:
            raise RuntimeError(f'Could not download {command} from {release.url}: {error}') from error
        with archive.open('rb') as source:
            if file_digest(source, 'sha256').hexdigest() != release.sha256:
                raise ValueError(f'{command} release failed SHA-256 verification')

        executable = Path(directory) / target.name
        with ZipFile(archive) as release_zip:
            with release_zip.open(release.executable) as source, executable.open('wb') as destination:
                shutil.copyfileobj(source, destination, 1024 * 1024)
            if command=='ffmpeg':
                probe=PurePosixPath(release.executable).with_name('ffprobe.exe').as_posix()
                with release_zip.open(probe) as source, target.with_name('ffprobe.exe').open('wb') as destination:
                    shutil.copyfileobj(source,destination,1024*1024)
        try:
            executable.replace(target)
        except PermissionError:
            if not target.is_file():
                raise
    return target


def has_h264_encoder(path):
    try:
        result = subprocess.run([str(path), '-hide_banner', '-encoders'],
                                capture_output=True, text=True, errors='replace', timeout=15)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0 and re.search(r'^\s*V\S*\s+libx264\s', result.stdout, re.M) is not None


def tool_path(value, command, cache=RUNTIME / 'bin'):
    if command == 'ffmpeg':
        import json
        expected = json.loads((Path(__file__).with_name('production') / 'tools/source_replay/toolchain-lock.json').read_bytes())['tools']['ffmpeg']
        def matches(path):
            if not path.is_file():return False
            with path.open('rb') as source:
                return file_digest(source,'sha256').hexdigest()==expected
        candidates = [value] if value else [shutil.which(command), *cache.glob('**/ffmpeg.exe')]
        candidate = next((Path(p).resolve() for p in candidates if p and Path(p).is_file()
                          and matches(Path(p))), None)
        if candidate is None:
            raise ValueError('Provide the current-main FFmpeg 8.0 with --ffmpeg (SHA-256 ' + expected + ')')
        value = str(candidate)
    candidate = value or shutil.which(command)
    if candidate:
        path = Path(candidate).expanduser().resolve()
        if path.is_file() and (command != 'ffmpeg' or has_h264_encoder(path)):
            # Keep the resolved host program with this portable pipeline.
            # A PATH executable is an installation source, not a build dependency.
            if not path.is_relative_to(RUNTIME):
                with path.open('rb') as source:
                    digest=file_digest(source,'sha256').hexdigest()
                target=cache / f'local-{command}-{digest[:16]}' / path.name
                target.parent.mkdir(parents=True,exist_ok=True)
                if not target.is_file():
                    shutil.copy2(path,target)
                if command=='ffmpeg':
                    probe=path.with_name('ffprobe'+path.suffix)
                    if probe.is_file() and not target.with_name(probe.name).is_file():
                        shutil.copy2(probe,target.with_name(probe.name))
                path=target
            return str(path)
        if value:
            raise ValueError(f'Invalid --{command} executable: {path}'
                             + (' (requires the libx264 encoder)' if command == 'ffmpeg' else ''))
    path = install_release(command, release_for(command), cache)
    if command == 'ffmpeg' and not has_h264_encoder(path):
        raise ValueError(f'Downloaded FFmpeg could not run with libx264: {path}')
    return str(path)


def ensure_python(cache=RUNTIME / 'python'):
    def matches(module, requirement):
        name, expected = requirement.split('==')
        try:
            return find_spec(module) is not None and installed_version(name) == expected
        except PackageNotFoundError:
            return False
    missing = [requirement for module, requirement in PYTHON_PACKAGES.items() if not matches(module, requirement)]
    if not missing:
        return

    release = sha256(' '.join(missing).encode()).hexdigest()[:12]
    target = cache / f'{sys.implementation.cache_tag}-{sysconfig.get_platform()}-{release}'
    if not target.is_dir():
        cache.mkdir(parents=True, exist_ok=True)
        with TemporaryDirectory(prefix='.install-', dir=cache) as directory:
            staging = Path(directory) / 'site-packages'
            python = sys.executable
            print(f'Installing Python dependencies: {", ".join(missing)}', flush=True)
            try:
                if find_spec('pip') is None:
                    environment = Path(directory) / 'venv'
                    venv.create(environment, with_pip=True)
                    python = str(environment / ('Scripts/python.exe' if sys.platform == 'win32'
                                                else 'bin/python'))
                subprocess.run([python, '-m', 'pip', 'install', '--disable-pip-version-check',
                                '--no-input', '--no-cache-dir', '--only-binary=:all:', '--no-deps',
                                '--target', str(staging), *missing], check=True)
            except subprocess.CalledProcessError as error:
                raise RuntimeError(f'Failed to install Python dependencies: {", ".join(missing)}') from error
            try:
                staging.rename(target)
            except OSError:
                if not target.is_dir():
                    raise
    sys.path.insert(0,str(target))
    invalidate_caches()
    unresolved = [module for module in PYTHON_PACKAGES if find_spec(module) is None]
    if unresolved:
        raise RuntimeError(f'Python dependency cache is incomplete: {target} ({", ".join(unresolved)})')
