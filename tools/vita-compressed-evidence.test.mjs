import test from 'node:test';
import assert from 'node:assert/strict';
import { COMPRESSED_STAGES, activeCompressedStage, compressedResultEvidence } from './vita-compressed-evidence.mjs';

function fixture() {
  const lines = ['TITLE_ID KNIF00012'], captures = {};
  for (const [stage, source] of Object.entries(COMPRESSED_STAGES)) {
    lines.push(`STAGE ${stage} BEGIN source=${source}`);
    if (source.endsWith('.gxt')) lines.push(`STAGE ${stage} GXT logical=960x540 runtime_decode=NONE`);
    lines.push(`STAGE ${stage} LOAD encoded_bytes=64 read_us=12 decode_us=0 upload_us=4 mapped_bytes=4096`,
      `STAGE ${stage} FRAME READY fence=COMPLETE`, `STAGE ${stage} FRAMES count=60 elapsed_us=2000 authority=CURRENT_RUNTIME`,
      `STAGE ${stage} RETIRE BEGIN`, `STAGE ${stage} PASS frames=61 texture_retired=true`);
    captures[stage] = { stage, captured: true, stage_valid_during_capture: true,
      artifact: { path: '/fixture.png', bytes: 128, sha256: 'a'.repeat(64) } };
  }
  lines.push('GXM CLOSE BEGIN', 'GXM CLOSE PASS', 'RESULT PASS', '');
  return { log: lines.join('\n'), captures };
}

test('active capture requires current ready fence and ends before retirement', () => {
  const begin = 'STAGE BACKGROUND_DXT1 BEGIN source=background-dxt1.gxt\n';
  const ready = begin + 'STAGE BACKGROUND_DXT1 FRAME READY fence=COMPLETE\n';
  assert.equal(activeCompressedStage(begin), undefined);
  assert.equal(activeCompressedStage(ready), 'BACKGROUND_DXT1');
  for (const ending of ['STAGE BACKGROUND_DXT1 RETIRE BEGIN', 'RESULT PASS', 'ERROR failure',
    'GXM CLOSE BEGIN', 'STAGE PORTRAIT_DXT5 BEGIN source=portrait-dxt5.gxt',
    'STAGE UNKNOWN BEGIN source=background-dxt1.gxt', 'STAGE BACKGROUND_DXT1 BEGIN source=wrong.gxt']) {
    assert.equal(activeCompressedStage(ready + ending + '\n'), undefined);
  }
});

test('all seven active captures, measured frames and checked retirements are necessary', () => {
  const { log, captures } = fixture();
  assert.equal(compressedResultEvidence(log, captures).valid, true);
  for (const marker of ['TITLE_ID KNIF00012', 'GXM CLOSE PASS', 'STAGE PORTRAIT_DXT5 RETIRE BEGIN',
    'STAGE BACKGROUND_DXT1 LOAD encoded_bytes=64 read_us=12 decode_us=0 upload_us=4 mapped_bytes=4096',
    'STAGE BACKGROUND_DXT5 FRAMES count=60 elapsed_us=2000 authority=CURRENT_RUNTIME']) {
    assert.equal(compressedResultEvidence(log.replace(marker, ''), captures).valid, false);
  }
  assert.equal(compressedResultEvidence(log + 'RESULT PASS\n', captures).valid, false);
  assert.equal(compressedResultEvidence(log.replace('GXM CLOSE PASS', 'ERROR failure\nGXM CLOSE PASS'), captures).valid, false);
});

test('stale/ambiguous captures and moved measurement lines fail closed', () => {
  const { log, captures } = fixture();
  for (const bad of [{ captured: false }, { stage: 'BACKGROUND_RGBA' }, { stage_valid_during_capture: false },
    { artifact: { path: '/fixture.png', bytes: 128, sha256: 'wrong' } }]) {
    const changed = structuredClone(captures);
    Object.assign(changed.PORTRAIT_DXT5, bad);
    assert.equal(compressedResultEvidence(log, changed).valid, false);
  }
  const load = 'STAGE BACKGROUND_DXT1 LOAD encoded_bytes=64 read_us=12 decode_us=0 upload_us=4 mapped_bytes=4096\n';
  const moved = log.replace(load, '').replace('STAGE BACKGROUND_DXT1 RETIRE BEGIN\n', 'STAGE BACKGROUND_DXT1 RETIRE BEGIN\n' + load);
  assert.equal(compressedResultEvidence(moved, captures).valid, false);
  assert.equal(compressedResultEvidence(log.replace('STAGE BACKGROUND_DXT1 GXT logical=960x540 runtime_decode=NONE',
    'STAGE BACKGROUND_DXT1 GXT logical=960x540 runtime_decode=CPU'), captures).valid, false);
});
