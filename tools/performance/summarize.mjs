// Summarize raw desktop runs without excluding slow samples. Host-only probes
// are intentionally not accepted as desktop evidence by this tool.
import {readFile} from 'node:fs/promises';
import path from 'node:path';

const limits = {
  startup:[10,3000,'max'], 'initial-index':[3,60000,'max'],
  'ordinary-open':[30,1000,'p95'], 'ordinary-save':[30,2000,'p95'],
  'stress-open':[30,5000,'p95'], 'stress-save':[30,10000,'p95'],
  search:[30,300,'p95'], 'ordinary-map':[30,1000,'p95'], 'stress-map':[30,3000,'p95'],
  'ordinary-input':[100,100,'p95'], 'stress-input':[100,200,'p95'],
};
const runs = process.argv.slice(2);
if (!runs.length) throw new Error('Usage: node summarize.mjs <desktop run directory> ...');
let build, corpus;
const groups = new Map();
const failures = [];
for (const metric of Object.keys(limits)) {
  groups.set(`${metric}/all`,{metric,cacheState:'all',samples:[]});
}
function add(metric, sample) {
  if (!limits[metric]) throw new Error(`Unknown acceptance metric: ${metric}`);
  if (sample.cacheState === 'all') throw new Error('Use a concrete cache state for raw samples');
  if (!Number.isFinite(sample.elapsedMs) || sample.elapsedMs < 0) throw new Error('Invalid elapsedMs');
  const key = `${metric}/${sample.cacheState ?? 'unspecified'}`;
  const group = groups.get(key) ?? {metric,cacheState:sample.cacheState ?? 'unspecified',samples:[]};
  group.samples.push(sample.elapsedMs); groups.set(key, group);
  const overallKey = `${metric}/all`;
  const overall = groups.get(overallKey) ?? {metric,cacheState:'all',samples:[]};
  overall.samples.push(sample.elapsedMs); groups.set(overallKey,overall);
}
for (const run of runs) {
  const launcher = JSON.parse(await readFile(path.join(run,'launcher.json'),'utf8'));
  if (launcher.traceEnabled) throw new Error('Diagnostic trace runs must be reported separately');
  const copy = JSON.parse(await readFile(path.join(launcher.root,'corpus-copy.json'),'utf8'));
  if (build && build !== launcher.sha256) throw new Error('Different binary hashes must be reported separately');
  if (corpus && corpus !== copy.manifestSha256) throw new Error('Different corpora must be reported separately');
  build = launcher.sha256; corpus = copy.manifestSha256;
  const startup = JSON.parse(await readFile(path.join(run,'startup.json'),'utf8'));
  if (startup.operation === 'startup') add('startup',{...startup,cacheState:'indexed'});
  let raw = '';
  try { raw = await readFile(path.join(run,'measurements.jsonl'),'utf8'); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  for (const line of raw.split(/\r?\n/u).filter(Boolean)) {
    const sample = JSON.parse(line);
    if (sample.metric) add(sample.metric,sample);
  }
  try {
    const failed = await readFile(path.join(run,'failures.jsonl'),'utf8');
    for (const line of failed.split(/\r?\n/u).filter(Boolean)) failures.push({run,...JSON.parse(line)});
  } catch (error) { if (error.code !== 'ENOENT') throw error; }
}
const report = [...groups.values()].map(({samples,...group}) => {
  samples.sort((a,b) => a-b);
  const gate = limits[group.metric];
  const p95 = samples[Math.ceil(samples.length*0.95)-1] ?? null, max = samples.at(-1) ?? null;
  const countEnough = gate && samples.length >= gate[0];
  return {...group,count:samples.length,minMs:samples[0] ?? null,p95Ms:p95,maxMs:max,
    requiredSamples:gate?.[0],limitMs:gate?.[1],statistic:gate?.[2],
    status:group.cacheState !== 'all' ? 'breakdown' : !countEnough?'insufficient-samples':(gate[2]==='max'?max:p95)<=gate[1]?'pass':'fail'};
});
console.log(JSON.stringify({binarySha256:build,manifestSha256:corpus,runs: runs.map(p=>path.resolve(p)),
  timingMatrixPassed:failures.length===0 && report.filter(group=>group.cacheState==='all').every(group=>group.status==='pass'),
  method:'nearest-rank; all supplied records retained; the timing matrix alone does not prove resource limits or cancellation behavior',failures,report},null,2));
