import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync, existsSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test, { type TestContext } from 'node:test';
import { seal } from '../tools/packaging/seal.mjs';

function fixture(t: TestContext) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cuenote-delivery-'));
  const target = 'aarch64-apple-darwin';
  const out = path.join(directory, target);
  mkdirSync(out);
  const installer = 'CueNote_0.1.1_aarch64.dmg';
  writeFileSync(path.join(out, installer), 'tested installer');
  mkdirSync(path.join(out, 'upgrade-fixture'));
  writeFileSync(path.join(out, 'upgrade-fixture/old.dmg'), 'must not be published');
  const receipt = { target, version: '0.1.1', sourceDirty: false, sourceCommit: 'a'.repeat(40),
    installer: { name: installer, sha256: createHash('sha256').update('tested installer').digest('hex') } };
  const lifecycle = { target, checkpoints: Array.from({ length: 8 }, () => ({ passed: true })) };
  const save = () => {
    writeFileSync(path.join(out, 'receipt.json'), JSON.stringify(receipt));
    writeFileSync(path.join(out, 'lifecycle.json'), JSON.stringify(lifecycle));
  };
  save();
  const previous = { GITHUB_REF_TYPE: process.env.GITHUB_REF_TYPE, GITHUB_REF_NAME: process.env.GITHUB_REF_NAME, GITHUB_SHA: process.env.GITHUB_SHA };
  Object.assign(process.env, { GITHUB_REF_TYPE: 'tag', GITHUB_REF_NAME: 'v0.1.1', GITHUB_SHA: receipt.sourceCommit });
  t.after(() => {
    for (const [key, value] of Object.entries(previous)) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    assert.equal(path.dirname(directory), path.resolve(os.tmpdir()));
    rmSync(directory, { recursive: true });
  });
  return { out, staging: path.join(directory, 'release'), receipt, lifecycle, save };
}

test('public delivery contains only the tested installer, receipts and valid checksums', t => {
  const { out, staging, receipt } = fixture(t);
  seal(out, true);
  const checksumName = `SHA256SUMS-${receipt.target}.txt`;
  assert.deepEqual(readdirSync(staging).sort(), [receipt.installer.name, `receipt-${receipt.target}.json`,
    `lifecycle-${receipt.target}.json`, checksumName].sort());
  for (const line of readFileSync(path.join(staging, checksumName), 'utf8').trim().split('\n')) {
    const [hash, name] = line.split('  ');
    assert.equal(createHash('sha256').update(readFileSync(path.join(staging, name))).digest('hex'), hash);
  }
  assert.throws(() => seal(out, true), /fresh output directory/);
});

for (const scenario of ['branch', 'wrong version', 'wrong commit', 'dirty source', 'failed lifecycle', 'missing checkpoint', 'changed installer', 'wrong target', 'unsafe filename']) {
  test(`public delivery refuses ${scenario} before staging assets`, t => {
    const { out, staging, receipt, lifecycle, save } = fixture(t);
    if (scenario === 'branch') process.env.GITHUB_REF_TYPE = 'branch';
    if (scenario === 'wrong version') process.env.GITHUB_REF_NAME = 'v0.1.2';
    if (scenario === 'wrong commit') process.env.GITHUB_SHA = 'b'.repeat(40);
    if (scenario === 'dirty source') receipt.sourceDirty = true;
    if (scenario === 'failed lifecycle') lifecycle.checkpoints[3].passed = false;
    if (scenario === 'missing checkpoint') lifecycle.checkpoints.pop();
    if (scenario === 'changed installer') writeFileSync(path.join(out, receipt.installer.name), 'changed');
    if (scenario === 'wrong target') lifecycle.target = 'x86_64-pc-windows-msvc';
    if (scenario === 'unsafe filename') receipt.installer.name = '../old.dmg';
    save();
    assert.throws(() => seal(out, true), /Release must match|Refuse delivery|Installer changed|Delivery target/);
    assert.equal(existsSync(staging), false);
  });
}
