// Finite synthetic stages. A previous READY marker cannot authorize a later capture.
export const COMPRESSED_STAGES = Object.freeze({
  BACKGROUND_RGBA: 'background.png', BACKGROUND_PVRTC4: 'background-pvrtc4.gxt',
  BACKGROUND_DXT1: 'background-dxt1.gxt', BACKGROUND_DXT5: 'background-dxt5.gxt',
  PORTRAIT_RGBA: 'portrait.png', PORTRAIT_PVRTC4: 'portrait-pvrtc4.gxt',
  PORTRAIT_DXT5: 'portrait-dxt5.gxt',
});
export const COMPRESSED_INPUTS = Object.freeze([...Object.values(COMPRESSED_STAGES), 'background.rgba', 'portrait.rgba']);

export function activeCompressedStage(log) {
  let active, ready = false;
  for (const line of log.split('\n')) {
    const begin = /^STAGE ([A-Z0-9_]+) BEGIN source=(\S+)$/.exec(line);
    if (begin) {
      active = Object.hasOwn(COMPRESSED_STAGES, begin[1]) && COMPRESSED_STAGES[begin[1]] === begin[2] ? begin[1] : undefined;
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

export function compressedResultEvidence(log, captures) {
  const lines = log.split('\n'), issues = [], stages = [];
  const exact = text => lines.filter(line => line === text).length;
  const only = prefix => {
    const matches = lines.filter(line => line.startsWith(prefix));
    return matches.length === 1 ? matches[0].slice(prefix.length) : undefined;
  };
  if (exact('TITLE_ID KNIF00012') !== 1 || exact('RESULT PASS') !== 1
      || /^RESULT FAIL|^ERROR /m.test(log) || !log.endsWith('RESULT PASS\n')) issues.push('Missing unambiguous compressed probe identity/result');
  if (exact('GXM CLOSE BEGIN') !== 1 || exact('GXM CLOSE PASS') !== 1) issues.push('Missing checked GXM teardown');
  let last = -1;
  for (const [stage, source] of Object.entries(COMPRESSED_STAGES)) {
    const markers = [`STAGE ${stage} BEGIN source=${source}`, `STAGE ${stage} FRAME READY fence=COMPLETE`,
      `STAGE ${stage} RETIRE BEGIN`, `STAGE ${stage} PASS frames=61 texture_retired=true`];
    let ordered = true;
    for (const marker of markers) {
      const position = lines.indexOf(marker);
      if (exact(marker) !== 1 || position <= last) ordered = false;
      last = position;
    }
    const load = /^encoded_bytes=([1-9][0-9]*) read_us=([0-9]+) decode_us=([0-9]+) upload_us=([0-9]+) mapped_bytes=([1-9][0-9]*)$/.exec(only(`STAGE ${stage} LOAD `) ?? '');
    const frames = /^count=60 elapsed_us=([1-9][0-9]*) authority=CURRENT_RUNTIME$/.exec(only(`STAGE ${stage} FRAMES `) ?? '');
    const loadPosition = lines.findIndex(line => line.startsWith(`STAGE ${stage} LOAD `));
    const framePosition = lines.findIndex(line => line.startsWith(`STAGE ${stage} FRAMES `));
    ordered &&= loadPosition > lines.indexOf(markers[0]) && loadPosition < lines.indexOf(markers[1])
      && framePosition > lines.indexOf(markers[1]) && framePosition < lines.indexOf(markers[2]);
    const capture = captures?.[stage];
    const captured = capture?.captured === true && capture.stage === stage
      && capture.stage_valid_during_capture === true && typeof capture.artifact?.path === 'string'
      && Number.isSafeInteger(capture.artifact.bytes) && capture.artifact.bytes > 25
      && /^[a-f0-9]{64}$/.test(capture.artifact.sha256 ?? '');
    const compressed = source.endsWith('.gxt');
    const valid = ordered && Boolean(load && frames) && captured
      && (!compressed || (Number(load?.[3]) === 0 && (only(`STAGE ${stage} GXT `) ?? '').endsWith(' runtime_decode=NONE')));
    stages.push({ stage, source, valid, captured, lifecycle_ordered: ordered,
      ...(load ? { encoded_bytes: Number(load[1]), read_us: Number(load[2]), decode_us: Number(load[3]),
        upload_us: Number(load[4]), mapped_bytes: Number(load[5]) } : {}),
      ...(frames ? { frame_count: 60, elapsed_us: Number(frames[1]), timing_authority: 'Vita3K emulator only' } : {}) });
    if (!valid) issues.push(`Incomplete active capture, lifecycle or load/frame measurements: ${stage}`);
  }
  return { valid: issues.length === 0, issues, stages, pixel_verified: false, hardware_verified: false };
}
