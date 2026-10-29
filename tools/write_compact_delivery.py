"""Write the bilingual 540p delivery report from verified local evidence."""
from __future__ import annotations
from collections import Counter
from datetime import datetime
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
SCENARIOS = ['boot', 'story', 'load', 'config', 'backlog', 'title-prime', 'title', 'movie']


def read(path):
    return json.loads(Path(path).read_text(encoding='utf-8-sig'))


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def size(path):
    paths = [p for p in Path(path).rglob('*') if p.is_file()]
    return dict(bytes=sum(p.stat().st_size for p in paths), files=len(paths))


def log_text(path):
    raw = Path(path).read_bytes()
    return raw.decode('utf-16') if raw[:2] in (b'\xff\xfe', b'\xfe\xff') else raw.decode('utf-8-sig')


def main():
    doc = ROOT / 'doc'
    validation = read(doc / 'compact-assets-validation.json')
    integrity = read(doc / 'compact-current-integrity.json')
    bilingual = read(doc / 'compact-bilingual-optimization.json')
    lossless = read(doc / 'compact-lossless-optimization.json')
    assert all(x['complete'] for x in (validation, integrity, bilingual, lossless))
    assert integrity['total_bytes'] == validation['after']['total_bytes']
    assert integrity['lossless_proof_chain_verified'] and integrity['bilingual_proof_chain_verified']
    assert [x['code'] for x in bilingual['languages']] == ['en', 'zc']
    assert bilingual['original_save_ids_preserved'] and not bilingual['player_saves_modified']
    assert bilingual['japanese_voice_retained']
    runtime = Path(validation['output'])
    image_source = Path(validation['image_source'])
    executable = runtime / 'knife.exe'
    qa = ROOT / '.work/qa/knife-compact-qa.exe'
    exe_hash, qa_hash = digest(executable), digest(qa)
    evidence, all_cases = {}, {}
    for language, filename in [('zc', 'compact-verification.json'), ('en', 'compact-en-verification.json')]:
        result = read(doc / filename)
        assert result['production_executable']['sha256'] == exe_hash
        cases = {c['name']: c for c in result['cases']}
        assert all(cases[n]['passed'] and cases[n]['executable_sha256'] == qa_hash for n in SCENARIOS)
        evidence[language] = dict(path='doc/' + filename,
            scenarios={n:dict(passed=True, elapsed_seconds=cases[n]['elapsed_seconds']) for n in SCENARIOS})
        all_cases[language] = cases
    assert all_cases['zc']['production-portable']['passed']
    units = {}
    for name, case in all_cases['zc'].items():
        if name.startswith('unit-'):
            assert case['exit_code'] == 0
            match = re.search(r'test result: ok\. (\d+) passed; 0 failed;', log_text(case['log']))
            assert match, name
            units[name] = int(match[1])
    assert len(units) == 5 and sum(units.values()) == 67
    python_tests = {}
    for filename, count in [('portable-images-python.log',4), ('lossless-optimizer-python.log',5), ('bilingual-python.log',3)]:
        text = log_text(doc / 'logs/compact' / filename)
        assert f'Ran {count} tests' in text and re.search(r'^OK\s*$', text, re.M), filename
        python_tests[filename] = count
    formats = Counter()
    for archive in validation['archives']:
        formats.update(archive['formats'])
    assert dict(formats) == integrity['image_formats'] == bilingual['image_formats']
    assert sum(formats.values()) == sum(a['image_views_including_languages'] for a in validation['archives']) == 35007
    assert integrity['editable_image_files'] == 35007
    plan = doc / 'PSVita-GXM移植实施计划.md'
    assert plan.is_file()
    git = lambda *a: subprocess.check_output(['git', '-C', str(ROOT/'knife'), *a], text=True).strip()
    report = dict(as_of=datetime.now().astimezone().isoformat(timespec='seconds'),
        profile='png-jpeg-540p-v1', upstream_version='0.9.4',
        languages=bilingual['languages'], fallback_ids=bilingual['fallback_ids'],
        images=dict(formats=dict(formats), total=sum(formats.values()), maximum_per_image=[960,540],
                    webp_count=0, source_path=str(image_source), source_folder=size(image_source)),
        assets=validation['after'], runtime_folder=size(runtime),
        bilingual_storage=dict(before=bilingual['before'], after=bilingual['after'],
            runtime_saved_bytes=bilingual['before']['runtime']-bilingual['after']['runtime'],
            image_source_saved_bytes=bilingual['before']['image_source']-bilingual['after']['image_source']),
        earlier_lossless_storage=dict(runtime_saved_bytes=lossless['runtime_saved_bytes'],
            image_source_saved_bytes=lossless['image_source_saved_bytes']),
        integrity=dict(path='doc/compact-current-integrity.json', as_of=integrity['as_of'], complete=True,
            lossless_proof_chain_verified=True, bilingual_proof_chain_verified=True),
        bilingual_receipt='doc/compact-bilingual-optimization.json',
        lossless_receipt='doc/compact-lossless-optimization.json',
        executable=dict(path=str(executable), bytes=executable.stat().st_size, sha256=exe_hash,
            machine='0x8664', optional_header_magic='0x20b'),
        qa_executable_sha256=qa_hash, language_scenarios=evidence,
        production_portable=all_cases['zc']['production-portable'],
        unit_test_counts=units, total_rust_tests=sum(units.values()),
        python_test_counts=python_tests, total_python_tests=sum(python_tests.values()),
        audio=validation['audio'], movies=validation['movies'], japanese_voice_retained=True,
        player_saves_modified=False, vita=dict(plan='doc/PSVita-GXM移植实施计划.md',
            toolchain_inventory='doc/vita-review-toolchain-inventory.json',
            historical_toolchain_inventory='doc/vita-toolchain-inventory.json',
            review='doc/PSVita-GXM审查采纳说明.md',
            development_renderer='VitaGL for bounded development validation',
            delivery_renderer='Direct GXM; no VitaGL fallback',
            rust_src_candidate='nightly-2026-05-28; present, build probe pending',
            native_shader_toolchain_verified=False, implemented=False,
            vpk_built=False, hardware_validated=False),
        local_branch=git('branch','--show-current'), base_commit=git('rev-parse','HEAD'),
        local_worktree_status=git('status','--short'),
        historical_remote_inspection=read(doc/'compact-git-status.json'),
        remote_rechecked_during_delivery=False, committed_during_task=False, pushed_during_task=False,
        limitations=['Finite Windows scenarios, no full-game playthrough',
            'Movie test checks startup and first decoded frame, not full playback',
            'Vita plan only; no Vita executable/VPK or hardware-performance claim',
            'Editable images do not automatically rebuild MRG archives'])
    (doc/'compact-delivery.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
    gib = lambda n: f'{n/1024**3:.3f}'
    mib = lambda n: f'{n/1024**2:.2f}'
    archive_rows = '\n'.join(f"| {a['name']} | {a['image_views_including_languages']:,} | {a['formats'].get('PNG',0):,} | {a['formats'].get('JPEG',0):,} |" for a in validation['archives'])
    scenario_rows = '\n'.join(f"| {n} | 通过，{evidence['en']['scenarios'][n]['elapsed_seconds']:.2f} s | 通过，{evidence['zc']['scenarios'][n]['elapsed_seconds']:.2f} s |" for n in SCENARIOS)
    saved = report['bilingual_storage']
    text = f"""# PNG/JPEG 540p 双语紧凑版交付报告

核验时间：{report['as_of']}。引擎基线 knife 0.9.4；本地分支 {report['local_branch']}，基线提交 {report['base_commit']}。本次改动留在本地，未提交、未推送。历史远端信息保留在 compact-git-status.json，不能当作本次最新远端核验。

## 语言与资源精简

游戏可选语言仅 English 与中文（澄空）。脚本/存档语言 ID 保持 1 / 2，物理资源槽变为 0 / 1；旧 ID 0 回退英语，3 回退澄空。文本五组缩为两组，保留文本块字节不变。原生简中、繁中与日文界面的专用资源移除；日语配音与两种语言共用的 CG 保留。

多语言 MRG、系统消息、控制提示、设置语言栏和图鉴缩略图同步精简。UI 删除项保留空索引槽，别名表维护旧脚本名称，避免重排条目破坏引用。菜单中文名称为“中文（澄空）”。精简清单与前后哈希见 compact-bilingual-optimization.json。

## 实际占用与画质

| 项目 | 当前值 |
| --- | ---: |
| 运行目录（包含引擎、资源和现有存档等） | {report['runtime_folder']['bytes']:,} B / {gib(report['runtime_folder']['bytes'])} GiB |
| assets 实际资源 | {report['assets']['total_bytes']:,} B / {gib(report['assets']['total_bytes'])} GiB |
| 可编辑图片目录 | {report['images']['source_folder']['bytes']:,} B / {gib(report['images']['source_folder']['bytes'])} GiB |
| 本次双语精简节省运行目录 | {mib(saved['runtime_saved_bytes'])} MiB |
| 本次双语精简节省图片源 | {mib(saved['image_source_saved_bytes'])} MiB |
| 此前无损优化另节省运行目录 | {mib(lossless['runtime_saved_bytes'])} MiB |

保留 35,007 张可编辑图片：PNG 31,527 张，JPEG 3,480 张，删除 1,004 张冗余 PNG；没有 WebP。每张普通图片不超过 960×540。两张双语图鉴条带物理高度 1,080，每语言切片仍为 960×540；不是单语言高清图。3 个保留的 TGA 是低分辨率启动/存档图标。视频仅保留 logo、英语 OP、澄空 OP 共 3 个 MP4。

本次没有改变 PNG/JPEG 格式，也没有对 JPEG 再次有损编码。保留的普通图片像素/字节与此前无损优化结果一致；条带只移除不保留的语言切片，语言栏重排为两项。完整资产与图片源复核见 compact-current-integrity.json，双语和无损证明链均通过。

| 图像归档 | 图片数 | PNG | JPEG |
| --- | ---: | ---: | ---: |
{archive_rows}

音频仍为 9,470 个 Ogg Opus，48 kHz；另有引擎使用的 BFSAR 音频库。保留日语配音，不是额外的日文界面语言。

## Windows 验证

67 个 Rust 测试与 12 个 Python 测试通过。当前 QA EXE 同时通过两种语言各 8 个有限场景，总计 16 个；生产 EXE 从独立工作目录启动通过。个人存档未改动；测试使用隔离存档。

| 场景 | 英语 | 澄空 |
| --- | --- | --- |
{scenario_rows}

生产 EXE：{executable.stat().st_size:,} B，AMD64 PE32+。SHA-256：{exe_hash}。

QA EXE SHA-256：{qa_hash}。证据分别为 compact-en-verification.json、compact-verification.json 与 logs/compact、logs/compact-en。视频验证覆盖启动与第一帧解码显示；尚未做全片播放或全流程通关验证。

## 工程包与 PSVita 计划

工程包为 dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-project.zip，含源码、构建脚本、语言精简工具、Windows EXE、文档与测试证据；游戏资产和个人存档仍放在各自目录，不在源码 ZIP 内。新解压目录的构建结果由 compact-source-package-verification.json 单独记录，运行验证工具成功前不视为通过。

[PSVita GXM 移植实施计划](PSVita-GXM移植实施计划.md) 与 [审查采纳说明](PSVita-GXM审查采纳说明.md) 已修订：VitaGL 用于开发阶段有限验证，最终直接 GXM，不自动回退。计划明确共同绘制接口、Rust 唯一最终链接、ELF→VELF→SELF、960×544 中的 960×540 画布、AudioOut/Opus、AvPlayer 与实际内存账本。补充清单 vita-review-toolchain-inventory.json 确认 vita-elf-create、VitaGL 库和 nightly-2026-05-28 对应 rust-src 已存在；psp2cgc 在所查范围未发现，原生 shader 门槛未解除。工期在 M0/M2 后重估，21–40 个工作日不作为承诺。当前没有 Vita 后端、VPK 或真机结果。
"""
    (doc/'PNG-JPEG紧凑版交付报告.md').write_text(text,encoding='utf-8')
    print(json.dumps(dict(runtime=report['runtime_folder'], images=report['images']['formats'],
        bilingual_saved_bytes=saved['runtime_saved_bytes'], rust_tests=67,python_tests=12,
        scenarios=16, executable_sha256=exe_hash, plan=str(plan)),ensure_ascii=False,indent=2))


if __name__ == '__main__':
    main()
