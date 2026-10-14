import assert from 'node:assert/strict';
import test from 'node:test';
import { Editor, getSchema, type NodeViewRenderer, type NodeViewRendererProps } from '@tiptap/core';
import type { EditorView } from '@tiptap/pm/view';
import type { Block, MindMapNode } from '../src/generated/note-format.ts';
import { createEditorExtensions } from '../src/editor-extensions.ts';
import { fromEditorDocument, toEditorDocument } from '../src/editor-model.ts';
import { applyMindMapMutation, fromMindElixirData, toMindElixirData } from '../src/mindmap-tree.ts';
import { createMindMapTextInputQueue, type MindMapEditorBridge } from '../src/mindmap-session.ts';

const ROOT = '11111111-1111-4111-8111-111111111111';
const CHILD = '22222222-2222-4222-8222-222222222222';
const GRANDCHILD = '33333333-3333-4333-8333-333333333333';

const map = {
  type: 'mindMap',
  mapId: 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
  rootId: ROOT,
  nodes: [
    { id: ROOT, text: '  根节点\n', children: [CHILD] },
    { id: CHILD, text: '', children: [GRANDCHILD] },
    { id: GRANDCHILD, text: ' <script>中文 & text </script> ', children: [] },
  ],
} as const;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function makeBridge(overrides: Partial<MindMapEditorBridge> = {}) {
  const candidates: Block[][] = [];
  const pending: Promise<void>[] = [];
  const bridge: MindMapEditorBridge = {
    canStart: () => true,
    preflight: async blocks => {
      candidates.push(blocks);
      return () => true;
    },
    track: operation => pending.push(operation),
    registerFlush: () => () => undefined,
    ...overrides,
  };
  return { bridge, candidates, pending, async wait() { await Promise.all(pending); } };
}

function createMapEditor(bridge: MindMapEditorBridge, content: Record<string, unknown>) {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false, undefined, undefined, undefined, bridge),
    content,
  });
  const view = editor.view as EditorView;
  view.updateState(editor.state.reconfigure({ plugins: editor.extensionManager.plugins }));
  return editor;
}

class TestElement extends EventTarget {
  readonly children: TestElement[] = [];
  readonly attributes = new Map<string, string>();
  readonly listenerCalls = new Map<string, { add: number; remove: number }>();
  readonly ownerDocument: TestDocument;
  className = '';
  textContent = '';
  hidden = false;
  disabled = false;
  draggable = false;
  type = '';
  rows = 0;
  spellcheck = false;
  private controlValue = '';
  readonly tagName: string;

  constructor(tagName: string, ownerDocument: TestDocument) {
    super();
    this.tagName = tagName;
    this.ownerDocument = ownerDocument;
  }

  append(...nodes: TestElement[]) { this.children.push(...nodes); }
  override addEventListener(type: string, callback: EventListenerOrEventListenerObject | null, options?: boolean | AddEventListenerOptions) {
    const counts = this.listenerCalls.get(type) ?? { add: 0, remove: 0 };
    counts.add += 1;
    this.listenerCalls.set(type, counts);
    super.addEventListener(type, callback, options);
  }
  override removeEventListener(type: string, callback: EventListenerOrEventListenerObject | null, options?: boolean | EventListenerOptions) {
    const counts = this.listenerCalls.get(type) ?? { add: 0, remove: 0 };
    counts.remove += 1;
    this.listenerCalls.set(type, counts);
    super.removeEventListener(type, callback, options);
  }
  insertBefore(node: TestElement, before: TestElement) {
    const index = this.children.indexOf(before);
    this.children.splice(index < 0 ? this.children.length : index, 0, node);
  }
  replaceChildren(...nodes: TestElement[]) { this.children.splice(0, this.children.length, ...nodes); }
  setAttribute(name: string, value: string) { this.attributes.set(name, value); }
  getAttribute(name: string) { return this.attributes.get(name) ?? null; }
  removeAttribute(name: string) { this.attributes.delete(name); }
  contains(target: EventTarget | null): boolean {
    return target === this || this.children.some(child => child.contains(target));
  }
  focus() { this.ownerDocument.activeElement = this; }
  get value() { return this.controlValue; }
  set value(value: string) { this.controlValue = value; }
}

class TestDocument {
  activeElement: TestElement | null = null;
  readonly defaultView = {
    Node: TestElement,
    setTimeout,
    clearTimeout,
  };

  createElement(tagName: string) { return new TestElement(tagName, this); }
}

function findTestElement(root: TestElement, testId: string): TestElement | undefined {
  if (root.getAttribute('data-testid') === testId) return root;
  for (const child of root.children) {
    const found = findTestElement(child, testId);
    if (found) return found;
  }
  return undefined;
}

function firstParagraphText(editor: Editor) {
  const first = fromEditorDocument(editor.getJSON(), false)[0];
  if (first?.type !== 'paragraph') return '';
  const child = first.children[0];
  return child?.type === 'text' ? child.text : '';
}

function dispatchShortcut(target: TestElement, key: string, options: { ctrl?: boolean; meta?: boolean; shift?: boolean } = {}) {
  const event = new Event('keydown', { bubbles: true, cancelable: true });
  Object.defineProperties(event, {
    key: { value: key },
    ctrlKey: { value: options.ctrl ?? false },
    metaKey: { value: options.meta ?? false },
    shiftKey: { value: options.shift ?? false },
  });
  target.dispatchEvent(event);
  return event;
}

function createMindMapNodeView(editor: Editor, document: TestDocument, getPos: () => number, mounted = true) {
  const extension = editor.extensionManager.extensions.find(item => item.name === 'mindMap');
  assert.ok(extension);
  const renderer = (extension.config as unknown as { addNodeView?: () => NodeViewRenderer }).addNodeView?.call(extension);
  assert.ok(renderer);
  const editorDOM = document.createElement('div');
  const view = {
    dom: editorDOM,
    state: editor.state,
    editable: true,
    isDestroyed: false,
    dispatch: (transaction: Parameters<EditorView['dispatch']>[0]) => editor.dispatchTransaction(transaction),
    updateState: (state: Editor['state']) => {
      editor.editorState = state;
      view.state = state;
    },
    destroy() { view.isDestroyed = true; },
    focus() { editorDOM.focus(); },
  };
  if (mounted) Object.defineProperty(editor, 'editorView', { configurable: true, value: view, writable: true });
  const node = editor.state.doc.child(editor.state.doc.resolve(getPos()).index());
  return renderer({ editor, node, getPos, view } as unknown as NodeViewRendererProps);
}

test('saved mind map renders during initial view construction before Tiptap mounts the editor', () => {
  const editor = createMapEditor(makeBridge().bridge, toEditorDocument([map], false));
  const document = new TestDocument();
  assert.throws(() => editor.view.dom, /not available/);
  try {
    const nodeView = createMindMapNodeView(editor, document, () => 0, false);
    const root = nodeView.dom as unknown as TestElement;
    assert.equal(root.ownerDocument, document);
    assert.equal(root.getAttribute('data-note-mind-map'), '');
    assert.ok(findTestElement(root, 'mind-map-open'));
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [map]);
    nodeView.destroy?.();
  } finally {
    editor.destroy();
  }
});

test('MindElixir adapter preserves raw node text and the flat tree round trip', () => {
  const data = toMindElixirData(map);
  assert.deepEqual(fromMindElixirData(data, map), map);
});

test('MindElixir adapter rejects unsupported rich node data instead of dropping it', () => {
  const data = toMindElixirData(map);
  data.nodeData.children![0]!.style = { color: 'red' };
  assert.throws(() => fromMindElixirData(data, map), /unsupported|不支持/);
});

test('mind map is a top-level atomic editor node with native undo and redo', () => {
  const blocks: Block[] = [
    { type: 'paragraph', children: [{ type: 'text', text: '前', marks: [] }] },
    map,
    { type: 'paragraph', children: [{ type: 'text', text: '后', marks: [] }] },
  ];
  const document = toEditorDocument(blocks, false);
  const schema = getSchema(createEditorExtensions(false));
  const node = schema.nodeFromJSON(document).child(1);
  assert.equal(node.type.name, 'mindMap');
  assert.equal(node.isAtom, true);
  assert.equal(schema.topNodeType.contentMatch.defaultType?.name, 'paragraph');
  assert.deepEqual(fromEditorDocument(document, false), blocks);

  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: document,
  });
  const view = editor.view as EditorView;
  view.updateState(editor.state.reconfigure({ plugins: editor.extensionManager.plugins }));
  try {
    const position = editor.state.doc.child(0).nodeSize;
    editor.view.dispatch(editor.state.tr.delete(position, position + node.nodeSize));
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [blocks[0], blocks[2]]);
    assert.equal(editor.commands.undo(), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), blocks);
    assert.equal(editor.commands.redo(), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [blocks[0], blocks[2]]);
    assert.equal(editor.commands.undo(), true);

    const nestedMap = editor.schema.nodes.mindMap.create({
      mapId: map.mapId,
      rootId: map.rootId,
      nodes: map.nodes,
    });
    const nestedList = editor.schema.nodes.bulletList.create(null, [
      editor.schema.nodes.listItem.create(null, [nestedMap]),
    ]);
    const currentMapPosition = editor.state.doc.child(0).nodeSize;
    editor.view.dispatch(editor.state.tr.replaceWith(
      currentMapPosition,
      currentMapPosition + editor.state.doc.child(1).nodeSize,
      nestedList,
    ));
    assert.equal(editor.state.doc.child(1).type.name, 'mindMap');
  } finally {
    editor.destroy();
  }
});

test('flat native maps keep children ordered and all node text is a plain string', () => {
  const input = map.nodes as unknown as MindMapNode[];
  const converted = toMindElixirData({ ...map, nodes: input });
  assert.equal(converted.nodeData.topic, '  根节点\n');
  assert.deepEqual(converted.nodeData.children?.map(node => node.topic), ['']);
  assert.equal(converted.nodeData.children?.[0]?.children?.[0]?.topic, ' <script>中文 & text </script> ');
});

test('500-node representative branch trees round-trip without changing ids or raw text', () => {
  const id = (value: number) => `00000000-0000-4000-8000-${value.toString(16).padStart(12, '0')}`;
  const nodes: MindMapNode[] = Array.from({ length: 500 }, (_, index) => ({
    id: id(index + 1),
    text: index % 7 === 0 ? `\n节点 ${index}  ` : `节点 ${index}`,
    children: index === 0 ? [id(2), id(3)] : [index * 2 + 1, index * 2 + 2]
      .filter(child => child < 500)
      .map(child => id(child + 1)),
  }));
  const large = {
    type: 'mindMap',
    mapId: id(501),
    rootId: id(1),
    nodes,
  };
  assert.deepEqual(fromMindElixirData(toMindElixirData(large), large), large);
});

test('canonical tree mutations keep raw text and only add UUID-addressable nodes', () => {
  const withChild = applyMindMapMutation(map, {
    type: 'addChild',
    parentId: ROOT,
    node: { id: '44444444-4444-4444-8444-444444444444', text: '  ' },
  });
  assert.equal(withChild.nodes.find(node => node.id.endsWith('444444444444'))?.text, '  ');
  assert.deepEqual(withChild.nodes[0]?.children, [CHILD, '44444444-4444-4444-8444-444444444444']);
  assert.throws(() => applyMindMapMutation(withChild, {
    type: 'remove',
    nodeIds: [ROOT],
  }), /root|根|不存在/i);
});

test('simple-section schemas do not expose mind maps', () => {
  assert.equal(getSchema(createEditorExtensions(true)).nodes.mindMap, undefined);
});

test('bridge-only insertion at document end keeps body text and creates an editable trailing paragraph', async () => {
  const session = makeBridge();
  const original = {
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'text', text: 'body text' }] }],
  };
  const editor = createMapEditor(session.bridge, original);

  try {
    editor.commands.setTextSelection(editor.state.doc.content.size - 1);
    assert.equal(editor.commands.insertMindMap(), true);
    await session.wait();

    const blocks = fromEditorDocument(editor.getJSON(), false);
    assert.equal(blocks[0]?.type, 'paragraph');
    assert.deepEqual(blocks[0], { type: 'paragraph', children: [{ type: 'text', text: 'body text', marks: [] }] });
    assert.equal(blocks[1]?.type, 'mindMap');
    assert.deepEqual(blocks[2], { type: 'paragraph', children: [] });
    assert.equal(session.candidates[0]?.at(-1)?.type, 'paragraph');
    assert.equal(editor.commands.undo(), true);
    assert.deepEqual(editor.getJSON(), original);
    assert.equal(editor.commands.redo(), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), blocks);
  } finally {
    editor.destroy();
  }
});

test('bridge-only insertion refuses a session transition without changing content', async () => {
  let preflightCalls = 0;
  const session = makeBridge({
    canStart: () => false,
    preflight: async () => { preflightCalls += 1; return () => true; },
  });
  const editor = createMapEditor(session.bridge, {
    type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'unchanged' }] }],
  });
  const original = editor.getJSON();

  try {
    assert.equal(editor.commands.insertMindMap(), true);
    await session.wait();
    assert.equal(preflightCalls, 0);
    assert.deepEqual(editor.getJSON(), original);
  } finally {
    editor.destroy();
  }
});

test('bridge-only insertion retries a late preflight against the latest editor document', async () => {
  const firstPreflight = deferred<() => boolean>();
  const candidates: Block[][] = [];
  let calls = 0;
  const session = makeBridge({
    preflight: async blocks => {
      candidates.push(blocks);
      calls += 1;
      return calls === 1 ? firstPreflight.promise : () => true;
    },
  });
  const editor = createMapEditor(session.bridge, {
    type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'base' }] }],
  });
  const originalOn = editor.on.bind(editor);
  let destroyRegistrations = 0;
  editor.on = ((event: string, listener: (...args: never[]) => unknown) => {
    if (event === 'destroy') destroyRegistrations += 1;
    return originalOn(event as never, listener as never);
  }) as typeof editor.on;

  try {
    assert.equal(editor.commands.insertMindMap(), true);
    editor.commands.setTextSelection(editor.state.doc.content.size - 1);
    editor.view.dispatch(editor.state.tr.insertText(' latest'));
    firstPreflight.resolve(() => true);
    await session.wait();

    assert.equal(calls, 2);
    assert.equal(destroyRegistrations, 1);
    assert.deepEqual(candidates[1]?.[0], {
      type: 'paragraph', children: [{ type: 'text', text: 'base latest', marks: [] }],
    });
    assert.equal(fromEditorDocument(editor.getJSON(), false).some(block => block.type === 'mindMap'), true);
  } finally {
    editor.destroy();
  }
});

test('moving a mind map to the end creates a trailing paragraph in the same undoable action', () => {
  const session = makeBridge();
  const editor = createMapEditor(session.bridge, toEditorDocument([
    { type: 'paragraph', children: [{ type: 'text', text: 'before', marks: [] }] },
    map,
    { type: 'paragraph', children: [{ type: 'text', text: 'after', marks: [] }] },
  ], false));
  const fakeDocument = new TestDocument();
  const mapPosition = editor.state.doc.child(0).nodeSize;
  assert.equal(editor.isEditable, true);
  assert.equal(editor.state.doc.resolve(mapPosition).index(), 1);
  const nodeView = createMindMapNodeView(editor, fakeDocument, () => mapPosition);

  try {
    const moveDown = findTestElement(nodeView.dom as TestElement, 'mind-map-move-down');
    assert.ok(moveDown);
    assert.equal(moveDown.disabled, false);
    moveDown.dispatchEvent(new Event('click'));

    const moved = fromEditorDocument(editor.getJSON(), false);
    assert.deepEqual(moved.slice(0, 2), [
      { type: 'paragraph', children: [{ type: 'text', text: 'before', marks: [] }] },
      { type: 'paragraph', children: [{ type: 'text', text: 'after', marks: [] }] },
    ]);
    assert.equal(moved[2]?.type, 'mindMap');
    assert.deepEqual(moved[3], { type: 'paragraph', children: [] });
    assert.equal(editor.commands.undo(), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [
      { type: 'paragraph', children: [{ type: 'text', text: 'before', marks: [] }] },
      map,
      { type: 'paragraph', children: [{ type: 'text', text: 'after', marks: [] }] },
    ]);
    assert.equal(editor.commands.redo(), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), moved);
    const panel = (nodeView.dom as TestElement).children.find(child => child.className === 'mind-map-editor-panel');
    assert.equal(panel?.listenerCalls.get('keydown')?.add, 1);
  } finally {
    const panel = (nodeView.dom as TestElement).children.find(child => child.className === 'mind-map-editor-panel');
    nodeView.destroy?.();
    assert.equal(panel?.listenerCalls.get('keydown')?.remove, 1);
    editor.destroy();
  }
});

test('deleting a map returns focus to the body and preserves a single undoable block change', () => {
  const before: Block = { type: 'paragraph', children: [{ type: 'text', text: 'before', marks: [] }] };
  const after: Block = { type: 'paragraph', children: [{ type: 'text', text: 'after', marks: [] }] };
  const editor = createMapEditor(makeBridge().bridge, toEditorDocument([before, map, after], false));
  const document = new TestDocument();
  const nodeView = createMindMapNodeView(editor, document, () => editor.state.doc.child(0).nodeSize);
  try {
    const remove = findTestElement(nodeView.dom as TestElement, 'mind-map-delete');
    assert.ok(remove);
    remove.focus();
    remove.dispatchEvent(new Event('click'));
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [before, after]);
    assert.equal(document.activeElement, editor.view.dom);
    assert.equal(editor.commands.undo(), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [before, map, after]);
    assert.equal(editor.commands.redo(), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [before, after]);
  } finally {
    nodeView.destroy?.();
    editor.destroy();
  }
});

test('composition final input survives a pending close transition and settle waits for preflight', async () => {
  const scheduled: Array<() => void> = [];
  const preflightStarted = deferred<void>();
  const preflightGate = deferred<void>();
  const forced: boolean[] = [];
  const acceptedCandidates: string[] = [];
  let settleFinished = false;
  let canStart = true;
  let rawText = 'accepted';
  const queue = createMindMapTextInputQueue(async force => {
    forced.push(force);
    if (!force && !canStart) {
      rawText = 'accepted';
      return;
    }
    const candidate = rawText;
    preflightStarted.resolve(undefined);
    await preflightGate.promise;
    acceptedCandidates.push(candidate);
  }, callback => scheduled.push(callback));

  queue.compositionStart();
  rawText = '组合中';
  await queue.input();
  canStart = false; // Ctrl+S or switching began while composition was active.
  const settling = queue.settle();
  const compositionEnd = queue.compositionEnd();
  rawText = '组合最终'; // browsers may send the final input event after compositionend.
  const finalInputTask = queue.input();
  scheduled.shift()?.();
  const awaitingSettle = settling.then(() => { settleFinished = true; });
  await preflightStarted.promise;
  assert.equal(settleFinished, false, 'flush must keep waiting while the final IME candidate is preflighted');
  preflightGate.resolve(undefined);
  await Promise.all([compositionEnd, finalInputTask]);
  await awaitingSettle;

  assert.deepEqual(acceptedCandidates, ['组合最终']);
  assert.equal(rawText, '组合最终');
  assert.equal(forced.at(-1), true);
});

test('collapsed map open focus routes body undo and redo while panel shortcuts stay isolated', async () => {
  const session = makeBridge();
  const editor = createMapEditor(session.bridge, toEditorDocument([
    { type: 'paragraph', children: [{ type: 'text', text: 'body', marks: [] }] },
    map,
    { type: 'paragraph', children: [{ type: 'text', text: 'tail', marks: [] }] },
  ], false));
  const document = new TestDocument();
  const nodeView = createMindMapNodeView(editor, document, () => editor.state.doc.child(0).nodeSize);
  const root = nodeView.dom as TestElement;
  const open = findTestElement(root, 'mind-map-open');
  const panel = root.children.find(child => child.className === 'mind-map-editor-panel');
  const deleteButton = findTestElement(root, 'mind-map-delete');
  assert.ok(open && panel && deleteButton);

  try {
    editor.view.dispatch(editor.state.tr.insertText(' changed', 5));
    assert.equal(firstParagraphText(editor), 'body changed');

    open.focus();
    assert.equal(document.activeElement, open);
    const undoEvent = dispatchShortcut(open, 'z', { ctrl: true });
    assert.equal(undoEvent.defaultPrevented, true);
    assert.equal(firstParagraphText(editor), 'body');

    const redoEvent = dispatchShortcut(open, 'z', { meta: true, shift: true });
    assert.equal(redoEvent.defaultPrevented, true);
    assert.equal(firstParagraphText(editor), 'body changed');

    const ordinaryButtonEvent = dispatchShortcut(deleteButton, 'z', { ctrl: true });
    assert.equal(ordinaryButtonEvent.defaultPrevented, false);
    assert.equal(firstParagraphText(editor), 'body changed');

    const panelEvent = dispatchShortcut(panel, 'z', { ctrl: true });
    assert.equal(panelEvent.defaultPrevented, true);
    await new Promise(resolve => setTimeout(resolve, 0));
    assert.equal(firstParagraphText(editor), 'body changed');
  } finally {
    nodeView.destroy?.();
    editor.destroy();
  }
});

test('settle waits for a second composition that starts during the first preflight', async () => {
  const scheduled: Array<() => void> = [];
  const started = [deferred<void>(), deferred<void>()];
  const gates = [deferred<void>(), deferred<void>()];
  const candidates: string[] = [];
  const accepted: string[] = [];
  const published: string[] = [];
  let rawText = '';
  let settleFinished = false;
  let index = 0;
  const queue = createMindMapTextInputQueue(async force => {
    assert.equal(force, true);
    const current = index++;
    candidates.push(rawText);
    started[current]?.resolve(undefined);
    await gates[current]!.promise;
    accepted.push(candidates[current]!);
  }, callback => scheduled.push(callback), () => {
    if (accepted.length > 0) published.push(accepted.at(-1)!);
  });

  queue.compositionStart();
  rawText = '组合A最终';
  const firstEnd = queue.compositionEnd();
  scheduled.shift()?.();
  await started[0]!.promise;
  const settling = queue.settle().then(() => { settleFinished = true; });

  queue.compositionStart();
  rawText = '组合B中';
  await queue.input();
  const secondEnd = queue.compositionEnd();
  rawText = '组合B最终';
  const finalInput = queue.input();
  scheduled.shift()?.();
  await started[1]!.promise;

  gates[0]!.resolve(undefined);
  await firstEnd;
  await Promise.resolve();
  assert.equal(settleFinished, false, 'the first composition completing must not release the flush while the second is active');
  assert.deepEqual(published, [], 'an earlier composition must not publish while a later composition is still active');

  gates[1]!.resolve(undefined);
  await Promise.all([secondEnd, finalInput, settling]);
  assert.deepEqual(candidates, ['组合A最终', '组合B最终']);
  assert.deepEqual(accepted, ['组合A最终', '组合B最终']);
  assert.deepEqual(published, ['组合B最终']);
  assert.equal(settleFinished, true);
});

test('composition publishes the accepted text after delayed preflight without blur', async () => {
  const scheduled: Array<() => void> = [];
  const preflightStarted = deferred<void>();
  const preflightGate = deferred<void>();
  let rawText = '';
  let acceptedText = '';
  let publishedText = '';
  const queue = createMindMapTextInputQueue(async force => {
    assert.equal(force, true);
    const candidate = rawText;
    preflightStarted.resolve(undefined);
    await preflightGate.promise;
    acceptedText = candidate;
  }, callback => scheduled.push(callback), () => {
    publishedText = acceptedText;
  });

  queue.compositionStart();
  rawText = 'IME final value';
  await queue.input();
  const compositionEnd = queue.compositionEnd();
  scheduled.shift()?.(); // compositionend's deferred callback has run; preflight still waits.
  await preflightStarted.promise;
  assert.equal(acceptedText, '');
  assert.equal(publishedText, '', 'do not publish a candidate before preflight accepts it');

  preflightGate.resolve(undefined);
  await compositionEnd;
  assert.equal(acceptedText, 'IME final value');
  assert.equal(publishedText, 'IME final value', 'publish after stable composition completion without waiting for blur');
});
