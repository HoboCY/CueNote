// Deterministic real-UI actions derived from the frozen fixture manifest.
import {readFile, mkdir, writeFile} from 'node:fs/promises';
import path from 'node:path';

const [manifestPath, destination] = process.argv.slice(2);
if (!manifestPath || !destination) throw new Error('Usage: node make-plans.mjs <manifest.json> <new output directory>');
const manifest = JSON.parse(await readFile(manifestPath,'utf8'));
if (manifest.format !== 'cuenote-q51-performance-corpus' || manifest.scale !== 'full') throw new Error('The full Q51 corpus is required');
await mkdir(destination); // Do not overwrite an earlier acceptance plan.
const notes = profile => manifest.notes.filter(n => n.collection === 'library' && n.profile === profile);
const scroll = selector => ({kind:'scroll',selector});
const click = selector => ({kind:'click',selector});
const text = value => ({kind:'text',text:value});
const key = (key,code,modifiers=0) => ({kind:'key',key,code,modifiers});
const selectAll = key('a','KeyA',2);
const titleInput = '#note-title-input';
const heading = '[data-testid="rich-editor-input-content"] h1';
const open = (note,name,metric,cacheState) => ({
  name,metric,cacheState,timeoutMs:15000,
  setup:[scroll('[data-note-id="'+note.noteId+'"][data-testid="normal-note-item"]')],
  actions:[click('[data-note-id="'+note.noteId+'"][data-testid="normal-note-item"]')],
  expected:{kind:'editor',noteId:note.noteId,revealFirstImage:true},
});
async function save(name,plan) {
  await writeFile(path.join(destination,name+'.json'),JSON.stringify(plan,null,2)+'\n',{flag:'wx'});
}
await save('index',[{name:'initial-index',metric:'initial-index',cacheState:'first',
  actions:[click('.empty-actions .primary-button')],expected:{kind:'index',count:1000},capture:'indexed'}]);
for (const profile of ['ordinary','stress']) {
  const list = notes(profile), label = profile === 'ordinary' ? '普通' : '压力';
  const firstCount = Math.min(30,list.length);
  const opens = Array.from({length:30+firstCount},(_,i) => {
    const first = i < firstCount;
    const note = list[first ? i : (i-firstCount)%firstCount];
    return open(note,profile+'-open-'+String(i+1).padStart(2,'0'),profile+'-open',first?'first':'cached');
  });
  await save(profile+'-opens',opens);
  await save(profile+'-select',[open(list[0],profile+'-select')]);
  await save(profile+'-saves',Array.from({length:32},(_,i) => {
    const value = label+'样本 '+String(i+1).padStart(2,'0')+' · SHARED-Q51-'+profile.toUpperCase();
    return {name:profile+'-save-'+(i+1),metric:profile+'-save',cacheState:i===0?'first':i>=29?'rotation':'cached',
      beforeExpected:{kind:'save'},
      setup:[scroll(titleInput),click(titleInput),selectAll,text(value)],
      setupExpected:{kind:'dirty-title',text:value},
      actions:[key('s','KeyS',2)],expected:{kind:'save'},timeoutMs:60000};
  }));
  await save(profile+'-inputs',Array.from({length:100},(_,i) => {
    const value='内容区 · '+label+'样本 '+String((i+1)%100).padStart(2,'0');
    return {name:profile+'-input-'+(i+1),metric:profile+'-input',cacheState:'cached',timeoutMs:5000,
      setup:[scroll(heading),click(heading),key('Home','Home'),key('End','End',8)],
      setupExpected:{kind:'content-heading-selection'},
      actions:[text(value)],expected:{kind:'content-heading',text:value}};
  }));
  const maps=[];
  for(let i=0;i<30;i++) {
    maps.push({name:profile+'-map-'+(i+1),metric:profile+'-map',cacheState:i===0?'first':'cached',
      setup:[scroll('[data-testid="mind-map-open"]')],
      actions:[click('[data-testid="mind-map-open"]')],
      expected:{kind:'map',count:profile==='ordinary'?50:500},timeoutMs:10000});
    maps.push({name:profile+'-collapse-'+(i+1),
      setup:[scroll('[data-testid="mind-map-collapse"]')],
      actions:[click('[data-testid="mind-map-collapse"]')],expected:{kind:'map-collapsed'},timeoutMs:5000});
  }
  await save(profile+'-maps',maps);
}
// Include shared, per-note, case-folded, and zero-result queries. Each result
// set fits on the first page; the UI driver checks every returned note ID.
const queries = ['SHARED-Q51-ORDINARY','SHARED-Q51-STRESS','UNIQ-ORD-000','UNIQ-STR-00']
  .map(query => manifest.knownQueries.find(q => q.query === query)).filter(Boolean);
queries.push({query:'uniq-ord-000',noteIds:queries.find(q=>q.query==='UNIQ-ORD-000').noteIds});
queries.push({query:'Q51-NO-SUCH-TOKEN',noteIds:[]});
await save('search',Array.from({length:30},(_,i) => {
  const q = queries[i%queries.length];
  return {name:'search-'+(i+1),metric:'search',cacheState:i<queries.length?'first':'cached',
    setup:[scroll('#sidebar-search-input'),click('#sidebar-search-input'),selectAll],
    actions:[text(q.query)],expected:{kind:'search',total:q.noteIds.length,ids:q.noteIds},timeoutMs:5000};
}));
await save('clear-search',[{name:'clear-search',setup:[scroll('.sidebar-search-clear')],
  actions:[click('.sidebar-search-clear')],expected:{kind:'index',count:1000}}]);
const cancellation = [];
for (const [kind,number] of [['switch',41],['close',42]]) {
  const value='压力样本 '+number+' · SHARED-Q51-STRESS';
  const target='[data-testid="normal-note-item"][data-note-id="'+notes('ordinary')[1].noteId+'"]';
  cancellation.push({
    name:'request-'+kind+'-during-save',beforeExpected:{kind:'save'},
    setup:[scroll(titleInput),click(titleInput),selectAll,text(value)],
    setupExpected:{kind:'dirty-title',text:value},
    actions:[key('s','KeyS',2),...(kind==='switch' ? [scroll(target),click(target)] : [{kind:'request-close'}])],
    expected:{kind:'transition',text:kind==='switch'?'切换前':'关闭前',saving:true},
    timeoutMs:10000,capture:kind+'-save-wait',
  });
  cancellation.push({
    name:'cancel-'+kind+'-while-saving',
    setupExpected:{kind:'transition',text:kind==='switch'?'切换前':'关闭前',saving:true},
    actions:[click('[data-dialog-cancel]')],
    expected:{kind:'transition-cancelled',noteId:notes('stress')[0].noteId,title:value},timeoutMs:1000,
  });
  cancellation.push({name:'cancel-'+kind+'-still-finishes-safe-save',expected:{kind:'save'},timeoutMs:20000});
}
await save('cancel-save-wait',cancellation);
console.log(JSON.stringify({destination,ordinary:notes('ordinary').length,stress:notes('stress').length,queries:queries.map(q=>({query:q.query,count:q.noteIds.length}))}));
