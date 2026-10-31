// Uses the installed Vita3K bridge protocol; all test state is project-local.
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { promises as fs } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { GameInputScenario, parseGameLog, gameInteractionEvidence,
  waitForNaturalStop, gameExitEvidence } from './vita-game-evidence.mjs';
import { COMPRESSED_INPUTS, activeCompressedStage, compressedResultEvidence } from './vita-compressed-evidence.mjs';
import { UI_COLOR_STAGES, UI_COLOR_INPUTS, activeUiColorStage, uiColorResultEvidence } from './vita-ui-color-evidence.mjs';
import { validateGameAssetStage } from './vita-game-stage.mjs';
import { validate270pAssets } from './vita-270p-assets.mjs';
import { GameSaveScenario, gameSaveEvidence } from './vita-save-evidence.mjs';
import { GameBacklogScenario, gameBacklogEvidence } from './vita-backlog-evidence.mjs';
import { activeGroupPage, groupResultEvidence } from './vita-group-evidence.mjs';

function movieInputInventory(receipt) {
  if (receipt.schema !== 'knife-vita-three-mp4-native-probe-v1' || !Array.isArray(receipt.inputs)) {
    throw new Error('Missing original native movie probe input authority');
  }
  const expected = new Set(['movie/logo.mp4', 'movie/opening_en.mp4', 'movie/opening_zc.mp4']);
  const inputs = receipt.inputs.map(input => {
    if (!expected.delete(input.relative_path)) throw new Error(`Unexpected or duplicate movie input ${input.relative_path}`);
    if (!Number.isSafeInteger(input.bytes) || input.bytes < 1 || input.bytes > 1024 * 1024 * 1024
        || typeof input.sha256 !== 'string' || !/^[a-f0-9]{64}$/.test(input.sha256)) {
      throw new Error(`Invalid original movie identity ${input.relative_path}`);
    }
    return { relative_path: input.relative_path, bytes: input.bytes, sha256: input.sha256 };
  });
  if (expected.size || inputs.length !== 3) throw new Error('Incomplete original three-MP4 input inventory');
  return inputs;
}

function activeMovieStage(log) {
  let active;
  let ready = false;
  for (const line of log.split('\n')) {
    const begin = /^ASSET (LOGO|OPENING_EN|OPENING_ZC) BEGIN /.exec(line);
    if (begin) { active = begin[1]; ready = false; continue; }
    if (active && line.startsWith(`ASSET ${active} FRAME READY timestamp_ms=`)
        && /^ASSET (LOGO|OPENING_EN|OPENING_ZC) FRAME READY timestamp_ms=[1-9][0-9]* gpu_fence=COMPLETE$/.test(line)) ready = true;
    if (active && new RegExp(`^ASSET ${active} (NATIVE EOF|STOPPED|TEXTURE RETIRED|CLOSE |PASS |FAIL |CLEANUP FAIL)`).test(line)) {
      active = undefined; ready = false;
    }
    if (line.startsWith('RESULT ') || line.startsWith('GXM CLOSE ')) { active = undefined; ready = false; }
  }
  return ready ? active : undefined;
}

function movieResultEvidence(log, captures) {
  const lines = log.split('\n');
  const count = text => lines.filter(line => line === text).length;
  const prefixCount = prefix => lines.filter(line => line.startsWith(prefix)).length;
  const issues = [];
  const uniqueFields = prefix => {
    const selected = lines.filter(line => line.startsWith(prefix));
    if (selected.length !== 1) return undefined;
    const fields = Object.create(null);
    for (const field of selected[0].slice(prefix.length).split(' ')) {
      const separator = field.indexOf('=');
      if (separator < 1) return undefined;
      const key = field.slice(0, separator);
      if (Object.hasOwn(fields, key)) return undefined;
      fields[key] = field.slice(separator + 1);
    }
    return fields;
  };
  const samplingEvidence = stage => {
    const source = uniqueFields(`ASSET ${stage} SOURCE_VIDEO `);
    const native = uniqueFields(`ASSET ${stage} NATIVE_VIDEO `);
    const view = uniqueFields(`ASSET ${stage} FIRST_VIDEO `);
    if (!source || !native || !view || source.authority !== 'MP4_AVCC_SPS'
        || view.sampling_authority !== 'MP4_AVCC_SPS' || view.native_format !== 'GxmYvu420P2Csc1'
        || view.gpu_fence !== 'COMPLETE' || native.crop_units !== 'UNVERIFIED') return false;
    // These are the three digest-locked source files, whose SPS crop has been
    // independently read. Native Ex crop fields retain their raw API meaning.
    const expected = { coded_width: 960, coded_height: 544, crop_left: 0, crop_top: 0,
      crop_right: 0, crop_bottom: 4, visible_width: 960, visible_height: 540, sar_num: 1, sar_den: 1 };
    if (!Object.entries(expected).every(([key, value]) => source[key] !== '' && Number(source[key]) === value)) return false;
    const width = Number(native.storage_width), height = Number(native.storage_height);
    if (width !== 960 || (height !== 544 && height !== 540)
        || Number(view.texture_width) !== width || Number(view.texture_height) !== height
        || Number(view.width) !== 960 || Number(view.height) !== 540
        || !Number.isFinite(Number(native.raw_aspect)) || Number(native.raw_aspect) <= 0
        || !['crop_left','crop_right','crop_top','crop_bottom'].every(key =>
          Number.isSafeInteger(Number(native[key])) && Number(native[key]) >= 0 && Number(native[key]) <= 0xffffffff)
        || Number(view.timestamp_ms) !== Number(native.timestamp_ms)
        || view.native_layout !== (height === 544 ? 'Coded' : 'Visible')) return false;
    const uv = ['uv_left','uv_top','uv_right','uv_bottom'].map(key => Number(view[key]));
    const expectedUv = [0, 0, 1, height === 544 ? 540 / 544 : 1];
    return uv.every((value, index) => Number.isFinite(value) && Math.abs(value - expectedUv[index]) < 1e-6)
      && Number.isFinite(Number(view.display_aspect)) && Math.abs(Number(view.display_aspect) - 16 / 9) < 1e-5;
  };
  const results = [...log.matchAll(/^RESULT (PASS|FAIL)$/gm)];
  if (results.length !== 1 || results[0][1] !== 'PASS' || !log.endsWith('RESULT PASS\n')) {
    issues.push('Missing unambiguous final RESULT PASS');
  }
  if (count('TITLE_ID KNIF00011') !== 1) issues.push('Missing or ambiguous native movie title identity');
  if (count('GXM READY native_YVU420P2_CSC1 CPU_stride=UNKNOWN_AND_UNUSED') !== 1
      || count('GXM CLOSE PASS') !== 1 || prefixCount('GXM CLOSE ') !== 1) {
    issues.push('Missing checked native GXM initialization/teardown');
  }
  if (/^ASSET .*(?:\bFAIL\b)|^ERROR /m.test(log)) issues.push('Movie log contains a failure');
  const stages = ['LOGO', 'OPENING_EN', 'OPENING_ZC'].map(stage => {
    const capture = captures?.[stage];
    const sampling = samplingEvidence(stage);
    const markers = prefixCount(`ASSET ${stage} BEGIN `) === 1
      && prefixCount(`ASSET ${stage} FRAME READY timestamp_ms=`) === 1
      && prefixCount(`ASSET ${stage} FIRST_VIDEO width=960 height=540 `) === 1
      && count(`ASSET ${stage} TEXTURE RETIRED gpu_fence=COMPLETE`) === 1
      && count(`ASSET ${stage} CLOSE PASS`) === 1 && prefixCount(`ASSET ${stage} CLOSE `) === 1
      && prefixCount(`ASSET ${stage} PASS `) === 1;
    const controls = stage === 'LOGO'
      ? prefixCount('ASSET LOGO FIRST_AUDIO ') === 1 && prefixCount('ASSET LOGO NATIVE EOF ') === 1
      : count(`ASSET ${stage} EXPECTED_AUDIO_ABSENT source_inventory=VIDEO_ONLY`) === 1
        && prefixCount(`ASSET ${stage} PAUSED `) === 1 && prefixCount(`ASSET ${stage} RESUMED `) === 1
        && prefixCount(`ASSET ${stage} STOPPED `) === 1
        && prefixCount(`ASSET ${stage} SEEK `) === 1
        && (prefixCount(`ASSET ${stage} SEEK REJECTED error=`) === 1
          || prefixCount(`ASSET ${stage} SEEK OBSERVED target_ms=1000 `) === 1);
    const captured = capture?.captured === true && capture.stage_valid_during_capture === true
      && capture.width === 960 && capture.height === 544 && capture.artifact?.bytes > 0
      && typeof capture.artifact?.sha256 === 'string' && /^[a-f0-9]{64}$/.test(capture.artifact.sha256);
    if (!markers || !controls) issues.push(`Missing actual ${stage} decode/control/close markers`);
    if (!sampling) issues.push(`Missing consistent ${stage} source SPS/native texture/UV evidence`);
    if (!captured) issues.push(`Missing captured artifact while ${stage} was still active`);
    return { stage, markers, sampling, controls, captured };
  });
  return { valid: issues.length === 0, issues, stages };
}

async function stableFileIdentity(filename) {
  const before = await fs.stat(filename);
  if (!before.isFile() || before.size < 1 || before.size > 1024 * 1024 * 1024) throw new Error(`Invalid movie file ${filename}`);
  const digest = createHash('sha256');
  let bytes = 0;
  for await (const block of createReadStream(filename, { highWaterMark: 1024 * 1024 })) {
    bytes += block.length;
    if (bytes > 1024 * 1024 * 1024) throw new Error('Movie input grew past the native file bound');
    digest.update(block);
  }
  const after = await fs.stat(filename);
  if (bytes !== before.size || after.size !== before.size || after.mtimeMs !== before.mtimeMs
      || after.ino !== before.ino || after.dev !== before.dev) throw new Error(`Movie changed while hashing: ${filename}`);
  return { bytes, sha256: digest.digest('hex') };
}

async function stageCanonicalGameAssets(root, pref, overrideSource) {
  const canonical = path.join(root, 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets');
  const source = overrideSource ?? canonical;
  const canonicalSource = await fs.realpath(source);
  let stageManifest;
  if (overrideSource) {
    const profile270 = path.join(root, 'dist/Witch-on-the-Holy-Night-Chengkong-270p-Vita/assets');
    stageManifest = canonicalSource.toLowerCase() === path.resolve(profile270).toLowerCase()
      ? await validate270pAssets({ source: canonicalSource, canonical, expectedRoot: profile270 })
      : await validateGameAssetStage({ source: canonicalSource,
        stagedBase: path.join(root, '.work/vita/game-gxt-stage'), canonical });
  }
  const profile = await fs.readFile(path.join(canonicalSource, 'language-profile.txt'), 'utf8');
  if (profile.replaceAll('\r\n', '\n').trim() !== 'knife-languages-v1\n1\ten\tEnglish\n2\tzc\t中文（澄空）') {
    throw new Error('Actual game assets require the existing English/Chengkong asset profile');
  }
  const destination = path.join(pref, 'ux0/data/knife-woh/assets');
  await fs.mkdir(path.dirname(destination), { recursive: true });
  await fs.mkdir(path.join(pref, 'ux0/data/knife-woh-dev'), { recursive: true });
  try {
    await fs.lstat(destination);
    if ((await fs.realpath(destination)).toLowerCase() !== canonicalSource.toLowerCase()) {
      throw new Error('Existing game asset staging points elsewhere; it will not be replaced');
    }
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
    await fs.symlink(canonicalSource, destination, 'junction');
  }
  return { source: canonicalSource, guest_path: 'ux0:data/knife-woh/assets',
    ...(stageManifest ? { staged_manifest: stageManifest } : {}),
    method: 'NTFS junction; ordinary game code reads source assets; no complete asset copy',
    language_profile_sha256: createHash('sha256').update(profile).digest('hex'),
    limitations: 'No OS-enforced read-only mount; game saves use the separate DEV namespace' };
}

async function movieCaptureIdentity(capture, work) {
  if (!capture.captured || capture.width !== 960 || capture.height !== 544 || typeof capture.path !== 'string') {
    throw new Error('Movie capture did not produce the expected actual 960x544 frame');
  }
  const filename = await fs.realpath(capture.path);
  const relative = path.relative(await fs.realpath(work), filename);
  if (relative.startsWith('..') || path.isAbsolute(relative)) throw new Error('Movie capture is outside this DEV run');
  const stat = await fs.stat(filename);
  if (!stat.isFile() || stat.size < 26 || stat.size > 16 * 1024 * 1024) throw new Error('Invalid bounded movie capture artifact');
  const bytes = await fs.readFile(filename);
  if (!bytes.subarray(0, 8).equals(Buffer.from([137,80,78,71,13,10,26,10]))
      || bytes.readUInt32BE(16) !== 960 || bytes.readUInt32BE(20) !== 544) {
    throw new Error('Movie capture PNG metadata does not match the bridge frame');
  }
  return { path: filename, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') };
}

async function captureGameFrame(bridge, logfile, work, relativePath, timeoutMs = 5000) {
  const before = await fs.readFile(logfile, 'utf8');
  const capture = await bridge.request('screen.capture', { relativePath }, timeoutMs);
  const after = await fs.readFile(logfile, 'utf8');
  const beforeEvidence = parseGameLog(before);
  const afterEvidence = parseGameLog(after);
  const attempt = { ...capture, bridge_captured: capture.captured, at: new Date().toISOString(),
    before_state: beforeEvidence.latest_state, after_state: afterEvidence.latest_state,
    before_log_length: before.length, after_log_length: after.length,
    before_log_sha256: createHash('sha256').update(before).digest('hex'),
    after_log_sha256: createHash('sha256').update(after).digest('hex') };
  try {
    attempt.artifact = { ...await movieCaptureIdentity(capture, work), width: 960, height: 544 };
  } catch (error) { attempt.artifact_error = String(error); }
  attempt.captured = capture.captured === true && Boolean(attempt.artifact)
    && beforeEvidence.issues.length === 0 && afterEvidence.issues.length === 0
    && Boolean(attempt.before_state) && Boolean(attempt.after_state);
  return { attempt, after };
}

async function readWitchSaveSnapshot(filename) {
  const before = await fs.stat(filename);
  if (!before.isFile() || before.size !== 0x3fb800) throw new Error('Missing exact-size Witch save file');
  const bytes = await fs.readFile(filename);
  const after = await fs.stat(filename);
  if (bytes.length !== before.size || after.size !== before.size || after.mtimeMs !== before.mtimeMs
      || after.ino !== before.ino || after.dev !== before.dev) throw new Error('Witch save changed while reading');
  return bytes;
}

async function readRetainedSaveSnapshots(interactions, work) {
  const files = [];
  for (const capture of interactions?.captures ?? []) {
    const artifact = capture.file_artifact;
    if (!artifact || artifact.bytes !== 0x3fb800) throw new Error('Missing retained original save file');
    const filename = await fs.realpath(artifact.path), relative = path.relative(await fs.realpath(work), filename);
    if (relative.startsWith('..') || path.isAbsolute(relative)) throw new Error('Retained save escapes this DEV run');
    const bytes = await readWitchSaveSnapshot(filename);
    if (createHash('sha256').update(bytes).digest('hex') !== artifact.sha256) throw new Error('Retained save identity changed');
    // Recheck each actual PNG too; a receipt cannot stand in for its artifact.
    const png = await movieCaptureIdentity(capture, work);
    if (png.bytes !== capture.artifact.bytes || png.sha256 !== capture.artifact.sha256)
      throw new Error('Retained save/load screenshot identity changed');
    files.push(bytes);
  }
  return files;
}

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
let mode = 'platform';
let mapping = 'disabled';
let renderer = 'Vulkan';
let gameInputs = false;
let gameCgInputs = false;
let gameSaveInputs = false;
let gameBacklogInputs = false;
let gameAssetsRootArgument;
let vpkArgument;
while (args.length) {
  const argument = args.shift();
  if (argument === '--gxm') mode = 'gxm';
  else if (argument === '--image') mode = 'image';
  else if (argument === '--effects') mode = 'effects';
  else if (argument === '--audio') mode = 'audio';
  else if (argument === '--audio-mixer') mode = 'audio-mixer';
  else if (argument === '--pipeline') mode = 'pipeline';
  else if (argument === '--text') mode = 'text';
  else if (argument === '--game') mode = 'game';
  else if (argument === '--game-inputs') { mode = 'game'; gameInputs = true; }
  else if (argument === '--game-cg-inputs') { mode = 'game'; gameInputs = true; gameCgInputs = true; }
  else if (argument === '--game-save-inputs') { mode = 'game'; gameInputs = true; gameSaveInputs = true; }
  else if (argument === '--game-backlog-inputs') { mode = 'game'; gameInputs = true; gameBacklogInputs = true; }
  else if (argument === '--game-assets-root') {
    if (gameAssetsRootArgument !== undefined || !args.length || args[0].startsWith('--')) {
      throw new Error('--game-assets-root requires one absolute directory and may occur once');
    }
    gameAssetsRootArgument = args.shift();
  }
  else if (argument === '--movie') mode = 'movie';
  else if (argument === '--compressed') mode = 'compressed';
  else if (argument === '--ui-color') mode = 'ui-color';
  else if (argument === '--memory-mapping') mapping = args.shift();
  else if (argument === '--renderer') renderer = args.shift();
  else if (argument.startsWith('--') || vpkArgument) {
    throw new Error('Usage: verify-vita.mjs [--gxm|--image|--effects|--audio|--audio-mixer|--pipeline|--text|--game|--game-inputs|--game-cg-inputs|--game-save-inputs|--game-backlog-inputs|--movie|--compressed|--ui-color] [--memory-mapping disabled|page-table] [--renderer Vulkan|OpenGL] [--game-assets-root ABSOLUTE_STAGED_ASSETS] [DEV.vpk]');
  } else vpkArgument = argument;
}
if (!['disabled', 'page-table'].includes(mapping) || !['Vulkan', 'OpenGL'].includes(renderer)) {
  throw new Error('Unsupported mapping or renderer');
}
if (gameInputs && mode !== 'game') throw new Error('--game-inputs cannot be combined with another probe mode');
if (gameSaveInputs && (gameCgInputs || gameBacklogInputs || !gameAssetsRootArgument))
  throw new Error('--game-save-inputs requires a fresh --game-assets-root profile and cannot combine with CG inputs');
if (gameBacklogInputs && (gameCgInputs || !gameAssetsRootArgument))
  throw new Error('--game-backlog-inputs requires a fresh --game-assets-root profile and cannot combine with CG inputs');
if (gameAssetsRootArgument !== undefined && (mode !== 'game' || !path.isAbsolute(gameAssetsRootArgument))) {
  throw new Error('--game-assets-root requires --game/--game-inputs and an absolute staged directory');
}
const graphical = ['gxm', 'effects', 'pipeline', 'text', 'game', 'movie', 'compressed', 'ui-color'].includes(mode);
const assetRunTag = gameAssetsRootArgument ? `staged-${Date.now()}-${randomBytes(3).toString('hex')}` : undefined;
const runName = graphical ? `${gameBacklogInputs ? 'game-backlog-inputs' : gameSaveInputs ? 'game-save-inputs' : gameCgInputs ? 'game-cg-inputs' : gameInputs ? 'game-inputs' : mode}${assetRunTag ? '-' + assetRunTag : ''}-${renderer.toLowerCase()}-${mapping}` : mode;
const executable = process.env.KNIFE_VITA3K ?? (() => { throw new Error('Set KNIFE_VITA3K to the emulator executable'); })();
const bridgePath = path.join(path.dirname(executable), 'mcp/server/dist/bridge-client.js');
const { BridgeClient } = await import(pathToFileURL(bridgePath).href);
const work = path.join(root, '.work/vita/vita3k');
const pref = path.join(work, assetRunTag ? `pref-${assetRunTag}` : 'pref');
const evidence = path.join(root, 'doc/logs/vita');
if (assetRunTag) await fs.mkdir(pref); // A staged run must have a fresh, distinct emulator profile.
else await fs.mkdir(pref, { recursive: true });
await fs.mkdir(evidence, { recursive: true });
// Only the six existing fonts used by the original text engine are staged.
// Verify the recorded input identities before launching the guest.
const stagedInputs = [];
let compressedStaging;
let uiColorStaging;
if (mode === 'ui-color') {
  const fixtureRoot = path.join(root, '.work/vita/ui-color-fixtures');
  const manifestPath = path.join(fixtureRoot, 'manifest.json');
  const manifestBytes = await fs.readFile(manifestPath);
  if (manifestBytes.length > 1024 * 1024) throw new Error('UI color fixture manifest exceeds its bound');
  const manifest = JSON.parse(manifestBytes);
  if (manifest.schema !== 'knife-vita-ui-color-fixtures-v1' || manifest.synthetic !== true
      || manifest.no_game_data !== true || manifest.actualexecuted !== true
      || manifest.contract?.title_id !== 'KNIF00013'
      || JSON.stringify(manifest.contract.stages) !== JSON.stringify(UI_COLOR_STAGES)) {
    throw new Error('UI color probe requires the executed synthetic production-color fixtures');
  }
  const expectedSizes = [160 * 112 * 4, 960 * 540 * 4, 160 * 112 * 4, 32832];
  for (const [index, name] of UI_COLOR_INPUTS.entries()) {
    const input = manifest.files?.[name];
    const source = path.join(fixtureRoot, name), observed = await stableFileIdentity(source);
    if (observed.bytes !== expectedSizes[index] || observed.bytes !== input?.bytes || observed.sha256 !== input.sha256) {
      throw new Error(`Changed or invalid UI color fixture: ${name}`);
    }
    const destination = path.join(pref, 'ux0/data/knife-woh-dev/ui-color-fixtures', name);
    await fs.mkdir(path.dirname(destination), { recursive: true });
    await fs.copyFile(source, destination);
    const copied = await stableFileIdentity(destination);
    if (copied.bytes !== observed.bytes || copied.sha256 !== observed.sha256) throw new Error(`UI fixture copy changed: ${name}`);
    stagedInputs.push({ relative_path: name, ...observed, guest_path: `ux0:data/knife-woh-dev/ui-color-fixtures/${name}` });
  }
  uiColorStaging = { manifest_path: manifestPath, fixture_manifest_sha256: createHash('sha256').update(manifestBytes).digest('hex'),
    verified: stagedInputs.length === 4, synthetic: true, no_game_data: true };
}
if (mode === 'compressed') {
  const fixtureRoot = path.join(root, '.work/vita/compressed-fixtures');
  const manifestPath = path.join(fixtureRoot, 'manifest.json');
  const manifestBytes = await fs.readFile(manifestPath);
  const manifest = JSON.parse(manifestBytes);
  if (manifest.schema !== 'knife-vita-compressed-synthetic-fixtures-v1' || manifest.synthetic !== true
      || manifest.no_game_data !== true || manifest.verified !== true || manifest.failures?.length) {
    throw new Error('Compressed probe requires verified synthetic offline fixtures');
  }
  for (const name of COMPRESSED_INPUTS) {
    const matches = manifest.files.filter(file => file.path === name);
    if (matches.length !== 1) throw new Error(`Missing/ambiguous compressed fixture identity: ${name}`);
    const input = matches[0], bytes = await fs.readFile(path.join(fixtureRoot, name));
    const sha256 = createHash('sha256').update(bytes).digest('hex');
    if (!bytes.length || bytes.length > 16 * 1024 * 1024 || bytes.length !== input.bytes || sha256 !== input.sha256) {
      throw new Error(`Changed or oversized compressed fixture: ${name}`);
    }
    const destination = path.join(pref, 'ux0/data/knife-woh-dev/compressed-fixtures', name);
    await fs.mkdir(path.dirname(destination), { recursive: true });
    await fs.writeFile(destination, bytes);
    const observed = await stableFileIdentity(destination);
    if (observed.bytes !== bytes.length || observed.sha256 !== sha256) throw new Error(`Staged fixture changed: ${name}`);
    stagedInputs.push({ relative_path: name, bytes: bytes.length, sha256, guest_path: `ux0:data/knife-woh-dev/compressed-fixtures/${name}` });
  }
  compressedStaging = { manifest_path: manifestPath, fixture_manifest_sha256: createHash('sha256').update(manifestBytes).digest('hex'),
    verified: stagedInputs.length === 9, synthetic: true, no_game_data: true };
}
if (mode === 'text') {
  const fontReceipt = JSON.parse(await fs.readFile(path.join(root, 'doc/vita-text-verification.json'), 'utf8'));
  const assetRoot = path.join(root, 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets');
  const selected = new Set(['en/HelveticaNeueLTGEO-55Roman.otf', 'zc/arshanghaisonggbpro_lt.otf',
    'nintendo_udsg-r_std_003.bfttf', 'nintendo_udsg-r_org_zh-cn_003.bfttf',
    'nintendo_udsg-r_ext_zh-cn_003.bfttf', 'exfont_nx01.bffnt']);
  for (const input of fontReceipt.inputs) {
    const source = path.resolve(root, input.path);
    const relative = path.relative(assetRoot, source).replaceAll('\\', '/');
    if (!selected.delete(relative)) throw new Error(`Unexpected or duplicate text input ${relative}`);
    const bytes = await fs.readFile(source);
    const digest = createHash('sha256').update(bytes).digest('hex');
    if (bytes.length !== input.bytes || digest !== input.sha256) throw new Error(`Text input changed: ${relative}`);
    const destination = path.join(pref, 'ux0/data/knife-woh-dev/text-assets', relative);
    await fs.mkdir(path.dirname(destination), { recursive: true });
    await fs.writeFile(destination, bytes);
    stagedInputs.push({ relative_path: relative, bytes: bytes.length, sha256: digest });
  }
  if (selected.size) throw new Error('Incomplete text input inventory');
}
let movieStaging;
if (mode === 'game') stagedInputs.push(await stageCanonicalGameAssets(root, pref, gameAssetsRootArgument));
if (mode === 'movie') {
  const authorityPath = path.join(root, 'doc/vita-movie-probe-verification.json');
  movieStaging = { checked_at: new Date().toISOString(), authority_path: authorityPath,
    input_root: 'ux0:data/knife-woh/assets', inputs: [], verified: false, runtime_executed: false };
  try {
    const authorityBytes = await fs.readFile(authorityPath);
    movieStaging.authority_sha256 = createHash('sha256').update(authorityBytes).digest('hex');
    const inventory = movieInputInventory(JSON.parse(authorityBytes));
    const junction = await stageCanonicalGameAssets(root, pref);
    movieStaging.junction = junction;
    stagedInputs.push(junction);
    for (const input of inventory) {
      const filename = await fs.realpath(path.join(junction.source, input.relative_path));
      const relative = path.relative(junction.source, filename);
      if (relative.startsWith('..') || path.isAbsolute(relative)) throw new Error(`Movie input escapes the canonical asset root: ${input.relative_path}`);
      const actual = await stableFileIdentity(filename);
      const observed = { relative_path: input.relative_path, source: filename,
        guest_path: `ux0:data/knife-woh/assets/${input.relative_path}`,
        expected_bytes: input.bytes, expected_sha256: input.sha256,
        ...actual, verified: actual.bytes === input.bytes && actual.sha256 === input.sha256 };
      movieStaging.inputs.push(observed);
      if (!observed.verified) throw new Error(`Original movie input changed: ${input.relative_path}`);
      stagedInputs.push(observed);
    }
    movieStaging.verified = movieStaging.inputs.length === 3 && movieStaging.inputs.every(input => input.verified);
  } catch (error) {
    movieStaging.error = String(error);
    throw error;
  } finally {
    // Keep this prelaunch check even when a mismatched/incomplete inventory
    // rejects launch, and preserve each attempt independently of later runs.
    const filename = path.join(evidence, `${runName}-staged-inputs-${Date.now()}-${randomBytes(3).toString('hex')}.json`);
    const bytes = JSON.stringify(movieStaging, null, 2) + '\n';
    await fs.writeFile(filename, bytes, { flag: 'wx' });
    await fs.writeFile(path.join(root, 'doc/vita-movie-staged-inputs.json'), bytes);
    movieStaging = { path: filename, sha256: createHash('sha256').update(bytes).digest('hex'),
      verified: movieStaging.verified, inputs: movieStaging.inputs };
  }
}
// Reuse the existing firmware tree; all emulator writes stay in this DEV profile.
const firmware = process.env.KNIFE_VITA3K_FIRMWARE ?? (() => { throw new Error('Set KNIFE_VITA3K_FIRMWARE to your firmware directory'); })();
const firmwareLink = path.join(pref, 'vs0');
try { await fs.lstat(firmwareLink); }
catch (error) {
  if (error.code !== 'ENOENT') throw error;
  await fs.symlink(path.resolve(firmware), firmwareLink, 'junction');
}
const config = path.join(work, assetRunTag ? `config-${assetRunTag}.yml` : 'config.yml');
// This installed fork requires the full keyboard configuration even on a DEV run.
let configText = await fs.readFile(path.join(path.dirname(executable), 'config.yml'), 'utf8');
const configOverrides = {
  'pref-path': JSON.stringify(pref.replaceAll('\\', '/') + '/'),
  'show-live-area-screen': 'false', 'check-for-updates-mode': '0',
  'discord-rich-presence': 'false',
  // Both mapping values are non-default in this fork, so its config merge
  // applies them. Keep each renderer/mapping result independent: PageTable
  // has a normal context-destruction deadlock in this exact emulator build.
  // The fork ignores override values equal to its compiled defaults when
  // merging with the user's config. Its backend selector is case-insensitive;
  // use the non-default spelling so requested Vulkan replaces a saved OpenGL.
  ...(graphical ? { 'memory-mapping': mapping, 'backend-renderer': renderer.toUpperCase() } : {}),
};
for (const [key, value] of Object.entries(configOverrides)) {
  const line = new RegExp(`^${key}:.*$`, 'm');
  if (!line.test(configText)) throw new Error(`Missing Vita3K configuration key ${key}`);
  configText = configText.replace(line, `${key}: ${value}`);
}
await fs.writeFile(config, configText);
const pipeName = `knife-vita-${process.pid}-${randomBytes(6).toString('hex')}`;
const bridge = new BridgeClient(pipeName, randomBytes(32).toString('hex'));
const debuggerExecutable = process.env.KNIFE_VITA3K_DEBUGGER;
let hostDebugger;
if (debuggerExecutable) {
  if (!path.isAbsolute(debuggerExecutable)) throw new Error('KNIFE_VITA3K_DEBUGGER must be an absolute executable path');
  hostDebugger = { executable: debuggerExecutable, executable_identity: await stableFileIdentity(debuggerExecutable),
    source_identity: await stableFileIdentity(path.join(root, 'tools/vita3k-owned-debugger.cpp')),
    target_identity: await stableFileIdentity(executable),
    output_directory: path.join(work, `debug-${runName}-${Date.now()}-${randomBytes(3).toString('hex')}`),
    scope: 'debug events of the owned Vita3K child only; exception handling and actual exit preserved',
    exit_wait_ms: 30000 };
}
const emulatorArguments = ['-c', config, '-f', '-w'];
const child = spawn(debuggerExecutable ?? executable,
  hostDebugger ? [hostDebugger.output_directory, '--', executable, ...emulatorArguments] : emulatorArguments, {
  cwd: work, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
  env: { ...process.env, VITA3K_MCP_PIPE: pipeName,
    VITA3K_MCP_TOKEN: bridge.token, VITA3K_MCP_ARTIFACT_ROOT: work },
});
const chunks = [];
child.stdout.on('data', data => chunks.push(data));
child.stderr.on('data', data => chunks.push(data));
const exited = new Promise(resolve => child.once('exit', (code, signal) => resolve({ code, signal })));
let receipt = { tested_at: new Date().toISOString(), executable, profile: pref, config_overrides: configOverrides,
  ...(hostDebugger ? { host_debugger: { ...hostDebugger, launcher_pid: child.pid } } : {}),
  ...(assetRunTag ? { game_assets_override: { requested_path: gameAssetsRootArgument,
    effective_source: stagedInputs.find(item => item.guest_path === 'ux0:data/knife-woh/assets')?.source,
    stage_manifest: stagedInputs.find(item => item.guest_path === 'ux0:data/knife-woh/assets')?.staged_manifest,
    language_profile_sha256: stagedInputs.find(item => item.guest_path === 'ux0:data/knife-woh/assets')?.language_profile_sha256,
    language_contract: ['1 en English', '2 zc 中文（澄空）'], profile_isolated: true } } : {}),
  ...(stagedInputs.length ? { staged_inputs: stagedInputs } : {}),
  ...(compressedStaging ? { compressed_staging: compressedStaging,
    fixture_manifest_sha256: compressedStaging.fixture_manifest_sha256, expected_title_id: 'KNIF00012',
    pixel_verified: false, claim_limitations: 'synthetic compressed sampling, lifecycle and timings in Vita3K; pixel oracle recorded separately; no real-hardware performance claim' } : {}),
  ...(uiColorStaging ? { ui_color_staging: uiColorStaging,
    fixture_manifest_sha256: uiColorStaging.fixture_manifest_sha256, expected_title_id: 'KNIF00013',
    evidence_kind: 'gpu-capture', pixel_verified: false,
    claim_limitations: 'synthetic production-native Alter/Sprite/Alpha/Present only; archive/cache/facade and real hardware remain separate' } : {}),
  ...(movieStaging ? { staged_input_receipt: movieStaging, runtime_timeout_seconds: 60,
    expected_title_id: 'KNIF00011',
    claim_limitations: 'finite native decode/control/EOF and captured frame artifacts; not a pixel oracle, audible output, full openings or successful native seek' } : {}),
  ...(graphical ? { renderer_requested: renderer } : {}),
  scope: gameBacklogInputs ? 'DEV fresh profile; real Menu/return then bounded history advancement, backlog open/older/newer/close/reopen/close; typed readonly cursor/scroll evidence and frames; finite Game/Ui only'
    : gameSaveInputs ? 'DEV fresh profile; twelve real controller pulses: write one empty manual slot, advance, load and restore; actual file snapshots and frames; finite Game/Ui only'
    : gameCgInputs ? 'DEV real controller story/Menu/return then at most 12 Cross pulses to actual scene 1 pc >= 100; finite Game/Ui only'
    : gameInputs ? 'DEV real controller cross/story, triangle/Menu screenshot, circle/return; finite Game/Ui only'
    : mode === 'game' ? 'DEV actual Game/Ui entry with a finite frame limit; no full-playthrough claim' : `DEV ${mode} probe`,
  runtime_verified: false, hardware_verified: false };
let logfile;
let nativeLogfile;
let inputScenario;
const savePath = path.join(pref, 'ux0/data/knife-woh-dev/saves/witch/savedata000');
async function validateGameInputs(log) {
  if (gameBacklogInputs) return gameBacklogEvidence(log, receipt.game_interactions);
  if (!gameSaveInputs) return gameInteractionEvidence(log, receipt.game_interactions);
  const files = await readRetainedSaveSnapshots(receipt.game_interactions, work);
  return gameSaveEvidence(log, receipt.game_interactions, files);
}
try {
  await bridge.connect(20000);
  console.log('Vita3K bridge connected');
  const vpk = path.resolve(vpkArgument ?? path.join(root, `.work/vita/${mode}/knife-vita-${mode}-dev.vpk`));
  receipt.vpk_sha256 = createHash('sha256').update(await fs.readFile(vpk)).digest('hex');
  logfile = path.join(pref, mode === 'game' ? 'ux0/data/knife-woh-dev/game-runtime.log' : `ux0/data/knife-woh-dev/${mode}-probe.log`);
  nativeLogfile = graphical ? path.join(pref, 'ux0/data/knife-woh-dev/gxm-native.log') : undefined;
  // Preserve the prior DEV result, then require this launch to create a new file.
  // The installed runtime did not truncate an existing log in a previous run.
  for (const [filename, key] of [[logfile, 'previous_log'], [nativeLogfile, 'previous_native_log']]) {
    if (!filename) continue;
    try {
      const previous = `${filename}.previous-${Date.now()}`;
      await fs.rename(filename, previous);
      receipt[key] = previous;
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  // NTFS can defer LastWriteTime updates while the Vita log handle stays open.
  // Renaming the prior result and checking absence proves freshness without
  // excluding all of the live observations until the probe closes its file.
  try {
    await fs.lstat(logfile);
    throw new Error('DEV log still exists before launch');
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
  receipt.log_was_absent_before_launch = true;
  if (gameSaveInputs) {
    try {
      await fs.lstat(path.dirname(path.dirname(savePath)));
      throw new Error('Save/load validation requires an absent DEV saves directory before launch');
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
    receipt.save_path_was_absent_before_launch = true;
    receipt.save_guest_path = 'ux0:data/knife-woh-dev/saves/witch/savedata000';
    receipt.save_host_path = savePath;
  }
  const launchRequestedAt = Date.now();
  receipt.launch = await bridge.request('app.launch', {
    sessionId: `knife-${mode}-${Date.now()}`, contentPath: vpk, appArgs: [], replace: false,
  });
  const deadline = Date.now() + (gameSaveInputs || gameBacklogInputs ? 300000 : gameInputs ? 180000 : mode === 'game' ? 120000 : mode === 'movie' ? 60000 : ['compressed', 'ui-color', 'effects'].includes(mode) ? 45000 : 20000);
  if (gameInputs) {
    inputScenario = gameBacklogInputs ? new GameBacklogScenario() : gameSaveInputs ? new GameSaveScenario() : new GameInputScenario(gameCgInputs
      ? { stepTimeoutMs: 30000, continuation: { scene: 1, pc: 100, max_pulses: 12 } } : {});
    receipt.game_interactions = inputScenario.receipt;
  }
  while (Date.now() < deadline) {
    if (mode === 'effects') {
      try {
        const before = await fs.readFile(logfile, 'utf8'), page = activeGroupPage(before);
        if (page && !receipt.group_captures?.[page]?.captured) {
          const stateBefore = await bridge.request('session.status', {}, 2000);
          const capture = await bridge.request('screen.capture', {
            relativePath: `${runName}-group-${page}-${launchRequestedAt}-${(receipt.group_capture_attempts?.length ?? 0) + 1}.png`,
          }, Math.max(1, Math.min(5000, deadline - Date.now())));
          const stateAfter = await bridge.request('session.status', {}, 2000);
          const after = await fs.readFile(logfile, 'utf8');
          const observed = (log, session) => ({ active_page: activeGroupPage(log), log_length: log.length,
            log_sha256: createHash('sha256').update(log).digest('hex'), session });
          const live = state => state.phase === 'running' && state.frameReady === true
            && state.resolution?.width === 960 && state.resolution?.height === 544;
          const attempt = { ...capture, page, bridge_captured: capture.captured, at: new Date().toISOString(),
            state_before: observed(before, stateBefore), state_after: observed(after, stateAfter),
            page_valid_during_capture: activeGroupPage(after) === page && after.startsWith(before)
              && live(stateBefore) && live(stateAfter) };
          try { attempt.artifact = await movieCaptureIdentity(capture, work); }
          catch (error) { attempt.artifact_error = String(error); }
          attempt.captured = capture.captured === true && attempt.page_valid_during_capture && Boolean(attempt.artifact);
          (receipt.group_capture_attempts ??= []).push(attempt);
          if (attempt.captured) { receipt.group_captures ??= {}; receipt.group_captures[page] = attempt; }
        }
      } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    if (mode === 'ui-color') {
      try {
        const before = await fs.readFile(logfile, 'utf8'), stage = activeUiColorStage(before);
        if (stage && !receipt.ui_color_captures?.[stage]?.captured) {
          const stateBefore = await bridge.request('session.status', {}, 2000);
          const capture = await bridge.request('screen.capture', {
            relativePath: `${runName}-${stage.toLowerCase()}-${launchRequestedAt}-${(receipt.ui_color_capture_attempts?.length ?? 0) + 1}.png`,
          }, Math.max(1, Math.min(5000, deadline - Date.now())));
          const stateAfter = await bridge.request('session.status', {}, 2000);
          const after = await fs.readFile(logfile, 'utf8');
          const observed = (log, session) => ({ active_stage: activeUiColorStage(log), log_length: log.length,
            log_sha256: createHash('sha256').update(log).digest('hex'), last_line: log.trim().split('\n').at(-1), session });
          const live = state => state.phase === 'running' && state.frameReady === true
            && state.resolution?.width === 960 && state.resolution?.height === 544;
          const attempt = { ...capture, stage, bridge_captured: capture.captured, at: new Date().toISOString(),
            state_before: observed(before, stateBefore), state_after: observed(after, stateAfter),
            stage_valid_during_capture: activeUiColorStage(after) === stage && after.startsWith(before)
              && live(stateBefore) && live(stateAfter) };
          try { attempt.artifact = await movieCaptureIdentity(capture, work); }
          catch (error) { attempt.artifact_error = String(error); }
          attempt.captured = capture.captured === true && attempt.stage_valid_during_capture && Boolean(attempt.artifact);
          (receipt.ui_color_capture_attempts ??= []).push(attempt);
          if (attempt.captured) { receipt.ui_color_captures ??= {}; receipt.ui_color_captures[stage] = attempt; }
        }
      } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    if (mode === 'compressed') {
      try {
        const before = await fs.readFile(logfile, 'utf8');
        const stage = activeCompressedStage(before);
        if (stage && !receipt.compressed_captures?.[stage]?.captured) {
          const capture = await bridge.request('screen.capture', {
            relativePath: `${runName}-${stage.toLowerCase()}-${launchRequestedAt}.png`,
          }, Math.max(1, Math.min(5000, deadline - Date.now())));
          const after = await fs.readFile(logfile, 'utf8');
          const attempt = { ...capture, stage, bridge_captured: capture.captured, at: new Date().toISOString(),
            stage_valid_during_capture: activeCompressedStage(after) === stage,
            before_log_sha256: createHash('sha256').update(before).digest('hex'),
            after_log_sha256: createHash('sha256').update(after).digest('hex') };
          try { attempt.artifact = await movieCaptureIdentity(capture, work); }
          catch (error) { attempt.artifact_error = String(error); }
          attempt.captured = capture.captured === true && attempt.stage_valid_during_capture && Boolean(attempt.artifact);
          (receipt.compressed_capture_attempts ??= []).push(attempt);
          if (attempt.captured) { receipt.compressed_captures ??= {}; receipt.compressed_captures[stage] = attempt; }
        }
      } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    if (mode === 'movie') {
      try {
        // Read fresh published data before AND after capture. The same asset
        // must still be active throughout; old stage markers cannot authorize
        // a later asset's screenshot or a frame after native EOF/stop.
        const before = await fs.readFile(logfile, 'utf8');
        const stage = activeMovieStage(before);
        if (stage && !receipt.movie_captures?.[stage]?.captured) {
          const capture = await bridge.request('screen.capture', {
            relativePath: `${runName}-${stage.toLowerCase()}-frame-${launchRequestedAt}-${(receipt.movie_capture_attempts?.length ?? 0) + 1}.png`,
          }, Math.max(1, Math.min(5000, deadline - Date.now())));
          const after = await fs.readFile(logfile, 'utf8');
          const attempt = { ...capture, stage, bridge_captured: capture.captured,
            at: new Date().toISOString(),
            stage_valid_during_capture: activeMovieStage(after) === stage,
            before_log_sha256: createHash('sha256').update(before).digest('hex'),
            after_log_sha256: createHash('sha256').update(after).digest('hex'),
            before_last_line: before.trim().split('\n').at(-1),
            after_last_line: after.trim().split('\n').at(-1) };
          try { attempt.artifact = await movieCaptureIdentity(capture, work); }
          catch (error) { attempt.artifact_error = String(error); }
          attempt.captured = capture.captured === true && attempt.stage_valid_during_capture && Boolean(attempt.artifact);
          (receipt.movie_capture_attempts ??= []).push(attempt);
          if (attempt.captured) { receipt.movie_captures ??= {}; receipt.movie_captures[stage] = attempt; }
        }
      } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    if (mode === 'game') {
      try {
        let gameLog = await fs.readFile(logfile, 'utf8');
        const action = inputScenario?.observe(gameLog);
        if (action?.kind === 'pulse') {
          // Recheck the latest published UI state immediately before sending.
          gameLog = await fs.readFile(logfile, 'utf8');
          const step = inputScenario.beginPulse(action.button, gameLog);
          const observation = {};
          let pressedAt;
          try {
            observation.set_result = await bridge.request('input.set', { buttons: [action.button] }, 1500);
            pressedAt = performance.now();
            await new Promise(resolve => setTimeout(resolve, 100));
          } catch (error) { observation.set_error = String(error); }
          finally {
            observation.held_ms = pressedAt === undefined ? 0 : performance.now() - pressedAt;
            try { observation.clear_result = await bridge.request('input.clear', {}, 1500); }
            catch (error) { observation.clear_error = String(error); }
            observation.cleared_at_ms = Date.now();
            inputScenario.endPulse(step, observation);
          }
        } else if (action?.kind === 'capture-menu') {
          const { attempt, after } = await captureGameFrame(bridge, logfile, work,
            `${runName}-menu-${launchRequestedAt}.png`, Math.max(1, Math.min(5000, deadline - Date.now())));
          (receipt.game_capture_attempts ??= []).push({ purpose: 'menu', ...attempt });
          inputScenario.recordMenuCapture(attempt, Date.now(), after);
        } else if (['capture-backlog-open', 'capture-backlog-scroll', 'capture-backlog-return'].includes(action?.kind)) {
          const { attempt, after } = await captureGameFrame(bridge, logfile, work,
            `${runName}-${action.kind}-${launchRequestedAt}.png`, Math.max(1, Math.min(5000, deadline - Date.now())));
          (receipt.game_capture_attempts ??= []).push({ purpose: action.kind, ...attempt });
          inputScenario.recordBacklogCapture(action.kind, attempt, Date.now(), after);
          gameLog = after;
        } else if (['capture-saved', 'capture-restored'].includes(action?.kind)) {
          const { attempt, after } = await captureGameFrame(bridge, logfile, work,
            `${runName}-${action.kind}-${launchRequestedAt}.png`, Math.max(1, Math.min(5000, deadline - Date.now())));
          const bytes = await readWitchSaveSnapshot(savePath);
          const retained = path.join(work, `${runName}-${action.kind}-${launchRequestedAt}-savedata000.bin`);
          await fs.writeFile(retained, bytes, { flag: 'wx' });
          attempt.file_artifact = { path: retained, bytes: bytes.length,
            sha256: createHash('sha256').update(bytes).digest('hex') };
          (receipt.game_capture_attempts ??= []).push({ purpose: action.kind, ...attempt });
          inputScenario.recordCapture(action.kind, attempt, bytes, Date.now(), after);
          gameLog = after;
        }
        if (inputScenario?.receipt.error) throw new Error(inputScenario.receipt.error);
        // These are threshold observations: the before/after STATE records
        // show the actual frame, rather than claiming an exact frame capture.
        const frame = parseGameLog(gameLog).latest_state?.value.frame;
        const pending = [120, 240, 720, 1200, 1800, 2400, 3000, 3600, 4200]
          .filter(target => frame >= target && !receipt.game_captures?.[target]?.captured);
        if (pending.length) {
          const target = pending.at(-1);
          const { attempt } = await captureGameFrame(bridge, logfile, work,
            `${runName}-after-frame-${target}-${launchRequestedAt}.png`, Math.max(1, Math.min(5000, deadline - Date.now())));
          receipt.game_captures ??= {};
          receipt.game_captures[target] = { requested_frame_threshold: target, ...attempt };
          if (attempt.captured && !receipt.screen_capture?.captured) receipt.screen_capture = attempt;
        }
      } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
    if (mode === 'text') {
      for (const stage of ['EN', 'ZC']) {
        if (receipt.probe_log?.includes(`\nSTAGE ${stage} FRAME READY\n`)
            && !receipt.probe_log.includes(`\nSTAGE ${stage} HOLD COMPLETE\n`)
            && !receipt.text_captures?.[stage]?.captured) {
          receipt.text_captures ??= {};
          receipt.text_captures[stage] = await bridge.request('screen.capture', {
            relativePath: `${runName}-${stage.toLowerCase()}-frame-${launchRequestedAt}.png`,
          }, 5000);
        }
      }
    }
    if (mode === 'pipeline' && receipt.probe_log?.includes('\nRT SCANOUT READY;')
        && !receipt.scanout_capture?.captured) {
      receipt.scanout_capture = await bridge.request('screen.capture', {
        relativePath: `${runName}-direct-rt-frame-${launchRequestedAt}.png`,
      }, 5000);
    }
    if (mode === 'pipeline' && receipt.probe_log?.includes('\nSWAPPED DISPLAY READY;')
        && !receipt.probe_log.includes('\nRT SCANOUT READY;')
        && !receipt.crossover_capture?.captured) {
      receipt.crossover_capture = await bridge.request('screen.capture', {
        relativePath: `${runName}-swapped-display-frame-${launchRequestedAt}.png`,
      }, 5000);
    }
    // Vita3K's file layer can buffer a live log until close. Observe the
    // displayed frame independently, while the owned application is running.
    // A configured render target can make frameReady true before a one-frame
    // fixture has submitted its scene. Effects closes/reopens its live log at
    // this marker, so wait for that submission before accepting a screenshot.
    const fixturePresented = mode === 'game' ? receipt.probe_log?.includes('\nFRAME 1 submitted\n')
      : !['effects', 'pipeline'].includes(mode) || receipt.probe_log?.includes('\nFRAME READY;');
    if (graphical && !['text', 'movie', 'game', 'compressed', 'ui-color'].includes(mode) && fixturePresented && !receipt.screen_capture?.captured) {
      const state = await bridge.request('session.status');
      if (state.frameReady && state.resolution.width > 0 && state.resolution.height > 0) {
        receipt.screen_capture = await bridge.request('screen.capture', {
          relativePath: `${runName}-frame-${launchRequestedAt}.png`,
        }, 5000);
        (receipt.capture_attempts ??= []).push({ at: new Date().toISOString(),
          state, captured: receipt.screen_capture.captured });
      }
    }
    try {
      const logStat = await fs.stat(logfile);
      receipt.first_log_observation ??= { at: new Date().toISOString(),
        size: logStat.size, birthtime_ms: logStat.birthtimeMs, mtime_ms: logStat.mtimeMs };
      const result = await fs.readFile(logfile, 'utf8');
      receipt.last_observed_stage = result.trim().split('\n').at(-1);
      receipt.probe_log = result;
      receipt.probe_log_sha256 = createHash('sha256').update(result).digest('hex');
      if (/^RESULT (PASS|FAIL)$/m.test(result)) {
        const results = [...result.matchAll(/^RESULT (PASS|FAIL)$/gm)];
        receipt.runtime_verified = results.length === 1 && results[0][1] === 'PASS'
          && result.endsWith('RESULT PASS\n');
        if (mode === 'game' && receipt.runtime_verified) {
          const markers = ['ASSETS verified Witch/EN+Chengkong', 'ARCHIVES opened',
            'GXM renderer ready', 'FONTS ready', 'GAME ready', 'UI booted; input ready', 'FRAME 1 submitted'];
          receipt.game_boot_markers = markers.map(text => ({ text, observed: result.includes(`\n${text}\n`) }));
          receipt.runtime_verified = receipt.game_boot_markers.every(marker => marker.observed)
            && /\nFINITE RUN COMPLETE frames=[1-9][0-9]*\n/.test(result);
          if (!receipt.runtime_verified) receipt.validation_error = 'Game PASS is missing actual boot, frame, or bounded-run markers';
          if (gameInputs) {
            inputScenario.observe(result);
            receipt.game_input_validation = await validateGameInputs(result);
            receipt.runtime_verified &&= receipt.game_input_validation.valid;
            if (!receipt.game_input_validation.valid) receipt.validation_error = receipt.game_input_validation.issues.join('; ');
          }
        }
        if (mode === 'movie' && receipt.runtime_verified) {
          receipt.movie_validation = movieResultEvidence(result, receipt.movie_captures);
          receipt.runtime_verified = movieStaging?.verified === true && receipt.movie_validation.valid;
          if (!receipt.runtime_verified) receipt.validation_error = receipt.movie_validation.issues.join('; ');
        }
        if (mode === 'compressed' && receipt.runtime_verified) {
          receipt.compressed_validation = compressedResultEvidence(result, receipt.compressed_captures);
          receipt.runtime_verified = compressedStaging?.verified === true && receipt.compressed_validation.valid;
          if (!receipt.runtime_verified) receipt.validation_error = receipt.compressed_validation.issues.join('; ');
        }
        if (mode === 'ui-color' && receipt.runtime_verified) {
          receipt.ui_color_validation = uiColorResultEvidence(result, receipt.ui_color_captures);
          receipt.runtime_verified = uiColorStaging?.verified === true && receipt.ui_color_validation.valid;
          if (!receipt.runtime_verified) receipt.validation_error = receipt.ui_color_validation.issues.join('; ');
        }
        if (mode === 'effects' && receipt.runtime_verified) {
          receipt.group_validation = groupResultEvidence(result, receipt.group_captures);
          receipt.runtime_verified = receipt.group_validation.valid;
          if (!receipt.runtime_verified) receipt.validation_error = receipt.group_validation.issues.join('; ');
        }
        if (results.length !== 1 || (results[0][1] === 'PASS' && !receipt.runtime_verified)) {
          receipt.validation_error ??= 'Ambiguous result or unexpected text after RESULT PASS';
        }
        console.log(result);
        break;
      }
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
    await new Promise(resolve => setTimeout(resolve, 200));
  }
  receipt.state = await bridge.request('session.status', {}, 2000);
  if (!receipt.runtime_verified) {
    const failure = receipt.probe_log?.replace(/^RESULT FAIL\n/gm, '').trim();
    receipt.result_complete = /^RESULT (PASS|FAIL)$/m.test(receipt.probe_log ?? '');
    throw new Error(receipt.validation_error ?? (receipt.result_complete ? failure :
      `No complete ${mode} result within the runtime window; last stage: ${receipt.last_observed_stage ?? 'no log'}`));
  }
} catch (error) {
  receipt.error = String(error);
  process.exitCode = 1;
  console.error(error);
} finally {
  receipt.probe_verified = receipt.runtime_verified;
  // RESULT PASS is a guest log observation, not proof that its native and
  // emulated threads have stopped. Never issue shutdown during guest teardown.
  receipt.natural_stop = await waitForNaturalStop(timeout => bridge.request('session.status', {}, timeout),
    { timeoutMs: mode === 'game' ? 120000 : 15000 });
  if (receipt.natural_stop.stopped) {
    receipt.shutdown_requested_after_phase = 'stopped';
    receipt.shutdown_requested_at = new Date().toISOString();
    try { receipt.shutdown_result = await bridge.request('emulator.shutdown', {}, 2000); }
    catch (error) { receipt.shutdown_request_error = String(error); }
  }
  let actualExit = await Promise.race([exited, new Promise(resolve => setTimeout(resolve, hostDebugger?.exit_wait_ms ?? 5000, null))]);
  if (!actualExit) {
    receipt.forced_process_termination = true;
    child.kill();
    actualExit = await Promise.race([exited, new Promise(resolve => setTimeout(resolve, 5000, null))]);
  }
  receipt.exit = actualExit ?? { code: child.exitCode, signal: child.signalCode, observation_timed_out: true };
  if (hostDebugger) {
    try {
      const eventsPath = path.join(hostDebugger.output_directory, 'events.jsonl');
      const identity = await stableFileIdentity(eventsPath);
      if (identity.bytes > 8 * 1024 * 1024) throw new Error('Owned debugger event log exceeds 8 MiB');
      const events = (await fs.readFile(eventsPath, 'utf8')).trim().split('\n').map(line => JSON.parse(line));
      const owned = events.filter(event => event.kind === 'owned_process');
      const exits = events.filter(event => event.kind === 'exit');
      if (owned.length !== 1 || exits.length !== 1 || exits[0].code !== receipt.exit.code)
        throw new Error('Owned debugger did not preserve one complete child identity and actual exit');
      const dumps = [];
      for (const event of events.filter(event => event.dump_written)) {
        if (path.resolve(path.dirname(event.dump)) !== path.resolve(hostDebugger.output_directory))
          throw new Error('Owned debugger dump escapes its output directory');
        dumps.push({ path: event.dump, ...await stableFileIdentity(event.dump) });
      }
      Object.assign(receipt.host_debugger, { complete: true, owned_process: owned[0], child_exit: exits[0],
        events: { path: eventsPath, ...identity }, dumps,
        exceptions: events.filter(event => event.kind === 'exception'),
        failures: events.filter(event => ['launch_failed', 'debug_wait_failed', 'continue_failed', 'owned_process_timeout'].includes(event.kind)) });
    } catch (error) {
      receipt.host_debugger.complete = false;
      receipt.host_debugger.error = String(error);
      receipt.runtime_verified = false;
      process.exitCode = 1;
    }
  }
  receipt.exit_validation = gameExitEvidence(receipt.natural_stop, receipt.exit, receipt.forced_process_termination === true);
  receipt.emulator_shutdown_verified = receipt.exit_validation.valid;
  if (!receipt.emulator_shutdown_verified) {
    receipt.runtime_verified = false;
    receipt.shutdown_error = receipt.exit_validation.issues.join('; ');
    process.exitCode = 1;
  }
  bridge.close();
  // Preserve incomplete observations too: a stalled teardown must keep its
  // failure and last native stage, rather than inherit an earlier passing log.
  for (const [filename, key, output] of [[logfile, 'probe_log', `${runName}-runtime.log`],
    [nativeLogfile, 'native_log', `${runName}-native-runtime.log`]]) {
    if (!filename) continue;
    try {
      const result = await fs.readFile(filename, 'utf8');
      receipt[key] = result;
      receipt[`${key}_sha256`] = createHash('sha256').update(result).digest('hex');
      await fs.writeFile(path.join(evidence, output), result);
    } catch (error) { if (error.code !== 'ENOENT') receipt[`${key}_read_error`] = String(error); }
  }
  if (gameInputs) {
    try { receipt.game_input_validation = await validateGameInputs(receipt.probe_log ?? ''); }
    catch (error) { receipt.game_input_validation = { valid: false, issues: [String(error)] }; }
    if (!receipt.game_input_validation.valid || (gameSaveInputs && !receipt.save_path_was_absent_before_launch)) {
      receipt.runtime_verified = false;
      receipt.game_input_validation_error = receipt.game_input_validation.issues.join('; ');
      process.exitCode = 1;
    }
  }
  if (mode === 'movie') {
    receipt.movie_validation = movieResultEvidence(receipt.probe_log ?? '', receipt.movie_captures);
    const terminated = (receipt.native_log ?? '').split('\n').filter(line => line === 'destroy: terminated').length;
    receipt.native_teardown = { terminated_count: terminated, verified: terminated === 1 };
    if (!receipt.movie_validation.valid || !receipt.native_teardown.verified || !movieStaging?.verified) {
      receipt.runtime_verified = false;
      receipt.movie_validation_error = [...receipt.movie_validation.issues,
        ...(!receipt.native_teardown.verified ? ['Missing single complete native GXM teardown'] : []),
        ...(!movieStaging?.verified ? ['Original three-MP4 inputs were not verified'] : [])].join('; ');
      process.exitCode = 1;
    }
  }
  if (mode === 'compressed') {
    receipt.compressed_validation = compressedResultEvidence(receipt.probe_log ?? '', receipt.compressed_captures);
    const terminated = (receipt.native_log ?? '').split('\n').filter(line => line === 'destroy: terminated').length;
    receipt.native_teardown = { terminated_count: terminated, verified: terminated === 1 };
    if (!receipt.compressed_validation.valid || !receipt.native_teardown.verified || !compressedStaging?.verified) {
      receipt.runtime_verified = false;
      receipt.compressed_validation_error = [...receipt.compressed_validation.issues,
        ...(!receipt.native_teardown.verified ? ['Missing single complete native GXM teardown'] : [])].join('; ');
      process.exitCode = 1;
    }
  }
  if (mode === 'effects') {
    receipt.group_validation = groupResultEvidence(receipt.probe_log ?? '', receipt.group_captures);
    const terminated = (receipt.native_log ?? '').split('\n').filter(line => line === 'destroy: terminated').length;
    receipt.native_teardown = { terminated_count: terminated, verified: terminated === 1 };
    if (!receipt.group_validation.valid || !receipt.native_teardown.verified) {
      receipt.runtime_verified = false;
      receipt.group_validation_error = [...receipt.group_validation.issues,
        ...(!receipt.native_teardown.verified ? ['Missing single complete native GXM teardown'] : [])].join('; ');
      process.exitCode = 1;
    }
  }
  if (mode === 'ui-color') {
    receipt.ui_color_validation = uiColorResultEvidence(receipt.probe_log ?? '', receipt.ui_color_captures);
    const terminated = (receipt.native_log ?? '').split('\n').filter(line => line === 'destroy: terminated').length;
    receipt.native_teardown = { terminated_count: terminated, verified: terminated === 1 };
    if (!receipt.ui_color_validation.valid || !receipt.native_teardown.verified || !uiColorStaging?.verified) {
      receipt.runtime_verified = false;
      receipt.ui_color_validation_error = [...receipt.ui_color_validation.issues,
        ...(!receipt.native_teardown.verified ? ['Missing single complete native GXM teardown'] : []),
        ...(!uiColorStaging?.verified ? ['Synthetic UI color fixtures were not verified'] : [])].join('; ');
      process.exitCode = 1;
    }
  }
  if (mode === 'gxm' && receipt.probe_log?.includes('READBACK FILE WRITTEN\n')) {
    // A later DEV run reuses the guest path. Keep the actual byte observation
    // beside this launch's screenshot before another launch can replace it.
    const source = path.join(pref, 'ux0/data/knife-woh-dev/gxm-probe.rgba');
    try {
      const bytes = await fs.readFile(source);
      if (bytes.length !== 960 * 544 * 4) throw new Error(`Unexpected readback length ${bytes.length}`);
      const retained = path.join(work, `${runName}-readback-${receipt.first_log_observation?.birthtime_ms ?? Date.now()}.rgba`);
      await fs.writeFile(retained, bytes, { flag: 'wx' });
      receipt.cpu_readback = { path: retained, bytes: bytes.length,
        sha256: createHash('sha256').update(bytes).digest('hex') };
    } catch (error) { receipt.cpu_readback_retention_error = String(error); }
  }
  const emulatorLog = Buffer.concat(chunks);
  if (graphical) {
    const log = emulatorLog.toString('utf8');
    const observed = [];
    if (/\[renderer::vulkan::VKState::create\]: Vulkan device:/.test(log)) observed.push('Vulkan');
    if (/\[GameWindow::create_gl_context\]: Created OpenGL /.test(log)) observed.push('OpenGL');
    receipt.renderer_observation = { backends: observed,
      matches_request: observed.length === 1 && observed[0] === renderer };
    if (!receipt.renderer_observation.matches_request) {
      receipt.runtime_verified = false;
      receipt.renderer_validation_error = `Expected ${renderer}; observed ${observed.join(', ') || 'no backend identity'}`;
      process.exitCode = 1;
    }
  }
  await fs.writeFile(path.join(evidence, `vita3k-${runName}.log`), emulatorLog);
  await fs.writeFile(path.join(root, `doc/vita-${runName}-runtime.json`), JSON.stringify(receipt, null, 2) + '\n');
}


