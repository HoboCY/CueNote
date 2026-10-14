import { Node, mergeAttributes, type Editor, type Extensions, type NodeViewRendererProps } from '@tiptap/core';
import { closeHistory } from '@tiptap/pm/history';
import { Fragment, type Node as ProseMirrorNode } from '@tiptap/pm/model';
import { NodeSelection, Plugin, type Transaction } from '@tiptap/pm/state';
import type { EditorView } from '@tiptap/pm/view';
import MindElixir, { RIGHT, type MindElixirInstance, type NodeObj, type Topic } from 'mind-elixir';
import type { Block } from './generated/note-format.ts';
import { fromEditorDocument } from './editor-model.ts';
import { assertMindMapCollection, readMindMap, type MindMapBlock } from './mindmap-model.ts';
import { createMindMapTextInputQueue, type MindMapEditorBridge, type MindMapTextInputQueue } from './mindmap-session.ts';
import {
  applyMindMapMutation,
  createMindMap,
  createUuidV4,
  fromMindElixirData,
  toMindElixirData,
  type MindMapMutation,
  type NewMindMapNode,
} from './mindmap-tree.ts';

const maximumPreflightRetries = 24;
const mapRefreshers = new WeakMap<Editor, Set<() => void>>();
const mapFlushers = new WeakMap<Editor, Set<() => Promise<void>>>();
const expandedViews = new WeakMap<Editor, Set<(expanded: boolean) => void>>();
const destroyedEditors = new WeakSet<Editor>();
const observedEditors = new WeakSet<Editor>();

export interface MindMapEditorRuntime {
  getBridge(): MindMapEditorBridge | undefined;
  canEdit(): boolean;
  isCurrent(editor: Editor): boolean;
  blocksFor(editor: Editor, replacement: MindMapBlock): Block[];
  overlayBlocks(blocks: Block[]): Block[];
  publishMap(editor: Editor, map: MindMapBlock): void;
  clearLiveMap(mapId: string): void;
  setExpanded(editor: Editor, mapId: string, expanded: boolean): void;
  reportError(message: string): void;
  reportStatus(message: string): void;
}

declare module '@tiptap/core' {
  interface Commands<ReturnType> {
    cuenoteMindMap: {
      /** Insert a new editable map after the current top-level content block. */
      insertMindMap: () => ReturnType;
    };
  }
}

function registerRefresher(editor: Editor, refresh: () => void) {
  const refreshers = mapRefreshers.get(editor) ?? new Set<() => void>();
  refreshers.add(refresh);
  mapRefreshers.set(editor, refreshers);
  return () => {
    refreshers.delete(refresh);
    if (refreshers.size === 0) mapRefreshers.delete(editor);
  };
}

function registerFlusher(editor: Editor, flush: () => Promise<void>) {
  const flushers = mapFlushers.get(editor) ?? new Set<() => Promise<void>>();
  flushers.add(flush);
  mapFlushers.set(editor, flushers);
  return () => {
    flushers.delete(flush);
    if (flushers.size === 0) mapFlushers.delete(editor);
  };
}

export function refreshMindMapNodeViews(editor: Editor) {
  for (const refresh of mapRefreshers.get(editor) ?? []) refresh();
}

export async function settleMindMapNodeViews(editor: Editor) {
  const flushers = [...(mapFlushers.get(editor) ?? [])];
  await Promise.all(flushers.map(flush => flush()));
}

export function hasExpandedMindMap(editor: Editor) {
  return (expandedViews.get(editor)?.size ?? 0) > 0;
}

function observeEditor(editor: Editor) {
  if (observedEditors.has(editor)) return;
  observedEditors.add(editor);
  editor.on('destroy', () => destroyedEditors.add(editor));
}

function editorIsCurrent(runtime: MindMapEditorRuntime | undefined, editor: Editor, view?: EditorView) {
  observeEditor(editor);
  return !destroyedEditors.has(editor) && !view?.isDestroyed && (runtime?.isCurrent(editor) ?? true);
}

function bridgeFor(runtime: MindMapEditorRuntime | undefined, fallback?: MindMapEditorBridge) {
  return runtime?.getBridge() ?? fallback;
}

function canEditMap(runtime: MindMapEditorRuntime | undefined, editor: Editor, fallbackBridge?: MindMapEditorBridge) {
  return (runtime?.canEdit() ?? editor.isEditable) && Boolean(bridgeFor(runtime, fallbackBridge));
}

function report(runtime: MindMapEditorRuntime | undefined, cause: unknown) {
  runtime?.reportError(errorText(cause));
}

function errorText(cause: unknown) {
  if (cause instanceof Error) return cause.message;
  if (cause && typeof cause === 'object' && 'message' in cause
    && typeof cause.message === 'string') return cause.message;
  return String(cause);
}

function withMap(blocks: Block[], map: MindMapBlock): Block[] {
  let replaced = false;
  const next = blocks.map(block => {
    if (block.type !== 'mindMap' || block.mapId !== map.mapId) return block;
    replaced = true;
    return map;
  });
  if (!replaced) throw new Error('导图已从当前正文移除。');
  assertMindMapCollection(next);
  return next;
}

function track(bridge: MindMapEditorBridge, operation: Promise<unknown>) {
  try {
    bridge.track(operation.then(() => undefined, () => undefined));
  } catch {
    // The editor still owns the operation; a bridge bookkeeping error must not
    // leave an unhandled rejection or mutate a map after its node view vanished.
  }
}

function topLevelInsertion(editor: Editor): { position: number; map: MindMapBlock } {
  const doc = editor.state.doc;
  const selection = editor.state.selection;
  if (selection instanceof NodeSelection && selection.node.type.name !== 'text') {
    let offset = 0;
    let result: { position: number; map: MindMapBlock } | null = null;
    doc.forEach((node, position) => {
      if (position === selection.from) result = { position: position + node.nodeSize, map: createMindMap() };
      offset = position + node.nodeSize;
    });
    if (result) return result;
    return { position: offset, map: createMindMap() };
  }

  const cursor = selection.$head.pos;
  let insertion = doc.content.size;
  doc.forEach((node, position) => {
    if (cursor >= position && cursor <= position + node.nodeSize) insertion = position + node.nodeSize;
  });
  return { position: insertion, map: createMindMap() };
}

function makeMapInsertion(editor: Editor, position: number, map: MindMapBlock): Transaction {
  const node = editor.schema.nodes.mindMap?.create({ mapId: map.mapId, rootId: map.rootId, nodes: map.nodes });
  if (!node) throw new Error('当前内容区不支持插入思维导图。');
  const transaction = editor.state.tr.insert(position, node);
  if (transaction.doc.lastChild?.type === node.type) {
    const paragraph = editor.schema.nodes.paragraph?.createAndFill();
    if (!paragraph) throw new Error('无法创建导图后的编辑段落。');
    transaction.insert(transaction.doc.content.size, paragraph);
  }
  transaction.setSelection(NodeSelection.create(transaction.doc, position));
  return transaction;
}

async function insertMap(
  editor: Editor,
  runtime: MindMapEditorRuntime | undefined,
  fallbackBridge: MindMapEditorBridge | undefined,
) {
  if (!editor.isEditable || !canEditMap(runtime, editor, fallbackBridge)) return false;
  const bridge = bridgeFor(runtime, fallbackBridge);
  if (!bridge) return false;
  try {
    if (!bridge.canStart()) {
      runtime?.reportError('笔记正在切换或关闭，暂时不能开始新的导图操作。');
      return false;
    }
  } catch (cause) {
    report(runtime, cause);
    return false;
  }

  const operation = (async () => {
    for (let attempt = 0; attempt < maximumPreflightRetries; attempt += 1) {
      if (!editorIsCurrent(runtime, editor)) return false;
      const basisDoc = editor.state.doc;
      const { position, map } = topLevelInsertion(editor);
      const transaction = makeMapInsertion(editor, position, map);
      const candidate = runtime?.overlayBlocks(fromEditorDocument(transaction.doc.toJSON(), false))
        ?? fromEditorDocument(transaction.doc.toJSON(), false);
      const commitIfCurrent = await bridge.preflight(candidate);
      if (!editorIsCurrent(runtime, editor)) return false;
      if (editor.state.doc !== basisDoc) {
        await new Promise<void>(resolve => setTimeout(resolve, 0));
        continue;
      }
      if (!commitIfCurrent()) continue;
      const view = editor.view;
      if (!view || view.isDestroyed) return false;
      view.dispatch(closeHistory(view.state.tr));
      view.dispatch(transaction);
      view.dispatch(closeHistory(view.state.tr));
      runtime?.reportStatus('已在当前顶层内容块之后插入思维导图。');
      return true;
    }
    throw new Error('导图插入期间正文持续变化，请稍后重试。');
  })().catch(cause => {
    report(runtime, cause);
    return false;
  });
  track(bridge, operation);
  return await operation;
}

function nodeIndexAt(doc: ProseMirrorNode, position: number) {
  let index = -1;
  let foundPosition = -1;
  doc.forEach((_node, offset, childIndex) => {
    if (offset === position) {
      index = childIndex;
      foundPosition = offset;
    }
  });
  return index < 0 ? null : { index, position: foundPosition };
}

function moveTopLevel(editor: Editor, position: number, direction: 'up' | 'down') {
  const doc = editor.state.doc;
  const current = nodeIndexAt(doc, position);
  if (!current) return false;
  const targetIndex = current.index + (direction === 'up' ? -1 : 1);
  if (targetIndex < 0 || targetIndex >= doc.childCount) return false;
  const node = doc.child(current.index);
  const siblings = Array.from({ length: doc.childCount }, (_, index) => doc.child(index));
  [siblings[current.index], siblings[targetIndex]] = [siblings[targetIndex]!, siblings[current.index]!];
  const transaction = editor.state.tr.replaceWith(0, doc.content.size, Fragment.fromArray(siblings));
  if (transaction.doc.lastChild?.type.name === 'mindMap') {
    const paragraph = editor.schema.nodes.paragraph?.createAndFill();
    if (!paragraph) return false;
    transaction.insert(transaction.doc.content.size, paragraph);
  }
  const newPosition = siblings.slice(0, targetIndex).reduce((sum, sibling) => sum + sibling.nodeSize, 0);
  const mapPosition = Math.max(0, Math.min(newPosition, transaction.doc.content.size - node.nodeSize));
  if (transaction.doc.nodeAt(mapPosition)?.type.name === 'mindMap') {
    transaction.setSelection(NodeSelection.create(transaction.doc, mapPosition));
  }
  editor.view.dispatch(closeHistory(editor.state.tr));
  editor.view.dispatch(transaction);
  editor.view.dispatch(closeHistory(editor.state.tr));
  return true;
}

function removeTopLevel(editor: Editor, position: number, node: ProseMirrorNode) {
  const doc = editor.state.doc;
  const transaction = editor.state.tr;
  const replacement = doc.childCount === 1 ? editor.schema.nodes.paragraph?.createAndFill() : null;
  if (replacement) transaction.replaceWith(position, position + node.nodeSize, replacement);
  else transaction.delete(position, position + node.nodeSize);
  editor.view.dispatch(closeHistory(editor.state.tr));
  editor.view.dispatch(transaction);
  editor.view.dispatch(closeHistory(editor.state.tr));
  editor.view.focus();
  return true;
}

function appendPreview(preview: HTMLElement, map: MindMapBlock) {
  preview.replaceChildren();
  const list = preview.ownerDocument.createElement('ul');
  list.className = 'mind-map-preview-tree';
  const items = new Map<string, HTMLLIElement>();
  for (const node of map.nodes) {
    const item = preview.ownerDocument.createElement('li');
    const label = preview.ownerDocument.createElement('span');
    label.className = 'mind-map-preview-text';
    label.textContent = node.text;
    item.append(label);
    items.set(node.id, item);
  }
  for (const node of map.nodes) {
    const item = items.get(node.id)!;
    if (node.id === map.rootId) list.append(item);
    if (node.children.length === 0) continue;
    const children = preview.ownerDocument.createElement('ul');
    for (const id of node.children) {
      const child = items.get(id);
      if (child) children.append(child);
    }
    item.append(children);
  }
  preview.append(list);
}

function nodeById(map: MindMapBlock, id: string | null) {
  return id ? map.nodes.find(node => node.id === id) ?? null : null;
}

function nodeId(topic: Topic | undefined) {
  return topic?.nodeObj?.id ?? null;
}

function topicFor(instance: MindElixirInstance | null, id: string | null): Topic | null {
  if (!instance || !id) return null;
  try {
    return instance.findEle(id);
  } catch {
    return null;
  }
}

function createNodeView(
  { editor, node, getPos, view }: NodeViewRendererProps,
  fallbackBridge: MindMapEditorBridge | undefined,
  runtime: MindMapEditorRuntime | undefined,
) {
  const owner = view.dom.ownerDocument;
  const root = owner.createElement('figure');
  root.className = 'mind-map-node';
  root.setAttribute('data-note-mind-map', '');
  root.setAttribute('contenteditable', 'false');
  root.draggable = false;

  const preview = owner.createElement('div');
  preview.className = 'mind-map-preview';
  preview.setAttribute('role', 'group');
  preview.setAttribute('aria-label', '思维导图预览');
  root.append(preview);

  const controls = owner.createElement('div');
  controls.className = 'mind-map-block-controls';
  controls.setAttribute('role', 'group');
  controls.setAttribute('aria-label', '思维导图操作');
  const makeButton = (testId: string, label: string, parent: HTMLElement) => {
    const button = owner.createElement('button');
    button.type = 'button';
    button.className = 'mind-map-action';
    button.setAttribute('data-testid', testId);
    button.setAttribute('aria-label', label);
    button.textContent = label;
    parent.append(button);
    return button;
  };
  const open = makeButton('mind-map-open', '展开编辑', controls);
  const moveUp = makeButton('mind-map-move-up', '上移', controls);
  const moveDown = makeButton('mind-map-move-down', '下移', controls);
  const remove = makeButton('mind-map-delete', '删除导图', controls);
  root.append(controls);

  const panel = owner.createElement('section');
  panel.className = 'mind-map-editor-panel';
  panel.hidden = true;
  panel.setAttribute('aria-label', '思维导图编辑器');
  const toolbar = owner.createElement('div');
  toolbar.className = 'mind-map-toolbar';
  toolbar.setAttribute('role', 'toolbar');
  toolbar.setAttribute('aria-label', '导图节点操作');
  const edit = makeButton('mind-map-edit-text', '编辑节点文字', toolbar);
  const addChild = makeButton('mind-map-add-child', '添加子节点', toolbar);
  const addSibling = makeButton('mind-map-add-sibling', '添加同级节点', toolbar);
  const indent = makeButton('mind-map-indent', '降为子节点', toolbar);
  const outdent = makeButton('mind-map-outdent', '提升层级', toolbar);
  const removeNode = makeButton('mind-map-remove-node', '删除节点', toolbar);
  const undo = makeButton('mind-map-undo', '撤销导图操作', toolbar);
  const redo = makeButton('mind-map-redo', '重做导图操作', toolbar);
  const collapse = makeButton('mind-map-collapse', '收起导图', toolbar);
  panel.append(toolbar);

  const canvas = owner.createElement('div');
  canvas.className = 'mind-map-canvas';
  canvas.setAttribute('data-testid', 'mind-map-canvas');
  panel.append(canvas);

  const textEditor = owner.createElement('div');
  textEditor.className = 'mind-map-text-editor';
  textEditor.hidden = true;
  const textInput = owner.createElement('textarea');
  textInput.className = 'mind-map-text-input';
  textInput.rows = 3;
  textInput.setAttribute('data-testid', 'mind-map-text-input');
  textInput.setAttribute('aria-label', '导图节点文字');
  textInput.spellcheck = true;
  const finishText = makeButton('mind-map-finish-text', '完成文字编辑', textEditor);
  textEditor.insertBefore(textInput, finishText);
  panel.append(textEditor);

  const error = owner.createElement('figcaption');
  error.className = 'mind-map-error';
  error.setAttribute('role', 'alert');
  error.setAttribute('data-testid', 'mind-map-error');
  error.hidden = true;
  root.append(panel, error);

  let currentNode = node;
  let currentMap: MindMapBlock;
  let selectedId: string | null = null;
  let editingId: string | null = null;
  let instance: MindElixirInstance | null = null;
  let destroyed = false;
  let expanded = false;
  let textInputQueue: MindMapTextInputQueue;
  let pendingTextRevision = 0;
  let acceptedTextRevision = 0;
  let inputWorker: Promise<void> | null = null;
  let inputFailure: Error | null = null;
  let focusingTextAction = false;
  let lastCommittedMap = '';
  let unregisterFlusher: () => void = () => undefined;
  let unregisterBridge: () => void = () => undefined;
  let editingBaseline: MindMapBlock | null = null;
  let pendingApply: { promise: Promise<void>; resolve: () => void; timer: number } | null = null;
  let historyTask: Promise<void> | null = null;
  const undoStack: MindMapBlock[] = [];
  const redoStack: MindMapBlock[] = [];
  const activeBridge = () => bridgeFor(runtime, fallbackBridge);
  const editorCurrent = () => !destroyed && editorIsCurrent(runtime, editor);
  const canStartMapOperation = () => {
    try { return activeBridge()?.canStart() ?? false; }
    catch { return false; }
  };

  try {
    currentMap = readMindMap({ type: 'mindMap', ...node.attrs });
  } catch (cause) {
    currentMap = readMindMap({
      type: 'mindMap',
      mapId: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
      rootId: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
      nodes: [{ id: 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb', text: '导图数据无效', children: [] }],
    });
    error.textContent = errorText(cause);
    error.hidden = false;
  }
  lastCommittedMap = JSON.stringify(currentMap);

  const position = () => {
    try {
      const value = getPos();
      return typeof value === 'number' && Number.isInteger(value) ? value : null;
    } catch {
      return null;
    }
  };

  const currentTopLevelNode = () => {
    const pos = position();
    if (pos === null) return null;
    const doc = editor.state.doc;
    const resolved = doc.resolve(pos);
    if (resolved.depth !== 0) return null;
    const index = resolved.index();
    if (index >= doc.childCount) return null;
    const actual = doc.child(index);
    return actual.type.name === 'mindMap' ? { node: actual, index, position: pos } : null;
  };

  const setError = (message: string | null) => {
    error.textContent = message ?? '';
    error.hidden = message === null;
  };

  const cloneMap = (map: MindMapBlock): MindMapBlock => ({
    type: 'mindMap',
    mapId: map.mapId,
    rootId: map.rootId,
    nodes: map.nodes.map(item => ({ id: item.id, text: item.text, children: [...item.children] })),
  });

  const finishPendingApply = () => {
    if (!pendingApply) return;
    owner.defaultView?.clearTimeout(pendingApply.timer);
    const complete = pendingApply.resolve;
    pendingApply = null;
    complete();
    refreshControls();
  };

  const setExpanded = (value: boolean) => {
    if (expanded === value) return;
    const views = expandedViews.get(editor) ?? new Set<(expanded: boolean) => void>();
    if (value) {
      views.add(setExpanded);
      expandedViews.set(editor, views);
      unregisterFlusher = registerFlusher(editor, settle);
      unregisterBridge = activeBridge()?.registerFlush(settle) ?? (() => undefined);
      runtime?.setExpanded(editor, currentMap.mapId, true);
    } else {
      views.delete(setExpanded);
      if (views.size === 0) expandedViews.delete(editor);
      unregisterFlusher();
      unregisterBridge();
      unregisterFlusher = () => undefined;
      unregisterBridge = () => undefined;
      runtime?.setExpanded(editor, currentMap.mapId, false);
    }
    expanded = value;
    panel.hidden = !value;
    open.textContent = value ? '导图已展开' : '展开编辑';
    open.disabled = value || !canEditMap(runtime, editor, fallbackBridge);
    if (!value) {
      textEditor.hidden = true;
      editingId = null;
      controls.hidden = false;
      destroyInstance();
      refreshControls();
      if (!destroyed) open.focus();
      return;
    }
    controls.hidden = true;
    if (!instance) initializeMindElixir();
    refreshControls();
  };

  const commitMapToEditor = () => {
    if (JSON.stringify(currentMap) === lastCommittedMap) {
      runtime?.clearLiveMap(currentMap.mapId);
      return;
    }
    const found = currentTopLevelNode();
    if (!found) throw new Error('导图已从当前正文移除。');
    const replacement = editor.schema.nodes.mindMap.create({
      mapId: currentMap.mapId,
      rootId: currentMap.rootId,
      nodes: currentMap.nodes,
    });
    const view = editor.view;
    if (!view || view.isDestroyed) throw new Error('当前编辑器已关闭。');
    view.dispatch(closeHistory(view.state.tr));
    view.dispatch(view.state.tr.setNodeMarkup(found.position, replacement.type, replacement.attrs));
    view.dispatch(closeHistory(view.state.tr));
    lastCommittedMap = JSON.stringify(currentMap);
    runtime?.clearLiveMap(currentMap.mapId);
  };

  async function finishTextEdit() {
    while (editingId) {
      if (textInputQueue.isComposing()) {
        await textInputQueue.settle();
        continue;
      }
      const worker = inputWorker;
      if (worker) {
        await worker;
        continue;
      }
      if (inputFailure) throw inputFailure;

      const id = editingId;
      const accepted = nodeById(currentMap, id);
      if (!accepted) throw new Error('正在编辑的导图节点已不存在。');
      if (textInput.value !== accepted.text) {
        queueTextValidation(true);
        if (inputWorker) await inputWorker;
        continue;
      }

      if (instance && accepted.text !== (instance.getObjById(id, instance.nodeData)?.topic ?? '')) {
        const topic = topicFor(instance, id);
        if (!topic) throw new Error('正在编辑的导图节点已不可见。');
        await instance.setNodeTopic(topic, accepted.text);
        continue;
      }

      if (textInputQueue.isComposing() || inputWorker || textInput.value !== nodeById(currentMap, id)?.text) continue;
      if (inputFailure) throw inputFailure;
      if (editingBaseline && JSON.stringify(editingBaseline) !== JSON.stringify(currentMap)) {
        undoStack.push(editingBaseline);
        redoStack.length = 0;
      }
      editingBaseline = null;
      textEditor.hidden = true;
      editingId = null;
      instance?.enableEdit();
      refreshControls();
    }
  }

  async function validateTextInput(force = false) {
    if (!editingId || !editorCurrent()) return;
    const bridge = activeBridge();
    if (!bridge) throw new Error('导图服务当前不可用。');
    const requested = pendingTextRevision;
    while (acceptedTextRevision < requested) {
      if (!editorCurrent()) return;
      const revision = pendingTextRevision;
      const nodeId = editingId;
      const rawText = textInput.value;
      const candidate = applyMindMapMutation(currentMap, { type: 'text', nodeId, text: rawText });
      const candidateBlocks = runtime?.blocksFor(editor, candidate) ?? withMap(fromEditorDocument(editor.getJSON(), false), candidate);
      try {
        const commitIfCurrent = await bridge.preflight(candidateBlocks);
        if (!editorCurrent()) return;
        if (revision !== pendingTextRevision) continue;
        if (!commitIfCurrent()) continue;
        currentMap = candidate;
        acceptedTextRevision = revision;
        inputFailure = null;
        appendPreview(preview, currentMap);
        setError(null);
        if (!textInputQueue.isComposing()) runtime?.publishMap(editor, currentMap);
      } catch (cause) {
        if (revision !== pendingTextRevision) continue;
        const message = errorText(cause);
        setError(message);
        if (/5[,.]?000|五千|节点合计/.test(message)) {
          const accepted = nodeById(currentMap, nodeId);
          if (accepted) textInput.value = accepted.text;
          acceptedTextRevision = revision;
          inputFailure = null;
          return;
        }
        inputFailure = cause instanceof Error ? cause : new Error(message);
        throw inputFailure;
      }
    }
    if (!textInputQueue.isComposing() && acceptedTextRevision === pendingTextRevision) {
      runtime?.publishMap(editor, currentMap);
    }
    void force;
  }

  function queueTextValidation(force = false) {
    pendingTextRevision += 1;
    inputFailure = null;
    if (inputWorker) return inputWorker;
    const bridge = activeBridge();
    if (!force) {
      try {
        if (!canEditMap(runtime, editor, fallbackBridge) || !bridge?.canStart()) {
          const accepted = nodeById(currentMap, editingId);
          if (accepted) textInput.value = accepted.text;
          pendingTextRevision = acceptedTextRevision;
          setError('笔记正在切换或只读，不能开始新的导图操作。');
          return Promise.resolve();
        }
      } catch (cause) {
        report(runtime, cause);
        return Promise.resolve();
      }
    }
    const operation = validateTextInput(force).catch(cause => {
      inputFailure = cause instanceof Error ? cause : new Error(errorText(cause));
      setError(inputFailure.message);
    }).finally(() => {
      inputWorker = null;
      refreshControls();
    });
    inputWorker = operation;
    if (bridge) track(bridge, operation);
    return operation;
  }

  textInputQueue = createMindMapTextInputQueue(
    force => queueTextValidation(force),
    callback => {
      const view = owner.defaultView;
      if (view) view.setTimeout(callback, 0);
      else setTimeout(callback, 0);
    },
    () => {
      if (editorCurrent() && editingId && !inputFailure && !textInputQueue.isComposing()
        && acceptedTextRevision === pendingTextRevision) runtime?.publishMap(editor, currentMap);
    },
  );

  const onTextInput = () => { void textInputQueue.input(); };
  textInput.addEventListener('input', onTextInput);
  textInput.addEventListener('compositionstart', () => textInputQueue.compositionStart());
  textInput.addEventListener('compositionend', () => { void textInputQueue.compositionEnd(); });
  textInput.addEventListener('keydown', event => {
    if ((event.ctrlKey || event.metaKey) && ['z', 'y'].includes(event.key.toLowerCase())) event.stopPropagation();
  });
  const onPanelKeyDown = (event: KeyboardEvent) => {
    if (!(event.ctrlKey || event.metaKey)) return;
    const key = event.key.toLowerCase();
    if (!['z', 'y'].includes(key)) return;
    if (event.target === textInput) {
      event.stopPropagation();
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (key === 'y' || event.shiftKey) void applyHistory('redo');
    else void applyHistory('undo');
  };
  panel.addEventListener('keydown', onPanelKeyDown);
  const onCollapsedHistoryKeyDown = (event: KeyboardEvent) => {
    if (expanded || !editor.isEditable || event.target !== open || !(event.ctrlKey || event.metaKey)) return;
    const key = event.key.toLowerCase();
    if (!['z', 'y'].includes(key)) return;
    event.preventDefault();
    event.stopPropagation();
    if (key === 'y' || event.shiftKey) editor.commands.redo();
    else editor.commands.undo();
  };
  open.addEventListener('keydown', onCollapsedHistoryKeyDown);

  function startTextEdit(topic: Topic) {
    if (!canEditMap(runtime, editor, fallbackBridge)) return;
    const id = nodeId(topic);
    const nodeData = nodeById(currentMap, id);
    if (!id || !nodeData) return;
    selectedId = id;
    editingId = id;
    editingBaseline = cloneMap(currentMap);
    textInput.value = nodeData.text;
    textEditor.hidden = false;
    setError(null);
    instance?.disableEdit();
    textInput.focus();
    refreshControls();
  }

  const operationMutation = (name: string, args: unknown[]): MindMapMutation | null => {
    const topicObject = (value: unknown) => {
      if (!value || typeof value !== 'object' || !('nodeObj' in value)) return null;
      return (value as Topic).nodeObj;
    };
    const selected = (value: unknown): NodeObj[] => Array.isArray(value)
      ? value.map(topicObject).filter((node): node is NodeObj => Boolean(node))
      : [];
    const newNode = (value: unknown): NewMindMapNode | null => {
      if (!value || typeof value !== 'object') return null;
      const data = value as NodeObj;
      if (typeof data.id !== 'string' || typeof data.topic !== 'string'
        || (data.children && data.children.length > 0)
        || Object.keys(data).some(key => !['id', 'topic', 'children', 'direction', 'expanded'].includes(key))) return null;
      return { id: data.id, text: data.topic };
    };
    if (name === 'addChild') {
      const parentId = topicObject(args[0])?.id ?? selectedId ?? currentMap.rootId;
      const node = newNode(args[1]);
      return node ? { type: 'addChild', parentId, node } : null;
    }
    if (name === 'insertSibling') {
      const id = topicObject(args[1])?.id ?? selectedId;
      const node = newNode(args[2]);
      return id && node ? { type: 'insertSibling', targetId: id, side: args[0] === 'before' ? 'before' : 'after', node } : null;
    }
    if (name === 'insertParent') {
      const id = topicObject(args[0])?.id ?? selectedId;
      const node = newNode(args[1]);
      return id && node ? { type: 'insertParent', targetId: id, node } : null;
    }
    if (name === 'reshapeNode') {
      const id = topicObject(args[0])?.id ?? null;
      const patch = args[1] && typeof args[1] === 'object' ? args[1] as Record<string, unknown> : {};
      if (!id || Object.keys(patch).some(key => key !== 'topic') || typeof patch.topic !== 'string') return null;
      return { type: 'text', nodeId: id, text: patch.topic };
    }
    if (name === 'removeNodes') {
      const ids = selected(args[0]).map(item => item.id);
      return ids.length > 0 ? { type: 'remove', nodeIds: ids } : null;
    }
    if (name === 'moveNodeIn' || name === 'moveNodeBefore' || name === 'moveNodeAfter') {
      const ids = selected(args[0]).map(item => item.id);
      const targetId = topicObject(args[1])?.id ?? null;
      const placement = name === 'moveNodeIn' ? 'in' : name === 'moveNodeBefore' ? 'before' : 'after';
      return ids.length > 0 && targetId ? { type: 'move', nodeIds: ids, targetId, placement } : null;
    }
    if (name === 'moveUpNode' || name === 'moveDownNode') {
      const id = topicObject(args[0])?.id ?? selectedId;
      if (!id) return null;
      const node = currentMap.nodes.find(item => item.id === id);
      const parent = node && currentMap.nodes.find(item => item.children.includes(id));
      if (!node || !parent) return null;
      const index = parent.children.indexOf(id);
      const targetId = parent.children[index + (name === 'moveUpNode' ? -1 : 1)];
      if (!targetId) return null;
      return { type: 'move', nodeIds: [id], targetId, placement: name === 'moveUpNode' ? 'before' : 'after' };
    }
    return null;
  };

  const preflightOperation = async (name: string, args: unknown[]) => {
    const bridge = activeBridge();
    if (!bridge || !canEditMap(runtime, editor, fallbackBridge)) return false;
    if (pendingApply) return false;
    try {
      if (!bridge.canStart()) {
        setError('笔记正在切换或只读，不能开始新的导图操作。');
        return false;
      }
    } catch (cause) {
      report(runtime, cause);
      return false;
    }
    const operation = (async () => {
      for (let attempt = 0; attempt < maximumPreflightRetries; attempt += 1) {
        if (!editorCurrent()) return false;
        const mutation = operationMutation(name, args);
        if (!mutation) return false;
        const candidate = applyMindMapMutation(currentMap, mutation);
        const blocks = runtime?.blocksFor(editor, candidate)
          ?? withMap(fromEditorDocument(editor.getJSON(), false), candidate);
        const commitIfCurrent = await bridge.preflight(blocks);
        if (!editorCurrent()) return false;
        if (!commitIfCurrent()) continue;
        if (pendingApply) return false;
        let resolve!: () => void;
        const promise = new Promise<void>(complete => { resolve = complete; });
        const timer = owner.defaultView?.setTimeout(() => {
          if (pendingApply?.promise === promise) finishPendingApply();
        }, 5_000) ?? 0;
        pendingApply = { promise, resolve, timer };
        track(bridge, promise);
        refreshControls();
        return true;
      }
      throw new Error('导图内容持续变化，操作没有应用，请稍后重试。');
    })().catch(cause => {
      setError(errorText(cause));
      report(runtime, cause);
      return false;
    });
    track(bridge, operation);
    return await operation;
  };

  const refreshControls = () => {
    const editable = canEditMap(runtime, editor, fallbackBridge);
    const bodyEditable = editable && editor.isEditable;
    const mapBusy = Boolean(pendingApply || editingId);
    const top = currentTopLevelNode();
    controls.hidden = expanded;
    open.disabled = expanded || !editable;
    moveUp.disabled = !bodyEditable || !top || top.index === 0;
    moveDown.disabled = !bodyEditable || !top || top.index >= editor.state.doc.childCount - 1;
    remove.disabled = !bodyEditable || !top;
    for (const button of [edit, addChild, addSibling, indent, outdent, removeNode]) button.disabled = !editable || mapBusy;
    undo.disabled = !editable || Boolean(pendingApply) || undoStack.length === 0;
    redo.disabled = !editable || Boolean(pendingApply) || redoStack.length === 0;
    collapse.disabled = !expanded;
    textInput.disabled = !editable;
    finishText.disabled = !editable;
    if (instance && !editable) instance.disableEdit();
    else if (instance && editable && !editingId) instance.enableEdit();
  };

  const selectedTopic = () => topicFor(instance, selectedId);

  const runSelectedMutation = async (name: string, makeArgs: (topic: Topic) => unknown[], fallbackTopic?: Topic) => {
    const topic = selectedTopic() ?? fallbackTopic;
    if (!instance || !topic) return;
    const args = makeArgs(topic);
    const api = instance as unknown as Record<string, (...args: unknown[]) => Promise<void>>;
    try {
      await api[name](...args);
    } catch (cause) {
      report(runtime, cause);
    }
  };

  const openSelectedText = () => {
    const topic = selectedTopic();
    if (topic) startTextEdit(topic);
  };
  edit.addEventListener('click', openSelectedText);
  addChild.addEventListener('click', () => {
    const target = selectedTopic() ?? topicFor(instance, currentMap.rootId);
    if (!instance || !target) return;
    const child: NodeObj = { id: createUuidV4(), topic: '新节点', children: [] };
    void runSelectedMutation('addChild', () => [target, child], target);
  });
  addSibling.addEventListener('click', () => {
    const target = selectedTopic();
    if (!target || !instance) return;
    const sibling: NodeObj = { id: createUuidV4(), topic: '新节点', children: [] };
    void runSelectedMutation('insertSibling', () => ['after', target, sibling]);
  });
  indent.addEventListener('click', () => {
    const target = selectedTopic();
    if (!target || !instance || !target.nodeObj.parent) return;
    const siblings = target.nodeObj.parent.children ?? [];
    const index = siblings.indexOf(target.nodeObj);
    const parentSibling = siblings[index - 1];
    const parentTopic = parentSibling ? topicFor(instance, parentSibling.id) : null;
    if (parentTopic) void runSelectedMutation('moveNodeIn', () => [[target], parentTopic]);
  });
  outdent.addEventListener('click', () => {
    const target = selectedTopic();
    const parent = target?.nodeObj.parent;
    if (!target || !parent?.parent || !instance) return;
    const parentTopic = topicFor(instance, parent.id);
    if (parentTopic) void runSelectedMutation('moveNodeAfter', () => [[target], parentTopic]);
  });
  removeNode.addEventListener('click', () => {
    const target = selectedTopic();
    if (target) void runSelectedMutation('removeNodes', () => [[target]]);
  });
  const focusTextAction = (button: HTMLButtonElement) => {
    focusingTextAction = true;
    try {
      button.focus({ preventScroll: true });
    } finally {
      focusingTextAction = false;
    }
  };
  undo.addEventListener('click', () => {
    focusTextAction(undo);
    void applyHistory('undo');
  });
  redo.addEventListener('click', () => {
    focusTextAction(redo);
    void applyHistory('redo');
  });
  collapse.addEventListener('click', () => {
    focusTextAction(collapse);
    void settle().then(() => {
      commitMapToEditor();
      setExpanded(false);
      runtime?.reportStatus('思维导图已保存到正文。');
    }).catch(cause => setError(errorText(cause)));
  });
  finishText.addEventListener('click', () => {
    void finishTextEdit().catch(cause => setError(errorText(cause)));
  });
  const openMap = () => {
    if (!canEditMap(runtime, editor, fallbackBridge)) return;
    if (!canStartMapOperation()) return;
    setError(null);
    setExpanded(true);
  };
  open.addEventListener('click', openMap);
  preview.tabIndex = 0;
  preview.addEventListener('click', openMap);
  preview.addEventListener('keydown', event => {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    openMap();
  });
  moveUp.addEventListener('click', () => {
    if (!editor.isEditable || !canStartMapOperation()) return;
    const top = currentTopLevelNode();
    if (top) moveTopLevel(editor, top.position, 'up');
  });
  moveDown.addEventListener('click', () => {
    if (!editor.isEditable || !canStartMapOperation()) return;
    const top = currentTopLevelNode();
    if (top) moveTopLevel(editor, top.position, 'down');
  });
  remove.addEventListener('click', () => {
    if (!editor.isEditable || !canStartMapOperation()) return;
    const top = currentTopLevelNode();
    if (top) {
      runtime?.clearLiveMap(currentMap.mapId);
      removeTopLevel(editor, top.position, top.node);
    }
  });

  function onMapOperation() {
    if (!instance || destroyed) {
      finishPendingApply();
      return;
    }
    try {
      const updated = fromMindElixirData(instance.getData(), currentMap);
      if (JSON.stringify(updated) !== JSON.stringify(currentMap)) {
        const before = editingBaseline ?? currentMap;
        undoStack.push(cloneMap(before));
        redoStack.length = 0;
        editingBaseline = null;
        currentMap = updated;
        runtime?.publishMap(editor, currentMap);
      }
      appendPreview(preview, currentMap);
      setError(null);
    } catch (cause) {
      setError(errorText(cause));
      instance.disableEdit();
    } finally {
      finishPendingApply();
    }
    refreshControls();
  }

  function onSelectNodes(nodes: NodeObj[]) {
    selectedId = nodes.at(-1)?.id ?? null;
    refreshControls();
  }

  async function applyHistory(direction: 'undo' | 'redo') {
    const bridge = activeBridge();
    if (!bridge || !canEditMap(runtime, editor, fallbackBridge) || pendingApply || historyTask) return;
    let completeHistory!: () => void;
    historyTask = new Promise<void>(resolve => { completeHistory = resolve; });
    try {
      if (editingId) await finishTextEdit();
      if (!bridge.canStart()) throw new Error('笔记正在切换或只读，不能开始新的导图操作。');
      const source = direction === 'undo' ? undoStack : redoStack;
      if (source.length === 0) return;
      const target = cloneMap(source[source.length - 1]!);
      const previous = cloneMap(currentMap);
      const operation = (async () => {
        const blocks = runtime?.blocksFor(editor, target)
          ?? withMap(fromEditorDocument(editor.getJSON(), false), target);
        const commitIfCurrent = await bridge.preflight(blocks);
        if (!editorCurrent() || JSON.stringify(currentMap) !== JSON.stringify(previous) || !commitIfCurrent()) return;
        source.pop();
        (direction === 'undo' ? redoStack : undoStack).push(previous);
        currentMap = target;
        editingBaseline = null;
        runtime?.publishMap(editor, currentMap);
        appendPreview(preview, currentMap);
        destroyInstance();
        initializeMindElixir();
        if (instance) instance.clearHistory?.();
      })().catch(cause => {
        setError(errorText(cause));
        report(runtime, cause);
      });
      track(bridge, operation);
      await operation;
    } catch (cause) {
      setError(errorText(cause));
      report(runtime, cause);
    } finally {
      completeHistory();
      historyTask = null;
      refreshControls();
    }
  }

  function initializeMindElixir() {
    try {
      const data = toMindElixirData(currentMap);
      instance = new MindElixir({
        el: canvas,
        direction: RIGHT,
        editable: true,
        contextMenu: false,
        toolBar: false,
        keypress: false,
        allowUndo: false,
        newTopicName: '新节点',
        pasteHandler: event => { event.preventDefault(); event.stopPropagation(); },
        before: {
          beginEdit: async () => true,
          addChild: (...args) => preflightOperation('addChild', args),
          insertSibling: (...args) => preflightOperation('insertSibling', args),
          insertParent: (...args) => preflightOperation('insertParent', args),
          reshapeNode: (...args) => preflightOperation('reshapeNode', args),
          removeNodes: (...args) => preflightOperation('removeNodes', args),
          moveUpNode: (...args) => preflightOperation('moveUpNode', args),
          moveDownNode: (...args) => preflightOperation('moveDownNode', args),
          moveNodeIn: (...args) => preflightOperation('moveNodeIn', args),
          moveNodeBefore: (...args) => preflightOperation('moveNodeBefore', args),
          moveNodeAfter: (...args) => preflightOperation('moveNodeAfter', args),
          copyNode: async () => false,
          copyNodes: async () => false,
        },
      });
      instance.generateNewObj = () => ({ id: createUuidV4(), topic: '新节点' });
      const initialized = instance.init(data);
      if (initialized instanceof Error) throw initialized;
      const mind = instance;
      mind.editTopic = topic => {
        if (!topic.nodeObj.dangerouslySetInnerHTML) startTextEdit(topic);
      };
      mind.bus.addListener('operation', onMapOperation);
      mind.bus.addListener('selectNodes', onSelectNodes);
      instance.enableEdit();
      refreshControls();
    } catch (cause) {
      destroyInstance();
      setError(`无法打开此导图，原始内容仍然保留：${errorText(cause)}`);
      if (expanded) setExpanded(false);
      controls.hidden = false;
    }
  }

  function destroyInstance() {
    if (!instance) return;
    try {
      instance.bus.removeListener('operation', onMapOperation);
      instance.bus.removeListener('selectNodes', onSelectNodes);
      instance.destroy();
    } catch {
      // MindElixir may have failed half-way through init; dropping the view is safe.
    }
    instance = null;
    canvas.replaceChildren();
  }

  const settle = async () => {
    while (true) {
      await textInputQueue.settle();
      if (historyTask) {
        await historyTask;
        continue;
      }
      if (pendingApply) {
        await pendingApply.promise;
        continue;
      }
      if (inputWorker) {
        await inputWorker;
        continue;
      }
      if (textInputQueue.isComposing()) continue;
      if (editingId && pendingTextRevision > acceptedTextRevision) {
        queueTextValidation(true);
        if (inputWorker) await inputWorker;
        continue;
      }
      if (inputFailure) throw inputFailure;
      if (editingId) {
        await finishTextEdit();
        continue;
      }
      if (expanded && JSON.stringify(currentMap) !== lastCommittedMap) runtime?.publishMap(editor, currentMap);
      return;
    }
  };

  // Changing focus during mousedown can move the button before mouseup, especially
  // when an IME commits. The click handlers move focus and then await text flushes.
  for (const button of [collapse, undo, redo]) {
    button.addEventListener('mousedown', event => {
      if (event.button === 0) event.preventDefault();
    });
  }
  textInput.addEventListener('blur', () => {
    // These click handlers already flush; avoid a second concurrent text finish.
    if (focusingTextAction) return;
    if (editingId && !textEditor.contains(owner.activeElement)) {
      void finishTextEdit().catch(cause => setError(errorText(cause)));
    }
  });

  appendPreview(preview, currentMap);
  const unregisterRefresh = registerRefresher(editor, refreshControls);
  const refreshOnDocumentChange = ({ transaction }: { transaction: Transaction }) => {
    if (transaction.docChanged) refreshControls();
  };
  editor.on('transaction', refreshOnDocumentChange);
  refreshControls();

  return {
    dom: root,
    update(nextNode: ProseMirrorNode) {
      if (nextNode.type !== currentNode.type) return false;
      const previousMapId = currentMap.mapId;
      const changed = nextNode.attrs.mapId !== currentNode.attrs.mapId
        || nextNode.attrs.rootId !== currentNode.attrs.rootId
        || JSON.stringify(nextNode.attrs.nodes) !== JSON.stringify(currentNode.attrs.nodes);
      currentNode = nextNode;
      if (changed) {
        try {
          const nextMap = readMindMap({ type: 'mindMap', ...nextNode.attrs });
          if (JSON.stringify(nextMap) !== JSON.stringify(currentMap)) {
            if (expanded) setExpanded(false);
            currentMap = nextMap;
            selectedId = null;
            editingId = null;
            textEditor.hidden = true;
            destroyInstance();
            lastCommittedMap = JSON.stringify(currentMap);
            runtime?.clearLiveMap(previousMapId);
          }
          appendPreview(preview, currentMap);
          setError(null);
        } catch (cause) {
          setError(errorText(cause));
        }
      }
      refreshControls();
      return true;
    },
    stopEvent(event: Event) {
      const NodeType = root.ownerDocument.defaultView?.Node;
      return Boolean(NodeType && event.target instanceof NodeType && root.contains(event.target as globalThis.Node));
    },
    ignoreMutation() { return true; },
    destroy() {
      destroyed = true;
      if (expanded) setExpanded(false);
      panel.removeEventListener('keydown', onPanelKeyDown);
      open.removeEventListener('keydown', onCollapsedHistoryKeyDown);
      unregisterRefresh();
      editor.off('transaction', refreshOnDocumentChange);
      destroyInstance();
      runtime?.clearLiveMap(currentMap.mapId);
    },
  };
}

function createMindMapNode(bridge: MindMapEditorBridge | undefined, runtime: MindMapEditorRuntime | undefined): Extensions[number] {
  return Node.create({
    name: 'mindMap',
    priority: 1,
    group: 'block',
    atom: true,
    selectable: true,
    draggable: false,
    isolating: true,
    marks: '',

    addAttributes() {
      return {
        mapId: { default: null },
        rootId: { default: null },
        nodes: { default: null },
      };
    },

    parseHTML() { return []; },

    renderHTML({ HTMLAttributes }) {
      return ['figure', mergeAttributes({ 'data-note-mind-map': '' }, HTMLAttributes)];
    },

    addProseMirrorPlugins() {
      return [new Plugin({
        filterTransaction(transaction) {
          let nested = false;
          let invalid = false;
          transaction.doc.descendants((candidate, _position, parent) => {
            if (candidate.type.name !== 'mindMap') return !nested && !invalid;
            if (parent?.type.name !== 'doc') {
              nested = true;
              return false;
            }
            try {
              readMindMap({ type: 'mindMap', ...candidate.attrs });
            } catch (cause) {
              invalid = true;
              report(runtime, cause);
              return false;
            }
            return !invalid;
          });
          if (nested) runtime?.reportError('思维导图只能放在正文顶层，无法移入列表、引用或表格。');
          return !nested && !invalid;
        },
      })];
    },

    addCommands() {
      return {
        insertMindMap: () => ({ editor }) => {
          void insertMap(editor, runtime, bridge);
          return true;
        },
      };
    },

    addNodeView() {
      return props => createNodeView(props, bridge, runtime);
    },
  });
}

export function createMindMapExtensions(
  bridge?: MindMapEditorBridge,
  runtime?: MindMapEditorRuntime,
): Extensions {
  return [createMindMapNode(bridge, runtime)];
}

