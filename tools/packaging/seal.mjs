import { mkdirSync, copyFileSync, cpSync, existsSync } from 'node:fs';
import path from 'node:path';
import { root, readJson, target, run, sha256 } from './common.mjs';

const out = path.join(root, 'package-output', target());
const receipt = readJson(path.join(out, 'receipt.json'));
const lifecycle = readJson(path.join(out, 'lifecycle.json'));
if (receipt.sourceDirty || lifecycle.checkpoints.length !== 8 || lifecycle.checkpoints.some(c => !c.passed)) {
  throw new Error('Refuse delivery without a clean source commit and completed lifecycle checks');
}
const staging = path.join(out, 'delivery');
if (existsSync(staging)) throw new Error('Use a fresh output directory for delivery');
mkdirSync(staging);
if (sha256(path.join(out, receipt.installer.name)) !== receipt.installer.sha256) throw new Error('Installer changed after validation');
for (const file of [receipt.installer.name, 'receipt.json', 'lifecycle.json']) copyFileSync(path.join(out, file), path.join(staging, file));
cpSync(path.join(root, 'package-resources'), path.join(staging, 'licenses'), { recursive: true });
copyFileSync(path.join(root, 'docs/personal-packages.md'), path.join(staging, 'README.md'));
const archive = path.join(out, `CueNote_${receipt.version}_${receipt.target}.tar.gz`);
run('tar', ['-czf', archive, '-C', staging, '.']);
const encrypted = path.join(root, 'package-output/encrypted', `${path.basename(archive)}.age`);
run(process.execPath, ['tools/packaging/encrypt.mjs', archive, encrypted]);
