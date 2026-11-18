"""One-time recovery of recipes and explicit private exceptions from 100% DXT.

Requires the matching original BASE RomFS for provenance. The resulting kit
contains game data; keep it local. Later builds do not need the reference pack.
"""
import argparse
import json
from pathlib import Path
import subprocess
import tempfile

from recover_runtime_recipe import recover
from export_runtime_grade_luts import generate
from recovery_kit import prepare


def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ('reference','romfs','output'):p.add_argument('--'+name,type=Path,required=True)
    p.add_argument('--wsl',help='WSL distribution containing pinned Rust nightly-2026-05-28')
    a=p.parse_args()
    if a.output.exists():raise ValueError('Kit output must not exist')
    a.output.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='kit-prepare-',dir=a.output.parent) as tmp:
        tmp=Path(tmp);recipe=recover(a.reference);rp=tmp/'recipe.json'
        rp.write_text(json.dumps(recipe,separators=(',',':'))+'\n','utf8')
        source=tmp/'grade.rs';source.write_text(generate(),'utf8');exe=tmp/'grade-export'
        if a.wsl:
            def linux(path):
                return subprocess.check_output(['wsl.exe','-d',a.wsl,'--','wslpath','-u',path.resolve().as_posix()],text=True).strip()
            subprocess.run(['wsl.exe','-d',a.wsl,'--','/root/.cargo/bin/rustc','+nightly-2026-05-28',
                            '--edition=2024','-O','-Awarnings',linux(source),'-o',linux(exe)],check=True)
            command=['wsl.exe','-d',a.wsl,'--',linux(exe)]
        else:
            exe=exe.with_suffix('.exe')
            subprocess.run(['rustc','+nightly-2026-05-28','--edition=2024','-O','-Awarnings',str(source),'-o',str(exe)],check=True)
            command=[str(exe)]
        keys={(v['grade'],v['bright_bits'],v['contrast_bits'])
              for ar in recipe['archives'].values() for v in ar['variants']}
        # Include all neutral-brightness grades, including thumbnail-only states.
        keys.update((g,'3F800000','3F800000') for g in range(83))
        request=''.join(f'{g} {b} {c}\n' for g,b,c in sorted(keys)).encode('ascii')
        luts=tmp/'grade-luts.bin'
        with luts.open('wb') as output:subprocess.run(command,input=request,stdout=output,check=True)
        prepare(a.reference,a.romfs,rp,luts,a.output)


if __name__=='__main__':main()
