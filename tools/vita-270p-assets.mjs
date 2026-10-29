// Bind scene pixels and the optional selective native-density UI to one pack.
import { promises as fs } from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { fileIdentity, readManifest } from './vita-game-stage.mjs';

const same = (a, b) => process.platform === 'win32' ? a.toLowerCase() === b.toLowerCase() : a === b;
const exact = (a, b) => Array.isArray(a) && a.length === b.length && a.every((v, i) => v === b[i]);
const names = ['allpachdml', 'allpacml', 'jallpachd', 'jallpac', 'allui', 'parts'];
const requireFlags = (value, flags) => flags.every(flag => value?.[flag] === true);

async function inventory(root, relative = '') {
  const result = [];
  for (const entry of await fs.readdir(path.join(root, relative), { withFileTypes: true })) {
    const file = relative ? `${relative}/${entry.name}` : entry.name;
    if (entry.isDirectory()) result.push(...await inventory(root, file));
    else if (entry.isFile()) result.push(file);
    else throw new Error(`270p inventory contains a non-regular file: ${file}`);
  }
  return result;
}

export async function validate270pAssets({ source, canonical, expectedRoot }) {
  const root = await fs.realpath(source), expected = await fs.realpath(expectedRoot);
  const baseline = await fs.realpath(canonical);
  if (!same(root, expected) || same(root, baseline)) throw new Error('270p assets must use the exact private destination directory');
  const filename = path.join(root, 'vita-270p-assets.json');
  const receipt = await readManifest(filename, 8 * 1024 ** 2), m = receipt.manifest, v = m.verification;
  const nativeUI = m.schema === 'knife-270p-assets-v2';
  const validProfile = nativeUI
    ? m.method === '270p-scenes-540p-ui-preserve-logical-v1' && exact(m.composite, [960, 540])
      && m.ui_policy?.archive === 'allui' && m.ui_policy?.method === 'ui-selective-native-half-v1'
      && exact(m.ui_policy?.max_physical, [960, 540]) && m.ui_policy?.retain_sufficient_small_ui === true
      && m.ui_policy?.runtime_hd_fallback === false && v?.selective_native_ui_verified === true
    : m.schema === 'knife-270p-assets-v1' && m.method === 'half-physical-preserve-logical-codec-strips-v1';
  if (!validProfile
      || !same(path.resolve(m.output_assets), root) || !same(await fs.realpath(m.source_assets), baseline)
      || !exact(m.render, [480, 270]) || !exact(m.display, [960, 540])
      || !exact(m.languages, ['en', 'zc']) || !exact(m.codecs, [1, 2, 7])
      || !requireFlags(v, ['all_pass', 'all_textures_resized', 'all_entries_reopened',
        'logical_coordinates_preserved', 'decoded_pixels_verified', 'codecs_preserved', 'strips_resized_separately'])
      || v.archives !== 6 || v.hd_fallback !== false || !Array.isArray(m.files)
      || m.files.length < 18 || m.files.length > 30000 || !Array.isArray(m.archives)
      || !exact(m.archives.map(a => a.name), names)) throw new Error('Invalid complete 270p conversion metadata');
  const wanted = new Set(['vita-270p-assets.json']);
  for (const f of m.files) {
    if (typeof f.path !== 'string' || !f.path || f.path.includes('\\') || f.path.includes(':')
        || path.posix.normalize(f.path) !== f.path || f.path.startsWith('/')
        || f.path === '..' || f.path.startsWith('../') || f.path === 'perf-enable.txt' || wanted.has(f.path)) {
      throw new Error(`Invalid or duplicate 270p file path: ${f.path}`);
    }
    wanted.add(f.path);
  }
  const actual = await inventory(root);
  let perfFlag = null;
  if (actual.includes('perf-enable.txt')) {
    const flagPath = path.join(root, 'perf-enable.txt');
    if ((await fs.stat(flagPath)).size > 512) throw new Error('Performance flag exceeds 512 bytes');
    const bytes = await fs.readFile(flagPath);
    let value;
    try { value = new TextDecoder('utf-8', { fatal: true }).decode(bytes).trim(); }
    catch { throw new Error('Performance flag is not valid UTF-8'); }
    if (!['0', '1'].includes(value)) throw new Error('Performance flag must contain 0 or 1');
    wanted.add('perf-enable.txt');
    perfFlag = { path: flagPath, enabled: value === '1' };
  }
  if (actual.length !== wanted.size || actual.some(p => !wanted.has(p))) {
    throw new Error('270p inventory has missing or unaudited files');
  }
  let cursor = 0;
  await Promise.all(Array.from({ length: 4 }, async () => {
    for (;;) {
      const index = cursor++;
      if (index >= m.files.length) return;
      const file = m.files[index];
      await fileIdentity(path.join(root, file.path), file);
    }
  }));
  let textures = 0, entries = 0;
  for (const archive of m.archives) {
    for (const suffix of ['.mrg', '.hed', '.270p.json']) {
      if (!wanted.has(archive.name + suffix)) throw new Error('270p archive identity missing');
    }
    for (const suffix of ['.mrg', '.hed']) {
      // NTFS file IDs can exceed Number's exact integer range; keep identity
      // comparisons in bigint, just like fileIdentity/readManifest do.
      const a = await fs.stat(path.join(root, archive.name + suffix), { bigint: true });
      const b = await fs.stat(path.join(baseline, archive.name + suffix), { bigint: true });
      if (a.dev === b.dev && a.ino === b.ino) throw new Error('270p archive is still the canonical 540p file');
    }
    const { manifest: audit } = await readManifest(path.join(root, archive.name + '.270p.json'), 32 * 1024 ** 2);
    const ui = nativeUI && archive.name === 'allui';
    const validAudit = ui ? audit.schema === 'knife-270p-archive-v2' && audit.method === 'ui-selective-native-half-v1'
      && requireFlags(audit.verification, ['selective_native_ui_verified', 'unchanged_small_ui_verified'])
      : audit.schema === 'knife-270p-archive-v1';
    if (!validAudit || audit.name !== archive.name
        || !requireFlags(audit.verification, ['all_entries_reopened', 'decoded_pixels_verified',
          'logical_coordinates_preserved', 'codecs_preserved', 'labels_and_indices_preserved', 'strips_resized_separately'])
        || !Array.isArray(audit.records) || audit.records.length !== audit.statistics?.entries
        || audit.records.length !== archive.statistics?.entries) throw new Error('Invalid per-archive 270p pixel audit');
    let count = 0, restored = 0;
    for (let index = 0; index < audit.records.length; ++index) {
      const record = audit.records[index];
      if (record.index !== index || !Array.isArray(record.textures)) throw new Error('270p archive indices changed');
      for (const image of record.textures) {
        const { before, physical, logical, strips, codec } = image;
        if (![1, 2].includes(strips) || ![1, 2, 7].includes(codec)
            || !Array.isArray(before) || before.length !== 2 || !Array.isArray(physical) || physical.length !== 2
            || !Array.isArray(logical) || logical.length !== 2
            || ![...before, ...physical, ...logical].every(n => Number.isSafeInteger(n) && n > 0)
            || before[1] % strips !== 0) {
          throw new Error('270p texture audit contains unscaled pixels or invalid language strips');
        }
        const previous = [Math.ceil(before[0] / 2), Math.ceil(before[1] / strips / 2) * strips];
        const expected = ui ? [Math.max(previous[0], Math.min(before[0], Math.ceil(logical[0] / 2))),
          Math.max(previous[1] / strips, Math.min(before[1] / strips, Math.ceil(logical[1] / strips / 2))) * strips]
          : previous;
        if (!exact(physical, expected) || physical[0] > (ui ? 960 : 480) || physical[1] / strips > (ui ? 540 : 270)
            || (ui && (!exact(image.previous_physical, previous)
              || image.restored !== !exact(physical, previous)
              || !/^[0-9a-f]{64}$/.test(image.previous_kimg_sha256)))) {
          throw new Error('270p texture audit contains unscaled pixels or invalid native UI density');
        }
        if (ui && image.restored) ++restored;
        ++count;
      }
    }
    if (count !== audit.statistics.textures || count !== archive.statistics.textures) throw new Error('270p texture count mismatch');
    if (ui && (restored !== audit.statistics.ui_restored || restored !== archive.statistics.ui_restored
        || count - restored !== audit.statistics.byte_identical_small_ui)) throw new Error('Native UI recovery count mismatch');
    textures += count; entries += audit.records.length;
  }
  if (textures !== m.statistics?.textures || entries !== m.statistics?.entries) throw new Error('270p total count mismatch');
  return { path: filename, schema: m.schema, sha256: createHash('sha256').update(receipt.bytes).digest('hex'),
    render: m.render, composite: m.composite ?? m.render, display: m.display, perf_flag: perfFlag,
    textures_verified: textures, entries_verified: entries,
    complete_file_hashes_checked: true, private_archive_hashes_checked: true, verification: v };
}
