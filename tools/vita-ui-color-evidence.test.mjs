import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { UI_COLOR_STAGES, UI_COLOR_CONTRACT, UI_COLOR_PATH,
  activeUiColorStage, uiColorResultEvidence } from './vita-ui-color-evidence.mjs';

const hash = value => createHash('sha256').update(value).digest('hex');
function fixture() {
  let log = `TITLE_ID KNIF00013\n${UI_COLOR_CONTRACT}\n${UI_COLOR_PATH}\nGXM READY baseline_cdram=4456448 common_cdram=8650752\n`;
  const captures = {};
  for (const [index, stage] of UI_COLOR_STAGES.entries()) {
    log += `STAGE ${stage} BEGIN\nSTAGE ${stage} LOAD elapsed_us=1234 mapped_bytes=262144\nSTAGE ${stage} FRAME READY fence=COMPLETE\n`;
    const state = { active_stage: stage, log_length: log.length, log_sha256: hash(log),
      session: { phase: 'running', frameReady: true, resolution: { width: 960, height: 544 } } };
    captures[stage] = { stage, captured: true, stage_valid_during_capture: true,
      state_before: structuredClone(state), state_after: structuredClone(state),
      artifact: { path: `/actual/stage-${index}.png`, bytes: 128, sha256: String(index + 1).repeat(64) } };
    log += `STAGE ${stage} RETIRE BEGIN\nSTAGE ${stage} PASS frames=61 texture_retired=true\n`;
  }
  log += 'GXM CLOSE BEGIN\nGXM CLOSE PASS\nRESULT PASS\n';
  return { log, captures };
}

test('only a currently live UI color stage authorizes capture', () => {
  const begin = 'STAGE UI_RUNTIME_ALTER BEGIN\n';
  const ready = `${begin}STAGE UI_RUNTIME_ALTER FRAME READY fence=COMPLETE\n`;
  assert.equal(activeUiColorStage(begin), undefined);
  assert.equal(activeUiColorStage(ready), 'UI_RUNTIME_ALTER');
  for (const ending of ['STAGE UI_RUNTIME_ALTER RETIRE BEGIN', 'STAGE UI_RUNTIME_ALTER PASS frames=61 texture_retired=true',
    'STAGE UI_OFFLINE_RGBA BEGIN', 'STAGE UNKNOWN BEGIN', 'GXM CLOSE BEGIN', 'ERROR failed', 'RESULT PASS']) {
    assert.equal(activeUiColorStage(ready + ending + '\n'), undefined, ending);
  }
});

test('three ordered distinct GPU captures and checked production native lifecycle pass', () => {
  const { log, captures } = fixture();
  const result = uiColorResultEvidence(log, captures);
  assert.equal(result.valid, true, JSON.stringify(result));
  assert.equal(result.stages.length, 3);
  assert.equal(result.pixel_verified, false);
  assert.equal(result.hardware_verified, false);
});

test('missing identity, contract, stage, load or teardown cannot pass', () => {
  const { log, captures } = fixture();
  for (const marker of ['TITLE_ID KNIF00013', UI_COLOR_CONTRACT, UI_COLOR_PATH,
    'GXM READY baseline_cdram=4456448 common_cdram=8650752', 'STAGE UI_OFFLINE_DXT5 BEGIN',
    'STAGE UI_RUNTIME_ALTER LOAD elapsed_us=1234 mapped_bytes=262144',
    'STAGE UI_OFFLINE_RGBA RETIRE BEGIN', 'GXM CLOSE PASS']) {
    assert.equal(uiColorResultEvidence(log.replace(marker, ''), captures).valid, false, marker);
  }
  for (const bad of [log + 'RESULT PASS\n', log + 'extra text\n', log.replace('GXM CLOSE PASS', 'ERROR failed\nGXM CLOSE PASS')]) {
    assert.equal(uiColorResultEvidence(bad, captures).valid, false);
  }
});

test('capture proof must refer to actual live prefixes of the completed guest log', () => {
  const { log, captures } = fixture();
  for (const change of [capture => { capture.state_after.log_sha256 = '0'.repeat(64); },
    capture => { capture.state_before.log_length = 1; },
    capture => { capture.state_after.active_stage = 'UI_OFFLINE_RGBA'; },
    capture => { capture.state_after.log_length = log.length; capture.state_after.log_sha256 = hash(log); },
    capture => { capture.state_before.session.phase = 'stopped'; },
    capture => { capture.state_after.session.frameReady = false; },
    capture => { capture.state_after.session.resolution.width = 1280; },
    capture => { capture.artifact.path = captures.UI_RUNTIME_ALTER.artifact.path; },
    capture => { capture.artifact.sha256 = 'wrong'; },
    capture => { capture.captured = false; }]) {
    const changed = structuredClone(captures);
    change(changed.UI_OFFLINE_DXT5);
    assert.equal(uiColorResultEvidence(log, changed).valid, false);
  }
});
