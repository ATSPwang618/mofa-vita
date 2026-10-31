// Finite observation of the actual unbounded release; no guest memory writes.
import { promises as fs } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { randomBytes, createHash } from 'node:crypto';
import { spawn, execFileSync } from 'node:child_process';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const work = path.join(root, '.work/vita/vita3k');
const lang = process.argv[2] ?? 'zc';
if (!['en', 'zc'].includes(lang)) throw new Error('Usage: verify-vita-release-270p.mjs [en|zc]');
// This controls only the Windows emulator, never the GXM guest build.
const backend = (process.env.KNIFE_VITA3K_BACKEND ?? 'VULKAN').toUpperCase();
const measure = process.env.KNIFE_VITA_PERF === '1';
if (!['OPENGL', 'VULKAN'].includes(backend)) throw new Error('KNIFE_VITA3K_BACKEND must be OPENGL or VULKAN');
const tag = `release270-${lang}-${backend.toLowerCase()}-${Date.now()}-${randomBytes(3).toString('hex')}`;
const pref = path.join(work, `pref-${tag}`);
const executable = process.env.KNIFE_VITA3K_EXE ?? path.join(root, '.work/vita/emulator-memory-fixed/Vita3K.exe');
const debuggerExe = process.env.KNIFE_VITA3K_DEBUGGER ?? path.join(root, '.work/vita/debugger/vita3k-owned-debugger.exe');
const vpk = path.join(root, '.work/vita/game-release/knife-vita-game-270p.vpk');
const assets = path.join(root, 'dist/Witch-on-the-Holy-Night-Chengkong-270p-Vita/assets');
const { BridgeClient } = await import(pathToFileURL(path.join(path.dirname(executable), 'mcp/server/dist/bridge-client.js')).href);
const bridge = new BridgeClient(`knife-${tag}`, randomBytes(32).toString('hex'));
const digest = data => createHash('sha256').update(data).digest('hex');
const identity = async filename => {
  const data = await fs.readFile(filename);
  return { path: filename, bytes: data.length, sha256: digest(data) };
};
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const receipt = { tested_at: new Date().toISOString(), language: lang, profile: pref,
  guest_renderer: 'direct GXM', host_emulator_backend_requested: backend,
  scope: 'Finite capture and real-controller input against the unbounded release VPK; requested host stop, no hardware/FPS/full-game claim',
  build: await identity(path.join(root, 'doc/vita-game-release-build.json')),
  observation_tool: await identity(fileURLToPath(import.meta.url)),
  image_check_tool: await identity(path.join(root, 'tools/vita_capture_check.py')),
  vpk: await identity(vpk), emulator: await identity(executable), debugger: await identity(debuggerExe),
  asset_manifest: await identity(path.join(assets, 'vita-270p-assets.json')),
  captures: [], observations: [], inputs: [], runtime_verified: false, hardware_verified: false };
receipt.performance_enabled = measure;

await fs.mkdir(path.join(pref, 'ux0/data/knife-woh'), { recursive: true });
if (measure) await fs.writeFile(path.join(pref, 'ux0/data/knife-woh/perf-enable.txt'), '1\n', { flag: 'wx' });
await fs.symlink(assets, path.join(pref, 'ux0/data/knife-woh/assets'), 'junction');
await fs.symlink(process.env.KNIFE_VITA3K_FIRMWARE ?? (() => { throw new Error('Set KNIFE_VITA3K_FIRMWARE to your firmware directory'); })(), path.join(pref, 'vs0'), 'junction');
if (lang === 'zc') {
  // Use only our isolated generated DEV fixture; never a personal save.
  const source = path.join(root, '.work/vita/vita3k/pref-staged-1793140745791-9ceb68/ux0/data/knife-woh-dev/saves/witch/savedata000');
  const data = await fs.readFile(source);
  if (data.length !== 0x3FB800 || data.readInt16LE(0x28) !== 1) throw new Error('Unexpected generated English fixture layout');
  const before = digest(data);
  data.writeInt16LE(2, 0x28); // WITCH.sys_config (4) + wcfg.LANGUAGE (0x24).
  const dest = path.join(pref, 'ux0/data/knife-woh/saves/witch/savedata000');
  await fs.mkdir(path.dirname(dest), { recursive: true });
  await fs.writeFile(dest, data, { flag: 'wx' });
  receipt.generated_language_fixture = { source, source_sha256: before, output: await identity(dest),
    changed_offset: 0x28, old_i16: 1, new_i16: 2, personal_save: false };
}
let configText = await fs.readFile(path.join(path.dirname(executable), 'config.yml'), 'utf8');
receipt.config_overrides = { 'pref-path': JSON.stringify(pref.replaceAll('\\', '/') + '/'),
  'show-live-area-screen': 'false', 'check-for-updates-mode': '0', 'discord-rich-presence': 'false',
  'memory-mapping': 'disabled', 'backend-renderer': backend };
for (const [key, value] of Object.entries(receipt.config_overrides)) {
  const expression = new RegExp(`^${key}:.*$`, 'm');
  if (!expression.test(configText)) throw new Error(`Missing configuration ${key}`);
  configText = configText.replace(expression, `${key}: ${value}`);
}
const config = path.join(work, `config-${tag}.yml`);
await fs.writeFile(config, configText, { flag: 'wx' });
const debugDir = path.join(work, `debug-${tag}`);
const child = spawn(debuggerExe, [debugDir, '--', executable, '-c', config, '-f', '-w'], {
  cwd: work, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
  env: { ...process.env, VITA3K_MCP_PIPE: bridge.pipeName, VITA3K_MCP_TOKEN: bridge.token, VITA3K_MCP_ARTIFACT_ROOT: work } });
receipt.debug_directory = debugDir;
const chunks = [];
child.stdout.on('data', chunk => chunks.push(chunk));
child.stderr.on('data', chunk => chunks.push(chunk));
let exitResult;
const exited = new Promise(resolve => child.once('exit', (code, signal) => { exitResult = { code, signal }; resolve(exitResult); }));
async function capture(label) {
  const result = await bridge.request('screen.capture', { relativePath: `${tag}-${label}.png` }, 10000);
  if (!result.captured) throw new Error(`No frame captured: ${label}`);
  const filename = path.isAbsolute(result.path) ? result.path : path.join(work, result.path);
  receipt.captures.push({ label, ...result, artifact: await identity(filename) });
  console.log(`Captured ${label}: ${filename}`);
}
async function pulse(button) {
  receipt.inputs.push({ button, at: new Date().toISOString(), press: await bridge.request('input.set', { buttons: [button] }) });
  await delay(130);
  await bridge.request('input.clear', {});
}
try {
  await bridge.connect(20000);
  receipt.launch = await bridge.request('app.launch', { sessionId: tag, contentPath: vpk, appArgs: [], replace: false });
  let ready = false;
  for (let i = 0; i < 45; ++i) {
    // Creating a host GL context can hold the emulator UI thread during boot.
    // Keep a slow status reply distinct from a guest rendering failure.
    const state = await bridge.request('session.status', {}, 15000);
    receipt.observations.push({ at: new Date().toISOString(), ...state });
    if (state.phase === 'running' && state.frameReady) { ready = true; break; }
    if (exitResult) throw new Error(`Emulator exited during boot: ${JSON.stringify(exitResult)}`);
    await delay(1000);
  }
  if (!ready) throw new Error('Release did not produce a running frame within the observation bound');
  await delay(10000);
  await capture('startup');
  await pulse('cross');
  await delay(10000);
  for (let i = 0; i < 12; ++i) { await pulse('cross'); await delay(1800); }
  await capture('story');
  await pulse('triangle'); await delay(2500); await capture('menu');
  await pulse('circle'); await delay(1800); await capture('return');
  receipt.final_state = await bridge.request('session.status');
  receipt.control_sequence_completed = receipt.final_state.phase === 'running' && receipt.final_state.titleId === 'KNIF00270';
  // frameReady/FPS do not prove a visible picture: OpenGL can submit black frames.
  receipt.image_check = JSON.parse(execFileSync(process.env.KNIFE_VITA_CAPTURE_PYTHON ?? 'python',
    [path.join(root, 'tools/vita_capture_check.py')], {
      input: JSON.stringify(receipt.captures.map(capture => ({ label: capture.label,
        path: capture.artifact.path, expected_sha256: capture.artifact.sha256 }))),
      encoding: 'utf8', windowsHide: true, timeout: 10000, maxBuffer: 1024 * 1024 }));
  receipt.requested_stop = await bridge.request('session.stop', {}, 30000);
  receipt.after_stop = await bridge.request('session.status', {}, 5000);
  receipt.shutdown = await bridge.request('emulator.shutdown', {}, 3000);
} catch (error) {
  receipt.error = String(error);
  try { receipt.shutdown_after_error = await bridge.request('emulator.shutdown', {}, 2000); } catch {}
} finally {
  bridge.close();
  let timer;
  receipt.exit = await Promise.race([exited, new Promise(resolve => { timer = setTimeout(() => resolve(null), 30000); })]);
  clearTimeout(timer);
  if (!receipt.exit) {
    // The debugger owns this one child; request its established owned cleanup.
    receipt.forced_owned_cleanup = true;
    child.kill();
    receipt.exit = await exited;
  }
  const output = Buffer.concat(chunks);
  const log = path.join(work, `${tag}-host.log`);
  await fs.writeFile(log, output, { flag: 'wx' });
  receipt.host_log = await identity(log);
  if (measure) {
    // Read only this run's own profile after owned shutdown; preserve raw evidence.
    const source = path.join(pref, 'ux0/data/knife-woh/performance.log');
    try {
      const performance = path.join(root, 'doc/logs/vita', `${tag}-performance.log`);
      const data = await fs.readFile(source);
      await fs.writeFile(performance, data, { flag: 'wx' });
      receipt.performance_log = await identity(performance);
      receipt.performance_summary = JSON.parse(execFileSync(process.env.KNIFE_VITA_CAPTURE_PYTHON ?? 'python',
        [path.join(root, 'tools/read-vita-performance.py'), performance, '--environment', 'emulator'],
        { encoding: 'utf8', windowsHide: true, timeout: 10000, maxBuffer: 1024 * 1024 }));
      receipt.performance_verified = receipt.performance_summary.windows > 0
        && receipt.performance_summary.queue_and_jpeg_bound_violations.length === 0;
    } catch (error) { receipt.performance_error = String(error); receipt.performance_verified = false; }
  }
  const diagnostics = path.join(pref, 'ux0/data/knife-woh-dev');
  try { receipt.unexpected_diagnostic_files = await fs.readdir(diagnostics); }
  catch (error) { if (error.code !== 'ENOENT') throw error; receipt.unexpected_diagnostic_files = []; }
  receipt.runtime_verified = Boolean(receipt.control_sequence_completed && receipt.image_check?.required_views_nonblack
    && !receipt.error && receipt.exit?.code === 0
    && !receipt.forced_owned_cleanup && receipt.unexpected_diagnostic_files.length === 0
    && (!measure || receipt.performance_verified));
  const report = path.join(root, 'doc', `vita-270p-${tag}.json`);
  await fs.writeFile(report, JSON.stringify(receipt, null, 2) + '\n', { flag: 'wx' });
  console.log(JSON.stringify({ receipt: report, runtime_verified: receipt.runtime_verified,
    control_sequence_completed: receipt.control_sequence_completed,
    required_views_nonblack: receipt.image_check?.required_views_nonblack,
    exit: receipt.exit, error: receipt.error }, null, 2));
  process.exitCode = receipt.runtime_verified ? 0 : 1;
}
