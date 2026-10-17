import { readFileSync, writeFileSync, copyFileSync, mkdirSync, readdirSync, existsSync, cpSync } from 'node:fs';
import path from 'node:path';
import { root, verifyInputs, tauri, capture, run, readJson, sha256, writeJson } from './common.mjs';

const input = verifyInputs();
const fixture = process.argv.includes('--upgrade-fixture');
const targetDir = path.resolve(process.env.CARGO_TARGET_DIR ?? 'src-tauri/target/packages');
process.env.CARGO_TARGET_DIR = targetDir;
const output = path.join(root, 'package-output', input.target);
mkdirSync(output, { recursive: true });
const files = ['package.json', 'package-lock.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock', 'src-tauri/tauri.conf.json'];
const originals = new Map(files.map(f => [f, readFileSync(path.join(root, f))]));
const bundles = process.platform === 'win32' ? 'nsis' : 'app,dmg';
if (!existsSync(path.join(root, 'package-resources/components.json'))) throw new Error('Run npm run package:licenses first');
function build(version, destination) {
  // CueNote has no updater. Keep the packaged executable byte-identical to the
  // receipt's binary instead of Tauri's temporary per-installer updater marker.
  tauri('build', '--no-binary-patching', '--target', input.target, '--bundles', bundles, '--config', 'src-tauri/tauri.package.conf.json', '--', '--locked');
  const bundleRoot = path.join(targetDir, input.target, 'release/bundle');
  const dir = process.platform === 'win32' ? 'nsis' : 'dmg';
  const extension = process.platform === 'win32' ? '.exe' : '.dmg';
  const installers = readdirSync(path.join(bundleRoot, dir)).filter(f => f.includes(`_${version}_`) && f.endsWith(extension));
  if (installers.length !== 1) throw new Error(`Expected one ${version} installer, got ${installers}`);
  mkdirSync(destination, { recursive: true });
  const installer = path.join(destination, installers[0]);
  copyFileSync(path.join(bundleRoot, dir, installers[0]), installer);
  if (process.platform === 'darwin') cpSync(path.join(bundleRoot, 'macos/CueNote.app'), path.join(destination, 'CueNote.app'), { recursive: true, preserveTimestamps: true, verbatimSymlinks: true });
  return installer;
}
let fixtureInstaller;
try {
  if (fixture) {
    // Synthetic older metadata, same source: tests installer version-upgrade behavior,
    // not migration from a historical released format. Never included in delivery.
    const older = '0.0.0';
    if (input.version === older) throw new Error('Release version must be newer than upgrade fixture');
    for (const file of ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json']) {
      const data = readJson(path.join(root, file));
      data.version = older;
      if (file === 'package-lock.json') data.packages[''].version = older;
      writeJson(path.join(root, file), data);
    }
    writeFileSync(path.join(root, 'src-tauri/Cargo.toml'), originals.get('src-tauri/Cargo.toml').toString().replace(/^version = "[^"]+"/m, `version = "${older}"`));
    const cargoLock = originals.get('src-tauri/Cargo.lock').toString();
    const changed = cargoLock.replace(/(name = "cuenote"\r?\nversion = ")[^"]+("\r?\n)/, (_, before, after) => `${before}${older}${after}`);
    if (changed === cargoLock) throw new Error('Cannot locate root package in Cargo.lock');
    writeFileSync(path.join(root, 'src-tauri/Cargo.lock'), changed);
    fixtureInstaller = build(older, path.join(output, 'upgrade-fixture'));
  }
} finally {
  for (const [file, bytes] of originals) writeFileSync(path.join(root, file), bytes);
}
const installer = build(input.version, output);
const binary = process.platform === 'win32'
  ? path.join(targetDir, input.target, 'release/cuenote.exe')
  : path.join(output, 'CueNote.app/Contents/MacOS/cuenote');
let architecture;
if (process.platform === 'win32') {
  const pe = readFileSync(binary);
  if (pe.readUInt32LE(pe.readUInt32LE(0x3c)) !== 0x4550) throw new Error('Missing PE signature');
  const machine = pe.readUInt16LE(pe.readUInt32LE(0x3c) + 4);
  if (machine !== 0x8664) throw new Error(`Wrong PE machine ${machine}`);
  architecture = 'x86_64';
} else {
  architecture = capture('lipo', ['-archs', binary]);
  if (architecture !== 'arm64') throw new Error(`Wrong Mach-O architecture ${architecture}`);
  run('codesign', ['--verify', '--deep', '--strict', path.join(output, 'CueNote.app')]);
  const plist = path.join(output, 'CueNote.app/Contents/Info.plist');
  for (const [key, expected] of [['CFBundleIdentifier', 'com.hobocy.cuenote'], ['CFBundleShortVersionString', input.version], ['LSMinimumSystemVersion', '14.0']]) {
    if (capture('/usr/libexec/PlistBuddy', ['-c', `Print :${key}`, plist]) !== expected) throw new Error(`Wrong ${key}`);
  }
}
writeJson(path.join(output, 'receipt.json'), { format: 1, ...input, architecture,
  sourceCommit: capture('git', ['rev-parse', 'HEAD']), sourceDirty: Boolean(capture('git', ['status', '--porcelain', '--untracked-files=no'])),
  identifier: 'com.hobocy.cuenote', features: ['tauri/custom-protocol'],
  installer: { name: path.basename(installer), sha256: sha256(installer) }, binarySha256: sha256(binary),
  fixtureInstaller: fixtureInstaller && path.relative(output, fixtureInstaller).replaceAll('\\', '/'),
  fixturePurpose: fixture ? 'Synthetic 0.0.0 -> current metadata upgrade of same source; not historical data migration' : undefined,
  lockfiles: Object.fromEntries(['package-lock.json', 'src-tauri/Cargo.lock', 'rust-toolchain.toml', '.node-version'].map(f => [f, sha256(path.join(root, f))])),
  tools: { rustc: capture('rustc', ['--version', '--verbose']), cargo: capture('cargo', ['--version']), node: process.version,
    tauri: readJson(path.join(root, 'node_modules/@tauri-apps/cli/package.json')).version,
    xcode: process.platform === 'darwin' ? capture('xcodebuild', ['-version']) : undefined },
  runner: { os: process.platform, image: process.env.ImageOS, imageVersion: process.env.ImageVersion,
    runId: process.env.GITHUB_RUN_ID, attempt: process.env.GITHUB_RUN_ATTEMPT },
  signing: process.platform === 'darwin' ? 'ad-hoc; not notarized' : 'unsigned personal test',
});
console.log(`Built ${input.target}: ${path.basename(installer)}`);
