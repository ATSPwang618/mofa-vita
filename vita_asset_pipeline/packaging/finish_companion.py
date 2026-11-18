"""Publish the main engine built against the actual delivered asset metadata."""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import struct
import sys
import zipfile
from xml.etree import ElementTree

ROOT=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT))
from vita_asset_pipeline.build_binding import check


def digest(data):
    return hashlib.sha256(data).hexdigest()


def finish():
    assets=ROOT/'dist/ux0/data/WitchontheHolyNight/assets'
    binding=check(ROOT,assets)
    delivery=json.loads((assets.parent/'delivery.json').read_bytes())
    if not delivery['validation']['passed'] or delivery['manifest_sha256']!=binding['manifest_sha256']:
        raise ValueError('Complete delivery has not passed validation and synchronization')
    output=ROOT/'.work/vita/game-release-540p'
    package=output/'knife-vita-game-540p.vpk'
    eboot=(output/'eboot.bin').read_bytes()
    if eboot[:4]!=b'SCE\0':raise ValueError('EBOOT is not a signed SELF')
    spec=importlib.util.spec_from_file_location('main_package_check',ROOT/'tools/check-vita-package.py')
    checker=importlib.util.module_from_spec(spec);spec.loader.exec_module(checker)
    sfo=checker.parse_sfo((output/'param.sfo').read_bytes())
    for name,expected in dict(TITLE_ID='KNIF00540',APP_VER='00.01',ATTRIBUTE2=12).items():
        if sfo.get(name)!=expected:raise ValueError('Wrong package SFO '+name)
    files={'eboot.bin':eboot,'sce_sys/param.sfo':(output/'param.sfo').read_bytes(),
           'LICENSE':(ROOT/'knife/LICENSE').read_bytes()}
    for source in (ROOT/'knife/vita/sce_sys').rglob('*'):
        if source.is_file():
            name='sce_sys/'+source.relative_to(ROOT/'knife/vita/sce_sys').as_posix()
            if name in files:raise ValueError('Duplicate package input '+name)
            files[name]=source.read_bytes()
    license_root=ROOT/'knife/vita/vendor/dependencies/licenses'
    for source in license_root.rglob('*'):
        if source.is_file():files['licenses/native-dependencies/'+source.relative_to(license_root).as_posix()]=source.read_bytes()
    notice=ROOT/'knife/vita/vendor/shacccg-ext'
    files['sce_module/SceShaccCgExt-NOTICE.txt']=(notice/'NOTICE').read_bytes()+b'\n'+(notice/'LICENSE').read_bytes()
    for name,size in [('icon0.png',(128,128)),('livearea/contents/bg.png',(840,500)),
                      ('livearea/contents/startup.png',(280,158)),('pic0.png',(960,544))]:
        png=files['sce_sys/'+name]
        if png[:8]!=b'\x89PNG\r\n\x1a\n' or struct.unpack_from('>II',png,16)!=size or png[25]!=3:
            raise ValueError('Invalid indexed LiveArea image '+name)
    template=ElementTree.fromstring(files['sce_sys/livearea/contents/template.xml'])
    if template.tag!='livearea':raise ValueError('Invalid LiveArea root')
    for element in template.iter():
        if element.tag in ('image','startup-image'):
            name=(element.text or '').strip()
            if not name or '..' in Path(name).parts or 'sce_sys/livearea/contents/'+name not in files:
                raise ValueError('Invalid LiveArea image reference '+name)
    pending=package.with_suffix('.vpk.tmp')
    with zipfile.ZipFile(pending,'w',zipfile.ZIP_DEFLATED,compresslevel=9) as archive:
        for name,data in sorted(files.items()):archive.writestr(name,data)
    with zipfile.ZipFile(pending) as archive:
        if archive.testzip() is not None:raise ValueError('VPK CRC failure')
        if set(archive.namelist())!=set(files):raise ValueError('VPK inventory mismatch')
        for name,data in files.items():
            if archive.read(name)!=data:raise ValueError('VPK payload mismatch '+name)
    pending.replace(package)
    release=ROOT/'release';release.mkdir(exist_ok=True)
    app=ROOT/'dist/ux0/app/KNIF00540';app.mkdir(parents=True,exist_ok=True)
    for source,destination in [(package,release/'mofa-vita.vpk'),(output/'eboot.bin',release/'eboot.bin'),
                               (output/'eboot.bin',app/'eboot.bin'),(package,ROOT/'dist/mofa-vita.vpk')]:
        temp=destination.with_suffix(destination.suffix+'.tmp');shutil.copyfile(source,temp);temp.replace(destination)
    report=dict(schema='mofa-companion-binding-v1',passed=True,hardware_tested=False,
                manifest_sha256=binding['manifest_sha256'],title_id=sfo['TITLE_ID'],attribute2=sfo['ATTRIBUTE2'],
                eboot_sha256=digest(eboot),vpk_sha256=digest(package.read_bytes()),
                vpk_entries={name:dict(bytes=len(data),sha256=digest(data)) for name,data in files.items()},
                sony_module_embedded=False,shader_policy='existing main embedded shaders and existing external compiler fallback',
                external_compiler_locations=['ur0:/data/libshacccg.suprx','ur0:/data/external/libshacccg.suprx'])
    (release/'companion-binding.json').write_text(json.dumps(report,indent=2)+'\n',encoding='utf8')
    (ROOT/'dist/companion-binding.json').write_text(json.dumps(report,indent=2)+'\n',encoding='utf8')
    (ROOT/'dist/README-安装.md').write_text('''# 魔法使之夜 Vita 本次完整交付

1. 用 VitaShell 安装本目录的 mofa-vita.vpk，标题 ID 为 KNIF00540。
2. 关闭游戏，将 ux0 文件夹内的内容按同名目录复制到设备的 ux0:。
   游戏数据最终位置为 ux0:data/WitchontheHolyNight/assets/。
3. 保留设备上 WitchontheHolyNight 目录原有的存档和设置；完整替换 assets，
   避免混入旧版本归档。ux0/app/KNIF00540/eboot.bin 与本 VPK 内程序完全相同。

音频为 voice.mrg 与 bgm.mrg 两个 bank，无需散装 stream 目录。
本次实际资源清单和项目生成索引已同步，后续正常构建会使用这份清单。
资源核验结果在 ux0/data/WitchontheHolyNight/asset-validation.json，
程序与清单绑定结果在 companion-binding.json。

VPK 不包含 Sony 编译器模块。程序沿用主分支的内嵌着色器及外部编译器回退；
需要回退时使用设备已有的 ur0:data/libshacccg.suprx。
本次完成主机侧转换、文件和构建检查，尚未进行 Vita 实机游戏验证。
''',encoding='utf8')
    print(json.dumps(report,indent=2))


if __name__=='__main__':finish()
