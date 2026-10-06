"""Compare recorded KAG pages with extracted scripts, including unfinished runs."""
import argparse
import collections
import json
from pathlib import Path
import re


PAGE = re.compile(r'\b([\w.-]+\.ks)\s*:\s*(\*page\d+)\|')


def inventory(root):
    pages = collections.defaultdict(set)
    for path in root.rglob('*.ks'):
        source = path.read_text(encoding='utf-8-sig').replace('\r', '')
        pages[path.name].update(re.findall(r'(?m)^(\*page\d+)\b', source))
    return pages


def collect(output, cases):
    rows = []
    for case in cases:
        directory = Path(case['recording']) if 'recording' in case else output / case['name']
        summary = directory / 'summary.json'
        compact_markers = directory / 'markers.json'
        events = directory / 'events.jsonl'
        if summary.exists():
            markers = json.loads(summary.read_text(encoding='utf-8'))['markers']
        elif compact_markers.exists():
            markers = json.loads(compact_markers.read_text(encoding='utf-8'))
        elif events.exists():
            markers = []
            with events.open(encoding='utf-8') as source:
                for line in source:
                    if '"marker"' not in line:
                        continue
                    try:
                        event = json.loads(line)
                    except json.JSONDecodeError:
                        # The writer may still be appending its final line.
                        continue
                    if event['type'] == 'marker':
                        markers.append(event)
        else:
            rows.append(dict(case=case['name'], status='not started', pages=[]))
            continue
        seen = set()
        last_page = None
        for marker in markers:
            if match := PAGE.search(marker.get('detail', '')):
                last_page = match[1] + match[2]
                seen.add(last_page)
        messages = '\n'.join(m.get('detail', '') for m in markers)
        run_path = directory / 'run.json'
        run = json.loads(run_path.read_text(encoding='utf-8')) if run_path.exists() else {}
        process_path = directory / 'process.json'
        process = json.loads(process_path.read_text(encoding='utf-8')) if process_path.exists() else {}
        missing = [s for s in case.get('expect', []) if s not in messages]
        failures = [m['detail'] for m in markers if 'PERF-FAIL:' in m.get('detail', '')]
        if not run_path.exists():
            status = 'interrupted' if process else 'running'
        elif run.get('error') or process.get('exit_code', 0) or process.get('timed_out'):
            status = 'error'
        elif missing or failures or run.get('dropped_events'):
            status = 'incomplete'
        else:
            status = 'complete'
        rows.append(dict(case=case['name'], status=status, pages=sorted(seen),
                         last_page=last_page, expected_scripts=case.get('story_scripts', []),
                         missing=missing, failures=failures, error=run.get('error'),
                         dropped_events=run.get('dropped_events'), elapsed_ms=run.get('elapsed_ms')))
    return rows


def report(output, scripts, also=(), destination=None):
    cases = []
    rows = []
    for directory in [output, *also]:
        manifest = json.loads((directory/'cases.json').read_text(encoding='utf-8'))
        cases.extend(manifest['cases'])
        rows.extend(collect(directory, manifest['cases']))
    static = inventory(scripts)
    defined = {script + page for script, pages in static.items() for page in pages}
    visited = {page for row in rows for page in row['pages']}
    roots = {script for case in cases for script in case.get('story_scripts', [])}
    missing = {script: sorted(page for page in pages if script+page not in visited)
               for script, pages in static.items()}
    missing = {script: pages for script, pages in missing.items() if pages}
    states = dict(collections.Counter(row['status'] for row in rows))
    result = dict(sources=[str(p.resolve()) for p in [output, *also]],
                  states=states, defined_pages=len(defined), visited_pages=len(defined & visited),
                  archive_root_scripts=len(roots), visited_scripts=len({p.split('*')[0] for p in visited}),
                  unknown_pages=sorted(visited-defined), missing_pages=missing, cases=rows)
    destination = destination or output
    destination.mkdir(parents=True, exist_ok=True)
    (destination/'coverage.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    text = ['# Scene coverage', '',
            f'Cases: {states}. Pages reached: {result["visited_pages"]}/{len(defined)}. '
            f'Scripts reached: {result["visited_scripts"]}; archive roots: {len(roots)}.', '',
            'A page marker confirms entry, not execution of every conditional branch or completion of every animation. '
            'Instant text and automatic clicks shorten reading waits. Effects keep their original clock.', '',
            '| Case | Status | Pages | Last page |', '|---|---|---:|---|']
    for row in rows:
        text.append(f'| {row["case"]} | {row["status"]} | {len(row["pages"])} | {row.get("last_page", "")} |')
    text += ['', '## Pages not reached', '']
    for script, pages in sorted(missing.items()):
        text.append(f'- {script}: {", ".join(pages)}')
    for row in rows:
        if row.get('error') or row.get('failures'):
            text += ['', f'## {row["case"]}', '', row.get('error') or '\n'.join(row['failures'])]
    (destination/'coverage.md').write_text('\n'.join(text)+'\n', encoding='utf-8')
    print(f'{states}; pages {result["visited_pages"]}/{len(defined)}; scripts {result["visited_scripts"]}', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('scripts', type=Path)
    parser.add_argument('--also', type=Path, action='append', default=[], help='include another workload when counting reached pages')
    parser.add_argument('--out', type=Path, help='write coverage here instead of the first workload directory')
    args = parser.parse_args()
    report(args.output, args.scripts, args.also, args.out)
