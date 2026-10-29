// Display-only old/fused group fixture; CPU target backing is not an oracle.
import { createHash } from 'node:crypto';

export const GROUP_PAGES = Object.freeze(['cartesian-037', 'cartesian-100', 'alpha-exhaustive']);
const hash = value => createHash('sha256').update(value).digest('hex');
export function activeGroupPage(log) {
  let active;
  for (const line of log.split('\n')) {
    const ready = /^GROUP_FRAME READY page=(\S+) cases=(\d+) alpha_levels=(\d+) opacity=([\d.]+); actual displayed RGB comparison required$/.exec(line);
    if (ready) active = GROUP_PAGES.includes(ready[1]) ? ready[1] : undefined;
    else if (/^(GROUP_FRAME END |RESULT |FAIL |PASS texture retirement)/.test(line)) active = undefined;
  }
  return active;
}
export function groupResultEvidence(log, captures) {
  const issues = [], pages = [], usedPaths = new Set();
  const lines = log.split('\n');
  const expected = [
    'GROUP_FRAME READY page=cartesian-037 cases=33 alpha_levels=64 opacity=0.37; actual displayed RGB comparison required',
    'GROUP_FRAME READY page=cartesian-100 cases=33 alpha_levels=64 opacity=1; actual displayed RGB comparison required',
    'GROUP_FRAME READY page=alpha-exhaustive cases=1 alpha_levels=256 opacity=1; actual displayed RGB comparison required',
  ];
  let last = lines.findIndex(line => line.startsWith('FRAME READY;'));
  if (last < 0 || !log.startsWith('knife Vita DEV effects fixture v2;')
      || lines.filter(line => line.startsWith('GROUP_FRAME READY ')).length !== 3
      || lines.filter(line => line.startsWith('GROUP_FRAME END ')).length !== 3
      || lines.filter(line => line.startsWith('RESULT ')).length !== 1
      || !log.endsWith('RESULT PASS\n')) issues.push('Missing unambiguous finite group fixture identity/result');
  for (const [index, page] of GROUP_PAGES.entries()) {
    const begin = lines.indexOf(expected[index]), endText = `GROUP_FRAME END page=${page}`, end = lines.indexOf(endText);
    const ordered = begin > last && end > begin && lines.filter(line => line === expected[index]).length === 1
      && lines.filter(line => line === endText).length === 1;
    last = end;
    const capture = captures?.[page];
    const prefix = state => {
      if (!state || state.active_page !== page || !Number.isSafeInteger(state.log_length)
          || state.log_length < 1 || state.log_length > log.length || state.session?.phase !== 'running'
          || state.session.frameReady !== true || state.session.resolution?.width !== 960
          || state.session.resolution?.height !== 544) return false;
      const actual = log.slice(0, state.log_length);
      return actual.endsWith('\n') && hash(actual) === state.log_sha256 && activeGroupPage(actual) === page;
    };
    const artifact = capture?.artifact;
    const captured = capture?.captured === true && capture.page === page && capture.page_valid_during_capture === true
      && prefix(capture.state_before) && prefix(capture.state_after)
      && capture.state_before.log_length <= capture.state_after.log_length
      && typeof artifact?.path === 'string' && artifact.path.length > 0 && !usedPaths.has(artifact.path)
      && Number.isSafeInteger(artifact.bytes) && artifact.bytes > 25 && /^[a-f0-9]{64}$/.test(artifact.sha256 ?? '');
    if (typeof artifact?.path === 'string') usedPaths.add(artifact.path);
    pages.push({page, valid: ordered && captured, captured, lifecycle_ordered: ordered});
    if (!ordered || !captured) issues.push(`Missing ordered lifecycle/live capture: ${page}`);
  }
  return {valid: issues.length === 0, issues, pages, pixel_verified: false, hardware_verified: false};
}
