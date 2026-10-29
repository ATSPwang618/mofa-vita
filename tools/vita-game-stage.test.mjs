import test from 'node:test';
import assert from 'node:assert/strict';
import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { validateGameAssetStage } from './vita-game-stage.mjs';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const identity = bytes => ({ bytes: bytes.length, sha256: hash(bytes) });
const sourceHash = 'ac47dc8aff15d0bf957279e557e68b733c81c97e7f9421639433c9fa044b683b';

async function fixture(t, combined = true) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'knife-stage-test-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const canonical = path.join(root, 'canonical');
  const stagedBase = path.join(root, 'staged');
  const parent = path.join(stagedBase, 'menu', 'assets');
  const output = path.join(stagedBase, 'cg', 'assets');
  for (const dir of [canonical, parent, output]) await fs.mkdir(dir, { recursive: true });
  const files = [];
  for (const name of ['allui.hed', 'allui.nam', 'allui.mrg']) {
    const bytes = Buffer.from(`synthetic-private-menu-${name}`);
    await fs.writeFile(path.join(canonical, name), `synthetic-canonical-menu-${name}`);
    await fs.writeFile(path.join(parent, name), bytes);
    await fs.link(path.join(parent, name), path.join(output, name));
    files.push({ relative_path: name, mode: 'private-rebuilt', output: identity(bytes) });
  }
  const menu = { schema: 'vita-gxt-menu-stage-v1', source_assets: canonical, output_assets: parent,
    selection: { archive: 'allui', index: 11, texture: 'menu_btn_en' },
    replacement: { sha256: '1'.repeat(64), metadata: { codec: 8 } },
    verification: { all_pass: true }, files };
  const menuBytes = Buffer.from(JSON.stringify(menu));
  await fs.writeFile(path.join(parent, 'vita-gxt-stage-manifest.json'), menuBytes);
  await fs.link(path.join(parent, 'vita-gxt-stage-manifest.json'), path.join(output, 'vita-gxt-stage-manifest.json'));
  const cgFiles = [];
  for (const name of ['jallpac.hed', 'jallpac.nam', 'jallpac.mrg']) {
    const before = Buffer.from(`synthetic-original-cg-${name}`);
    const after = Buffer.from(`synthetic-private-cg-${name}`);
    await fs.writeFile(path.join(parent, name), before);
    await fs.writeFile(path.join(output, name), after);
    cgFiles.push({ relative_path: name, mode: 'private-rebuilt', same_file: false,
      source: identity(before), output: identity(after) });
  }
  const cg = { schema: 'vita-gxt-cg-stage-v1', source_assets: parent, output_assets: output,
    parent_manifest: { relative_path: 'vita-gxt-stage-manifest.json', ...identity(menuBytes), schema: menu.schema },
    selection: { archive: 'jallpac', index: 1884, name: 'IMG1227.JPG', source_kimg_sha256: sourceHash,
      output_kimg_sha256: '2'.repeat(64),
      source_metadata: { logical: [1400, 840], physical: [900, 540], codec: 1 },
      output_metadata: { logical: [1400, 840], physical: [900, 540], codec: 8,
        gxt: { format: '0x85000000', backing: [1024, 1024], payload_bytes: 524288 } } },
    files: cgFiles,
    verification: { all_pass: true, archive_entries: 22156, changed_archive_entries: 1,
      unchanged_entries_stored_and_decoded_exact: 22155, reopened_all_entries: true,
      exact_nam_and_hed_terminal: true, private_archive_files: 3,
      stale_jallpac_compact_audit_copied: false, parent_manifest_exact: true,
      parent_allui_output_hashes_match: true, source_inventory_and_file_identities_unchanged: true } };
  const cgPath = path.join(output, 'vita-gxt-cg-stage-manifest.json');
  const save = () => fs.writeFile(cgPath, JSON.stringify(cg));
  await save();
  return { canonical, stagedBase, source: combined ? output : parent,
    parent, output, cg, menu, save, validate() { return validateGameAssetStage(this); } };
}

test('accepts the verified menu-only stage and checks actual private archive hashes', async t => {
  const f = await fixture(t, false);
  const result = await f.validate();
  assert.equal(result.schema, 'vita-gxt-menu-stage-v1');
  assert.equal(result.private_archive_hashes_checked, true);
});

test('accepts a combined stage with an exact parent and independent CG archive files', async t => {
  const f = await fixture(t);
  const result = await f.validate();
  assert.equal(result.schema, 'vita-gxt-cg-stage-v1');
  assert.equal(result.parent_manifest.sha256, f.cg.parent_manifest.sha256);
  assert.equal(result.selection.index, 1884);
  assert.equal(result.private_archive_hashes_checked, true);
});

test('rejects an override resolving outside the experiment root or to canonical assets', async t => {
  const f = await fixture(t);
  await assert.rejects(validateGameAssetStage({ ...f, source: f.canonical }), /private staged directory/);
  await assert.rejects(validateGameAssetStage({ ...f, source: f.stagedBase }), /private staged directory/);
});

test('rejects wrong original locator/hash, DXT5 or changed logical dimensions', async t => {
  const f = await fixture(t);
  for (const [field, value] of [['index', 12631], ['name', 'IMG2328.JPG'],
    ['source_kimg_sha256', '3'.repeat(64)]]) {
    const old = f.cg.selection[field];
    f.cg.selection[field] = value; await f.save();
    await assert.rejects(f.validate(), /IMG1227/);
    f.cg.selection[field] = old;
  }
  f.cg.selection.output_metadata.gxt.format = '0x87000000'; await f.save();
  await assert.rejects(f.validate(), /IMG1227/);
  f.cg.selection.output_metadata.gxt.format = '0x85000000';
  f.cg.selection.output_metadata.logical = [900, 540]; await f.save();
  await assert.rejects(f.validate(), /IMG1227/);
});

test('rejects archive bytes changed after the manifest was written', async t => {
  const f = await fixture(t);
  await fs.appendFile(path.join(f.output, 'jallpac.mrg'), 'changed');
  await assert.rejects(f.validate(), /Changed.*jallpac.mrg/);
});

test('rejects a CG archive accidentally hard-linked to its source even with matching hashes', async t => {
  const f = await fixture(t);
  const name = 'jallpac.nam';
  await fs.unlink(path.join(f.output, name));
  await fs.link(path.join(f.parent, name), path.join(f.output, name));
  f.cg.files.find(item => item.relative_path === name).output = f.cg.files.find(item => item.relative_path === name).source;
  await f.save();
  await assert.rejects(f.validate(), /independent|private/);
});

test('rejects a changed parent or actual menu archive rather than trusting verification flags', async t => {
  const f = await fixture(t);
  await fs.appendFile(path.join(f.output, 'allui.mrg'), 'changed');
  await assert.rejects(f.validate(), /Changed.*allui.mrg/);
  const g = await fixture(t);
  await fs.appendFile(path.join(g.output, 'vita-gxt-stage-manifest.json'), ' ');
  await assert.rejects(g.validate(), /parent manifest/);
});

test('does not silently downgrade a present invalid CG manifest to the menu-only path', async t => {
  const f = await fixture(t);
  f.cg.verification.changed_archive_entries = 2; await f.save();
  await assert.rejects(f.validate(), /verification/);
});

async function backlogFixture(t) {
  const f = await fixture(t);
  const output = path.join(f.stagedBase, 'backlog', 'assets');
  await fs.mkdir(output, { recursive: true });
  const files = [];
  for (const name of await fs.readdir(f.output)) {
    const bytes = await fs.readFile(path.join(f.output, name));
    if (name.startsWith('allui.')) {
      const after = Buffer.from(`synthetic-private-backlog-${name}`);
      await fs.writeFile(path.join(output, name), after);
      files.push({ relative_path: name, mode: 'private-rebuilt', same_file: false,
        source: identity(bytes), output: identity(after) });
    } else {
      await fs.link(path.join(f.output, name), path.join(output, name));
    }
  }
  const cgBytes = await fs.readFile(path.join(f.output, 'vita-gxt-cg-stage-manifest.json'));
  const manifest = { schema: 'vita-gxt-backlog-stage-v1', source_assets: f.output, output_assets: output,
    parent_manifest: { relative_path: 'vita-gxt-cg-stage-manifest.json', schema: f.cg.schema, ...identity(cgBytes) },
    selections: [
      [6, 'DISP_PARTS_EN.NXZ', '1c4acabc39582579437f6577fd804eadbbf76842d73a41944a9c5871cc1cfa15'],
      [7, 'DISP_PARTS_ZC.NXZ', 'b244013533092537524c440e15ee351265d2ff2e88c4f14ce38384dd7b6845bf'],
    ].map(([index, name, atlas]) => ({ archive: 'allui', index, name, texture: 'bg_backlog',
      source_atlas_sha256: atlas,
      source_kimg_sha256: 'a34fa4f0918734efaa02ccff5fd214d8eb01d55b22d2c71475de2cadb257faa0',
      output_kimg_sha256: '4'.repeat(64), members: 50, other_member_records_exact: 49,
      source_metadata: { logical: [1920, 1080], physical: [960, 540], codec: 2 },
      output_metadata: { logical: [1920, 1080], physical: [960, 540], codec: 8,
        gxt: { format: '0x85000000', backing: [1024, 1024], payload_bytes: 524288 } } })),
    files, verification: { all_pass: true, archive_entries: 70, changed_archive_entries: 2,
      unchanged_entries_stored_and_decoded_exact: 68, reopened_all_entries: true,
      exact_nam_and_hed_terminal: true, private_archive_files: 3, parent_manifest_exact: true,
      parent_jallpac_output_hashes_match: true, source_inventory_and_file_identities_unchanged: true } };
  const save = () => fs.writeFile(path.join(output, 'vita-gxt-backlog-stage-manifest.json'), JSON.stringify(manifest));
  await save();
  return { ...f, source: output, backlogParent: f.output, backlog: manifest, saveBacklog: save };
}

test('accepts the two exact backlog atlas replacements over an independently validated CG parent', async t => {
  const f = await backlogFixture(t);
  const result = await f.validate();
  assert.equal(result.schema, 'vita-gxt-backlog-stage-v1');
  assert.equal(result.selections.length, 2);
  assert.equal(result.parent_manifest.schema, 'vita-gxt-cg-stage-v1');
  assert.equal(result.private_archive_hashes_checked, true);
});

test('rejects wrong backlog member, source identity, alpha format and coordinates', async t => {
  for (const mutate of [m => m.selections[0].index++, m => m.selections[1].source_kimg_sha256 = '0'.repeat(64),
    m => m.selections[0].output_metadata.gxt.format = '0x87000000',
    m => m.selections[0].output_metadata.logical = [960, 540], m => m.selections.pop()]) {
    const f = await backlogFixture(t);
    mutate(f.backlog); await f.saveBacklog();
    await assert.rejects(f.validate(), /backlog/);
  }
});

test('rejects changed backlog archive bytes or an unverified parent', async t => {
  const f = await backlogFixture(t);
  await fs.appendFile(path.join(f.source, 'allui.mrg'), 'changed');
  await assert.rejects(f.validate(), /Changed.*allui.mrg/);
  const g = await backlogFixture(t);
  await fs.appendFile(path.join(g.backlogParent, 'jallpac.mrg'), 'changed');
  await assert.rejects(g.validate(), /Changed.*jallpac.mrg/);
});

test('rejects parent cycles and present invalid backlog manifests without fallback', async t => {
  const f = await backlogFixture(t);
  f.backlog.source_assets = f.source; await f.saveBacklog();
  await assert.rejects(f.validate(), /backlog/);
  const g = await backlogFixture(t);
  g.backlog.verification.changed_archive_entries = 1; await g.saveBacklog();
  await assert.rejects(g.validate(), /backlog/);
});

test('rejects a backlog archive linked to its parent despite matching manifest hashes', async t => {
  const f = await backlogFixture(t);
  const name = 'allui.nam';
  await fs.unlink(path.join(f.source, name));
  await fs.link(path.join(f.backlogParent, name), path.join(f.source, name));
  f.backlog.files.find(x => x.relative_path === name).output = f.backlog.files.find(x => x.relative_path === name).source;
  await f.saveBacklog();
  await assert.rejects(f.validate(), /independent|private/);
});
