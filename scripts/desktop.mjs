// Disposable prototype launcher. No persistent environment or system settings.
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { existsSync } from 'node:fs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const built = process.argv.includes('--built');
const executable = path.join(root, 'src-tauri', 'target', 'debug', process.platform === 'win32' ? 'cuenote-prototype.exe' : 'cuenote-prototype');
if (built && !existsSync(executable)) {
  console.error('Build the local debug sample first: npm run build:desktop -- --debug');
  process.exit(1);
}
const scratch = path.join(root, '.scratch', 'PROTOTYPE-wipe-me');
console.log(`CueNote prototype scratch: ${scratch}`);
const child = spawn(built ? executable : process.execPath, built ? [] : [path.join(root, 'prototype/node_modules/@tauri-apps/cli/tauri.js'), 'dev'], {
  cwd: root,
  env: { ...process.env, CUENOTE_PROTOTYPE_SCRATCH_DIR: scratch },
  stdio: 'inherit',
  windowsHide: true,
});
child.on('error', error => { console.error(error.message); process.exitCode = 1; });
child.on('exit', code => { process.exitCode = code ?? 1; });
