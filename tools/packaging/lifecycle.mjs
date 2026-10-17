import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync, existsSync, readdirSync, rmSync } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { root, target, readJson, run, capture, sha256, writeJson } from './common.mjs';

if (process.env.GITHUB_ACTIONS !== 'true' || process.env.RUNNER_ENVIRONMENT !== 'github-hosted') {
  throw new Error('Lifecycle tests require a disposable GitHub-hosted runner');
}
const out = path.join(root, 'package-output', target());
const receipt = readJson(path.join(out, 'receipt.json'));
if (!receipt.fixtureInstaller) throw new Error('Build --upgrade-fixture before lifecycle tests');
const win = process.platform === 'win32';
const config = win ? path.join(process.env.APPDATA, receipt.identifier) : path.join(os.homedir(), 'Library/Application Support', receipt.identifier);
const local = win ? path.join(process.env.LOCALAPPDATA, receipt.identifier) : config;
const recovery = path.join(local, 'recovery');
const backup = path.join(local, 'backups');
const documents = win ? capture('pwsh', ['-NoProfile', '-Command', '[Environment]::GetFolderPath("MyDocuments")']) : path.join(os.homedir(), 'Documents');
const library = path.join(documents, 'CueNote');
run('cargo', ['run', '--locked', '--manifest-path', 'src-tauri/Cargo.toml', '--example', 'package_lifecycle_seed', '--', config, recovery, library]);
const sentinel = path.join(config, 'installer-retention-sentinel.txt');
writeFileSync(sentinel, 'CueNote installer must preserve this user-owned file\n');
function files(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const file = path.join(directory, entry.name);
    return entry.isDirectory() ? files(file) : entry.isFile() ? [file] : [];
  });
}
const protectedFiles = [sentinel, ...[library, recovery, backup].flatMap(files)];
const before = new Map(protectedFiles.map(f => [f, sha256(f)]));
const checkpoints = [];
function check(stage) {
  for (const [file, hash] of before) {
    if (!existsSync(file) || sha256(file) !== hash) throw new Error(`User data changed at ${stage}: ${file}`);
  }
  checkpoints.push({ stage, preservedFiles: before.size, passed: true });
  console.log(`Lifecycle ${stage}: ${before.size} protected file hashes unchanged`);
}
const installed = win ? path.join(process.env.LOCALAPPDATA, 'Programs/CueNote') : path.join(os.homedir(), 'Applications/CueNote.app');
if (existsSync(installed)) throw new Error('Refuse pre-existing application installation');
const oldInstaller = path.join(out, receipt.fixtureInstaller);
const installer = path.join(out, receipt.installer.name);
function windows(action, file, version) {
  const args = ['-NoProfile', '-File', 'tools/packaging/lifecycle-windows.ps1', '-Action', action, '-Path', file];
  if (version) args.push('-ExpectedVersion', version);
  run('pwsh', args);
}
function removeMacApp() {
  // This exact path is derived from the disposable runner HOME, never from package data.
  const allowed = path.join(os.homedir(), 'Applications/CueNote.app');
  if (path.resolve(installed) !== path.resolve(allowed)) throw new Error('Unexpected app removal path');
  rmSync(installed, { recursive: true });
}
function install(dmg, version) {
  if (win) windows('install', dmg, version);
  else {
    const mount = path.join(process.env.RUNNER_TEMP, `cuenote-dmg-${version}`);
    mkdirSync(mount, { recursive: true });
    run('hdiutil', ['verify', dmg]);
    run('hdiutil', ['attach', '-readonly', '-nobrowse', '-mountpoint', mount, dmg]);
    try {
      if (existsSync(installed)) removeMacApp();
      mkdirSync(path.dirname(installed), { recursive: true });
      run('ditto', [path.join(mount, 'CueNote.app'), installed]);
    } finally { run('hdiutil', ['detach', mount]); }
    run('codesign', ['--verify', '--deep', '--strict', installed]);
    if (capture('/usr/libexec/PlistBuddy', ['-c', 'Print :CFBundleShortVersionString', path.join(installed, 'Contents/Info.plist')]) !== version) throw new Error('Installed macOS version differs');
  }
  if (version === receipt.version) {
    const exe = path.join(installed, win ? 'cuenote.exe' : 'Contents/MacOS/cuenote');
    if (sha256(exe) !== receipt.binarySha256) throw new Error('Installed binary differs from release receipt');
    const licenses = path.join(installed, win ? 'licenses' : 'Contents/Resources/licenses');
    const inventory = readJson(path.join(licenses, 'components.json'));
    if (inventory.target !== target()) throw new Error('Installed license inventory has wrong target');
    for (const component of inventory.components) for (const text of component.texts) {
      if (sha256(path.join(licenses, text.file)) !== text.sha256) throw new Error(`Installed license text differs: ${text.file}`);
    }
  }
}
async function start() {
  if (win) return windows('start', path.join(installed, 'cuenote.exe'));
  const child = spawn(path.join(installed, 'Contents/MacOS/cuenote'), [], { stdio: 'inherit' });
  let startError;
  child.on('error', e => { startError = e; });
  await new Promise(resolve => setTimeout(resolve, 10000));
  if (startError || child.exitCode !== null || child.signalCode !== null) throw new Error(`macOS app exited during smoke startup: ${startError ?? child.exitCode}`);
  const exited = new Promise(resolve => child.once('exit', resolve));
  child.kill('SIGTERM');
  await exited;
}
function uninstall() {
  if (win) windows('uninstall', path.join(installed, 'uninstall.exe'));
  else removeMacApp();
}
install(oldInstaller, '0.0.0'); check('first-install-0.0.0');
await start(); check('start-0.0.0');
install(installer, receipt.version); check(`upgrade-0.0.0-to-${receipt.version}`);
await start(); check('start-upgraded-version');
uninstall(); check('uninstall');
install(installer, receipt.version); check('reinstall-current-version');
await start(); check('start-reinstalled-version');
uninstall(); check('final-uninstall');
writeJson(path.join(out, 'lifecycle.json'), { format: 1, target: target(), checkpoints,
  note: 'Disposable runner only. Native notes, history and manual backup hashes preserved. Does not replace user-device/OneDrive acceptance.',
  protectedFiles: [...before].map(([file, hash]) => ({ path: file.replace(os.homedir(), '<runner-home>'), sha256: hash })) });
console.log(`Lifecycle checks passed: ${checkpoints.length} stages, ${before.size} protected files`);
