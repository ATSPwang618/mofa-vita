import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import {
  GAME_BUTTONS, parseGameLog, gameInteractionEvidence, GameInputScenario,
  waitForNaturalStop, gameExitEvidence,
} from './vita-game-evidence.mjs';

const state = overrides => ({ frame: 120, screen: 'Game', title_state: 0, title_cursor: 0,
  overlay: 'None', scene: 2, pc: 30, wait: 'Click', paused: false, ended: false,
  menu_lock: false, page_messages: 1, page_chars: 12, page_waiting: true,
  page_visible: true, page_revealed: 12, page_order: 12,
  menu_state: 0, menu_cursor: -1, buttons: 0, ...overrides });
const line = (kind, value) => `${kind} ${JSON.stringify(value)}\n`;
const snapshot = value => line('STATE', value);
const input = (frame, buttons) => line('INPUT', { frame, buttons });
const hash = value => createHash('sha256').update(value).digest('hex');
function scenarioFixture(options = {}) {
  let now = 100;
  let log = snapshot(state({ screen: 'Title' })) + snapshot(state());
  const scenario = new GameInputScenario({ nowMs: 0, ...options });
  function pulse(button, values) {
    assert.deepEqual(scenario.observe(log, now), { kind: 'pulse', button });
    const step = scenario.beginPulse(button, log, now);
    scenario.endPulse(step, { set_result: { ok: true }, clear_result: { ok: true },
      held_ms: 100, cleared_at_ms: now + 100 });
    log += input(values.frame, GAME_BUTTONS[button])
      + snapshot(state({ ...values, buttons: GAME_BUTTONS[button] }))
      + input(values.frame + 1, 0) + snapshot(state({ ...values, frame: values.frame + 1 }));
    now += 200;
  }
  pulse('cross', { frame: 121, pc: 31, page_chars: 19 });
  pulse('triangle', { frame: 123, screen: 'Witch', overlay: 'Menu', paused: true,
    menu_state: 3, menu_cursor: 0, pc: 31 });
  assert.deepEqual(scenario.observe(log, now), { kind: 'capture-menu' });
  const captureState = parseGameLog(log).latest_state;
  scenario.recordMenuCapture({ captured: true, before_state: captureState,
    after_state: captureState, before_log_length: log.length, after_log_length: log.length,
    before_log_sha256: hash(log), after_log_sha256: hash(log),
    artifact: { path: '/fixture/menu.png', bytes: 100, sha256: 'a'.repeat(64), width: 960, height: 544 } }, now);
  pulse('circle', { frame: 125, pc: 31, page_chars: 19 });
  scenario.observe(log, now);
  return { scenario, log, now };
}

test('CG continuation requires actual post-menu progress to the declared script target', () => {
  const { scenario, log, now } = scenarioFixture({ continuation: { scene: 2, pc: 40, max_pulses: 2 } });
  assert.equal(scenario.receipt.completed, false);
  assert.equal(gameInteractionEvidence(log, scenario.receipt).valid, false);
  assert.deepEqual(scenario.observe(log, now), { kind: 'pulse', button: 'cross' });
  const step = scenario.beginPulse('cross', log, now);
  scenario.endPulse(step, { set_result: {}, clear_result: {}, held_ms: 100 });
  const progressed = log + input(127, 0x4000)
    + snapshot(state({ frame: 127, pc: 40, buttons: 0x4000 }))
    + input(128, 0) + snapshot(state({ frame: 128, pc: 40 }));
  assert.equal(scenario.observe(progressed, now + 200), null);
  const proof = gameInteractionEvidence(progressed, scenario.receipt);
  assert.equal(proof.valid, true, proof.issues.join('; '));
  assert.equal(scenario.receipt.completed, true);
  assert.equal(proof.continuation.target_state.value.pc, 40);
});

test('CG continuation does not press through a partially revealed page or ordinary timed wait', () => {
  const { scenario, log, now } = scenarioFixture({ continuation: { scene: 2, pc: 40, max_pulses: 2 } });
  for (const overrides of [{ page_revealed: 4, page_order: 20 }, { wait: 'Time', page_order: 12 }]) {
    assert.equal(scenario.observe(log + snapshot(state({ frame: 127, pc: 31, ...overrides })), now), null);
  }
});

test('CG continuation fails when its finite input budget is exhausted before target', () => {
  const { scenario, log, now } = scenarioFixture({ continuation: { scene: 2, pc: 40, max_pulses: 1 } });
  const step = scenario.beginPulse('cross', log, now);
  scenario.endPulse(step, { set_result: {}, clear_result: {}, held_ms: 100 });
  const stalled = log + input(127, 0x4000)
    + snapshot(state({ frame: 127, pc: 32, buttons: 0x4000 }))
    + input(128, 0) + snapshot(state({ frame: 128, pc: 32 }));
  assert.equal(scenario.observe(stalled, now + 200), null);
  assert.match(scenario.receipt.error, /budget/i);
  assert.equal(gameInteractionEvidence(stalled, scenario.receipt).valid, false);
});

test('CG continuation rejects invalid targets, forged target PCs and unrelated scene changes', () => {
  for (const continuation of [{ scene: 2, pc: 40, max_pulses: 0 }, { scene: 2, pc: 40, max_pulses: 33 },
    { scene: -1, pc: 40, max_pulses: 1 }, { scene: 2, pc: 1.5, max_pulses: 1 }]) {
    assert.throws(() => new GameInputScenario({ continuation }), /continuation/i);
  }
  const { scenario, log, now } = scenarioFixture({ continuation: { scene: 2, pc: 40, max_pulses: 2 } });
  scenario.receipt.latest_state.value.pc = 40;
  assert.equal(gameInteractionEvidence(log, scenario.receipt).valid, false);
  assert.equal(scenario.observe(log + snapshot(state({ frame: 127, scene: 3, pc: 40 })), now), null);
  assert.match(scenario.receipt.error, /scene/i);
});

test('strict log parser keeps exact record coordinates, unsigned masks and complete lines', () => {
  const log = 'FRAME 1 submitted\n' + snapshot(state({ buttons: 0x80000000 }))
    + input(121, 0xffffffff) + 'STATE {"frame":';
  const parsed = parseGameLog(log);
  assert.deepEqual(parsed.issues, []);
  assert.equal(parsed.states.length, 1);
  assert.equal(parsed.inputs[0].value.buttons, 0xffffffff);
  assert.equal(parsed.latest_state.value.buttons, 0x80000000);
  assert.equal(log.slice(parsed.latest_state.offset, parsed.latest_state.end_offset), snapshot(state({ buttons: 0x80000000 })));
  assert.equal(parsed.incomplete_line, true);
});
test('parser rejects malformed JSON, missing typed fields, non-u32 inputs and frame reversal', () => {
  for (const log of ['STATE {bad}\n', snapshot(state({ menu_lock: 0 })),
    snapshot(state({ pc: -1 })), input(1, 0x100000000),
    snapshot(state()) + input(119, 0)]) assert.ok(parseGameLog(log).issues.length, log);
});
test('bounded parser rejects oversized input and records', () => {
  assert.ok(parseGameLog('x'.repeat(8 * 1024 * 1024 + 1)).issues.length);
  assert.ok(parseGameLog(`STATE ${' '.repeat(8193)}\n`).issues.length);
});
test('cross cannot be sent from Title, locked Game, paused Game or an active physical button', () => {
  for (const overrides of [{ screen: 'Title' }, { menu_lock: true }, { paused: true },
    { ended: true }, { overlay: 'Menu' }, { buttons: 1 }]) {
    const scenario = new GameInputScenario({ nowMs: 0 });
    const log = snapshot(state(overrides));
    assert.equal(scenario.observe(log, 1), null);
    assert.throws(() => scenario.beginPulse('cross', log, 2), /authorized/);
  }
});
test('observed story advance, menu capture and return satisfy all three gates', () => {
  const { scenario, log } = scenarioFixture();
  const evidence = gameInteractionEvidence(log, scenario.receipt);
  assert.deepEqual(evidence.issues, []);
  assert.equal(evidence.valid, true);
  assert.equal(evidence.steps.length, 3);
  assert.equal(scenario.receipt.completed, true);
});
test('successful bridge acknowledgments alone cannot advance a scenario', () => {
  const scenario = new GameInputScenario({ nowMs: 0 });
  const log = snapshot(state());
  const step = scenario.beginPulse('cross', log, 1);
  scenario.endPulse(step, { set_result: {}, clear_result: {}, held_ms: 100, cleared_at_ms: 101 });
  assert.equal(scenario.observe(log + snapshot(state({ frame: 240, pc: 31 })), 200), null);
  assert.equal(gameInteractionEvidence(log, scenario.receipt).valid, false);
});
test('actual press and release without script/page progress cannot advance to triangle', () => {
  for (const revealed of [12, 13]) {
    const scenario = new GameInputScenario({ nowMs: 0 });
    const before = state();
    let log = snapshot(before);
    const step = scenario.beginPulse('cross', log, 1);
    scenario.endPulse(step, { set_result: {}, clear_result: {}, held_ms: 100 });
    log += input(121, 0x4000) + snapshot(state({ frame: 121, buttons: 0x4000, page_revealed: revealed }))
      + input(122, 0) + snapshot(state({ frame: 122, page_revealed: revealed }));
    assert.equal(scenario.observe(log, 200), null);
    assert.deepEqual(step.last_observation.issues, ['No script/page advance after cross']);
    assert.equal(step.last_observation.pressed.value.buttons, 0x4000);
    assert.equal(step.last_observation.released.value.buttons, 0);
    assert.equal(scenario.receipt.steps.length, 1);
  }
});
test('cross waits for a fully revealed nonempty WaitClick page and permits revealed >= order', () => {
  for (const overrides of [{ page_revealed: 11 }, { page_revealed: 0, page_order: 0 },
    { page_revealed: 1, page_order: 0 }, { wait: 'None' },
    { wait: 'Time { left: 2.0, click: true }' }]) {
    const scenario = new GameInputScenario({ nowMs: 0 });
    const log = snapshot(state(overrides));
    assert.equal(scenario.observe(log, 1), null, JSON.stringify(overrides));
    assert.throws(() => scenario.beginPulse('cross', log, 2), /authorized/);
  }
  for (const page_revealed of [12, 13]) {
    const scenario = new GameInputScenario({ nowMs: 0 });
    assert.deepEqual(scenario.observe(snapshot(state({ page_revealed })), 1), { kind: 'pulse', button: 'cross' });
  }
});
test('new page and menu metadata is mandatory and retains u32/i32 boundaries', () => {
  for (const field of ['page_revealed', 'page_order', 'menu_state', 'menu_cursor']) {
    const value = state();
    delete value[field];
    assert.ok(parseGameLog(snapshot(value)).issues.length, field);
  }
  for (const overrides of [{ page_revealed: -1 }, { page_order: 1.5 },
    { menu_state: 0x100000000 }, { menu_cursor: -0x80000001 }, { menu_cursor: 0x80000000 }]) {
    assert.ok(parseGameLog(snapshot(state(overrides))).issues.length, JSON.stringify(overrides));
  }
  assert.deepEqual(parseGameLog(snapshot(state({ menu_state: 0xffffffff, menu_cursor: -0x80000000 }))).issues, []);
});
test('triangle remains available after an observed advance while the next page is revealing', () => {
  const scenario = new GameInputScenario({ nowMs: 0 });
  let log = snapshot(state());
  const step = scenario.beginPulse('cross', log, 1);
  scenario.endPulse(step, { set_result: {}, clear_result: {}, held_ms: 100 });
  const nextPage = state({ pc: 31, page_order: 20, page_revealed: 2 });
  log += input(121, 0x4000) + snapshot({ ...nextPage, frame: 121, buttons: 0x4000 })
    + input(122, 0) + snapshot({ ...nextPage, frame: 122 });
  assert.deepEqual(scenario.observe(log, 200), { kind: 'pulse', button: 'triangle' });
});
function menuTransitionFixture(menuState) {
  const scenario = new GameInputScenario({ nowMs: 0 });
  let log = snapshot(state());
  const cross = scenario.beginPulse('cross', log, 1);
  scenario.endPulse(cross, { set_result: {}, clear_result: {}, held_ms: 100 });
  log += input(121, 0x4000) + snapshot(state({ frame: 121, buttons: 0x4000, pc: 31 }))
    + input(122, 0) + snapshot(state({ frame: 122, pc: 31 }));
  const triangle = scenario.beginPulse('triangle', log, 200);
  scenario.endPulse(triangle, { set_result: {}, clear_result: {}, held_ms: 100 });
  const menu = state({ screen: 'Witch', overlay: 'Menu', paused: true, pc: 31,
    menu_state: menuState, menu_cursor: 0 });
  log += input(123, 0x1000) + snapshot({ ...menu, frame: 123, buttons: 0x1000 })
    + input(124, 0) + snapshot({ ...menu, frame: 124 });
  return { scenario, log, menu };
}
test('triangle waits through fade-in 2 and fade-out 13 until actual menu state 3', () => {
  for (const menuState of [2, 13]) {
    const { scenario, log, menu } = menuTransitionFixture(menuState);
    assert.equal(scenario.observe(log, 400), null, `menu ${menuState}`);
    assert.equal(scenario.receipt.steps[1].last_observation.valid, false);
    assert.ok(scenario.receipt.steps[1].last_observation.issues.includes('No actual Menu overlay after triangle'));
    assert.deepEqual(scenario.observe(log + snapshot({ ...menu, frame: 125, menu_state: 3 }), 500),
      { kind: 'capture-menu' });
  }
});
test('Menu PNG and circle authorization require stable menu state 3 at both capture boundaries', () => {
  for (const [beforeStage, afterStage] of [[3, 2], [3, 13], [2, 3], [13, 3]]) {
    const { scenario, log, menu } = menuTransitionFixture(3);
    assert.deepEqual(scenario.observe(log, 400), { kind: 'capture-menu' });
    const beforeLog = log + snapshot({ ...menu, frame: 125, menu_state: beforeStage });
    const afterLog = beforeLog + snapshot({ ...menu, frame: 126, menu_state: afterStage });
    const beforeState = parseGameLog(beforeLog).latest_state;
    const afterState = parseGameLog(afterLog).latest_state;
    scenario.recordMenuCapture({ captured: true, before_state: beforeState, after_state: afterState,
      before_log_length: beforeLog.length, after_log_length: afterLog.length,
      before_log_sha256: hash(beforeLog), after_log_sha256: hash(afterLog),
      artifact: { path: '/fixture/menu.png', bytes: 100, sha256: 'a'.repeat(64), width: 960, height: 544 } }, 500, afterLog);
    assert.match(scenario.receipt.error, /Menu capture/);
  }
  for (const menuState of [2, 13]) {
    const { scenario, log, menu } = menuTransitionFixture(3);
    scenario.observe(log, 400);
    const latest = parseGameLog(log).latest_state;
    scenario.recordMenuCapture({ captured: true, before_state: latest, after_state: latest,
      before_log_length: log.length, after_log_length: log.length,
      before_log_sha256: hash(log), after_log_sha256: hash(log),
      artifact: { path: '/fixture/menu.png', bytes: 100, sha256: 'a'.repeat(64), width: 960, height: 544 } }, 500, log);
    const faded = log + snapshot({ ...menu, frame: 125, menu_state: menuState });
    assert.equal(scenario.observe(faded, 600), null);
    assert.throws(() => scenario.beginPulse('circle', faded, 601), /authorized/);
  }
});
test('missing release, unrelated button and changes only to frame/buttons do not pass', () => {
  for (const kind of ['release', 'wrong-button', 'no-progress']) {
    const { scenario, log } = scenarioFixture();
    let altered;
    if (kind === 'release') altered = log.replace(input(122, 0), '');
    if (kind === 'wrong-button') altered = log.replace(input(121, 0x4000), input(121, 0x2000));
    if (kind === 'no-progress') {
      altered = log.replace(snapshot(state({ frame: 121, pc: 31, page_chars: 19, buttons: 0x4000 })),
        snapshot(state({ frame: 121, buttons: 0x4000 })))
        .replace(snapshot(state({ frame: 122, pc: 31, page_chars: 19 })), snapshot(state({ frame: 122 })));
    }
    assert.equal(gameInteractionEvidence(altered, scenario.receipt).valid, false, kind);
  }
});
test('missing menu, missing screenshot hash, wrong capture state and a missing return fail closed', () => {
  for (const kind of ['menu', 'hash', 'capture-state', 'return']) {
    const { scenario, log } = scenarioFixture();
    let altered = log;
    if (kind === 'menu') altered = log.replaceAll('"overlay":"Menu"', '"overlay":"None"');
    if (kind === 'hash') delete scenario.receipt.menu_capture.artifact.sha256;
    if (kind === 'capture-state') scenario.receipt.menu_capture.after_state.value.overlay = 'None';
    if (kind === 'return') altered = log.slice(0, log.indexOf(input(125, 0x2000)));
    assert.equal(gameInteractionEvidence(altered, scenario.receipt).valid, false, kind);
  }
});
test('old input before the authorized state and stale or altered state anchors are rejected', () => {
  const { scenario, log } = scenarioFixture();
  scenario.receipt.steps[0].before_state.value.menu_lock = true;
  assert.equal(gameInteractionEvidence(log, scenario.receipt).valid, false);
  const second = new GameInputScenario({ nowMs: 0 });
  const before = input(1, 0x4000) + input(2, 0) + snapshot(state());
  const step = second.beginPulse('cross', before, 1);
  second.endPulse(step, { set_result: {}, clear_result: {}, held_ms: 100, cleared_at_ms: 101 });
  assert.equal(second.observe(before + snapshot(state({ frame: 240, pc: 31 })), 200), null);
});
test('a finite scenario timeout remains failed when a later log looks successful', () => {
  const scenario = new GameInputScenario({ nowMs: 0, stepTimeoutMs: 200, startTimeoutMs: 300 });
  assert.equal(scenario.observe(snapshot(state({ screen: 'Title' })), 301), null);
  assert.match(scenario.receipt.error, /Timed out/);
  assert.equal(scenario.observe(snapshot(state()), 302), null);
});
test('the requested hold is bounded and a failed clear cannot pass', () => {
  for (const heldMs of [0, 1001]) {
    const scenario = new GameInputScenario({ nowMs: 0 });
    const step = scenario.beginPulse('cross', snapshot(state()), 1);
    scenario.endPulse(step, { set_result: {}, clear_result: {}, held_ms: heldMs, cleared_at_ms: 101 });
    assert.ok(scenario.receipt.error);
  }
  const { scenario, log } = scenarioFixture();
  scenario.receipt.steps[0].clear_error = 'pipe closed';
  assert.equal(gameInteractionEvidence(log, scenario.receipt).valid, false);
});
test('natural-stop polling records phase changes and accepts only stopped', async () => {
  let time = 0;
  const responses = [{ phase: 'running' }, { phase: 'running' }, { phase: 'stopping' }, { phase: 'stopped' }];
  const observed = await waitForNaturalStop(async () => responses.shift(), {
    now: () => time, sleep: async ms => { time += ms; }, timeoutMs: 1000, pollMs: 100,
  });
  assert.equal(observed.stopped, true);
  assert.deepEqual(observed.transitions.map(item => item.phase), ['running', 'stopping', 'stopped']);
  assert.equal(observed.poll_count, 4);
});
test('natural-stop timeout or broken bridge cannot masquerade as a natural stop', async () => {
  let time = 0;
  const options = { now: () => time, sleep: async ms => { time += ms; }, timeoutMs: 250, pollMs: 100 };
  const timeout = await waitForNaturalStop(async () => ({ phase: 'running' }), options);
  assert.equal(timeout.stopped, false);
  assert.equal(timeout.timed_out, true);
  time = 0;
  const broken = await waitForNaturalStop(async () => { throw new Error('pipe closed'); }, options);
  assert.equal(broken.stopped, false);
  assert.ok(broken.errors.length);
});
test('a stopped-looking auxiliary state does not replace the exact stopped phase', async () => {
  let time = 0;
  const observed = await waitForNaturalStop(async () => ({ phase: 'stopping', state: 'stopped', running: false }), {
    now: () => time, sleep: async ms => { time += ms; }, timeoutMs: 250, pollMs: 100,
  });
  assert.equal(observed.stopped, false);
  assert.equal(observed.timed_out, true);
});
test('a 120 second game stop budget allows the remaining finite run without repairing an input failure', async () => {
  let time = 0;
  const scenario = new GameInputScenario({ nowMs: 0, startTimeoutMs: 100 });
  scenario.observe(snapshot(state({ screen: 'Title' })), 101);
  const inputFailure = scenario.receipt.error;
  const observed = await waitForNaturalStop(async requestTimeout => {
    assert.ok(requestTimeout >= 1 && requestTimeout <= 2000);
    return { phase: time < 60000 ? 'running' : 'stopped' };
  }, { now: () => time, sleep: async ms => { time += ms; }, timeoutMs: 120000, pollMs: 1000 });
  assert.equal(observed.stopped, true);
  assert.equal(observed.elapsed_ms, 60000);
  assert.equal(scenario.receipt.error, inputFailure);
  assert.equal(gameInteractionEvidence(snapshot(state()), scenario.receipt).valid, false);
});
test('final gate still requires exact process exit zero, a natural stop and no forced kill', () => {
  const good = { stopped: true };
  assert.equal(gameExitEvidence(good, { code: 0, signal: null }, false).valid, true);
  for (const result of [gameExitEvidence(good, { code: 0xc0000374, signal: null }, false),
    gameExitEvidence({ stopped: false }, { code: 0, signal: null }, false),
    gameExitEvidence(good, { code: 0, signal: 'SIGTERM' }, false),
    gameExitEvidence(good, { code: 0, signal: null }, true)]) assert.equal(result.valid, false);
});
