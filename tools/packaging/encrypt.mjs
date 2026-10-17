import { mkdirSync, writeFileSync, existsSync } from 'node:fs';
import path from 'node:path';
import { root, readJson, sha256, run, target } from './common.mjs';

target();
const input = path.resolve(process.argv[2] ?? '');
const output = path.resolve(process.argv[3] ?? '');
if (process.argv.length !== 4 || !existsSync(input) || existsSync(output) || !output.endsWith('.age')) {
  throw new Error('Usage: node tools/packaging/encrypt.mjs <archive> <new-output.age>');
}
const config = readJson(path.join(root, 'tools/packaging/age-tools.json'));
const spec = config[process.platform];
const toolsDir = path.resolve('src-tauri/target/package-tools');
mkdirSync(toolsDir, { recursive: true });
const archive = path.join(toolsDir, spec.archive);
if (!existsSync(archive)) {
  const response = await fetch(`https://github.com/FiloSottile/age/releases/download/v${config.version}/${spec.archive}`);
  if (!response.ok) throw new Error(`age download: ${response.status}`);
  writeFileSync(archive, Buffer.from(await response.arrayBuffer()));
}
if (sha256(archive) !== spec.sha256) throw new Error('age archive checksum mismatch');
// Windows bsdtar supports ZIP; macOS bsdtar supports tar.gz.
run('tar', ['-xf', archive, '-C', toolsDir]);
mkdirSync(path.dirname(output), { recursive: true });
run(path.join(toolsDir, 'age', process.platform === 'win32' ? 'age.exe' : 'age'),
  ['-R', path.join(root, 'tools/packaging/recipient.txt'), '-o', output, input]);
writeFileSync(`${output}.sha256`, `${sha256(output)}  ${path.basename(output)}\n`);
console.log(`Encrypted delivery: ${path.basename(output)}`);
