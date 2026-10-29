// Bounded real-controller save/load evidence. No guest memory or script writes.
import { createHash } from 'node:crypto';
import { parseGameLog } from './vita-game-evidence.mjs';

export const SAVE_BUTTONS = Object.freeze({ triangle: 0x1000, cross: 0x4000, circle: 0x2000, down: 0x40 });
const hash = value => createHash('sha256').update(value).digest('hex');
const clone = value => JSON.parse(JSON.stringify(value));
const uint = value => Number.isSafeInteger(value) && value >= 0;
const point = value => value && uint(value.scene) && value.scene > 0 && uint(value.pc);
const samePoint = (a, b) => point(a) && point(b) && a.scene === b.scene && a.pc === b.pc;
const typed = s => s && ['state', 'target', 'manual_count', 'dialog_active', 'popup_busy', 'cool'].every(k => uint(s[k]))
  && ['page', 'slot', 'lock', 'dialog_answer'].every(k => Number.isSafeInteger(s[k]))
  && ['saving', 'tabrow', 'selected_used', 'can_save'].every(k => typeof s[k] === 'boolean')
  && (s.point === null || point(s.point));
const idle = v => v?.buttons === 0 && v.save.dialog_active === 0 && v.save.popup_busy === 0;
const story = v => idle(v) && v.screen === 'Game' && v.overlay === 'None' && !v.paused && !v.ended
  && !v.menu_lock && v.wait === 'Click' && v.save.can_save && point(v) && point(v.save.point)
  && v.page_visible && v.page_waiting && v.page_order > 0 && v.page_revealed >= v.page_order;
const menu = (v, cursor) => idle(v) && v.screen === 'Witch' && v.overlay === 'Menu'
  && v.menu_state === 3 && v.menu_cursor === cursor;
const list = (v, saving, used) => idle(v) && v.screen === 'Witch' && v.overlay === 'Menu' && v.menu_state === 5
  && v.save.state === 2 && v.save.saving === saving && v.save.page === 0 && v.save.slot === 0
  && !v.save.tabrow && v.save.lock === 0 && v.save.selected_used === used && v.save.manual_count === +used;
const dialog = (v, saving) => v?.buttons === 0 && v.screen === 'Witch' && v.overlay === 'Menu'
  && v.menu_state === 5 && v.save.state === (saving ? 6 : 17) && v.save.saving === saving
  && v.save.dialog_active === 2 && v.save.dialog_answer === 0 && v.save.target === 0;
// save.point is the start of a replay block and can cover several displayed
// instructions. Observe advancement at the current scene/pc; restoration must
// recover that instruction and its visible page shape as well as the replay start.
const samePage = (a, b) => ['page_messages', 'page_chars', 'page_order'].every(k => a?.[k] === b?.[k]);
const changed = (v, origin) => story(v) && !samePoint(v, origin);
const restored = (v, origin) => story(v) && samePoint(v, origin) && samePage(v, origin)
  && samePoint(v.save.point, origin?.save.point) && v.save.manual_count === 1;
const PLAN = [
  ['triangle', v => story(v) && v.save.manual_count === 0, v => menu(v, 0)],
  ['cross', v => menu(v, 0), v => list(v, true, false) && v.save.cool === 0],
  ['cross', v => list(v, true, false) && v.save.cool === 0, v => dialog(v, true)],
  ['cross', v => dialog(v, true), v => list(v, true, true)],
  ['circle', v => list(v, true, true), v => menu(v, 0)],
  ['circle', v => menu(v, 0), (v, origin) => restored(v, origin)],
  ['cross', (v, origin) => restored(v, origin), changed],
  ['triangle', changed, v => menu(v, 0)],
  ['down', v => menu(v, 0), v => menu(v, 1)],
  ['cross', v => menu(v, 1), v => list(v, false, true) && v.save.cool === 0],
  ['cross', v => list(v, false, true) && v.save.cool === 0, v => dialog(v, false)],
  ['cross', v => dialog(v, false), restored],
];

export function inspectWitchSave(bytes) {
  const issues = [];
  if (!Buffer.isBuffer(bytes) || bytes.length !== 0x3fb800) return { valid: false, issues: ['Wrong Witch save file size'] };
  const manual = [];
  for (let n = 0; n < 120; n += 1) if (bytes.readUInt16LE(7168 + 30208 * n + 2)) manual.push(n);
  if (manual.length !== 1 || manual[0] !== 0) issues.push('Expected exactly the newly written manual slot 0');
  const record = bytes.subarray(7168, 7168 + 28112);
  const magic = record.readUInt16LE(0), used = record.readUInt16LE(2), scene = record.readUInt16LE(32);
  const offset = record.readBigInt64LE(16), next = record.readBigInt64LE(24);
  if (magic !== 1000 || used !== 1 || scene < 1 || scene > 245) issues.push('Invalid Witch slot 0 header/scene');
  if (offset < 0 || next <= offset) issues.push('Invalid saved script byte offsets');
  return { valid: !issues.length, issues, bytes: bytes.length, sha256: hash(bytes), record_sha256: hash(record),
    magic, used, scene, script_offset: offset.toString(), script_next: next.toString(), manual_slots: manual };
}

function anchorValid(log, parsed, anchor, length) {
  if (!uint(length) || length > log.length || !anchor) return false;
  const actual = parsed.states.filter(r => r.end_offset <= length).at(-1);
  return actual && JSON.stringify(actual) === JSON.stringify(anchor);
}
function pulseEvidence(log, parsed, step, index, origin) {
  const issues = [], [button, allowed, outcome] = PLAN[index] ?? [];
  if (step.button !== button || step.mask !== SAVE_BUTTONS[button]) issues.push('Wrong ordered button');
  if (!anchorValid(log, parsed, step.before_state, step.before_log_length)
      || hash(log.slice(0, step.before_log_length)) !== step.before_log_sha256) issues.push('Stale or changed authorized STATE anchor');
  const before = step.before_state?.value;
  if (!before || !typed(before.save) || !allowed?.(before, origin)) issues.push('Button not authorized by actual save UI');
  if (!step.set_completed || !step.clear_completed || step.set_error || step.clear_error
      || step.set_result?.ok === false || step.clear_result?.ok === false
      || !Number.isFinite(step.held_ms) || step.held_ms < 1 || step.held_ms > 1000) issues.push('Button set/clear/hold failed');
  const end = step.observed_until ?? log.length;
  if (!uint(end) || end > log.length || end < step.before_log_length) issues.push('Invalid outcome boundary');
  const records = parsed.records.filter(r => r.offset >= step.before_log_length && r.end_offset <= end);
  const pressed = records.find(r => r.kind === 'INPUT' && r.value.buttons === step.mask && r.value.frame > (before?.frame ?? Infinity));
  const released = records.find(r => r.kind === 'INPUT' && pressed && r.offset > pressed.offset
    && r.value.buttons === 0 && r.value.frame > pressed.value.frame);
  const paired = r => records.some(s => s.kind === 'STATE' && r && s.offset > r.offset
    && s.value.frame === r.value.frame && s.value.buttons === r.value.buttons);
  if (!pressed || !paired(pressed)) issues.push('Missing actual INPUT/STATE press');
  if (!released || !paired(released)) issues.push('Missing actual INPUT/STATE release');
  // Only an idle post-release STATE can authorize the next operation.
  const transition = records.find(r => r.kind === 'STATE' && released && r.offset > released.offset
    && typed(r.value.save) && outcome?.(r.value, origin));
  if (!transition) issues.push('Missing actual save UI/script outcome');
  return { valid: !issues.length, issues, index, button, pressed: pressed ?? null, released: released ?? null,
    transition: transition ?? null };
}
function captureEvidence(log, parsed, capture, bytes, index, steps, origin) {
  const issues = [], kind = index === 0 ? 'capture-saved' : 'capture-restored';
  if (!capture || !Buffer.isBuffer(bytes)) return { valid: false, issues: ['Missing capture or original save bytes'] };
  const test = index === 0 ? v => list(v, true, true) : v => restored(v, origin);
  for (const phase of ['before', 'after']) {
    const record = capture[`${phase}_state`], length = capture[`${phase}_log_length`];
    if (!anchorValid(log, parsed, record, length) || !typed(record?.value.save) || !test(record.value)
        || hash(log.slice(0, length)) !== capture[`${phase}_log_sha256`]) issues.push(`Invalid ${kind} ${phase} STATE`);
  }
  const previous = steps[index === 0 ? 3 : 11], next = index === 0 ? steps[4] : null;
  if (capture.kind !== kind || capture.captured !== true || capture.before_log_length > capture.after_log_length
      || previous?.observed_until > capture.before_log_length || !uint(previous?.observed_until)
      || (next && capture.after_log_length > next.before_log_length)) issues.push('Capture is outside its ordered save/load boundary');
  const a = capture.artifact;
  if (!a || a.width !== 960 || a.height !== 544 || typeof a.path !== 'string'
      || !uint(a.bytes) || a.bytes < 1 || a.bytes > 16 * 1024 * 1024 || !/^[a-f0-9]{64}$/.test(a.sha256)) issues.push('Missing identified 960x544 PNG');
  const file = inspectWitchSave(bytes);
  if (!file.valid || JSON.stringify(capture.file) !== JSON.stringify(file) || file.scene !== origin?.save.point?.scene) issues.push('Save bytes differ from verified slot metadata or saved scene');
  return { valid: !issues.length, issues, kind, file };
}

export function gameSaveEvidence(log, receipt, files = []) {
  const parsed = parseGameLog(log), issues = [...parsed.issues];
  if (parsed.states.some(r => !typed(r.value.save))) issues.push('Missing typed readonly save observation');
  if (receipt?.schema !== 'knife-vita-game-save-inputs-v1') issues.push('Missing save/load scenario');
  if (receipt?.error) issues.push(receipt.error);
  const steps = receipt?.steps;
  if (!Array.isArray(steps) || steps.length !== PLAN.length) issues.push('Expected twelve ordered save/load controller pulses');
  if (!Array.isArray(steps) || steps.length > PLAN.length) return { valid: false, issues };
  const origin = steps[0]?.before_state?.value;
  const observations = steps.map((s, i) => pulseEvidence(log, parsed, s, i, origin));
  observations.forEach(s => issues.push(...s.issues.map(issue => `Step ${s.index}: ${issue}`)));
  for (let i = 1; i < steps.length; i += 1) {
    if (!uint(steps[i - 1].observed_until) || steps[i - 1].observed_until > steps[i].before_log_length) issues.push('Overlapping or unordered pulses');
  }
  const captures = [0, 1].map(i => captureEvidence(log, parsed, receipt?.captures?.[i], files[i], i, steps, origin));
  captures.forEach(c => issues.push(...c.issues));
  if (captures[0].file?.record_sha256 !== captures[1].file?.record_sha256) issues.push('Saved slot bytes changed between save and load');
  return { valid: !issues.length, issues, steps: observations, captures, saved_point: origin?.save.point,
    displayed_point: origin && { scene: origin.scene, pc: origin.pc, page_messages: origin.page_messages,
      page_chars: origin.page_chars, page_order: origin.page_order },
    limitation: 'One empty manual slot, finite controller/save-file/restore observations in an isolated DEV profile; no hardware or full-playthrough claim' };
}

export class GameSaveScenario {
  constructor({ nowMs = Date.now(), stepTimeoutMs = 30000, startTimeoutMs = 90000 } = {}) {
    this.receipt = { schema: 'knife-vita-game-save-inputs-v1', steps: [], captures: [], completed: false };
    this.files = []; this.deadline = nowMs + startTimeoutMs; this.stepTimeoutMs = stepTimeoutMs;
  }
  observe(log, nowMs = Date.now()) {
    this.lastLog = log;
    const r = this.receipt;
    if (r.error || r.completed) return null;
    const parsed = parseGameLog(log);
    r.latest_state = parsed.latest_state;
    if (parsed.issues.length || parsed.states.some(s => !typed(s.value.save))) {
      r.error = [...parsed.issues, 'Missing or invalid typed save observation'].join('; '); return null;
    }
    if (nowMs > this.deadline) { r.error = `Timed out at save/load step ${r.steps.length}`; return null; }
    const current = r.steps.at(-1), origin = r.steps[0]?.before_state.value;
    if (current && !current.observed_until) {
      const proof = pulseEvidence(log, parsed, current, r.steps.length - 1, origin);
      current.last_observation = proof;
      if (!proof.valid) return null;
      current.observed_until = log.length;
      current.observed_log_sha256 = hash(log); this.deadline = nowMs + this.stepTimeoutMs;
    }
    const latest = parsed.latest_state?.value;
    if (!latest) return null;
    if (r.steps.length === 4 && r.captures.length === 0) return { kind: 'capture-saved' };
    if (r.steps.length === PLAN.length) {
      if (r.captures.length === 1) return { kind: 'capture-restored' };
      const proof = gameSaveEvidence(log, r, this.files);
      if (proof.valid) r.completed = true;
      else r.error = proof.issues.join('; ');
      return null;
    }
    const [button, allowed] = PLAN[r.steps.length];
    return allowed(latest, origin) ? { kind: 'pulse', button } : null;
  }
  beginPulse(button, log, nowMs = Date.now()) {
    const action = this.observe(log, nowMs);
    if (action?.kind !== 'pulse' || action.button !== button) throw new Error(`No authorized ${button} save/load pulse`);
    const step = { button, mask: SAVE_BUTTONS[button], before_state: clone(parseGameLog(log).latest_state),
      before_log_length: log.length, before_log_sha256: hash(log), requested_at_ms: nowMs };
    this.receipt.steps.push(step); this.deadline = nowMs + this.stepTimeoutMs; return step;
  }
  endPulse(step, observation) {
    Object.assign(step, clone(observation));
    step.set_completed = Object.hasOwn(observation, 'set_result') && !observation.set_error;
    step.clear_completed = Object.hasOwn(observation, 'clear_result') && !observation.clear_error;
    if (!step.set_completed || !step.clear_completed || step.set_result?.ok === false || step.clear_result?.ok === false
        || !Number.isFinite(step.held_ms) || step.held_ms < 1 || step.held_ms > 1000) this.receipt.error = 'Save/load controller pulse failed';
  }
  recordCapture(kind, capture, bytes, nowMs = Date.now(), log = this.lastLog) {
    const action = this.observe(log, nowMs), r = this.receipt, index = r.captures.length;
    if (action?.kind !== kind || !['capture-saved', 'capture-restored'].includes(kind)) throw new Error('No authorized save/load capture');
    const candidate = { kind, ...clone(capture), file: inspectWitchSave(bytes) };
    const proof = captureEvidence(log, parseGameLog(log), candidate, bytes, index, r.steps, r.steps[0].before_state.value);
    if (!proof.valid) { r.error = proof.issues.join('; '); return; }
    r.captures.push(candidate); this.files.push(Buffer.from(bytes)); this.deadline = nowMs + this.stepTimeoutMs;
  }
}
