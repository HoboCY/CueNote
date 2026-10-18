// Build a bounded, precompiled native acceptance kit for an ARM64 Mac.
// This is a CI/build-host tool; the delivered runner needs neither Rust nor Node.
import {
  chmodSync,
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  writeFileSync,
} from 'node:fs';
import path from 'node:path';
import {
  root,
  verifyInputs,
  run,
  capture,
  sha256,
  writeJson,
} from '../../packaging/common.mjs';

const input = verifyInputs();
if (input.target !== 'aarch64-apple-darwin') {
  throw new Error('Build this kit on an ARM64 Mac');
}
const sourceCommit = capture('git', ['rev-parse', 'HEAD']);
const sourceDirty = Boolean(capture('git', ['status', '--porcelain', '--untracked-files=no']));
if (sourceDirty) throw new Error('Do not deliver a kit from dirty source');

const suites = [
  'storage_interruptions',
  'backup_failures',
  'backup_restore_failures',
  'library_restore_failures',
  'save_failures',
  'backups',
  'backup_restore',
  'library_restore',
  'storage',
];
const probes = [
  'storage_fault_probe',
  'backup_fault_probe',
  'backup_restore_fault_probe',
  'library_restore_fault_probe',
];
const out = path.join(root, 'package-output/macos-native-acceptance');
const kit = path.join(out, 'CueNote-native-acceptance');
const archive = path.join(out, 'CueNote-macos-native-acceptance.tar.gz');
const encrypted = path.join(
  root,
  'package-output/native-acceptance-encrypted/CueNote-macos-native-acceptance.tar.gz.age',
);
if (existsSync(out)) throw new Error('Use a fresh build output for a new kit');
mkdirSync(path.join(kit, 'debug/deps'), { recursive: true });
mkdirSync(path.join(kit, 'debug/examples'), { recursive: true });
process.env.CARGO_TARGET_DIR = path.join(root, 'src-tauri/target/native-acceptance-macos');
const targetDir = process.env.CARGO_TARGET_DIR;

function seal(source, destination) {
  copyFileSync(source, destination);
  chmodSync(destination, 0o755);
  run('codesign', ['--force', '--sign', '-', destination]);
  if (capture('lipo', ['-archs', destination]) !== 'arm64') {
    throw new Error(`Not an ARM64 binary: ${destination}`);
  }
  run('codesign', ['--verify', '--strict', destination]);
}

const compiled = capture(
  'cargo',
  [
    'test',
    '--locked',
    '--no-run',
    '--manifest-path',
    'src-tauri/Cargo.toml',
    '--target',
    input.target,
    '--features',
    'storage-fault-injection',
    ...suites.flatMap((name) => ['--test', name]),
    '--message-format=json',
  ],
  { maxBuffer: 64 * 1024 * 1024 },
);
const testArtifacts = compiled
  .split(/\r?\n/u)
  .filter(Boolean)
  .map((line) => JSON.parse(line))
  .filter(
    (row) =>
      row.reason === 'compiler-artifact' &&
      row.profile.test &&
      row.profile.debug_assertions === true &&
      row.executable &&
      suites.includes(row.target.name),
  );
if (
  testArtifacts.length !== suites.length ||
  new Set(testArtifacts.map((row) => row.target.name)).size !== suites.length
) {
  throw new Error('Expected exactly the nine native acceptance test binaries');
}

const suiteTestCounts = {};
for (const artifact of testArtifacts) {
  const name = artifact.target.name;
  const destination = path.join(kit, 'debug/deps', name);
  seal(artifact.executable, destination);
  const listed = capture(destination, ['--list']);
  const match = listed.match(/^(\d+) tests?, (\d+) benchmarks?$/mu);
  if (!match) throw new Error(`Cannot read test inventory for ${name}`);
  const count = Number(match[1]);
  if (!Number.isInteger(count) || count < 1) {
    throw new Error(`${name} contains no tests; refusing a release or empty harness`);
  }
  suiteTestCounts[name] = count;
}

run(
  'cargo',
  [
    'build',
    '--locked',
    '--manifest-path',
    'src-tauri/Cargo.toml',
    '--target',
    input.target,
    '--features',
    'storage-fault-injection',
    ...probes.flatMap((name) => ['--example', name]),
  ],
);
const probeDir = path.join(targetDir, input.target, 'debug/examples');
for (const name of probes) {
  const source = path.join(probeDir, name);
  if (!existsSync(source)) throw new Error(`Missing fault probe: ${name}`);
  seal(source, path.join(kit, 'debug/examples', name));
}

mkdirSync(path.join(kit, 'licenses'), { recursive: true });
cpSync(path.join(root, 'package-resources'), path.join(kit, 'licenses'), {
  recursive: true,
  preserveTimestamps: true,
  verbatimSymlinks: true,
});
for (const name of ['run-acceptance.command', 'README.md']) {
  const destination = path.join(kit, name);
  copyFileSync(path.join(root, 'tools/acceptance/native-macos', name), destination);
  chmodSync(destination, name.endsWith('.command') ? 0o755 : 0o644);
}

const suitesManifest = suites
  .map((name) => `${name}\t${suiteTestCounts[name]}`)
  .join('\n');
writeFileSync(path.join(kit, 'suites.tsv'), `${suitesManifest}\n`);

const payload = [
  'run-acceptance.command',
  'README.md',
  'suites.tsv',
  ...suites.map((name) => `debug/deps/${name}`),
  ...probes.map((name) => `debug/examples/${name}`),
];
const binaries = Object.fromEntries(
  payload.map((relative) => [relative, sha256(path.join(kit, relative))]),
);
const receipt = {
  format: 1,
  ...input,
  sourceCommit,
  sourceDirty: false,
  debug: true,
  features: ['storage-fault-injection'],
  platform: input.target,
  architecture: 'arm64',
  signing: 'ad-hoc; not notarized',
  suites: suiteTestCounts,
  totalTests: Object.values(suiteTestCounts).reduce((total, count) => total + count, 0),
  probes,
  binaries,
  runner: {
    image: process.env.ImageOS,
    imageVersion: process.env.ImageVersion,
    runId: process.env.GITHUB_RUN_ID,
    attempt: process.env.GITHUB_RUN_ATTEMPT,
  },
  scope:
    'Bounded native fault/restore harnesses only; not frozen device acceptance and not a product release.',
};
writeJson(path.join(kit, 'receipt.json'), receipt);

const sums = ['receipt.json', ...payload]
  .sort()
  .map((relative) => `${sha256(path.join(kit, relative))}  ${relative}`)
  .join('\n');
writeFileSync(path.join(kit, 'SHA256SUMS'), `${sums}\n`);

run('tar', ['-czf', archive, '-C', kit, '.']);
run(process.execPath, ['tools/packaging/encrypt.mjs', archive, encrypted]);
console.log(
  JSON.stringify({
    encrypted,
    sourceCommit,
    suites: suiteTestCounts,
    probes,
  }),
);
