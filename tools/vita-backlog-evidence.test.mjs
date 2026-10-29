import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { parseGameLog } from './vita-game-evidence.mjs';
import { BACKLOG_BUTTONS, GameBacklogScenario, gameBacklogEvidence } from './vita-backlog-evidence.mjs';

const hash = text => createHash('sha256').update(text).digest('hex');
const line = (kind, value) => `${kind} ${JSON.stringify(value)}\n`;
const state = overrides => ({ frame: 120, buttons: 0, screen: 'Game', overlay: 'None',
  title_state: 0, title_cursor: 0, menu_state: 0, menu_cursor: -1,
  scene: 1, pc: 125, wait: 'Click', paused: false, ended: false, menu_lock: false,
  page_messages: 1, page_chars: 49, page_order: 49, page_revealed: 49,
  page_visible: true, page_waiting: true,
  backlog: { state: 0, entries: 0, available: 6, cursor: 0, scroll: 8, target: 8 }, ...overrides });

function fixture({ available = 6 } = {}) {
  const scenario = new GameBacklogScenario({ nowMs: 0 });
  let log = line('STATE', state({ backlog: { ...state().backlog, available } }));
  let frame = 120, now = 100;
  function append(value) { log += line('STATE', state({ ...value, frame: ++frame })); }
  function pulse(button, value) {
    assert.deepEqual(scenario.observe(log, now), { kind: 'pulse', button });
    const step = scenario.beginPulse(button, log, now);
    scenario.endPulse(step, { set_result: { ok: true }, clear_result: { ok: true }, held_ms: 100 });
    log += line('INPUT', { frame: ++frame, buttons: BACKLOG_BUTTONS[button] })
      + line('STATE', state({ ...value, frame, buttons: BACKLOG_BUTTONS[button] }));
    log += line('INPUT', { frame: ++frame, buttons: 0 }) + line('STATE', state({ ...value, frame }));
    now += 200;
  }
  function capture(kind) {
    assert.equal(scenario.observe(log, now)?.kind, kind);
    const anchor = parseGameLog(log).latest_state;
    const item = { captured: true, before_state: anchor, after_state: anchor,
      before_log_length: log.length, after_log_length: log.length,
      before_log_sha256: hash(log), after_log_sha256: hash(log),
      artifact: { path: `/fixture/${kind}.png`, bytes: 100, sha256: 'a'.repeat(64), width: 960, height: 544 } };
    if (kind === 'capture-menu') scenario.recordMenuCapture(item, now, log);
    else scenario.recordBacklogCapture(kind, item, now, log);
  }
  const b = { ...state().backlog, available };
  pulse('cross', { pc: 126, backlog: b });
  pulse('triangle', { pc: 126, screen: 'Witch', overlay: 'Menu', paused: true,
    menu_state: 3, menu_cursor: 0, backlog: b });
  capture('capture-menu');
  pulse('circle', { pc: 126, backlog: b });
  return { scenario, pulse, capture, append, get log() { return log; }, get now() { return now; } };
}

function complete() {
  const f = fixture();
  const open = cursor => ({ pc: 126, screen: 'Witch', overlay: 'Backlog', paused: true,
    backlog: { state: 5, entries: 6, available: 6, cursor,
      scroll: cursor >= 3 ? 190 - 116 * (cursor - 1) : 8,
      target: cursor >= 3 ? 190 - 116 * (cursor - 1) : 8 } });
  f.pulse('square', open(0)); f.capture('capture-backlog-open');
  for (let n = 1; n <= 4; n++) f.pulse('up', open(n));
  f.capture('capture-backlog-scroll');
  f.pulse('down', { ...open(3), backlog: { ...open(3).backlog, scroll: -158, target: -158 } });
  f.pulse('circle', { pc: 126, backlog: { ...state().backlog, entries: 6 } });
  f.pulse('square', open(0));
  f.pulse('circle', { pc: 126, backlog: { ...state().backlog, entries: 6 } });
  f.capture('capture-backlog-return'); f.scenario.observe(f.log, f.now);
  return f;
}

test('menu return, backlog opening, both scroll directions and reopening require actual controller outcomes', () => {
  const f = complete();
  const proof = gameBacklogEvidence(f.log, f.scenario.receipt);
  assert.equal(proof.valid, true, proof.issues.join('; '));
  assert.equal(f.scenario.receipt.completed, true);
  assert.equal(proof.steps.length, 9);
  assert.equal(proof.captures.length, 3);
  assert.equal(proof.steps[4].transition.value.backlog.scroll, -158);
});

test('story advances are bounded and opening waits for enough real history entries', () => {
  const f = fixture({ available: 2 });
  assert.deepEqual(f.scenario.observe(f.log, f.now), { kind: 'pulse', button: 'cross' });
  assert.throws(() => f.scenario.beginPulse('square', f.log, f.now), /authorized/i);
  f.pulse('cross', { pc: 131, backlog: { ...state().backlog, available: 6 } });
  assert.deepEqual(f.scenario.observe(f.log, f.now), { kind: 'pulse', button: 'square' });
  const g = fixture({ available: 2 });
  for (let n = 0; n < 12; n++) g.pulse('cross', { pc: 127 + n,
    backlog: { ...state().backlog, available: 2 } });
  assert.equal(g.scenario.observe(g.log, g.now), null);
  assert.match(g.scenario.receipt.error, /budget/i);
});

test('partial pages, timed waits and malformed backlog observations cannot authorize input', () => {
  for (const value of [{ page_revealed: 1 }, { wait: 'Time' }, { backlog: { state: 5 } }]) {
    const f = fixture(); f.append({ pc: 126, ...value });
    assert.equal(f.scenario.observe(f.log, f.now), null);
  }
});

test('a forged receipt, missing INPUT or unchanged cursor cannot pass backlog verification', () => {
  const f = complete();
  assert.equal(gameBacklogEvidence('', f.scenario.receipt).valid, false);
  assert.equal(gameBacklogEvidence(f.log.replace(/^INPUT .*\n/gm, ''), f.scenario.receipt).valid, false);
  const receipt = structuredClone(f.scenario.receipt);
  receipt.steps[1].button = 'down';
  assert.equal(gameBacklogEvidence(f.log, receipt).valid, false);
  const g = fixture();
  const open = { pc: 126, screen: 'Witch', overlay: 'Backlog', paused: true,
    backlog: { state: 5, entries: 6, available: 6, cursor: 0, scroll: 8, target: 8 } };
  g.pulse('square', open); g.capture('capture-backlog-open'); g.pulse('up', open);
  assert.equal(g.scenario.observe(g.log, g.now), null);
  assert.equal(g.scenario.receipt.steps[1].observed_until, undefined);
});

test('capture hashes, phase boundaries, script continuity and release evidence are enforced', () => {
  const f = complete();
  for (const mutate of [r => { r.captures[1].after_log_sha256 = '0'.repeat(64); },
    r => { r.captures[0].kind = 'capture-backlog-return'; },
    r => { r.steps[2].clear_completed = false; },
    r => { r.steps[7].before_state.value.pc += 1; },
    r => { r.steps[3].observed_until = r.steps[4].before_log_length + 1; }]) {
    const r = structuredClone(f.scenario.receipt); mutate(r);
    assert.equal(gameBacklogEvidence(f.log, r).valid, false);
  }
});
