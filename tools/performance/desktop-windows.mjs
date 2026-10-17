// Real optimized WebView2 UI driver. No IPC calls, DOM edits, or application mocks.
// Timing uses this process's monotonic clock, including CDP transport and paint waits.
import {spawn, execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {readFile, writeFile, mkdir, appendFile} from 'node:fs/promises';
import {openSync, closeSync} from 'node:fs';
import path from 'node:path';
import net from 'node:net';
import {fileURLToPath} from 'node:url';

const [command, ...args] = process.argv.slice(2);
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const driverDirectory = path.dirname(fileURLToPath(import.meta.url));
let record, run, socket, childExit, serial = 0;
const pending = new Map();
async function call(method, params = {}) {
  const id = ++serial;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 240000);
    pending.set(id, {resolve, reject, timer});
    socket.send(JSON.stringify({id, method, params}));
  });
}
async function connect() {
  const deadline = performance.now() + 30000;
  let target;
  while (performance.now() < deadline) {
    if (childExit !== undefined) throw new Error(`Application exited (${childExit}); see ${run}/stderr.txt`);
    try {
      const all = await (await fetch(`http://127.0.0.1:${record.port}/json/list`)).json();
      const pages = all.filter(t => t.type === 'page' && /^(https?:\/\/tauri\.localhost|tauri:\/\/localhost)\//u.test(t.url));
      if (pages.length > 1) throw new Error('Multiple application pages on the dedicated port');
      target = pages[0];
      if (target) break;
    } catch { /* WebView startup has not exposed its port yet. */ }
    await sleep(10);
  }
  if (!target) throw new Error('Owned application page did not start');
  const url = new URL(target.webSocketDebuggerUrl);
  if (!['localhost', '127.0.0.1', '[::1]'].includes(url.hostname) || +url.port !== record.port) throw new Error('Unexpected debugger endpoint');
  socket = new WebSocket(url);
  await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
  socket.onmessage = event => {
    const message = JSON.parse(event.data), request = pending.get(message.id);
    if (!request) return;
    clearTimeout(request.timer); pending.delete(message.id);
    if (message.error) request.reject(new Error(JSON.stringify(message.error))); else request.resolve(message.result);
  };
}
async function read(fn, argument = null) {
  const result = await call('Runtime.evaluate', {
    expression: `(${fn.toString()})(${JSON.stringify(argument)})`,
    returnByValue: true, awaitPromise: true,
  });
  if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails));
  return result.result.value;
}
const snapshot = () => {
  const title = document.querySelector('#note-title-input');
  const current = document.querySelector('[data-note-id][aria-current="page"]');
  const feedback = document.querySelector('.runtime-readonly.is-saving');
  const feedbackRect = feedback?.getBoundingClientRect();
  const images = [...document.querySelectorAll('.rich-image-preview')].map(img => {
    const r = img.getBoundingClientRect();
    return {visible:r.width > 0 && r.height > 0 && r.bottom > 0 && r.top < innerHeight,
      ready:img.complete && img.naturalWidth > 0, width:img.naturalWidth, top:r.top};
  });
  return {visibility:document.visibilityState,focused:document.hasFocus(),viewport:{width:innerWidth,height:innerHeight,devicePixelRatio},title:title?.value, editable:Boolean(title && !title.disabled && !title.readOnly &&
      document.querySelector('[data-testid="rich-editor-input-content"][contenteditable="true"]')),
    noteId:current?.getAttribute('data-note-id'), images,
    titleSelection:title ? {start:title.selectionStart,end:title.selectionEnd,focused:document.activeElement===title} : null,
    contentHeading:document.querySelector('[data-testid="rich-editor-input-content"] h1, [data-testid="rich-editor-input-content"] h2, [data-testid="rich-editor-input-content"] h3')?.textContent,
    selectedText:document.getSelection()?.toString().slice(0,1024),
    maps:document.querySelectorAll('.mind-map-preview-tree').length,
    expandedNodes:document.querySelectorAll('.mind-map-editor-panel:not([hidden]) me-tpc').length,
    mapCanEdit:Boolean(document.querySelector('.mind-map-editor-panel:not([hidden]) [data-testid="mind-map-edit-text"]:not([disabled])')),
    save:document.querySelector('.save-button')?.textContent?.trim(),
    savingFeedback:feedback ? {text:feedback.textContent,visible:Boolean(feedbackRect?.width && feedbackRect.top>=0 && feedbackRect.bottom<=innerHeight)} : null,
    notes:document.querySelectorAll('[data-testid="normal-note-item"]').length,
    hits:[...document.querySelectorAll('[data-testid="search-note-item"]')].map(el => el.getAttribute('data-note-id')),
    search:document.querySelector('.search-results-summary')?.textContent,
    progress:[...document.querySelectorAll('[role="status"]')].map(el => el.textContent),
    errors:[...document.querySelectorAll('[role="alert"]')].map(el => el.textContent),
    transition:document.querySelector('#unsaved-dialog-title')?.textContent,
    text:document.querySelector('.empty-workspace')?.innerText ?? ''};
};
const frames = () => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve(true))));
function ready(state, expected) {
  if (expected.kind === 'editor') return state.editable && state.noteId === expected.noteId && state.maps > 0 && state.images.every(img => !img.visible || img.ready) && (!expected.revealFirstImage || state.images[0]?.ready);
  if (expected.kind === 'index') return state.notes === expected.count && !state.progress.some(text => /正在建立搜索索引/u.test(text));
  if (expected.kind === 'save') return state.save === '已保存到本地';
  if (expected.kind === 'map') return state.expandedNodes === expected.count && state.mapCanEdit;
  if (expected.kind === 'map-collapsed') return state.expandedNodes === 0 && !state.mapCanEdit;
  if (expected.kind === 'transition') return state.transition?.includes(expected.text) && (!expected.saving || state.save === '正在保存…');
  if (expected.kind === 'transition-cancelled') return !state.transition && state.noteId === expected.noteId && state.title === expected.title;
  if (expected.kind === 'search') return state.search?.includes(`共 ${expected.total} 条`) && JSON.stringify([...state.hits].sort()) === JSON.stringify([...expected.ids].sort());
  if (expected.kind === 'title') return state.title === expected.text;
  if (expected.kind === 'dirty-title') return state.title === expected.text && state.save === '立即保存';
  if (expected.kind === 'content-heading') return state.contentHeading === expected.text;
  if (expected.kind === 'content-heading-selection') return Boolean(state.contentHeading) && state.selectedText === state.contentHeading;
  if (expected.kind === 'welcome') return state.text.includes('选择你的笔记库');
  throw new Error('Unknown expected state');
}
async function until(expected, timeoutMs = 240000, timings = {}) {
  const start = performance.now(), deadline = start + timeoutMs;
  let state;
  let revealed = false;
  do {
    state = await read(snapshot);
    const elapsed = performance.now()-start;
    for (const threshold of [1000,2000]) {
      if (elapsed >= threshold && !timings['feedback'+threshold]) {
        timings['feedback'+threshold] = {elapsedMs:elapsed,save:state.save,savingFeedback:state.savingFeedback,progress:state.progress,transition:state.transition};
      }
    }
    if (state.editable && timings.editableMs === undefined) timings.editableMs = performance.now()-start;
    if (expected.revealFirstImage && !revealed && state.editable && state.noteId === expected.noteId) {
      await action({kind:'scroll',selector:'.rich-image-preview'});
      revealed = true;
      timings.firstImageScrolledMs = performance.now()-start;
      continue;
    }
    if (ready(state, expected)) {
      await read(frames);
      state = await read(snapshot);
      if (ready(state, expected)) { timings.readyMs=performance.now()-start; return state; }
    }
    await sleep(5);
  } while (performance.now() < deadline);
  throw new Error(`Expected state timed out: ${JSON.stringify({expected,state})}`);
}
async function click(selector) {
  const box = await read(selector => {
    const el = document.querySelector(selector); if (!el || el.disabled) return null;
    const r = el.getBoundingClientRect();
    const x = r.left+r.width/2, y = r.top+r.height/2;
    return {x,y,width:r.width,height:r.height,
      visible:x>=0&&x<innerWidth&&y>=0&&y<innerHeight&&el.contains(document.elementFromPoint(x,y))};
  }, selector);
  if (!box || !box.visible || !box.width || !box.height) throw new Error(`Click target is unavailable or outside viewport: ${selector}`);
  await call('Input.dispatchMouseEvent', {type:'mousePressed',x:box.x,y:box.y,button:'left',clickCount:1});
  await call('Input.dispatchMouseEvent', {type:'mouseReleased',x:box.x,y:box.y,button:'left',clickCount:1});
}
async function key(key, code, modifiers = 0, text) {
  const virtualKey = code.startsWith('Key') ? code.charCodeAt(3) :
    ({Enter:13,Escape:27,Backspace:8,Delete:46,Home:36,End:35,ArrowLeft:37,ArrowRight:39,Tab:9}[code] ?? 0);
  const event = {key,code,modifiers,windowsVirtualKeyCode:virtualKey,nativeVirtualKeyCode:virtualKey};
  await call('Input.dispatchKeyEvent', {type:text ? 'keyDown':'rawKeyDown',...event,...(text ? {text}: {})});
  await call('Input.dispatchKeyEvent', {type:'keyUp',...event});
}
async function action(value) {
  if (value.kind === 'click') await click(value.selector);
  else if (value.kind === 'request-close') execFileSync(process.env.CUENOTE_PWSH ?? 'pwsh.exe',
    ['-NoProfile','-File',path.join(driverDirectory,'request-close-windows.ps1'),'-Run',run],{windowsHide:true,stdio:'pipe'});
  else if (value.kind === 'key') await key(value.key,value.code,value.modifiers??0,value.text);
  else if (value.kind === 'text') await call('Input.insertText', {text:value.text});
  else if (value.kind === 'wheel') await call('Input.dispatchMouseEvent',{type:'mouseWheel',x:value.x,y:value.y,deltaX:0,deltaY:value.delta});
  else if (value.kind === 'scroll') {
    const {root} = await call('DOM.getDocument');
    const {nodeId} = await call('DOM.querySelector',{nodeId:root.nodeId,selector:value.selector});
    if (!nodeId) throw new Error('Scroll target missing');
    await call('DOM.scrollIntoViewIfNeeded',{nodeId});
    await read(frames);
  }
  else throw new Error('Unknown UI action');
}
function assertOwner() {
  execFileSync(process.env.CUENOTE_PWSH ?? 'pwsh.exe', ['-NoProfile','-File',path.join(driverDirectory,'assert-owner.ps1'),'-Run',run],{windowsHide:true,stdio:'pipe'});
}
async function capture(name) {
  const shot = await call('Page.captureScreenshot', {format:'png'});
  await writeFile(path.join(run,name+'.png'),Buffer.from(shot.data,'base64'),{flag:'wx'});
}
try {
  if (command === 'launch') {
    const [root,exe,expectedSha,portString,mode = 'welcome',noteId] = args;
    if (!['welcome','editor','editor-first-image'].includes(mode)) throw new Error('Unknown launch readiness mode');
    if (process.platform !== 'win32' || !path.isAbsolute(root) || !path.isAbsolute(exe)) throw new Error('Windows and absolute paths required');
    if (await readFile(path.join(root,'.cuenote-performance'),'utf8') !== 'CueNote isolated performance root v1\n') throw new Error('Unmarked test root');
    if (sha(await readFile(exe)) !== expectedSha.toLowerCase()) throw new Error('Executable hash mismatch');
    const port = Number(portString);
    if (!Number.isInteger(port) || port < 9370 || port > 9379) throw new Error('Use a dedicated port 9370..9379');
    const server = net.createServer();
    await new Promise((resolve,reject) => { server.once('error',reject); server.listen(port,'127.0.0.1',resolve); });
    await new Promise(resolve => server.close(resolve));
    run = path.join(root,'measurements',new Date().toISOString().replace(/[:.]/gu,'-'));
    await mkdir(run,{recursive:true});
    const env = {...process.env,CUENOTE_PERF_ROOT:root,WEBVIEW2_USER_DATA_FOLDER:path.join(root,'webview'),WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:`--remote-debugging-port=${port}`};
    for (const name of Object.keys(env)) if (name.startsWith('CUENOTE_FAULT_') || name === 'CUENOTE_TEST_ROOT') delete env[name];
    const start = performance.now();
    const stdout = openSync(path.join(run,'stdout.txt'),'wx');
    const stderr = openSync(path.join(run,'stderr.txt'),'wx');
    const child = spawn(exe,[],{env,windowsHide:true,detached:true,stdio:['ignore',stdout,stderr]});
    closeSync(stdout); closeSync(stderr);
    child.on('exit', code => { childExit=code; });
    child.on('error', error => { childExit=error.message; });
    child.unref();
    record = {root,exe,sha256:expectedSha.toLowerCase(),pid:child.pid,port,run,profile:env.WEBVIEW2_USER_DATA_FOLDER,variant:'perf-acceptance',traceEnabled:env.CUENOTE_PERF_TRACE === '1',startedUtc:new Date().toISOString()};
    await writeFile(path.join(run,'launcher.json'),JSON.stringify(record,null,2),{flag:'wx'});
    await connect();
    const connectedMs = performance.now()-start;
    const readinessTimings = {};
    const state = await until(mode.startsWith('editor') ? {kind:'editor',noteId,revealFirstImage:mode === 'editor-first-image'} : {kind:'welcome'},240000,readinessTimings);
    const elapsedMs = performance.now()-start;
    assertOwner();
    const result = {operation:mode.startsWith('editor')?'startup':'empty-startup',readiness:mode,elapsedMs,connectedMs,readinessTimings,state,webview:await call('Browser.getVersion')};
    await writeFile(path.join(run,'startup.json'),JSON.stringify(result,null,2),{flag:'wx'});
    await capture('startup');
    console.log(JSON.stringify({run,pid:child.pid,elapsedMs}));
  } else {
    run = path.resolve(args[0]);
    record = JSON.parse(await readFile(path.join(run,'launcher.json'),'utf8'));
    if (path.resolve(record.run) !== run) throw new Error('Run record mismatch');
    assertOwner(); await connect();
    if (command === 'inspect') console.log(JSON.stringify(await read(snapshot)));
    else if (command === 'inspect-layout') console.log(JSON.stringify(await read(selector => {
      const element = document.querySelector(selector);
      if (!element) return null;
      const ancestors = [];
      for (let parent = element; parent; parent = parent.parentElement) {
        const rect = parent.getBoundingClientRect();
        ancestors.push({tag:parent.tagName,class:parent.className,top:rect.top,bottom:rect.bottom,
          left:rect.left,right:rect.right,scrollTop:parent.scrollTop,scrollHeight:parent.scrollHeight,
          clientHeight:parent.clientHeight,overflow:getComputedStyle(parent).overflow});
      }
      return {height:innerHeight,width:innerWidth,ancestors};
    },args[1])));
    else if (command === 'capture') await capture(args[1]);
    else if (command === 'actions') {
      const plan = JSON.parse(await readFile(args[1],'utf8'));
      for (const step of plan) {
        if (step.beforeExpected) await until(step.beforeExpected,step.timeoutMs);
        for (const setup of step.setup??[]) await action(setup);
        if (step.setupExpected) {
          const state = await read(snapshot);
          if (!ready(state,step.setupExpected)) throw new Error(`Setup did not produce the intended input: ${JSON.stringify({expected:step.setupExpected,state})}`);
        }
        if (step.profile) {
          if (!/^[a-z0-9-]+$/u.test(step.profile)) throw new Error('Unsafe profile filename');
          await call('Profiler.enable');
          await call('Profiler.start');
        }
        const start = performance.now();
        for (const input of step.actions??[]) await action(input);
        const inputMs = performance.now()-start;
        const readinessTimings = {};
        const state = step.expected ? await until(step.expected,step.timeoutMs,readinessTimings) : await read(snapshot);
        const result = {operation:step.name,metric:step.metric,cacheState:step.cacheState,elapsedMs:performance.now()-start,inputMs,readinessTimings,expected:step.expected,state};
        await appendFile(path.join(run,'measurements.jsonl'),JSON.stringify(result)+'\n');
        console.log(JSON.stringify({operation:result.operation,elapsedMs:result.elapsedMs}));
        if (step.profile) {
          const {profile} = await call('Profiler.stop');
          await writeFile(path.join(run,step.profile+'.cpuprofile'),JSON.stringify(profile),{flag:'wx'});
          await call('Profiler.disable');
        }
        if (step.capture) await capture(step.capture);
      }
    } else throw new Error('Use launch, inspect, capture, or actions');
  }
} catch (error) {
  if (run) {
    await appendFile(path.join(run,'failures.jsonl'),JSON.stringify({utc:new Date().toISOString(),command,message:String(error?.stack ?? error)})+'\n').catch(()=>{});
  }
  throw error;
} finally { socket?.close(); }
