// Runtime contract checks; explicitly not M1 timings or native input acceptance.
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { root, run, capture, readJson, sha256, writeJson } from '../../packaging/common.mjs';
if(process.platform!=='darwin' || process.env.RUNNER_ENVIRONMENT!=='github-hosted') throw new Error('CI runner required');
const out=path.join(root,'package-output/macos-performance'), kit=path.join(out,'delivery');
const driver=path.join(kit,'cuenote-performance-driver'), app=path.join(kit,'CueNote Performance.app/Contents/MacOS/cuenote');
const owned=path.join(process.env.HOME,'CueNote-observer-ci');
mkdirSync(owned,{mode:0o700});
writeFileSync(path.join(owned,'.cuenote-performance'),'CueNote isolated performance root v1\n',{flag:'wx',mode:0o600});
const launch=JSON.parse(capture(driver,['launch',owned,app,sha256(app)]));
let check;
try {
  check=JSON.parse(capture(driver,['protocol-check',launch.run]));
  if(!check.invalidRequestsRejected || check.paintedSnapshot.observedFrames!==2 ||
     !check.snapshot.text.includes('选择你的笔记库')) throw new Error('Observer check failed');
} finally { run(driver,['close',launch.run]); }
const permissions=JSON.parse(capture(driver,['doctor']));
writeJson(path.join(kit,'ci-observer-check.json'),{sourceCommit:readJson(path.join(kit,'receipt.json')).sourceCommit,
  observer:check,permissions,nativeInput:'not measured; calibration on the actual M1 is still required',
  note:'CI checks fixed reads, malformed/unauthorized rejection and normal close. Not performance scores.'});
// Do not include run directories, observer credentials or generated libraries.
const archive=path.join(out,'CueNote-macos-performance.tar.gz');
run('tar',['-czf',archive,'-C',kit,'.']);
run(process.execPath,['tools/packaging/encrypt.mjs',archive,path.join(root,'package-output/performance-encrypted/CueNote-macos-performance.tar.gz.age')]);
