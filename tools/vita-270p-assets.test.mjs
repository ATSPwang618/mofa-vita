import test from 'node:test';
import assert from 'node:assert/strict';
import { promises as fs } from 'node:fs';
import { createHash } from 'node:crypto';
import os from 'node:os';
import path from 'node:path';
import { validate270pAssets } from './vita-270p-assets.mjs';

async function fixture(t) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'knife-270p-test-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const source = path.join(root, 'assets'), canonical = path.join(root, 'canonical');
  await fs.mkdir(source); await fs.mkdir(canonical);
  const archives = [], files = [];
  const write = async (name, data) => {
    const bytes = Buffer.from(data);
    await fs.writeFile(path.join(source, name), bytes);
    files.push({ path: name, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') });
  };
  for (const name of ['allpachdml', 'allpacml', 'jallpachd', 'jallpac', 'allui', 'parts']) {
    for (const suffix of ['.mrg', '.hed']) {
      await write(name + suffix, `synthetic270-${name}${suffix}`);
      await fs.writeFile(path.join(canonical, name + suffix), `synthetic540-${name}${suffix}`);
    }
    const verification = { all_entries_reopened: true, decoded_pixels_verified: true,
      logical_coordinates_preserved: true, codecs_preserved: true, labels_and_indices_preserved: true,
      strips_resized_separately: true };
    const audit = { schema: 'knife-270p-archive-v1', name, verification, statistics: { entries: 1, textures: 1 },
      records: [{ index: 0, textures: [{ logical: [1920, 1080], before: [960, 540], physical: [480, 270],
        strips: 1, codec: 2 }] }] };
    await write(name + '.270p.json', JSON.stringify(audit));
    archives.push({ name, statistics: audit.statistics, verification });
  }
  const manifest = { schema: 'knife-270p-assets-v1', method: 'half-physical-preserve-logical-codec-strips-v1',
    source_assets: canonical, output_assets: source, render: [480, 270], display: [960, 540],
    languages: ['en', 'zc'], codecs: [1, 2, 7], archives, files, statistics: { entries: 6, textures: 6 },
    verification: { all_pass: true, archives: 6, all_textures_resized: true, all_entries_reopened: true,
      logical_coordinates_preserved: true, decoded_pixels_verified: true, codecs_preserved: true,
      strips_resized_separately: true, hd_fallback: false } };
  const save = () => fs.writeFile(path.join(source, 'vita-270p-assets.json'), JSON.stringify(manifest));
  const editAudit = async (name, change) => {
    const filename = name + '.270p.json';
    const audit = JSON.parse(await fs.readFile(path.join(source, filename), 'utf8'));
    change(audit);
    const bytes = Buffer.from(JSON.stringify(audit));
    await fs.writeFile(path.join(source, filename), bytes);
    Object.assign(files.find(f => f.path === filename), { bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex') });
    Object.assign(archives.find(a => a.name === name), { statistics: audit.statistics });
    await save();
  };
  await save();
  return { source, canonical, expectedRoot: source, manifest, save, editAudit };
}

test('accepts exact 270p files and complete per-texture physical audit', async t => {
  const f = await fixture(t);
  assert.equal((await validate270pAssets(f)).schema, 'knife-270p-assets-v1');
});
test('rejects a canonical hard link even when the complete output hash matches', async t => {
  const f = await fixture(t);
  const canonical = path.join(f.canonical, 'parts.mrg');
  await fs.unlink(canonical);
  await fs.link(path.join(f.source, 'parts.mrg'), canonical);
  await assert.rejects(validate270pAssets(f), /canonical 540p file/i);
});
test('rejects a changed archive even if its name and size still match', async t => {
  const f = await fixture(t);
  const file = path.join(f.source, 'parts.mrg');
  const data = await fs.readFile(file); data[0] ^= 1; await fs.writeFile(file, data);
  await assert.rejects(validate270pAssets(f), /hash|changed/i);
});
test('rejects 540p render metadata and an extra unaudited HD archive', async t => {
  const f = await fixture(t);
  f.manifest.render = [960, 540]; await f.save();
  await assert.rejects(validate270pAssets(f), /270p/i);
  f.manifest.render = [480, 270]; await f.save();
  await fs.writeFile(path.join(f.source, 'HD.mrg'), 'untracked');
  await assert.rejects(validate270pAssets(f), /inventory|unaudited/i);
});
test('rejects traversal and a different destination', async t => {
  const f = await fixture(t);
  f.manifest.files[0].path = '../escape.mrg'; await f.save();
  await assert.rejects(validate270pAssets(f), /path/i);
  await assert.rejects(validate270pAssets({ ...f, expectedRoot: f.canonical }), /directory|root/i);
});

async function nativeUI(t) {
  const f = await fixture(t);
  Object.assign(f.manifest, { schema: 'knife-270p-assets-v2', method: '270p-scenes-540p-ui-preserve-logical-v1',
    composite: [960, 540], ui_policy: { archive: 'allui', method: 'ui-selective-native-half-v1',
      max_physical: [960, 540], retain_sufficient_small_ui: true, runtime_hd_fallback: false } });
  f.manifest.verification.selective_native_ui_verified = true;
  await f.editAudit('allui', a => {
    a.schema = 'knife-270p-archive-v2'; a.method = 'ui-selective-native-half-v1';
    Object.assign(a.verification, { selective_native_ui_verified: true, unchanged_small_ui_verified: true });
    Object.assign(a.records[0].textures[0], { previous_physical: [480, 270], physical: [960, 540],
      restored: true, previous_kimg_sha256: '0'.repeat(64) });
    Object.assign(a.statistics, { ui_restored: 1, byte_identical_small_ui: 0 });
  });
  return f;
}

test('native UI v2 raises only audited UI density while scene pixels stay 270p', async t => {
  const f = await nativeUI(t);
  assert.deepEqual((await validate270pAssets(f)).composite, [960, 540]);
  await f.editAudit('parts', a => { a.records[0].textures[0].physical = [960, 540]; });
  await assert.rejects(validate270pAssets(f), /texture|pixel|unscaled/i);
});

test('rejects excessive UI density and a falsified previous 270p extent', async t => {
  const f = await nativeUI(t);
  await f.editAudit('allui', a => { a.records[0].textures[0].physical = [1920, 1080]; });
  await assert.rejects(validate270pAssets(f), /texture|pixel|unscaled/i);
  await f.editAudit('allui', a => {
    Object.assign(a.records[0].textures[0], { physical: [960, 540], previous_physical: [960, 540] });
  });
  await assert.rejects(validate270pAssets(f), /texture|pixel|unscaled/i);
});

test('accepts optional BOM/whitespace performance flag without changing core pack identity', async t => {
  const f = await fixture(t);
  const initial = await validate270pAssets(f);
  await fs.writeFile(path.join(f.source, 'perf-enable.txt'), '\uFEFF1\r\n');
  const enabled = await validate270pAssets(f);
  assert.equal(enabled.sha256, initial.sha256);
  assert.equal(enabled.perf_flag.enabled, true);
  await fs.writeFile(path.join(f.source, 'perf-enable.txt'), '0');
  assert.equal((await validate270pAssets(f)).perf_flag.enabled, false);
});

test('rejects invalid/oversized performance flags and unrelated extras', async t => {
  const f = await fixture(t);
  for (const content of ['enable', '1'.repeat(513), Buffer.from([0xff, 0x31])]) {
    await fs.writeFile(path.join(f.source, 'perf-enable.txt'), content);
    await assert.rejects(validate270pAssets(f), /performance|flag/i);
  }
});
