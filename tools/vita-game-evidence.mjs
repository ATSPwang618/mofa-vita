// Finite controller evidence only. This module never starts an emulator and
// never changes a guest flag, save, script counter, or UI state directly.
import { createHash } from 'node:crypto';

export const GAME_BUTTONS = Object.freeze({ cross: 0x4000, triangle: 0x1000, circle: 0x2000 });
const hash = value => createHash('sha256').update(value).digest('hex');
const digest = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const uint = (value, max = Number.MAX_SAFE_INTEGER) => Number.isSafeInteger(value) && value >= 0 && value <= max;
const clone = value => JSON.parse(JSON.stringify(value));
const boolFields = ['paused', 'ended', 'menu_lock', 'page_waiting', 'page_visible'];
const intFields = ['frame', 'title_state', 'scene', 'pc', 'page_messages', 'page_chars',
  'page_revealed', 'page_order'];

export function parseGameLog(log) {
  const result = { records: [], states: [], inputs: [], latest_state: null, issues: [], incomplete_line: false };
  if (typeof log !== 'string' || Buffer.byteLength(log) > 8 * 1024 * 1024) {
    result.issues.push('Game log exceeds the 8 MiB evidence budget or is not text');
    return result;
  }
  let offset = 0;
  let number = 0;
  let lastFrame = -1;
  for (const raw of log.split('\n')) {
    number += 1;
    const end = offset + raw.length + 1;
    if (end > log.length) { result.incomplete_line = raw.length > 0; break; }
    const text = raw.replace(/\r$/, '');
    const match = /^(STATE|INPUT) (.*)$/.exec(text);
    if (match) {
      try {
        if (raw.length > 8192 || result.records.length >= 20000) throw new Error('record budget exceeded');
        const value = JSON.parse(match[2]);
        if (!value || Array.isArray(value) || typeof value !== 'object'
            || !uint(value.frame) || !uint(value.buttons, 0xffffffff)) throw new Error('invalid frame/buttons');
        if (value.frame < lastFrame) throw new Error('frame reversal');
        if (match[1] === 'STATE') {
          if (!intFields.every(field => uint(value[field]))
              || !Number.isSafeInteger(value.title_cursor)
              || !uint(value.menu_state, 0xffffffff)
              || !Number.isSafeInteger(value.menu_cursor) || value.menu_cursor < -0x80000000
              || value.menu_cursor > 0x7fffffff
              || !boolFields.every(field => typeof value[field] === 'boolean')
              || !['screen', 'overlay', 'wait'].every(field => typeof value[field] === 'string'
                && value[field].length > 0 && value[field].length <= 512)) throw new Error('invalid typed STATE');
        } else if (Object.keys(value).some(key => !['frame', 'buttons'].includes(key))) {
          throw new Error('unexpected INPUT fields');
        }
        lastFrame = value.frame;
        const record = { kind: match[1], line: number, offset, end_offset: end, value };
        result.records.push(record);
        if (record.kind === 'STATE') { result.states.push(record); result.latest_state = record; }
        else result.inputs.push(record);
      } catch (error) {
        if (result.issues.length < 16) result.issues.push(`Line ${number}: ${error.message}`);
      }
    }
    offset = end;
  }
  return result;
}

function storyReady(value) {
  return value?.screen === 'Game' && value.overlay === 'None' && value.menu_lock === false
    && value.paused === false && value.ended === false && value.buttons === 0;
}
function advanceReady(value) {
  // The first cross must advance a completed page. Game::advance otherwise
  // only finishes its reveal animation without moving the script/page.
  return storyReady(value) && value.wait === 'Click' && value.page_order > 0
    && value.page_revealed >= value.page_order;
}
function menuVisible(value) {
  // Menu overlay presence includes fade-in (2) and fade-out (13). Only its
  // interactive state (3) is suitable for a screenshot or the return pulse.
  return value?.overlay === 'Menu' && value.menu_state === 3
    && ['Witch', 'Game'].includes(value.screen) && !value.ended;
}
function storyChanged(before, after) {
  // A running Time countdown alone is not evidence of an advance. Keep the
  // discrete wait variant and actual script/page observations instead.
  // page_revealed is deliberately excluded: typing/reveal alone is not an advance.
  const keys = ['scene', 'pc', 'page_messages', 'page_chars', 'page_waiting', 'page_visible'];
  return keys.some(key => before[key] !== after[key])
    || before.wait.split(/[ ({]/, 1)[0] !== after.wait.split(/[ ({]/, 1)[0];
}
function exactAnchor(parsed, anchor, prefixLength, latest = false) {
  if (!anchor || !uint(prefixLength)) return false;
  const actual = parsed.states.find(record => record.offset === anchor.offset && record.end_offset === anchor.end_offset);
  if (!actual || actual.end_offset > prefixLength || JSON.stringify(actual) !== JSON.stringify(anchor)) return false;
  return !latest || parsed.states.filter(record => record.end_offset <= prefixLength).at(-1)?.offset === actual.offset;
}
function pulseEvidence(log, parsed, step, end = log.length) {
  const issues = [];
  const before = step.before_state?.value;
  if (!Object.hasOwn(GAME_BUTTONS, step.button) || step.mask !== GAME_BUTTONS[step.button]) issues.push('Unknown button/mask');
  if (!uint(step.before_log_length) || step.before_log_length > log.length
      || !exactAnchor(parsed, step.before_state, step.before_log_length, true)
      || hash(log.slice(0, step.before_log_length)) !== step.before_log_sha256) issues.push('Authorized STATE anchor changed or is stale');
  if (!(step.button === 'circle' ? menuVisible(before) && before.buttons === 0
    : step.button === 'cross' ? advanceReady(before) : storyReady(before))) issues.push('Button was not authorized by the actual UI state');
  if (!step.set_completed || !step.clear_completed || step.set_error || step.clear_error
      || step.set_result?.ok === false || step.clear_result?.ok === false
      || !Number.isFinite(step.held_ms) || step.held_ms < 1 || step.held_ms > 1000) issues.push('Button set/clear/hold did not complete');
  if (!uint(end) || end < step.before_log_length || end > log.length) issues.push('Invalid observation boundary');
  const records = parsed.records.filter(record => record.offset >= step.before_log_length && record.end_offset <= end);
  const pressed = records.find(record => record.kind === 'INPUT' && record.value.buttons === step.mask
    && record.value.frame > (before?.frame ?? Infinity));
  const released = records.find(record => record.kind === 'INPUT' && pressed && record.offset > pressed.offset
    && record.value.buttons === 0 && record.value.frame > pressed.value.frame);
  const pressState = records.find(record => record.kind === 'STATE' && pressed && record.offset > pressed.offset
    && record.value.frame === pressed.value.frame && record.value.buttons === step.mask);
  const releaseState = records.find(record => record.kind === 'STATE' && released && record.offset > released.offset
    && record.value.frame === released.value.frame && record.value.buttons === 0);
  if (!pressed || !pressState) issues.push('Missing actual INPUT/STATE button press');
  if (!released || !releaseState) issues.push('Missing actual INPUT/STATE button release');
  const transition = records.find(record => record.kind === 'STATE' && pressed && record.offset > pressed.offset
    && (step.button === 'cross' ? record.value.screen === 'Game' && record.value.overlay === 'None'
      && before && storyChanged(before, record.value)
      : step.button === 'triangle' ? menuVisible(record.value) : storyReady(record.value)));
  if (!transition) issues.push(step.button === 'cross' ? 'No script/page advance after cross'
    : step.button === 'triangle' ? 'No actual Menu overlay after triangle' : 'No return to ordinary Game after circle');
  return { valid: issues.length === 0, button: step.button, issues,
    pressed: pressed ?? null, released: released ?? null, transition: transition ?? null };
}
function captureEvidence(log, parsed, capture, triangle, circle) {
  if (!capture) return false;
  const a = capture.artifact;
  return capture.captured === true && menuVisible(capture.before_state?.value) && menuVisible(capture.after_state?.value)
    && exactAnchor(parsed, capture.before_state, capture.before_log_length, true)
    && exactAnchor(parsed, capture.after_state, capture.after_log_length, true)
    && capture.before_log_length <= capture.after_log_length && capture.after_log_length <= log.length
    && hash(log.slice(0, capture.before_log_length)) === capture.before_log_sha256
    && hash(log.slice(0, capture.after_log_length)) === capture.after_log_sha256
    && triangle?.observed_until <= capture.before_log_length
    && (!circle || capture.after_log_length <= circle.before_log_length)
    && typeof a?.path === 'string' && uint(a.bytes, 16 * 1024 * 1024) && a.bytes > 0
    && a.width === 960 && a.height === 544 && digest(a.sha256);
}

function validContinuation(value) {
  return value && typeof value === 'object' && !Array.isArray(value)
    && uint(value.scene) && value.scene > 0 && uint(value.pc) && value.pc > 0
    && uint(value.max_pulses, 32) && value.max_pulses > 0;
}

function continuationEvidence(parsed, config, steps, observations) {
  const issues = [];
  if (!validContinuation(config)) return { valid: false, issues: ['Invalid CG continuation target or input budget'], target_state: null };
  if (steps.length - 3 > config.max_pulses) issues.push('CG continuation input budget exceeded');
  const returned = observations[2]?.transition;
  // Read the target from actual post-return records. Receipt.latest_state and
  // any caller-supplied target_state are never accepted as guest observations.
  const states = returned ? parsed.states.filter(record => record.offset > returned.offset
    && record.value.screen === 'Game' && record.value.overlay === 'None') : [];
  if (states.some(record => record.value.scene !== config.scene)) issues.push('CG continuation left the declared scene');
  const target = states.find(record => storyReady(record.value)
    && record.value.scene === config.scene && record.value.pc >= config.pc) ?? null;
  if (!target) issues.push('Missing actual post-menu CG continuation target STATE');
  return { valid: issues.length === 0, issues, target_state: target,
    scene: config.scene, pc: config.pc, max_pulses: config.max_pulses, pulses_used: Math.max(0, steps.length - 3) };
}

export function gameInteractionEvidence(log, receipt) {
  const parsed = parseGameLog(log);
  const issues = [...parsed.issues];
  const continued = receipt?.schema === 'knife-vita-game-inputs-v2';
  if (!continued && receipt?.schema !== 'knife-vita-game-inputs-v1') issues.push('Missing explicit game-inputs scenario');
  if (receipt?.error) issues.push(receipt.error);
  const steps = receipt?.steps ?? [];
  if (!Array.isArray(steps) || steps.length < 3 || (!continued && steps.length !== 3)
      || steps.length > 35 || steps.some((step, index) => step.button !== (index < 3 ? ['cross', 'triangle', 'circle'][index] : 'cross'))) {
    issues.push('Expected ordered cross/triangle/circle steps');
  }
  if (!Array.isArray(steps) || steps.length > 35) return { valid: false, issues };
  const observations = steps.map(step => pulseEvidence(log, parsed, step, step.observed_until ?? log.length));
  observations.forEach(step => issues.push(...step.issues.map(issue => `${step.button}: ${issue}`)));
  for (let index = 1; index < steps.length; index += 1) {
    if (!uint(steps[index - 1].observed_until) || steps[index - 1].observed_until > steps[index].before_log_length) {
      issues.push('Steps overlap or precede the previous observed outcome');
    }
  }
  const captureValid = captureEvidence(log, parsed, receipt?.menu_capture, steps[1], steps[2]);
  if (!captureValid) issues.push('Missing verified Menu PNG with matching before/after STATE');
  const continuation = continued ? continuationEvidence(parsed, receipt.continuation, steps, observations) : undefined;
  if (continuation) issues.push(...continuation.issues);
  return { valid: issues.length === 0, issues, steps: observations, menu_capture_verified: captureValid,
    ...(continuation ? { continuation } : {}),
    latest_state: parsed.latest_state, limitation: 'Finite observed controller, script/page and menu transitions; no full-playthrough or timing/causality proof' };
}

export class GameInputScenario {
  constructor({ nowMs = Date.now(), stepTimeoutMs = 15000, startTimeoutMs = 90000, continuation } = {}) {
    if (continuation !== undefined && !validContinuation(continuation)) throw new Error('Invalid CG continuation target or input budget');
    this.receipt = { schema: continuation ? 'knife-vita-game-inputs-v2' : 'knife-vita-game-inputs-v1',
      ...(continuation ? { continuation: clone(continuation) } : {}), requested_hold_ms: 100, steps: [],
      created_at_ms: nowMs, step_timeout_ms: stepTimeoutMs, start_timeout_ms: startTimeoutMs, completed: false };
    this.deadline = nowMs + startTimeoutMs;
    this.stepTimeoutMs = stepTimeoutMs;
  }
  observe(log, nowMs = Date.now()) {
    this.lastLog = log;
    const r = this.receipt;
    if (r.error || r.completed) return null;
    const parsed = parseGameLog(log);
    r.latest_state = parsed.latest_state;
    if (parsed.issues.length) { r.error = parsed.issues.join('; '); return null; }
    if (nowMs > this.deadline) { r.error = `Timed out waiting for actual ${r.steps.at(-1)?.button ?? 'Game/unlocked'} evidence`; return null; }
    const current = r.steps.at(-1);
    if (current && !current.observed_until) {
      const evidence = pulseEvidence(log, parsed, current);
      current.last_observation = evidence;
      if (!evidence.valid) return null;
      current.observed_until = log.length;
      current.observed_log_sha256 = hash(log);
      this.deadline = nowMs + this.stepTimeoutMs;
    }
    const latest = parsed.latest_state?.value;
    if (r.steps.length >= 3) {
      if (!r.continuation) { r.completed = gameInteractionEvidence(log, r).valid; return null; }
      const proof = gameInteractionEvidence(log, r);
      if (proof.continuation?.target_state && proof.valid) { r.completed = true; return null; }
      if (latest?.screen === 'Game' && latest.overlay === 'None' && latest.scene !== r.continuation.scene) {
        r.error = 'CG continuation left the declared scene'; return null;
      }
      if (r.steps.length - 3 >= r.continuation.max_pulses && advanceReady(latest)) {
        r.error = 'CG continuation input budget exhausted before target'; return null;
      }
      return advanceReady(latest) ? { kind: 'pulse', button: 'cross' } : null;
    }
    if (r.steps.length < 2) return (r.steps.length === 0 ? advanceReady(latest) : storyReady(latest))
      ? { kind: 'pulse', button: r.steps.length === 0 ? 'cross' : 'triangle' } : null;
    if (!menuVisible(latest) || latest.buttons !== 0) return null;
    return r.menu_capture ? { kind: 'pulse', button: 'circle' } : { kind: 'capture-menu' };
  }
  beginPulse(button, log, nowMs = Date.now()) {
    const action = this.observe(log, nowMs);
    if (action?.kind !== 'pulse' || action.button !== button) throw new Error(`No authorized ${button} pulse`);
    const parsed = parseGameLog(log);
    const step = { button, mask: GAME_BUTTONS[button], requested_at_ms: nowMs,
      before_state: clone(parsed.latest_state), before_log_length: log.length, before_log_sha256: hash(log) };
    this.receipt.steps.push(step);
    this.deadline = nowMs + this.stepTimeoutMs;
    return step;
  }
  endPulse(step, observation) {
    Object.assign(step, clone(observation));
    step.set_completed = Object.hasOwn(observation, 'set_result') && !observation.set_error;
    step.clear_completed = Object.hasOwn(observation, 'clear_result') && !observation.clear_error;
    if (!step.set_completed || !step.clear_completed || step.set_result?.ok === false || step.clear_result?.ok === false
        || !Number.isFinite(step.held_ms) || step.held_ms < 1 || step.held_ms > 1000) {
      this.receipt.error = `Button ${step.button} set/clear/hold failed`;
    }
  }
  recordMenuCapture(capture, nowMs = Date.now(), log = this.lastLog) {
    const parsed = parseGameLog(log);
    if (!captureEvidence(log, parsed, capture, this.receipt.steps[1])) {
      this.receipt.error = 'Menu capture lacks matching actual STATE and PNG identity';
      return;
    }
    this.receipt.menu_capture = clone(capture);
    this.deadline = nowMs + this.stepTimeoutMs;
  }
}

export async function waitForNaturalStop(status, { timeoutMs = 15000, pollMs = 200,
  now = Date.now, sleep = ms => new Promise(resolve => setTimeout(resolve, ms)) } = {}) {
  const started = now();
  const evidence = { stopped: false, timed_out: false, timeout_ms: timeoutMs, poll_count: 0, transitions: [], errors: [] };
  while (now() - started < timeoutMs) {
    try {
      const state = await status(Math.max(1, Math.min(2000, timeoutMs - (now() - started))));
      evidence.poll_count += 1;
      if (evidence.transitions.at(-1)?.phase !== state?.phase) {
        evidence.transitions.push({ elapsed_ms: now() - started, phase: state?.phase ?? null, state });
      }
      evidence.last_state = state;
      if (state?.phase === 'stopped') { evidence.stopped = true; break; }
    } catch (error) {
      if (evidence.errors.length < 8) evidence.errors.push({ elapsed_ms: now() - started, error: String(error) });
    }
    await sleep(Math.min(pollMs, Math.max(1, timeoutMs - (now() - started))));
  }
  evidence.elapsed_ms = now() - started;
  evidence.timed_out = !evidence.stopped;
  return evidence;
}

export function gameExitEvidence(naturalStop, exit, forcedKill = false) {
  const issues = [];
  if (naturalStop?.stopped !== true) issues.push('Guest never reached phase stopped before shutdown');
  if (forcedKill) issues.push('Emulator required forced process termination');
  if (exit?.code !== 0 || exit?.signal !== null) issues.push(`Actual emulator exit is ${exit?.code}, signal ${exit?.signal}`);
  return { valid: issues.length === 0, issues };
}
