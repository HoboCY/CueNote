import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { root, verifyInputs, run, capture, tauri, sha256, writeJson } from '../../packaging/common.mjs';

const input = verifyInputs();
if (input.target !== 'aarch64-apple-darwin') throw new Error('Build this kit on an ARM64 Mac');
const out = path.join(root, 'package-output/macos-performance');
const delivery = path.join(out, 'delivery');
if (existsSync(delivery)) throw new Error('Use a fresh build output for a new kit');
mkdirSync(delivery, { recursive: true });
process.env.CARGO_TARGET_DIR = path.join(root, 'src-tauri/target/performance-macos');
const config = JSON.parse(readFileSync(path.join(root, 'src-tauri/tauri.performance.conf.json'), 'utf8'));
config.bundle = { active: true, targets: ['app'], resources: {'../package-resources/':'licenses/'} };
const configFile = path.join(out, 'tauri-performance.json');
writeJson(configFile, config);
run('swiftc', ['-O', '-whole-module-optimization', '-target', 'arm64-apple-macosx14.0',
  'tools/performance/macos/Driver.swift', 'tools/performance/macos/Matrix.swift',
  'tools/performance/macos/main.swift', '-o', path.join(delivery, 'cuenote-performance-driver')]);
run('codesign', ['--force', '--sign', '-', path.join(delivery, 'cuenote-performance-driver')]);
tauri('build', '--no-binary-patching', '--target', input.target, '--features', 'perf-acceptance', '--bundles', 'app', '--config', configFile, '--', '--locked');
const release = path.join(process.env.CARGO_TARGET_DIR, input.target, 'release');
const app = path.join(delivery, 'CueNote Performance.app');
cpSync(path.join(release, 'bundle/macos/CueNote Performance.app'), app, { recursive:true, preserveTimestamps:true, verbatimSymlinks:true });
run('cargo', ['build','--locked','--release','--manifest-path','src-tauri/Cargo.toml','--target',input.target,'--example','performance_fixture']);
copyFileSync(path.join(release,'examples/performance_fixture'), path.join(delivery,'performance_fixture'));
run('codesign', ['--force','--sign','-',path.join(delivery,'performance_fixture')]);
const suites=['format_v1','image_content','rich_content','storage','mind_map_content','resource_preservation'];
const compiled=capture('cargo',['test','--locked','--release','--no-run','--manifest-path','src-tauri/Cargo.toml',
  '--target',input.target,...suites.flatMap(name=>['--test',name]),'--message-format=json'],{maxBuffer:20*1024*1024});
const testArtifacts=compiled.split(/\r?\n/u).filter(Boolean).map(line=>JSON.parse(line))
  .filter(row=>row.reason==='compiler-artifact' && row.profile.test && row.executable && suites.includes(row.target.name));
mkdirSync(path.join(delivery,'resource-tests'));
if(testArtifacts.length!==suites.length || new Set(testArtifacts.map(row=>row.target.name)).size!==suites.length)
  throw new Error(`Expected all ${suites.length} resource test binaries`);
for(const artifact of testArtifacts) {
  const dest=path.join(delivery,'resource-tests',artifact.target.name);
  copyFileSync(artifact.executable,dest); run('codesign',['--force','--sign','-',dest]);
}
const corpus = path.join(out,'fixed-corpus');
run(path.join(delivery,'performance_fixture'), ['generate',corpus]);
const manifest = path.join(corpus,'manifest.json');
if (sha256(manifest) !== '0e95e7288b26e37248899dedd09fe5c311866febc3d94d72f550bffbc28b0292') throw new Error('Mac fixture differs from frozen Q51 corpus');
run(process.execPath, ['tools/performance/make-plans.mjs',manifest,path.join(delivery,'plans')]);
copyFileSync(manifest,path.join(delivery,'manifest.json'));
for(const name of ['run-matrix.command','README.md']) copyFileSync(path.join(root,'tools/performance/macos',name),path.join(delivery,name));
cpSync(path.join(root,'package-resources'),path.join(delivery,'licenses'),{recursive:true});
const hashes = {};
for (const relative of ['cuenote-performance-driver','performance_fixture','CueNote Performance.app/Contents/MacOS/cuenote',
  ...suites.map(name=>'resource-tests/'+name)]) {
  const file = path.join(delivery,relative);
  if(capture('lipo',['-archs',file]) !== 'arm64') throw new Error(`Wrong architecture: ${relative}`);
  hashes[relative]=sha256(file);
}
run('codesign',['--verify','--deep','--strict',app]);
const sourceCommit=capture('git',['rev-parse','HEAD']);
const sourceDirty=Boolean(capture('git',['status','--porcelain','--untracked-files=no']));
if(sourceDirty) throw new Error('Do not deliver a kit from dirty source');
writeJson(path.join(delivery,'receipt.json'),{format:1,...input,sourceCommit,sourceDirty,
  applicationIdentifier:'com.hobocy.cuenote.performance',features:['perf-acceptance','tauri/custom-protocol'],
  binaries:hashes,manifestSha256:sha256(manifest),swift:capture('swiftc',['--version']),
  plans:Object.fromEntries(readdirSync(path.join(delivery,'plans')).sort().map(name=>[name,sha256(path.join(delivery,'plans',name))])),
  resourceSuites:suites,
  xcode:capture('xcodebuild',['-version']),runner:{image:process.env.ImageOS,imageVersion:process.env.ImageVersion,runId:process.env.GITHUB_RUN_ID},
  scope:'Isolated performance tools only; neither CI timings nor this kit establish M1 acceptance.'});
console.log(JSON.stringify({delivery,sourceCommit,hashes}));
