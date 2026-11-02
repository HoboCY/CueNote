import { mkdirSync, copyFileSync, cpSync, existsSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { root, readJson, target, run, sha256 } from './common.mjs';

export function seal(out, publicRelease = false) {
  const receipt = readJson(path.join(out, 'receipt.json'));
  const lifecycle = readJson(path.join(out, 'lifecycle.json'));
  if (receipt.sourceDirty || lifecycle.checkpoints.length !== 8 || lifecycle.checkpoints.some(c => !c.passed)) {
    throw new Error('Refuse delivery without a clean source commit and completed lifecycle checks');
  }
  if (publicRelease && (process.env.GITHUB_REF_TYPE !== 'tag' ||
      !/^v\d+\.\d+\.\d+$/.test(process.env.GITHUB_REF_NAME ?? '') ||
      process.env.GITHUB_REF_NAME !== `v${receipt.version}` || process.env.GITHUB_SHA !== receipt.sourceCommit)) {
    throw new Error('Release must match the version tag and source commit');
  }
  if (path.basename(receipt.installer.name) !== receipt.installer.name ||
      receipt.target !== path.basename(out) || lifecycle.target !== receipt.target) {
    throw new Error('Delivery target or installer name differs from the build receipt');
  }
  if (sha256(path.join(out, receipt.installer.name)) !== receipt.installer.sha256) throw new Error('Installer changed after validation');
  const staging = publicRelease ? path.join(path.dirname(out), 'release') : path.join(out, 'delivery');
  if (existsSync(staging)) throw new Error('Use a fresh output directory for delivery');
  mkdirSync(staging);
  const files = [receipt.installer.name, 'receipt.json', 'lifecycle.json'];
  for (const file of files) {
    const name = publicRelease && file.endsWith('.json') ? file.replace('.json', `-${receipt.target}.json`) : file;
    copyFileSync(path.join(out, file), path.join(staging, name));
  }
  if (publicRelease) {
    const names = [receipt.installer.name, `receipt-${receipt.target}.json`, `lifecycle-${receipt.target}.json`];
    writeFileSync(path.join(staging, `SHA256SUMS-${receipt.target}.txt`),
      names.map(name => `${sha256(path.join(staging, name))}  ${name}\n`).join(''));
    return;
  }
  cpSync(path.join(root, 'package-resources'), path.join(staging, 'licenses'), { recursive: true });
  copyFileSync(path.join(root, 'docs/personal-packages.md'), path.join(staging, 'README.md'));
  const archive = path.join(out, `CueNote_${receipt.version}_${receipt.target}.tar.gz`);
  run('tar', ['-czf', archive, '-C', staging, '.']);
  const encrypted = path.join(root, 'package-output/encrypted', `${path.basename(archive)}.age`);
  run(process.execPath, ['tools/packaging/encrypt.mjs', archive, encrypted]);
}

if (import.meta.main) seal(path.join(root, 'package-output', target()), process.argv.includes('--release'));
