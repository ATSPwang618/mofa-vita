// Finite real-controller backlog regression. Only guest-published STATE/INPUT
// records authorize actions; no script, save, flag or UI-state writes.
import { createHash } from 'node:crypto';
import { GameInputScenario, gameInteractionEvidence, parseGameLog } from './vita-game-evidence.mjs';

export const BACKLOG_BUTTONS = Object.freeze({ cross: 0x4000, triangle: 0x1000, circle: 0x2000,
  square: 0x8000, up: 0x10, down: 0x40 });
const hash = value => createHash('sha256').update(value).digest('hex');
const clone = value => JSON.parse(JSON.stringify(value));
const uint = value => Number.isSafeInteger(value) && value >= 0;
const typed = b => b && ['state', 'entries', 'available'].every(k => uint(b[k]))
  && ['cursor', 'scroll', 'target'].every(k => Number.isSafeInteger(b[k]));
const story = v => v && typed(v.backlog) && v.buttons === 0 && v.screen === 'Game'
  && v.overlay === 'None' && !v.paused && !v.ended && !v.menu_lock && v.wait === 'Click'
  && v.page_visible && v.page_waiting && v.page_order > 0 && v.page_revealed >= v.page_order;
const opened = v => v && typed(v.backlog) && v.buttons === 0 && v.screen === 'Witch'
  && v.overlay === 'Backlog' && v.backlog.state === 5 && v.backlog.entries >= 6
  && v.backlog.scroll === v.backlog.target;
const sameStory = (a, b) => ['scene', 'pc', 'page_messages', 'page_chars', 'page_order']
  .every(k => a?.[k] === b?.[k]);
const FIXED = ['open', 'older1', 'older2', 'older3', 'older4', 'newer', 'close', 'reopen', 'close-again'];
const CAPTURES = [
  { at: 1, kind: 'capture-backlog-open', test: v => opened(v) && v.backlog.cursor === 0 },
  { at: 5, kind: 'capture-backlog-scroll', test: v => opened(v) && v.backlog.cursor === 4 && v.backlog.scroll < 0 },
  { at: 9, kind: 'capture-backlog-return', test: story },
];

function definition(steps, index) {
  const previous = steps.slice(0, index), advanceCount = previous.filter(s => s.phase === 'advance').length;
  const fixedCount = previous.length - advanceCount;
  if (steps[index]?.phase === 'advance') return fixedCount === 0 && advanceCount < 12 ? {
    phase: 'advance', button: 'cross', allowed: v => story(v) && v.backlog.available < 6,
    outcome: (v, before) => story(v) && v.scene === before.scene && v.pc !== before.pc,
  } : null;
  const phase = FIXED[fixedCount];
  if (!phase) return null;
  if (phase === 'open' || phase === 'reopen') return { phase, button: 'square',
    allowed: v => story(v) && v.backlog.available >= 6,
    outcome: (v, before) => opened(v) && sameStory(v, before) && v.backlog.cursor === 0
      && v.backlog.scroll === 8 && v.backlog.entries === before.backlog.available };
  if (phase.startsWith('older') || phase === 'newer') {
    const expected = phase === 'newer' ? 4 : Number(phase.slice(-1)) - 1;
    const delta = phase === 'newer' ? -1 : 1;
    return { phase, button: delta > 0 ? 'up' : 'down',
      allowed: v => opened(v) && v.backlog.cursor === expected,
      outcome: (v, before) => opened(v) && sameStory(v, before)
        && v.backlog.entries === before.backlog.entries && v.backlog.cursor === expected + delta
        && (phase !== 'older4' || v.backlog.scroll < 0) };
  }
  return { phase, button: 'circle', allowed: opened,
    outcome: (v, before) => story(v) && sameStory(v, before) && v.backlog.state === 0 };
}

function anchorValid(log, parsed, anchor, length, digest) {
  return uint(length) && length <= log.length && anchor
    && JSON.stringify(parsed.states.filter(r => r.end_offset <= length).at(-1)) === JSON.stringify(anchor)
    && hash(log.slice(0, length)) === digest;
}
function pulseEvidence(log, parsed, steps, index) {
  const step = steps[index], spec = definition(steps, index), before = step.before_state?.value, issues = [];
  if (!spec || step.phase !== spec.phase || step.button !== spec.button
      || step.mask !== BACKLOG_BUTTONS[step.button]) issues.push('Wrong ordered backlog button/phase');
  if (!anchorValid(log, parsed, step.before_state, step.before_log_length, step.before_log_sha256))
    issues.push('Stale or changed authorized STATE anchor');
  if (!spec?.allowed(before)) issues.push('Button not authorized by actual backlog/story state');
  if (!step.set_completed || !step.clear_completed || step.set_error || step.clear_error
      || step.set_result?.ok === false || step.clear_result?.ok === false
      || !Number.isFinite(step.held_ms) || step.held_ms < 1 || step.held_ms > 1000)
    issues.push('Button set/clear/hold failed');
  const end = step.observed_until ?? log.length;
  if (!uint(end) || end > log.length || end < step.before_log_length) issues.push('Invalid outcome boundary');
  const records = parsed.records.filter(r => r.offset >= step.before_log_length && r.end_offset <= end);
  const pressed = records.find(r => r.kind === 'INPUT' && r.value.buttons === step.mask && r.value.frame > (before?.frame ?? Infinity));
  const released = records.find(r => r.kind === 'INPUT' && pressed && r.offset > pressed.offset
    && r.value.buttons === 0 && r.value.frame > pressed.value.frame);
  const paired = input => input && records.some(r => r.kind === 'STATE' && r.offset > input.offset
    && r.value.frame === input.value.frame && r.value.buttons === input.value.buttons);
  if (!paired(pressed) || !paired(released)) issues.push('Missing actual paired INPUT/STATE press/release');
  const transition = records.find(r => r.kind === 'STATE' && released && r.offset > released.offset
    && typed(r.value.backlog) && spec?.outcome(r.value, before));
  if (!transition) issues.push('Missing actual backlog cursor/scroll/return outcome');
  return { valid: !issues.length, issues, phase: step.phase, pressed: pressed ?? null,
    released: released ?? null, transition: transition ?? null };
}
function captureEvidence(log, parsed, capture, spec, steps) {
  const issues = [], fixed = steps.map((step, index) => ({ ...step, index })).filter(s => s.phase !== 'advance');
  const previous = fixed[spec.at - 1], next = fixed[spec.at], origin = fixed[0]?.before_state?.value;
  if (!capture) return { valid: false, issues: [`Missing ${spec.kind}`] };
  if (capture.kind !== spec.kind || capture.captured !== true || !uint(previous?.observed_until)
      || previous.observed_until > capture.before_log_length || capture.before_log_length > capture.after_log_length
      || (next && capture.after_log_length > next.before_log_length)) issues.push('Capture is outside its ordered backlog phase');
  for (const phase of ['before', 'after']) {
    const anchor = capture[`${phase}_state`];
    if (!anchorValid(log, parsed, anchor, capture[`${phase}_log_length`], capture[`${phase}_log_sha256`])
        || !spec.test(anchor?.value) || !sameStory(origin, anchor?.value)) issues.push('Capture lacks matching actual backlog/script STATE');
  }
  const a = capture.artifact;
  if (typeof a?.path !== 'string' || !uint(a.bytes) || a.bytes < 1 || a.bytes > 16 * 1024 * 1024
      || a.width !== 960 || a.height !== 544 || !/^[a-f0-9]{64}$/.test(a.sha256)) issues.push('Missing identified 960x544 PNG');
  return { valid: !issues.length, issues, kind: spec.kind };
}

export function gameBacklogEvidence(log, receipt) {
  const parsed = parseGameLog(log), issues = [...parsed.issues];
  const intro = gameInteractionEvidence(log, receipt?.intro);
  if (!intro.valid) issues.push(...intro.issues.map(i => `Menu/return: ${i}`));
  if (parsed.states.some(r => !typed(r.value.backlog))) issues.push('Missing typed readonly backlog observation');
  if (receipt?.schema !== 'knife-vita-game-backlog-inputs-v1' || receipt?.error) issues.push(receipt?.error ?? 'Missing backlog scenario');
  const steps = receipt?.steps;
  if (!Array.isArray(steps) || steps.length > 21) return { valid: false, issues: [...issues, 'Invalid bounded backlog steps'] };
  const fixed = steps.filter(s => s.phase !== 'advance');
  if (fixed.length !== FIXED.length) issues.push('Expected opening, four older entries, newer entry, close, reopen and close');
  const observations = steps.map((s, i) => pulseEvidence(log, parsed, steps, i));
  observations.forEach((s, i) => issues.push(...s.issues.map(issue => `Step ${i}: ${issue}`)));
  for (let i = 0; i < steps.length; i++) {
    const previous = i ? steps[i - 1] : receipt?.intro?.steps?.at(-1);
    if (!uint(previous?.observed_until) || previous.observed_until > steps[i].before_log_length)
      issues.push('Overlapping or unordered controller pulses');
  }
  const captures = CAPTURES.map((spec, i) => captureEvidence(log, parsed, receipt?.captures?.[i], spec, steps));
  captures.forEach(c => issues.push(...c.issues));
  return { valid: !issues.length, issues, intro, steps: observations, captures, latest_state: parsed.latest_state,
    limitation: 'Finite real-controller backlog/menu/history observations and captured frames; no pixel, voice, jump, full-game or hardware claim' };
}

export class GameBacklogScenario {
  constructor({ nowMs = Date.now(), stepTimeoutMs = 30000 } = {}) {
    this.intro = new GameInputScenario({ nowMs, stepTimeoutMs });
    this.receipt = { schema: 'knife-vita-game-backlog-inputs-v1', intro: this.intro.receipt,
      minimum_entries: 6, maximum_advance_pulses: 12, steps: [], captures: [], completed: false };
    this.deadline = nowMs + 90000; this.stepTimeoutMs = stepTimeoutMs;
  }
  observe(log, nowMs = Date.now()) {
    this.lastLog = log;
    const r = this.receipt;
    if (r.error || r.completed) return null;
    const parsed = parseGameLog(log);
    if (parsed.issues.length || parsed.states.some(s => !typed(s.value.backlog))) {
      r.error = parsed.issues.join('; ') || 'Missing typed readonly backlog observation'; return null;
    }
    if (!this.intro.receipt.completed) {
      const action = this.intro.observe(log, nowMs);
      if (this.intro.receipt.error) r.error = this.intro.receipt.error;
      if (!this.intro.receipt.completed) return action;
      this.deadline = nowMs + this.stepTimeoutMs;
    }
    r.latest_state = parsed.latest_state;
    if (nowMs > this.deadline) { r.error = `Timed out waiting for actual backlog ${r.steps.at(-1)?.phase ?? 'story'} evidence`; return null; }
    const current = r.steps.at(-1);
    if (current && !current.observed_until) {
      current.last_observation = pulseEvidence(log, parsed, r.steps, r.steps.length - 1);
      if (!current.last_observation.valid) return null;
      current.observed_until = log.length;
      this.deadline = nowMs + this.stepTimeoutMs;
    }
    const fixedCount = r.steps.filter(s => s.phase !== 'advance').length;
    const capture = CAPTURES.find(s => s.at === fixedCount && !r.captures.some(c => c.kind === s.kind));
    if (capture) return capture.test(parsed.latest_state?.value) ? { kind: capture.kind } : null;
    if (fixedCount === FIXED.length) { r.completed = gameBacklogEvidence(log, r).valid; return null; }
    const latest = parsed.latest_state?.value;
    const phase = fixedCount === 0 && story(latest) && latest.backlog.available < 6 ? 'advance' : FIXED[fixedCount];
    const spec = definition([...r.steps, { phase }], r.steps.length);
    if (!spec && phase === 'advance') { r.error = 'Backlog story advancement input budget exhausted'; return null; }
    return spec?.allowed(latest) ? { kind: 'pulse', button: spec.button } : null;
  }
  beginPulse(button, log, nowMs = Date.now()) {
    const action = this.observe(log, nowMs);
    if (action?.kind !== 'pulse' || action.button !== button) throw new Error(`No authorized ${button} pulse`);
    if (!this.intro.receipt.completed) return this.intro.beginPulse(button, log, nowMs);
    const fixedCount = this.receipt.steps.filter(s => s.phase !== 'advance').length;
    const phase = button === 'cross' ? 'advance' : FIXED[fixedCount];
    const step = { phase, button, mask: BACKLOG_BUTTONS[button], before_state: clone(parseGameLog(log).latest_state),
      before_log_length: log.length, before_log_sha256: hash(log), requested_at_ms: nowMs };
    this.receipt.steps.push(step); this.deadline = nowMs + this.stepTimeoutMs;
    return step;
  }
  endPulse(step, observation) {
    if (this.intro.receipt.steps.includes(step)) return this.intro.endPulse(step, observation);
    Object.assign(step, clone(observation));
    step.set_completed = Object.hasOwn(observation, 'set_result') && !observation.set_error;
    step.clear_completed = Object.hasOwn(observation, 'clear_result') && !observation.clear_error;
    if (!step.set_completed || !step.clear_completed || step.set_result?.ok === false || step.clear_result?.ok === false
        || !Number.isFinite(step.held_ms) || step.held_ms < 1 || step.held_ms > 1000)
      this.receipt.error = `Button ${step.button} set/clear/hold failed`;
  }
  recordMenuCapture(capture, nowMs, log) { this.intro.recordMenuCapture(capture, nowMs, log); }
  recordBacklogCapture(kind, capture, nowMs = Date.now(), log = this.lastLog) {
    const spec = CAPTURES.find(s => s.kind === kind);
    if (!spec) throw new Error('Unknown backlog capture');
    const item = { ...clone(capture), kind };
    const evidence = captureEvidence(log, parseGameLog(log), item, spec, this.receipt.steps);
    if (!evidence.valid) { this.receipt.error = evidence.issues.join('; '); return; }
    this.receipt.captures.push(item); this.deadline = nowMs + this.stepTimeoutMs;
  }
}
