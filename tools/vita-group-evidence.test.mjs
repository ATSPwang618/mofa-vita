import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { GROUP_PAGES, activeGroupPage, groupResultEvidence } from './vita-group-evidence.mjs';

function fixture() {
  let log = 'knife Vita DEV effects fixture v2; fixture\nFRAME READY; reference\n';
  const captures = {};
  for (const [index, page] of GROUP_PAGES.entries()) {
    log += `GROUP_FRAME READY page=${page} cases=${index === 2 ? 1 : 33} alpha_levels=${index === 2 ? 256 : 64} opacity=${index === 0 ? 0.37 : 1}; actual displayed RGB comparison required\n`;
    const state = {active_page: page, log_length: log.length,
      log_sha256: createHash('sha256').update(log).digest('hex'),
      session: {phase: 'running', frameReady: true, resolution: {width: 960, height: 544}}};
    captures[page] = {captured: true, page, page_valid_during_capture: true,
      state_before: structuredClone(state), state_after: structuredClone(state),
      artifact: {path: `/run/${page}.png`, bytes: 100, sha256: 'a'.repeat(64)}};
    log += `GROUP_FRAME END page=${page}\n`;
  }
  log += 'PASS texture retirement and CDRAM accounting; GXM CLOSE BEGIN\nRESULT PASS\n';
  return {log, captures};
}
test('finite ordered display captures are valid; pixel/hardware claims stay false', () => {
  const f = fixture(), result = groupResultEvidence(f.log, f.captures);
  assert.equal(result.valid, true);
  assert.equal(result.pixel_verified, false);
  assert.equal(result.hardware_verified, false);
});
test('end/result retire active capture authority', () => {
  const f = fixture(), c = f.captures[GROUP_PAGES[0]];
  const active = f.log.slice(0, c.state_before.log_length);
  assert.equal(activeGroupPage(active), GROUP_PAGES[0]);
  assert.equal(activeGroupPage(active + `GROUP_FRAME END page=${GROUP_PAGES[0]}\n`), undefined);
  assert.equal(activeGroupPage(active + 'RESULT FAIL\n'), undefined);
});
test('missing, stale or duplicate PNG cannot satisfy three pages', () => {
  for (const mutation of [c => delete c[GROUP_PAGES[1]],
    c => c[GROUP_PAGES[1]].artifact.path = c[GROUP_PAGES[0]].artifact.path,
    c => c[GROUP_PAGES[0]].state_after = c[GROUP_PAGES[1]].state_after]) {
    const f = fixture(); mutation(f.captures);
    assert.equal(groupResultEvidence(f.log, f.captures).valid, false);
  }
});
test('tampered prefixes and stopped sessions fail closed', () => {
  for (const mutation of [c => c.state_before.log_sha256 = 'b'.repeat(64),
    c => c.state_before.session.phase = 'stopped',
    c => c.state_after.session.resolution.height = 540,
    c => c.state_after.log_length = 0]) {
    const f = fixture(); mutation(f.captures[GROUP_PAGES[0]]);
    assert.equal(groupResultEvidence(f.log, f.captures).valid, false);
  }
});
test('ambiguous lifecycle and wrong alpha inventory cannot pass', () => {
  for (const change of [s => s.replace('cases=33', 'cases=32'),
    s => s.replace('GROUP_FRAME END page=cartesian-037\n', ''),
    s => s.replace('RESULT PASS\n', 'RESULT PASS\nRESULT PASS\n')]) {
    const f = fixture(); assert.equal(groupResultEvidence(change(f.log), f.captures).valid, false);
  }
});
