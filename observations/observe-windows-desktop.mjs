// Disposable observer: real CueNote WebView2 + Rust IPC, never mocked.
import { writeFile, readFile, readdir } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';
const outputRoot = process.argv[2];
const pages = await (await fetch('http://127.0.0.1:9232/json/list')).json();
const page = pages.find(p => p.type === 'page' && p.url === 'http://tauri.localhost/');
if (!page) throw new Error('Expected the isolated bundled CueNote WebView');
const socket = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
let sequence = 0;
const pending = new Map();
const errors = [];
socket.onmessage = ({ data }) => {
  const message = JSON.parse(data);
  if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails);
  const task = pending.get(message.id);
  if (task) { pending.delete(message.id); clearTimeout(task.timer); message.error ? task.reject(message.error) : task.resolve(message.result); }
};
const call = (method, params = {}) => new Promise((resolve, reject) => {
  const id = ++sequence;
  const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Timed out: ${method}`)); }, 180000);
  pending.set(id, { resolve, reject, timer });
  socket.send(JSON.stringify({ id, method, params }));
});
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const evaluate = async expression => {
  const result = await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true, userGesture: true });
  if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
  return result.result.value;
};
const waitFor = async (expression, limit = 180000) => {
  const start = performance.now();
  while (!(await evaluate(expression))) {
    if (performance.now() - start > limit) throw new Error(`Condition timeout: ${expression}`);
    await delay(100);
  }
};
const observations = [];
const record = (name, passed, detail) => {
  const row = { name, verdict: passed ? 'PASS' : 'FAIL', detail };
  observations.push(row); console.log(JSON.stringify(row));
  if (!passed) throw new Error(name);
};
const clickText = (selector, text) => evaluate(`(() => { const button = [...document.querySelectorAll(${JSON.stringify(selector)})].find(el => el.textContent.trim() === ${JSON.stringify(text)}); if (!button || button.disabled) throw new Error('Button unavailable: ' + ${JSON.stringify(text)}); button.click(); return true; })()`);
const focusEnd = selector => evaluate(`(() => { const el = document.querySelector(${JSON.stringify(selector)}); el.focus(); const range = document.createRange(); range.selectNodeContents(el); range.collapse(false); const selection = getSelection(); selection.removeAllRanges(); selection.addRange(range); return true; })()`);
const key = async (key, code, windowsVirtualKeyCode) => {
  await call('Input.dispatchKeyEvent', { type: 'keyDown', key, code, windowsVirtualKeyCode });
  await call('Input.dispatchKeyEvent', { type: 'keyUp', key, code, windowsVirtualKeyCode });
};
const ui = () => evaluate(`({title:document.querySelector('#note-title')?.value,cue:document.querySelector('.cue-editor')?.innerText,body:document.querySelector('.body-editor')?.innerText,summary:document.querySelector('.summary-editor')?.innerText,map:document.querySelector('.map-preview')?.innerText,images:[...document.querySelectorAll('.resizable-image img')].map(i=>({src:i.src,width:i.naturalWidth,height:i.naturalHeight})),status:document.querySelector('.state-summary')?.innerText,indicator:document.querySelector('.save-indicator')?.innerText})`);
const saveUI = async () => {
  const start = performance.now();
  await clickText('button', '保存笔记');
  await waitFor(`['保存完成','保存了开始操作时','保存冲突','保存失败'].some(text=>document.querySelector('.state-summary').innerText.includes(text))`);
  return { endToEndMs: Math.round(performance.now() - start), status: await evaluate(`document.querySelector('.state-summary').innerText`), metrics: await evaluate(`document.querySelector('.save-metrics')?.innerText`) };
};
let samplePath;
try {
  await call('Runtime.enable');
  await call('Page.enable');
  await waitFor(`document.querySelectorAll('.tiptap[contenteditable]').length === 3 && !document.querySelector('.loading-overlay')`);
  const initial = await evaluate(`({mode:document.querySelector('.paper-topline').innerText,saveEnabled:![...document.querySelectorAll('button')].find(b=>b.textContent.trim()==='保存笔记').disabled,status:document.querySelector('.state-summary').innerText})`);
  const info = await evaluate(`window.__TAURI_INTERNALS__.invoke('prototype_info')`);
  samplePath = info.path;
  if (!samplePath.includes('PROTOTYPE-wipe-me')) throw new Error('Wrong scratch path');
  record('Bundled Windows app starts in real desktop mode', initial.mode.includes('桌面文件') && initial.saveEnabled && initial.status.includes('尚无本机'), { ...initial, path: samplePath });

  for (const [selector, text] of [['.cue-editor', '桌面线索验证甲'], ['.body-editor', '桌面正文验证乙'], ['.summary-editor', '桌面总结验证丙']]) {
    await focusEnd(selector); await call('Input.insertText', { text });
  }
  await clickText('.cue-panel button', '撤销');
  const undone = await ui();
  record('Desktop section undo leaves other sections intact', !undone.cue.includes('桌面线索验证甲') && undone.body.includes('桌面正文验证乙') && undone.summary.includes('桌面总结验证丙'), { cueUndone: true, bodyRetained: true, summaryRetained: true });
  await focusEnd('.cue-editor'); await call('Input.insertText', { text: '桌面线索保存验证' });
  await evaluate(`(() => {const input=document.querySelector('#note-title');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,'CueNote 桌面保存验证');input.dispatchEvent(new Event('input',{bubbles:true}));})()`);

  await focusEnd('.body-editor');
  const fixture = await evaluate(`(async () => {const canvas=document.createElement('canvas');canvas.width=320;canvas.height=180;const ctx=canvas.getContext('2d');ctx.fillStyle='#28766f';ctx.fillRect(0,0,320,180);ctx.fillStyle='white';ctx.font='24px sans-serif';ctx.fillText('CueNote desktop',30,95);const blob=await new Promise(r=>canvas.toBlob(r,'image/png'));const dt=new DataTransfer();dt.items.add(new File([blob],'PROTOTYPE-image.png',{type:'image/png'}));document.querySelector('.body-editor').dispatchEvent(new ClipboardEvent('paste',{clipboardData:dt,bubbles:true,cancelable:true}));return{bytes:blob.size};})()`);
  await waitFor(`document.querySelector('.resizable-image img')?.naturalWidth === 320`);

  await evaluate(`document.querySelector('.map-open-button').click()`);
  await waitFor(`document.querySelector('me-tpc') !== null`);
  const point = await evaluate(`(() => {const r=document.querySelector('me-tpc').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
  for (const clickCount of [1, 2]) {
    await call('Input.dispatchMouseEvent', { type:'mousePressed', button:'left', ...point, clickCount });
    await call('Input.dispatchMouseEvent', { type:'mouseReleased', button:'left', ...point, clickCount });
    await delay(40);
  }
  await waitFor(`document.activeElement?.id === 'input-box'`);
  await call('Input.insertText', { text:'桌面导图持久化' });
  await key('Enter','Enter',13); await key('Tab','Tab',9);
  await waitFor(`document.activeElement?.id === 'input-box'`);
  await call('Input.insertText', { text:'重开后仍可编辑的子节点' });
  await key('Enter','Enter',13);
  await clickText('dialog button','完成');
  await waitFor(`!document.querySelector('dialog[open]')`);

  const before = await ui();
  const firstSave = await saveUI();
  record('UI first save writes native ZIP with text, PNG and editable map', firstSave.status.includes('保存完成'), { ...firstSave, fixture, onDiskBytes: (await readFile(samplePath)).length });
  const snapshot = await evaluate(`(async()=>{window.__savedSmall=await window.__TAURI_INTERNALS__.invoke('prototype_open');return {revision:window.__savedSmall.revision,title:window.__savedSmall.document.title,assetCount:window.__savedSmall.assets.length,assetReferences:JSON.stringify(window.__savedSmall.document).includes('asset:'),mapData:JSON.stringify(window.__savedSmall.document).includes('重开后仍可编辑的子节点')};})()`);
  record('Native reopen returns structured note and real image asset', snapshot.title === before.title && snapshot.assetCount === 1 && snapshot.assetReferences && snapshot.mapData, snapshot);
  await clickText('button', '重开文件');
  await waitFor(`document.querySelector('.state-summary').innerText.includes('已从桌面笔记文件重新打开')`);
  await waitFor(`document.querySelector('.resizable-image img')?.naturalWidth === 320`);
  const reopened = await ui();
  record('Three sections, title, image bytes and map preview survive UI reopen', ['title','cue','body','summary','map','images'].every(k => JSON.stringify(before[k]) === JSON.stringify(reopened[k])), { title:reopened.title, images:reopened.images.length, mapRestored:reopened.map.includes('桌面导图持久化') });
  await evaluate(`document.querySelector('.map-open-button').click()`);
  await waitFor(`document.querySelectorAll('me-tpc').length === 7`);
  record('Reopened map expands as seven editable nodes', await evaluate(`document.querySelector('dialog').innerText.includes('重开后仍可编辑的子节点')`), { nodes:7 });
  await clickText('dialog button','完成');

  await focusEnd('.body-editor');
  const largeFixture = await evaluate(`(async()=>{const canvas=document.createElement('canvas');canvas.width=2048;canvas.height=1536;const ctx=canvas.getContext('2d');const pixels=ctx.createImageData(canvas.width,canvas.height);let seed=102414;for(let i=0;i<pixels.data.length;i+=4){seed^=seed<<13;seed^=seed>>>17;seed^=seed<<5;pixels.data[i]=seed&255;pixels.data[i+1]=(seed>>>8)&255;pixels.data[i+2]=(seed>>>16)&255;pixels.data[i+3]=255;}ctx.putImageData(pixels,0,0);const blob=await new Promise(r=>canvas.toBlob(r,'image/png'));const dt=new DataTransfer();dt.items.add(new File([blob],'PROTOTYPE-large.png',{type:'image/png'}));document.querySelector('.body-editor').dispatchEvent(new ClipboardEvent('paste',{clipboardData:dt,bubbles:true,cancelable:true}));return {width:canvas.width,height:canvas.height,bytes:blob.size};})()`);
  await waitFor(`document.querySelectorAll('.resizable-image img').length === 2 && [...document.querySelectorAll('.resizable-image img')].some(i=>i.naturalWidth===2048) && document.querySelector('.save-indicator').innerText !== '图片读取中'`);
  const largeSave = await saveUI();
  record('Large generated PNG saves through actual UI and Rust service', largeSave.status.includes('保存完成'), { fixture:largeFixture, ...largeSave, onDiskBytes:(await readFile(samplePath)).length, build:'debug, unoptimized; one observation, not a benchmark' });
  const largeFingerprint = await evaluate(`(async()=>{window.__savedLarge=await window.__TAURI_INTERNALS__.invoke('prototype_open');const data=window.__savedLarge;return {revision:data.revision,assets:data.assets.length,largeImageBytes:Math.max(...data.assets.map(a=>atob(a.base64).length))};})()`);
  await clickText('button','重开文件');
  await waitFor(`document.querySelector('.state-summary').innerText.includes('已从桌面笔记文件重新打开') && [...document.querySelectorAll('.resizable-image img')].some(i=>i.naturalWidth===2048)`);
  record('Large PNG reopens and decodes from saved ZIP', largeFingerprint.assets === 2 && largeFingerprint.largeImageBytes === largeFixture.bytes, largeFingerprint);

  await focusEnd('.body-editor'); await call('Input.insertText',{text:'应保留的本机冲突草稿'});
  await clickText('button','模拟外部改动');
  await waitFor(`document.querySelector('.state-summary').innerText.includes('已模拟桌面文件外部改动')`);
  const externalBytes = await readFile(samplePath);
  const conflict = await saveUI();
  const afterConflict = await ui();
  const folders = await readdir(info.recoveryPath, { withFileTypes:true });
  const conflictFolders = folders.filter(item=>item.isDirectory() && item.name.startsWith('conflict-'));
  const folder = path.join(info.recoveryPath, conflictFolders.at(-1)?.name ?? 'MISSING');
  const currentCopy = await readFile(path.join(folder,'CURRENT.cuenote'));
  const draftCopy = await readFile(path.join(folder,'DRAFT.cuenote'));
  record('Real stale save preserves external file, local draft and both conflict copies', conflict.status.includes('保存冲突') && afterConflict.body.includes('应保留的本机冲突草稿') && externalBytes.equals(await readFile(samplePath)) && externalBytes.equals(currentCopy) && draftCopy.length > 0, { status:conflict.status, conflictCopies:2, currentSha256:createHash('sha256').update(currentCopy).digest('hex'), draftBytes:draftCopy.length });

  // Restore only the generated small sample using the actual current revision.
  await evaluate(`(async()=>{const current=await window.__TAURI_INTERNALS__.invoke('prototype_open');const {document,assets}=window.__savedSmall;await window.__TAURI_INTERNALS__.invoke('prototype_save',{snapshot:{document,assets},expectedRevision:current.revision});})()`);
  await call('Page.reload');
  await waitFor(`document.querySelectorAll('.tiptap[contenteditable]').length === 3 && !document.querySelector('.loading-overlay') && document.querySelector('.resizable-image img')?.naturalWidth === 320`);
  const final = await ui();
  record('Fresh WebView loads the saved small sample without Vite', final.title === before.title && final.body === before.body && final.map === before.map && final.images.length === 1, { title:final.title, status:final.status, url:page.url });
  await evaluate(`window.scrollTo(0,0)`);
  const screenshot=await call('Page.captureScreenshot',{format:'png',captureBeyondViewport:false});
  await writeFile(path.join(outputRoot,'desktop-20261014.png'),Buffer.from(screenshot.data,'base64'));
  record('No uncaught WebView errors during desktop observations', errors.length === 0, { count:errors.length });
} catch (error) {
  observations.push({name:'Observation execution',verdict:'FAIL',detail:String(error)});
  console.error(error);
} finally {
  await writeFile(path.join(outputRoot,'desktop-20261014.json'),JSON.stringify({date:'2026-10-14',build:'Tauri debug with bundled production frontend',observations,errors,limitations:['Programmatic text input is not a real Chinese IME test.','Synthetic ClipboardEvent is not the Windows clipboard.','No macOS or OneDrive sync executed.','Single debug run is not a release benchmark.']},null,2));
  socket.close();
}
if(observations.some(o=>o.verdict==='FAIL'))process.exitCode=1;
