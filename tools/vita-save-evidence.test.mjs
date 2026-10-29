import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { parseGameLog } from './vita-game-evidence.mjs';
import { GameSaveScenario, SAVE_BUTTONS, gameSaveEvidence, inspectWitchSave } from './vita-save-evidence.mjs';

const hash = value => createHash('sha256').update(value).digest('hex');
const line = (kind, value) => `${kind} ${JSON.stringify(value)}\n`;
const point = { scene: 1, pc: 31 };
const save = overrides => ({ state: 0, saving: false, page: 0, slot: 0, tabrow: false,
  lock: 0, target: 0, manual_count: 0, selected_used: false, dialog_active: 0,
  dialog_answer: 0, popup_busy: 0, cool: 0, can_save: true, point, ...overrides });
const state = (overrides = {}) => ({ frame: 120, screen: 'Game', title_state: 0, title_cursor: 0,
  overlay: 'None', scene: 1, pc: 32, wait: 'Click', paused: false, ended: false, menu_lock: false,
  page_messages: 1, page_chars: 18, page_waiting: true, page_visible: true, page_revealed: 18,
  page_order: 18, menu_state: 2, menu_cursor: 0, buttons: 0, save: save(), ...overrides });
const menu = (cursor = 0, s = {}) => state({ screen: 'Witch', overlay: 'Menu', paused: true,
  menu_state: 3, menu_cursor: cursor, save: save(s) });
const list = (saving, used) => ({ ...menu(saving ? 0 : 1, { state: 2, saving, manual_count: +used, selected_used: used }), menu_state: 5 });
const dialog = (saving, used) => ({ ...menu(saving ? 0 : 1, { state: saving ? 6 : 17, saving,
  manual_count: +used, selected_used: used, dialog_active: 2 }), menu_state: 5 });
const bytes = () => {
  const buffer = Buffer.alloc(0x3fb800), at = 7168;
  buffer.writeUInt16LE(1000, at); buffer.writeUInt16LE(1, at + 2);
  buffer.writeBigInt64LE(1234n, at + 16); buffer.writeBigInt64LE(1260n, at + 24);
  buffer.writeUInt16LE(1, at + 32);
  return buffer;
};

function fixture({ retainedSavePoint = false, restoredOverrides = null } = {}) {
  const scenario = new GameSaveScenario({ nowMs: 0 });
  let now = 100, frame = 120, log = line('STATE', state());
  const disk = [bytes(), bytes()];
  function capture(kind) {
    assert.deepEqual(scenario.observe(log, now), { kind });
    const anchor = parseGameLog(log).latest_state;
    scenario.recordCapture(kind, { captured: true, before_state: anchor, after_state: anchor,
      before_log_length: log.length, after_log_length: log.length,
      before_log_sha256: hash(log), after_log_sha256: hash(log), artifact: {
        path: `/fixture/${kind}.png`, bytes: 100, sha256: 'a'.repeat(64), width: 960, height: 544,
      } }, disk[kind === 'capture-saved' ? 0 : 1], now, log);
  }
  const outcomes = [menu(), list(true, false), dialog(true, false), list(true, true),
    menu(0, { manual_count: 1, selected_used: true }), state({ save: save({ manual_count: 1, selected_used: true }) }),
    state({ pc: 40, page_chars: 23, page_revealed: 23, page_order: 23,
      save: save({ manual_count: 1, selected_used: true, point: retainedSavePoint ? point : { scene: 1, pc: 39 } }) }),
    menu(0, { manual_count: 1, selected_used: true, point: { scene: 1, pc: 39 } }),
    menu(1, { manual_count: 1, selected_used: true, point: { scene: 1, pc: 39 } }),
    list(false, true), dialog(false, true), state({ save: save({ manual_count: 1, selected_used: true }), ...restoredOverrides })];
  for (const [index, outcome] of outcomes.entries()) {
    if (index === 4) capture('capture-saved');
    const action = scenario.observe(log, now);
    assert.equal(action?.kind, 'pulse', `Step ${index}: ${scenario.receipt.error}`);
    const step = scenario.beginPulse(action.button, log, now);
    scenario.endPulse(step, { set_result: { ok: true }, clear_result: { ok: true }, held_ms: 100 });
    frame += 2;
    log += line('INPUT', { frame, buttons: SAVE_BUTTONS[action.button] })
      + line('STATE', { ...outcome, frame, buttons: SAVE_BUTTONS[action.button] })
      + line('INPUT', { frame: frame + 1, buttons: 0 }) + line('STATE', { ...outcome, frame: frame + 1 });
    now += 200;
  }
  if (restoredOverrides) {
    scenario.observe(log, now);
    return { scenario, log, disk };
  }
  capture('capture-restored');
  scenario.observe(log, now);
  return { scenario, log, disk };
}

test('twelve real controller transitions, file records and restored save point are required together', () => {
  const f = fixture(), proof = gameSaveEvidence(f.log, f.scenario.receipt, f.disk);
  assert.equal(proof.valid, true, proof.issues.join('; '));
  assert.equal(f.scenario.receipt.completed, true);
  assert.equal(proof.steps.length, 12);
  assert.deepEqual(proof.saved_point, point);
});

test('advancing within a replay block is observed even when the stored replay start stays unchanged', () => {
  const f = fixture({ retainedSavePoint: true });
  const proof = gameSaveEvidence(f.log, f.scenario.receipt, f.disk);
  assert.equal(proof.valid, true, proof.issues.join('; '));
  assert.equal(f.scenario.receipt.completed, true);
});

test('a matching replay start cannot prove restoration of the displayed instruction and page', () => {
  for (const restoredOverrides of [{ pc: 40 }, { scene: 2 }, { page_chars: 23, page_revealed: 23, page_order: 23 },
    { page_messages: 2 }, { page_order: 17, page_revealed: 18 }]) {
    const f = fixture({ restoredOverrides });
    assert.equal(f.scenario.receipt.steps[11].last_observation.valid, false);
    assert.match(f.scenario.receipt.steps[11].last_observation.issues.join('; '), /actual save UI\/script outcome/);
    assert.equal(f.scenario.receipt.completed, false);
    assert.equal(gameSaveEvidence(f.log, f.scenario.receipt, f.disk).valid, false);
  }
});

test('button acknowledgements without actual guest input cannot prove saving or loading', () => {
  const f = fixture();
  assert.equal(gameSaveEvidence(f.log.replace(/^INPUT .*\n/gm, ''), f.scenario.receipt, f.disk).valid, false);
});

test('save record parser rejects truncation, wrong magic, invalid offsets and extra manual records', () => {
  assert.equal(inspectWitchSave(bytes()).valid, true);
  for (const mutate of [b => b.subarray(0, b.length - 1), b => { b.writeUInt16LE(999, 7168); return b; },
    b => { b.writeBigInt64LE(-1n, 7184); return b; },
    b => { b.writeUInt16LE(1, 7168 + 30208 + 2); return b; }]) {
    assert.equal(inspectWitchSave(mutate(bytes())).valid, false);
  }
});

test('a changed save record or a forged parsed receipt fails against the original file bytes', () => {
  const f = fixture(); f.disk[1][7168 + 100] = 1;
  assert.equal(gameSaveEvidence(f.log, f.scenario.receipt, f.disk).valid, false);
  const g = fixture(); g.scenario.receipt.captures[0].file.scene = 2;
  assert.equal(gameSaveEvidence(g.log, g.scenario.receipt, g.disk).valid, false);
});

test('script movement and returning to the saved point cannot be replaced with caller metadata', () => {
  const f = fixture(); f.scenario.receipt.steps[0].before_state.value.save.point.pc = 39;
  assert.equal(gameSaveEvidence(f.log, f.scenario.receipt, f.disk).valid, false);
  const g = fixture(); g.scenario.receipt.steps[6].observed_until = g.scenario.receipt.steps[6].before_log_length;
  assert.equal(gameSaveEvidence(g.log, g.scenario.receipt, g.disk).valid, false);
});

test('fresh-run guard never overwrites a used save slot and never enters from Title or an animation', () => {
  for (const overrides of [{ screen: 'Title' }, { menu_lock: true }, { paused: true },
    { save: save({ manual_count: 1, selected_used: true }) }, { save: save({ point: null }) }]) {
    const scenario = new GameSaveScenario({ nowMs: 0 });
    assert.equal(scenario.observe(line('STATE', state(overrides)), 1), null);
    assert.throws(() => scenario.beginPulse('triangle', line('STATE', state(overrides)), 2), /authorized/);
  }
});

test('missing typed save observations, failed input and timeouts stay failures', () => {
  const missing = state(); delete missing.save;
  const s = new GameSaveScenario({ nowMs: 0, startTimeoutMs: 20 });
  s.observe(line('STATE', missing), 1); assert.match(s.receipt.error, /save/i);
  const t = new GameSaveScenario({ nowMs: 0, startTimeoutMs: 20 });
  t.observe(line('STATE', state({ screen: 'Title' })), 21); assert.match(t.receipt.error, /time/i);
  const u = new GameSaveScenario({ nowMs: 0 });
  const step = u.beginPulse('triangle', line('STATE', state()), 1);
  u.endPulse(step, { set_result: { ok: false }, clear_result: {}, held_ms: 100 });
  assert.match(u.receipt.error, /failed/i);
});
