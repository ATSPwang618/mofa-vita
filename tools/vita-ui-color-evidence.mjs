// Production-native synthetic color stages; captures must retain live log prefixes.
import { createHash } from 'node:crypto';

export const UI_COLOR_STAGES = Object.freeze(['UI_RUNTIME_ALTER', 'UI_OFFLINE_RGBA', 'UI_OFFLINE_DXT5']);
export const UI_COLOR_INPUTS = Object.freeze(['ui-source.rgba', 'ui-checker.rgba', 'ui-offline.alter.rgba', 'ui-offline.gxt']);
export const UI_COLOR_CONTRACT = 'SYNTHETIC UI COLOR; logical=320x224 physical=160x112 BC_backing=256x128; source_srgb=true canvas_srgb=true linear_clamp=true straight_source_alpha=true';
export const UI_COLOR_PATH = 'PATH production-native Sprite/Alpha -> RGBA8 sRGB canvas -> Present; no facade/cache/archive equivalence claim';
const hash = value => createHash('sha256').update(value).digest('hex');

export function activeUiColorStage(log) {
  let active, ready = false;
  for (const line of log.split('\n')) {
    const begin = /^STAGE (\S+) BEGIN/.exec(line);
    if (begin) {
      active = UI_COLOR_STAGES.includes(begin[1]) && line === `STAGE ${begin[1]} BEGIN` ? begin[1] : undefined;
      ready = false;
      continue;
    }
    if (active && line === `STAGE ${active} FRAME READY fence=COMPLETE`) ready = true;
    if (active && (line === `STAGE ${active} RETIRE BEGIN` || line.startsWith(`STAGE ${active} PASS `))) {
      active = undefined; ready = false;
    }
    if (/^(RESULT |GXM CLOSE |ERROR )/.test(line)) { active = undefined; ready = false; }
  }
  return ready ? active : undefined;
}

export function uiColorResultEvidence(log, captures) {
  const lines = log.split('\n'), issues = [], stages = [], usedPaths = new Set();
  const exact = text => lines.filter(line => line === text).length;
  const matching = prefix => lines.filter(line => line.startsWith(prefix));
  if (exact('TITLE_ID KNIF00013') !== 1 || exact(UI_COLOR_CONTRACT) !== 1 || exact(UI_COLOR_PATH) !== 1
      || matching('TITLE_ID ').length !== 1 || matching('RESULT ').length !== 1
      || exact('RESULT PASS') !== 1 || !log.endsWith('RESULT PASS\n') || /^ERROR |^STAGE .*\bFAIL\b/m.test(log)) {
    issues.push('Missing unambiguous production UI color identity, contract or result');
  }
  const ready = matching('GXM READY ');
  if (ready.length !== 1 || !/^GXM READY baseline_cdram=[1-9][0-9]* common_cdram=[1-9][0-9]*$/.test(ready[0])) {
    issues.push('Missing measured GXM/common allocation baseline');
  }
  if (exact('GXM CLOSE BEGIN') !== 1 || exact('GXM CLOSE PASS') !== 1 || matching('GXM CLOSE ').length !== 2) {
    issues.push('Missing checked GXM teardown');
  }
  if (lines.filter(line => /^STAGE \S+ BEGIN$/.test(line)).length !== 3) issues.push('Unexpected or duplicate stage');
  let last = lines.indexOf(ready[0]);
  for (const stage of UI_COLOR_STAGES) {
    const markers = [`STAGE ${stage} BEGIN`, `STAGE ${stage} FRAME READY fence=COMPLETE`,
      `STAGE ${stage} RETIRE BEGIN`, `STAGE ${stage} PASS frames=61 texture_retired=true`];
    let ordered = true;
    for (const marker of markers) {
      const position = lines.indexOf(marker);
      if (exact(marker) !== 1 || position <= last) ordered = false;
      last = position;
    }
    const loads = matching(`STAGE ${stage} LOAD `);
    const load = loads.length === 1 ? /^STAGE \S+ LOAD elapsed_us=([0-9]+) mapped_bytes=([1-9][0-9]*)$/.exec(loads[0]) : undefined;
    const loadPosition = lines.indexOf(loads[0]);
    ordered &&= loadPosition > lines.indexOf(markers[0]) && loadPosition < lines.indexOf(markers[1]);
    const capture = captures?.[stage];
    const prefix = state => {
      if (!state || state.active_stage !== stage || !Number.isSafeInteger(state.log_length)
          || state.log_length < 1 || state.log_length > log.length || state.session?.phase !== 'running'
          || state.session.frameReady !== true || state.session.resolution?.width !== 960
          || state.session.resolution?.height !== 544) return false;
      const actual = log.slice(0, state.log_length);
      return actual.endsWith('\n') && hash(actual) === state.log_sha256 && activeUiColorStage(actual) === stage;
    };
    const artifact = capture?.artifact;
    const captured = capture?.captured === true && capture.stage === stage && capture.stage_valid_during_capture === true
      && prefix(capture.state_before) && prefix(capture.state_after)
      && capture.state_before.log_length <= capture.state_after.log_length
      && typeof artifact?.path === 'string' && artifact.path.length > 0 && !usedPaths.has(artifact.path)
      && Number.isSafeInteger(artifact.bytes) && artifact.bytes > 25 && /^[a-f0-9]{64}$/.test(artifact.sha256 ?? '');
    if (typeof artifact?.path === 'string') usedPaths.add(artifact.path);
    const valid = ordered && Boolean(load) && captured;
    stages.push({ stage, valid, captured, lifecycle_ordered: ordered,
      ...(load ? { load_us: Number(load[1]), mapped_bytes: Number(load[2]), timing_authority: 'Vita3K emulator only' } : {}) });
    if (!valid) issues.push(`Missing live capture, load measurement or ordered lifecycle: ${stage}`);
  }
  if (lines.indexOf('GXM CLOSE BEGIN') <= last || lines.indexOf('GXM CLOSE PASS') <= lines.indexOf('GXM CLOSE BEGIN')
      || lines.indexOf('RESULT PASS') <= lines.indexOf('GXM CLOSE PASS')) issues.push('Teardown/result order differs');
  return { valid: issues.length === 0, issues, stages, pixel_verified: false, hardware_verified: false };
}
