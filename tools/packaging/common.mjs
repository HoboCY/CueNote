import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

export const root = fileURLToPath(new URL('../../', import.meta.url));
export const readJson = (p) => JSON.parse(readFileSync(p, 'utf8').replace(/^\uFEFF/, ''));
export const sha256 = (p) => createHash('sha256').update(readFileSync(p)).digest('hex');
export function writeJson(p, value) {
  mkdirSync(path.dirname(p), { recursive: true });
  writeFileSync(p, `${JSON.stringify(value, null, 2)}\n`);
}
export function run(command, args, options = {}) {
  return execFileSync(command, args, { cwd: root, stdio: 'inherit', ...options });
}
export const capture = (command, args, options = {}) =>
  run(command, args, { stdio: ['ignore', 'pipe', 'inherit'], encoding: 'utf8', ...options }).trim();
export const tauri = (...args) => run(process.execPath, ['node_modules/@tauri-apps/cli/tauri.js', ...args]);
export function target() {
  if (process.platform === 'win32' && process.arch === 'x64') return 'x86_64-pc-windows-msvc';
  if (process.platform === 'darwin' && process.arch === 'arm64') return 'aarch64-apple-darwin';
  throw new Error(`Unsupported packaging host: ${process.platform}/${process.arch}`);
}
export function verifyInputs() {
  const nodeVersion = readFileSync(path.join(root, '.node-version'), 'utf8').trim();
  if (process.versions.node !== nodeVersion) throw new Error(`Use Node ${nodeVersion}`);
  const rust = readFileSync(path.join(root, 'rust-toolchain.toml'), 'utf8').match(/channel = "([^"]+)"/)[1];
  if (!capture('rustc', ['--version']).startsWith(`rustc ${rust} `)) throw new Error(`Use Rust ${rust}`);
  const version = readJson(path.join(root, 'package.json')).version;
  const lock = readJson(path.join(root, 'package-lock.json'));
  const config = readJson(path.join(root, 'src-tauri/tauri.conf.json'));
  const cargo = readFileSync(path.join(root, 'src-tauri/Cargo.toml'), 'utf8');
  if (config.version !== version || lock.version !== version || lock.packages[''].version !== version ||
      cargo.match(/^version = "([^"]+)"/m)[1] !== version) throw new Error('Package versions disagree');
  if (config.identifier !== 'com.hobocy.cuenote' || config.productName !== 'CueNote') throw new Error('Application identity changed');
  for (const key of ['TAURI_CONFIG', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CUENOTE_PERF_ROOT']) {
    if (process.env[key]) throw new Error(`Unset ${key} before packaging`);
  }
  return { version, target: target(), rust, node: nodeVersion };
}
