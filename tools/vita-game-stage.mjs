// Runtime input validation for explicitly scoped, local Vita GXT experiments.
// Archive equivalence remains the staging tool's claim; actual file hashes bind that
// claim to this run. This module never changes a source or stages an archive itself.
import { createHash } from 'node:crypto';
import { createReadStream, promises as fs } from 'node:fs';
import path from 'node:path';

export { fileIdentity, readManifest };

const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const digest = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
const samePath = (a, b) => process.platform === 'win32' ? a.toLowerCase() === b.toLowerCase() : a === b;
const sameFile = (a, b) => a.dev === b.dev && a.ino === b.ino;
const unchanged = (a, b) => sameFile(a, b) && a.size === b.size && a.mtimeNs === b.mtimeNs;
const exact = (value, expected) => Array.isArray(value) && value.length === expected.length
  && value.every((item, index) => item === expected[index]);

async function regular(filename) {
  const stat = await fs.lstat(filename, { bigint: true });
  if (!stat.isFile() || stat.isSymbolicLink()) throw new Error(`Expected regular staged file: ${filename}`);
  return stat;
}

async function readManifest(filename, limit) {
  const before = await regular(filename);
  if (before.size < 2n || before.size > BigInt(limit)) throw new Error(`Oversized or empty game stage manifest: ${filename}`);
  const bytes = await fs.readFile(filename);
  if (BigInt(bytes.length) !== before.size || !unchanged(before, await regular(filename))) {
    throw new Error(`Game stage manifest changed while reading: ${filename}`);
  }
  return { bytes, manifest: JSON.parse(bytes), stat: before };
}

async function fileIdentity(filename, expected) {
  if (!expected || !Number.isSafeInteger(expected.bytes) || expected.bytes < 1
      || expected.bytes > 4 * 1024 ** 3 || !digest(expected.sha256)) {
    throw new Error(`Invalid staged archive identity: ${filename}`);
  }
  const before = await regular(filename);
  if (before.size !== BigInt(expected.bytes)) throw new Error(`Changed staged archive file: ${filename}`);
  const hash = createHash('sha256');
  let bytes = 0;
  for await (const block of createReadStream(filename, { highWaterMark: 1024 * 1024 })) {
    bytes += block.length;
    if (bytes > expected.bytes) throw new Error(`Changed staged archive file: ${filename}`);
    hash.update(block);
  }
  if (bytes !== expected.bytes || hash.digest('hex') !== expected.sha256
      || !unchanged(before, await regular(filename))) throw new Error(`Changed staged archive file: ${filename}`);
  return before;
}

function oneRecord(manifest, name) {
  const records = manifest.files?.filter(file => file.relative_path === name);
  if (records?.length !== 1) throw new Error(`Missing or duplicate private archive identity: ${name}`);
  return records[0];
}

function isWithin(base, child) {
  const relative = path.relative(base, child);
  return relative !== '' && relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative);
}

async function validateMenu(manifest, output, actualAssets, canonical) {
  if (manifest.schema !== 'vita-gxt-menu-stage-v1' || manifest.verification?.all_pass !== true
      || !samePath(path.resolve(manifest.output_assets), output)
      || !samePath(await fs.realpath(manifest.source_assets), canonical)
      || manifest.selection?.archive !== 'allui' || manifest.selection?.index !== 11
      || manifest.selection?.texture !== 'menu_btn_en' || manifest.replacement?.metadata?.codec !== 8
      || !digest(manifest.replacement?.sha256)) {
    throw new Error('Staged game assets do not match the verified one-texture menu manifest');
  }
  for (const name of ['allui.hed', 'allui.nam', 'allui.mrg']) {
    const record = oneRecord(manifest, name);
    if (!['private-rebuilt', 'private-exact-copy'].includes(record.mode)) {
      throw new Error(`Missing private menu archive: ${name}`);
    }
    const actual = await fileIdentity(path.join(actualAssets, name), record.output);
    if (sameFile(actual, await regular(path.join(canonical, name)))) {
      throw new Error(`Menu archive is not independent from canonical source: ${name}`);
    }
    if (!samePath(output, actualAssets) && !sameFile(actual, await regular(path.join(output, name)))) {
      throw new Error(`Combined stage does not retain the exact parent menu archive: ${name}`);
    }
  }
}

export async function validateGameAssetStage(input) {
  return validateStage(input, true);
}

async function validateStage({ source, stagedBase, canonical }, allowBacklog) {
  const actualAssets = await fs.realpath(source);
  const base = await fs.realpath(stagedBase);
  const canonicalAssets = await fs.realpath(canonical);
  if (!isWithin(base, actualAssets) || samePath(actualAssets, canonicalAssets)) {
    throw new Error('Game asset override must resolve to a private staged directory');
  }
  const backlogPath = path.join(actualAssets, 'vita-gxt-backlog-stage-manifest.json');
  let backlog;
  try { backlog = await readManifest(backlogPath, 16 * 1024 ** 2); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  if (backlog) {
    if (!allowBacklog) throw new Error('Nested backlog stages are unsupported');
    return validateBacklog(backlogPath, backlog, actualAssets, base, canonicalAssets);
  }
  const menuPath = path.join(actualAssets, 'vita-gxt-stage-manifest.json');
  const menu = await readManifest(menuPath, 8 * 1024 ** 2);
  const cgPath = path.join(actualAssets, 'vita-gxt-cg-stage-manifest.json');
  let cg;
  try { cg = await readManifest(cgPath, 64 * 1024 ** 2); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  if (!cg) {
    await validateMenu(menu.manifest, actualAssets, actualAssets, canonicalAssets);
    return { path: menuPath, sha256: sha(menu.bytes), schema: menu.manifest.schema,
      replacement_sha256: menu.manifest.replacement.sha256, verification: menu.manifest.verification,
      private_archive_hashes_checked: true };
  }
  const manifest = cg.manifest;
  const selection = manifest.selection;
  const verify = manifest.verification;
  if (manifest.schema !== 'vita-gxt-cg-stage-v1' || !samePath(path.resolve(manifest.output_assets), actualAssets)
      || verify?.all_pass !== true || verify.archive_entries !== 22156 || verify.changed_archive_entries !== 1
      || verify.unchanged_entries_stored_and_decoded_exact !== 22155 || verify.reopened_all_entries !== true
      || verify.exact_nam_and_hed_terminal !== true || verify.private_archive_files !== 3
      || verify.stale_jallpac_compact_audit_copied !== false || verify.parent_manifest_exact !== true
      || verify.parent_allui_output_hashes_match !== true || verify.source_inventory_and_file_identities_unchanged !== true) {
    throw new Error('Combined CG stage lacks the required bounded archive verification');
  }
  if (selection?.archive !== 'jallpac' || selection.index !== 1884 || selection.name !== 'IMG1227.JPG'
      || selection.source_kimg_sha256 !== 'ac47dc8aff15d0bf957279e557e68b733c81c97e7f9421639433c9fa044b683b'
      || !digest(selection.output_kimg_sha256)
      || !exact(selection.source_metadata?.logical, [1400, 840]) || !exact(selection.source_metadata?.physical, [900, 540])
      || selection.source_metadata?.codec !== 1 || !exact(selection.output_metadata?.logical, [1400, 840])
      || !exact(selection.output_metadata?.physical, [900, 540]) || selection.output_metadata?.codec !== 8
      || selection.output_metadata?.gxt?.format !== '0x85000000'
      || !exact(selection.output_metadata?.gxt?.backing, [1024, 1024])
      || selection.output_metadata?.gxt?.payload_bytes !== 524288) {
    throw new Error('Combined stage is not the exact ordinary IMG1227 DXT1 experiment');
  }
  const parent = await fs.realpath(manifest.source_assets);
  if (!isWithin(base, parent) || samePath(parent, actualAssets) || samePath(parent, canonicalAssets)) {
    throw new Error('CG stage source must be the private parent menu stage');
  }
  const binding = manifest.parent_manifest;
  if (binding?.relative_path !== 'vita-gxt-stage-manifest.json' || binding.schema !== 'vita-gxt-menu-stage-v1'
      || binding.bytes !== menu.bytes.length || binding.sha256 !== sha(menu.bytes)
      || !sameFile(menu.stat, await regular(path.join(parent, binding.relative_path)))) {
    throw new Error('CG stage does not retain its exact parent manifest');
  }
  await validateMenu(menu.manifest, parent, actualAssets, canonicalAssets);
  for (const name of ['jallpac.hed', 'jallpac.nam', 'jallpac.mrg']) {
    const record = oneRecord(manifest, name);
    if (!['private-rebuilt', 'private-exact-copy'].includes(record.mode) || record.same_file !== false) {
      throw new Error(`Missing private CG archive identity: ${name}`);
    }
    const input = await fileIdentity(path.join(parent, name), record.source);
    const output = await fileIdentity(path.join(actualAssets, name), record.output);
    if (sameFile(input, output)) throw new Error(`CG archive is not independent from its source: ${name}`);
  }
  return { path: cgPath, sha256: sha(cg.bytes), schema: manifest.schema,
    replacement_sha256: selection.output_kimg_sha256, selection,
    parent_manifest: { path: menuPath, sha256: sha(menu.bytes), schema: menu.manifest.schema },
    verification: verify, private_archive_hashes_checked: true };
}

async function validateBacklog(filename, receipt, actual, base, canonical) {
  const m = receipt.manifest, v = m.verification;
  if (m.schema !== 'vita-gxt-backlog-stage-v1' || !samePath(path.resolve(m.output_assets), actual)
      || v?.all_pass !== true || v.archive_entries !== 70 || v.changed_archive_entries !== 2
      || v.unchanged_entries_stored_and_decoded_exact !== 68 || v.reopened_all_entries !== true
      || v.exact_nam_and_hed_terminal !== true || v.private_archive_files !== 3
      || v.parent_manifest_exact !== true || v.parent_jallpac_output_hashes_match !== true
      || v.source_inventory_and_file_identities_unchanged !== true) {
    throw new Error('Invalid bounded backlog stage verification');
  }
  const expected = [
    [6, 'DISP_PARTS_EN.NXZ', '1c4acabc39582579437f6577fd804eadbbf76842d73a41944a9c5871cc1cfa15'],
    [7, 'DISP_PARTS_ZC.NXZ', 'b244013533092537524c440e15ee351265d2ff2e88c4f14ce38384dd7b6845bf'],
  ];
  if (!Array.isArray(m.selections) || m.selections.length !== 2) throw new Error('Invalid backlog selection count');
  for (let i = 0; i < 2; ++i) {
    const s = m.selections[i], [index, name, atlas] = expected[i];
    if (s.archive !== 'allui' || s.index !== index || s.name !== name || s.texture !== 'bg_backlog'
        || s.source_atlas_sha256 !== atlas
        || s.source_kimg_sha256 !== 'a34fa4f0918734efaa02ccff5fd214d8eb01d55b22d2c71475de2cadb257faa0'
        || !digest(s.output_kimg_sha256) || s.output_kimg_sha256 !== m.selections[0].output_kimg_sha256
        || s.members !== 50 || s.other_member_records_exact !== 49
        || !exact(s.source_metadata?.logical, [1920, 1080]) || !exact(s.source_metadata?.physical, [960, 540])
        || s.source_metadata?.codec !== 2 || !exact(s.output_metadata?.logical, [1920, 1080])
        || !exact(s.output_metadata?.physical, [960, 540]) || s.output_metadata?.codec !== 8
        || s.output_metadata?.gxt?.format !== '0x85000000'
        || !exact(s.output_metadata?.gxt?.backing, [1024, 1024])
        || s.output_metadata?.gxt?.payload_bytes !== 524288) throw new Error('Invalid exact backlog DXT1 selection');
  }
  const parent = await fs.realpath(m.source_assets);
  if (!isWithin(base, parent) || samePath(parent, actual) || samePath(parent, canonical)) {
    throw new Error('Invalid private backlog parent');
  }
  // Validate the parent's own files, not the intentionally changed child allui.
  const proof = await validateStage({ source: parent, stagedBase: base, canonical }, false);
  const binding = m.parent_manifest;
  if (proof.schema !== 'vita-gxt-cg-stage-v1' || binding?.schema !== proof.schema
      || binding.relative_path !== 'vita-gxt-cg-stage-manifest.json' || binding.sha256 !== proof.sha256) {
    throw new Error('Invalid backlog CG parent binding');
  }
  for (const name of ['vita-gxt-cg-stage-manifest.json', 'vita-gxt-stage-manifest.json',
    'jallpac.hed', 'jallpac.nam', 'jallpac.mrg']) {
    const src = await regular(path.join(parent, name)), dst = await regular(path.join(actual, name));
    if (!sameFile(src, dst)) throw new Error(`Backlog stage does not retain exact parent file: ${name}`);
    if (name === binding.relative_path && Number(src.size) !== binding.bytes) {
      throw new Error('Invalid backlog parent manifest byte count');
    }
  }
  for (const name of ['allui.hed', 'allui.nam', 'allui.mrg']) {
    const record = oneRecord(m, name);
    if (!['private-rebuilt', 'private-exact-copy'].includes(record.mode) || record.same_file !== false) {
      throw new Error(`Missing private backlog archive: ${name}`);
    }
    const src = await fileIdentity(path.join(parent, name), record.source);
    const dst = await fileIdentity(path.join(actual, name), record.output);
    if (sameFile(src, dst) || sameFile(dst, await regular(path.join(canonical, name)))) {
      throw new Error(`Backlog archive is not independent: ${name}`);
    }
  }
  return { path: filename, sha256: sha(receipt.bytes), schema: m.schema, selections: m.selections,
    parent_manifest: { path: proof.path, sha256: proof.sha256, schema: proof.schema },
    verification: v, private_archive_hashes_checked: true };
}
