// Fixed read-only DOM observations, matching desktop-windows.mjs.
// Compiled into the isolated Mac performance build only; no external scripts.
if (request.paint) await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
if (visualViewport && (visualViewport.scale !== 1 || visualViewport.offsetLeft !== 0 || visualViewport.offsetTop !== 0)) {
  throw new Error('Pinch zoom and visual viewport offsets are unsupported');
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
if (request.operation === 'snapshot') return JSON.stringify(snapshot());
if (request.operation !== 'element') throw new Error('Unsupported observation');
const el = document.querySelector(request.selector);
if (!el) return JSON.stringify({missing:true});
const rect = element => {
  const r=element.getBoundingClientRect();
  return {x:r.x,y:r.y,width:r.width,height:r.height,top:r.top,bottom:r.bottom,left:r.left,right:r.right};
};
const r=rect(el), x=r.x+r.width/2, y=r.y+r.height/2;
const containers=[];
for(let parent=el.parentElement; parent; parent=parent.parentElement) {
  const style=getComputedStyle(parent);
  if(/auto|scroll/.test(style.overflowY) && parent.scrollHeight>parent.clientHeight) {
    containers.push({...rect(parent),scrollTop:parent.scrollTop,scrollHeight:parent.scrollHeight,clientHeight:parent.clientHeight});
  }
}
return JSON.stringify({rect:r,containers,viewport:{width:innerWidth,height:innerHeight,devicePixelRatio},
  visible:!el.disabled && r.width>0 && r.height>0 && x>=0 && y>=0 && x<innerWidth && y<innerHeight && el.contains(document.elementFromPoint(x,y)),
  focused:document.activeElement===el,selection:el.selectionStart===undefined?null:{start:el.selectionStart,end:el.selectionEnd},
  text:el.textContent?.slice(0,1024)});