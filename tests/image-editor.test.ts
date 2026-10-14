import assert from 'node:assert/strict';
import test from 'node:test';
import { Editor, type NodeViewRenderer, type NodeViewRendererProps } from '@tiptap/core';
import { Fragment, Slice } from '@tiptap/pm/model';
import type { EditorView } from '@tiptap/pm/view';
import type { AssetManifestEntry, Block } from '../src/generated/note-format.ts';
import { createEditorExtensions } from '../src/editor-extensions.ts';
import { refreshImageNodeViews, type ImageEditorRuntime } from '../src/image-extensions.ts';
import type { ImageEditorBridge } from '../src/image-session.ts';
import { fromEditorDocument } from '../src/editor-model.ts';

const IMAGE_ID = '74a5a566-c1b8-4bca-9fa2-3d45a5943449';

function makeManifest(overrides: Partial<AssetManifestEntry> = {}): AssetManifestEntry {
  return {
    id: IMAGE_ID,
    mediaType: 'image/png',
    byteLength: 4,
    sha256: 'a'.repeat(64),
    width: 1200,
    height: 600,
    ...overrides,
  };
}

function makeBridge(overrides: Partial<ImageEditorBridge> = {}) {
  const pending: Promise<void>[] = [];
  const candidates: Block[][] = [];
  let staged: Blob | undefined;
  const bridge: ImageEditorBridge = {
    canStart: () => true,
    stage: async file => {
      staged = file;
      return makeManifest();
    },
    read: async () => new Blob(),
    preflight: async blocks => {
      candidates.push(blocks);
      return () => true;
    },
    track: operation => pending.push(operation),
    ...overrides,
  };
  return {
    bridge,
    candidates,
    get staged() { return staged; },
    pending,
    async wait() { await Promise.all(pending); },
  };
}

function createEditor(
  bridge: ImageEditorBridge,
  content: Record<string, unknown> = { type: 'doc', content: [{ type: 'paragraph' }] },
  runtime?: ImageEditorRuntime,
  editable = true,
) {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false, undefined, bridge, runtime),
    content,
    editable,
  });
  const view = editor.view as EditorView;
  view.updateState(editor.state.reconfigure({ plugins: editor.extensionManager.plugins }));
  Object.defineProperty(editor, 'view', { configurable: true, value: view });
  return editor;
}

function makeImagePaste(file: Blob, plainText = ''): ClipboardEvent {
  const item = { kind: 'file', type: file.type, getAsFile: () => file };
  return {
    clipboardData: {
      files: [file],
      items: [item],
      getData: (type: string) => type === 'text/plain' ? plainText : '',
    },
  } as unknown as ClipboardEvent;
}

function makeCustomPaste({ files = [], items = [], plainText = '', html = '' }: {
  files?: Blob[];
  items?: Array<{ kind: string; type: string; getAsFile: () => Blob | null }>;
  plainText?: string;
  html?: string;
}): ClipboardEvent {
  return {
    clipboardData: {
      files,
      items,
      getData: (type: string) => type === 'text/plain' ? plainText : type === 'text/html' ? html : '',
    },
  } as unknown as ClipboardEvent;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function imagePosition(editor: Editor) {
  let position = -1;
  editor.state.doc.descendants((node, nodePosition) => {
    if (node.type.name === 'image') {
      position = nodePosition;
      return false;
    }
    return position < 0;
  });
  return position;
}

function paste(editor: Editor, event: ClipboardEvent): boolean {
  const view = editor.view as EditorView;
  for (const plugin of editor.extensionManager.plugins) {
    const handled = plugin.props.handlePaste?.(view, event, new Slice(Fragment.empty, 0, 0));
    if (handled) {
      return true;
    }
  }
  return false;
}

class TestElement extends EventTarget {
  readonly children: TestElement[] = [];
  readonly attributes = new Map<string, string>();
  readonly style: Record<string, string> = {};
  readonly ownerDocument: TestDocument;
  className = '';
  textContent = '';
  hidden = false;
  disabled = false;
  draggable = false;
  isConnected = false;
  type = '';
  min = '';
  step = '';
  readonly tagName: string;
  readonly propertyWrites: string[] = [];
  private maxValue = '';
  private controlValue = '';

  constructor(tagName: string, ownerDocument: TestDocument) {
    super();
    this.tagName = tagName;
    this.ownerDocument = ownerDocument;
  }

  append(...nodes: TestElement[]) { this.children.push(...nodes); }
  setAttribute(name: string, value: string) { this.attributes.set(name, value); }
  getAttribute(name: string) { return this.attributes.get(name) ?? null; }
  removeAttribute(name: string) { this.attributes.delete(name); }
  contains(target: EventTarget | null): boolean {
    return target === this || this.children.some(child => child.contains(target));
  }
  get max() { return this.maxValue; }
  set max(value: string) {
    this.maxValue = value;
    this.propertyWrites.push('max');
  }
  get value() { return this.controlValue; }
  set value(value: string) {
    this.controlValue = value;
    this.propertyWrites.push('value');
  }
}

class TestDocument {
  readonly defaultView = {
    Node: TestElement,
    IntersectionObserver: class {
      observe() {}
      disconnect() {}
    },
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

function createImageNodeView(editor: Editor, document: TestDocument, getPos: () => number | undefined = () => 0) {
  const imageExtension = editor.extensionManager.extensions.find(extension => extension.name === 'image');
  assert.ok(imageExtension);
  const config = imageExtension.config as unknown as { addNodeView?: () => NodeViewRenderer };
  const renderer = config.addNodeView?.call(imageExtension);
  assert.ok(renderer);
  const node = editor.state.doc.firstChild!;
  return renderer({ editor, node, getPos } as NodeViewRendererProps);
}

test('pasting PNG stages its source file and inserts after the selected paragraph', async () => {
  const bridge = makeBridge();
  const initialContent = {
      type: 'doc',
      content: [
        { type: 'paragraph', content: [{ type: 'text', text: 'keep this selected text' }] },
        { type: 'paragraph', content: [{ type: 'text', text: 'following text' }] },
      ],
  };
  const editor = createEditor(bridge.bridge, initialContent);

  try {
    editor.commands.setTextSelection({ from: 6, to: 10 });
    const source = new Blob([new Uint8Array([137, 80, 78, 71])], { type: 'image/png' });
    assert.equal(paste(editor, makeImagePaste(source)), true);
    await bridge.wait();

    assert.equal(bridge.staged, source);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [
      { type: 'paragraph', children: [{ type: 'text', text: 'keep this selected text', marks: [] }] },
      { type: 'image', assetId: IMAGE_ID, displayWidthPx: 800 },
      { type: 'paragraph', children: [{ type: 'text', text: 'following text', marks: [] }] },
    ] satisfies Block[]);
    assert.equal(bridge.candidates.length, 1);
    assert.deepEqual(bridge.candidates[0], fromEditorDocument(editor.getJSON(), false));
    assert.equal(editor.commands.undo(), true);
    assert.deepEqual(editor.getJSON(), initialContent);
    assert.equal(editor.commands.redo(), true);
    assert.equal(imagePosition(editor) >= 0, true);
  } finally {
    editor.destroy();
  }
});

test('a pending paste tracks typing and preflights the newest candidate before insertion', async () => {
  const stageGate = deferred<AssetManifestEntry>();
  let editor: Editor;
  const bridge = makeBridge({ stage: async () => stageGate.promise });
  editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'text', text: 'anchor text' }] }, { type: 'paragraph' }],
  });

  try {
    const source = new Blob(['png bytes'], { type: 'image/png' });
    assert.equal(paste(editor, makeImagePaste(source)), true);
    editor.commands.setTextSelection(12);
    (editor.view as EditorView).dispatch(editor.state.tr.insertText(' typed'));
    stageGate.resolve(makeManifest());
    await bridge.wait();

    const blocks = fromEditorDocument(editor.getJSON(), false);
    assert.deepEqual(blocks, [
      { type: 'paragraph', children: [{ type: 'text', text: 'anchor text typed', marks: [] }] },
      { type: 'image', assetId: IMAGE_ID, displayWidthPx: 800 },
      { type: 'paragraph', children: [] },
    ] satisfies Block[]);
    assert.deepEqual(bridge.candidates.at(-1), blocks);
  } finally {
    editor.destroy();
  }
});

test('image paste leaves companion text to the normal paste path', async () => {
  const stageGate = deferred<AssetManifestEntry>();
  const bridge = makeBridge({ stage: async () => stageGate.promise });
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'text', text: 'base' }] }, { type: 'paragraph' }],
  });

  try {
    editor.commands.setTextSelection(3);
    const event = makeCustomPaste({
      files: [new Blob(['png'], { type: 'image/png' })],
      plainText: 'pasted text',
    });
    assert.equal(paste(editor, event), false);
    (editor.view as EditorView).dispatch(editor.state.tr.insertText('pasted text'));
    stageGate.resolve(makeManifest());
    await bridge.wait();
    const blocks = fromEditorDocument(editor.getJSON(), false);
    assert.deepEqual(blocks[0], {
      type: 'paragraph', children: [{ type: 'text', text: 'bapasted textse', marks: [] }],
    });
    assert.equal(blocks[1]?.type, 'image');
  } finally {
    editor.destroy();
  }
});

test('image paste waits for compositionend before preflight and dispatch', async () => {
  let preflightCount = 0;
  const bridge = makeBridge({
    preflight: async () => { preflightCount += 1; return () => true; },
  });
  const editor = createEditor(bridge.bridge, {
    type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'input' }] }],
  });
  const view = editor.view as EditorView;
  let compositionEnd: (() => void) | undefined;
  const fakeDom = {
    clientWidth: 640,
    parentElement: null,
    addEventListener: (type: string, listener: () => void) => {
      if (type === 'compositionend') compositionEnd = listener;
    },
    removeEventListener: (type: string) => {
      if (type === 'compositionend') compositionEnd = undefined;
    },
  };
  Object.defineProperty(view, 'dom', { configurable: true, value: fakeDom });
  view.composing = true;

  try {
    assert.equal(paste(editor, makeImagePaste(new Blob(['png'], { type: 'image/png' }))), true);
    await new Promise<void>(resolve => setTimeout(resolve, 0));
    assert.equal(preflightCount, 0);
    assert.equal(imagePosition(editor), -1);
    view.composing = false;
    compositionEnd?.();
    await bridge.wait();
    assert.equal(preflightCount, 1);
    assert.notEqual(imagePosition(editor), -1);
  } finally {
    editor.destroy();
  }
});

test('a paste captured inside a list is inserted after the enclosing top-level list', async () => {
  const bridge = makeBridge();
  const statuses: string[] = [];
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [
      { type: 'bulletList', content: [{ type: 'listItem', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'inside list' }] }] }] },
      { type: 'paragraph', content: [{ type: 'text', text: 'after list' }] },
    ],
  }, { reportStatus: message => statuses.push(message) });

  try {
    editor.commands.setTextSelection(5);
    assert.equal(editor.state.selection.$head.depth > 1, true);
    assert.equal(paste(editor, makeImagePaste(new Blob(['png'], { type: 'image/png' }))), true);
    await bridge.wait();
    const blocks = fromEditorDocument(editor.getJSON(), false);
    assert.equal(blocks[1]?.type, 'image');
    assert.equal(statuses[0], '图片已插入列表这一顶层内容块之后。');
  } finally {
    editor.destroy();
  }
});

test('a deleted anchor block cancels a pending image paste without inserting at the next paragraph', async () => {
  const stageGate = deferred<AssetManifestEntry>();
  const errors: string[] = [];
  const bridge = makeBridge({ stage: async () => stageGate.promise });
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [
      { type: 'paragraph', content: [{ type: 'text', text: 'delete this anchor' }] },
      { type: 'paragraph', content: [{ type: 'text', text: 'keep this block' }] },
    ],
  }, { reportError: message => errors.push(message) });

  try {
    assert.equal(paste(editor, makeImagePaste(new Blob(['png'], { type: 'image/png' }))), true);
    const firstBlockSize = editor.state.doc.firstChild!.nodeSize;
    (editor.view as EditorView).dispatch(editor.state.tr.delete(0, firstBlockSize));
    stageGate.resolve(makeManifest());
    await assert.rejects(bridge.wait(), /目标内容块已被删除/);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [
      { type: 'paragraph', children: [{ type: 'text', text: 'keep this block', marks: [] }] },
    ] satisfies Block[]);
    assert.match(errors.join('\n'), /目标内容块已被删除/);
  } finally {
    editor.destroy();
  }
});

test('preflight rejection reports the native message and leaves the document untouched', async () => {
  const errors: string[] = [];
  const bridge = makeBridge({ preflight: async () => { throw { kind: 'draft-rejected', message: '草稿预检拒绝' }; } });
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'text', text: 'unchanged' }] }],
  }, { reportError: message => errors.push(message) });
  const original = editor.getJSON();

  try {
    assert.equal(paste(editor, makeImagePaste(new Blob(['png'], { type: 'image/png' }))), true);
    await assert.rejects(bridge.wait(), cause => (cause as { message?: unknown }).message === '草稿预检拒绝');
    assert.deepEqual(editor.getJSON(), original);
    assert.deepEqual(errors, ['草稿预检拒绝']);
  } finally {
    editor.destroy();
  }
});

test('a changed draft predicate retries preflight with a rebuilt candidate', async () => {
  let editor: Editor;
  let preflightCount = 0;
  const candidates: Block[][] = [];
  const bridge = makeBridge({
    preflight: async blocks => {
      candidates.push(blocks);
      preflightCount += 1;
      if (preflightCount === 1) {
        editor.commands.setTextSelection(3);
        (editor.view as EditorView).dispatch(editor.state.tr.insertText('latest '));
        return () => false;
      }
      return () => true;
    },
  });
  editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'text', text: 'draft' }] }],
  });

  try {
    assert.equal(paste(editor, makeImagePaste(new Blob(['png'], { type: 'image/png' }))), true);
    await bridge.wait();
    assert.equal(preflightCount, 2);
    assert.notDeepEqual(candidates[0], candidates[1]);
    assert.deepEqual(candidates[1], fromEditorDocument(editor.getJSON(), false));
  } finally {
    editor.destroy();
  }
});

test('session cancellation during staging prevents preflight and insertion', async () => {
  const stageGate = deferred<AssetManifestEntry>();
  let current = true;
  let preflightCalls = 0;
  const bridge = makeBridge({
    stage: async () => stageGate.promise,
    preflight: async () => { preflightCalls += 1; return () => true; },
  });
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'text', text: 'keep' }] }],
  }, { isCurrent: () => current });
  const original = editor.getJSON();

  try {
    assert.equal(paste(editor, makeImagePaste(new Blob(['png'], { type: 'image/png' }))), true);
    current = false;
    stageGate.resolve(makeManifest());
    await bridge.wait();
    assert.equal(preflightCalls, 0);
    assert.deepEqual(editor.getJSON(), original);
  } finally {
    editor.destroy();
  }
});

test('destroying the editor while staging prevents stale dispatch', async () => {
  const stageGate = deferred<AssetManifestEntry>();
  let preflightCalls = 0;
  const bridge = makeBridge({
    stage: async () => stageGate.promise,
    preflight: async () => { preflightCalls += 1; return () => true; },
  });
  const editor = createEditor(bridge.bridge);
  const original = editor.getJSON();

  assert.equal(paste(editor, makeImagePaste(new Blob(['png'], { type: 'image/png' }))), true);
  editor.destroy();
  stageGate.resolve(makeManifest());
  await bridge.wait();
  assert.equal(preflightCalls, 0);
  assert.deepEqual(editor.getJSON(), original);
});

test('read-only editors consume image paste without staging or changing content', async () => {
  const bridge = makeBridge();
  const editor = createEditor(bridge.bridge, {
    type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'read only' }] }],
  }, undefined, false);
  const original = editor.getJSON();

  try {
    assert.equal(paste(editor, makeImagePaste(new Blob(['png'], { type: 'image/png' }))), true);
    await bridge.wait();
    assert.equal(bridge.staged, undefined);
    assert.deepEqual(editor.getJSON(), original);
  } finally {
    editor.destroy();
  }
});

test('read-only image nodes reject resize, move, and delete commands', () => {
  const bridge = makeBridge();
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [{ type: 'image', attrs: { assetId: IMAGE_ID, displayWidthPx: 400 } }, { type: 'paragraph' }],
  }, undefined, false);
  const original = editor.getJSON();

  try {
    const position = imagePosition(editor);
    assert.equal(editor.commands.setImageDisplayWidth(position, 500), false);
    assert.equal(editor.commands.moveImageNode(position, 'down'), false);
    assert.equal(editor.commands.deleteImageNode(position), false);
    assert.deepEqual(editor.getJSON(), original);
  } finally {
    editor.destroy();
  }
});

test('image width controls preserve native mouse defaults and stay inside the node view', () => {
  const editor = createEditor(makeBridge().bridge, {
    type: 'doc',
    content: [{ type: 'image', attrs: { assetId: IMAGE_ID, displayWidthPx: 500 } }, { type: 'paragraph' }],
  });
  const fakeDocument = new TestDocument();
  const originalDocument = Object.getOwnPropertyDescriptor(globalThis, 'document');
  Object.defineProperty(globalThis, 'document', { configurable: true, value: fakeDocument });
  let nodeView: ReturnType<typeof createImageNodeView> | undefined;

  try {
    nodeView = createImageNodeView(editor, fakeDocument);
    const root = nodeView.dom as TestElement;
    const widthInput = findTestElement(root, 'rich-image-width')!;
    const resizeHandle = findTestElement(root, 'rich-image-resize-handle')!;
    assert.equal(widthInput.value, '500');
    assert.equal(resizeHandle.value, '500');
    assert.ok(
      resizeHandle.propertyWrites.indexOf('max') < resizeHandle.propertyWrites.indexOf('value'),
      'range max must be assigned before its value to avoid browser clamping',
    );
    for (const testId of ['rich-image-width', 'rich-image-resize-handle']) {
      const control = findTestElement(root, testId);
      assert.ok(control, `missing ${testId}`);
      const event = new Event('mousedown', { bubbles: true, cancelable: true });
      control.dispatchEvent(event);
      assert.equal(event.defaultPrevented, false, `${testId} should retain its native mouse default`);
      assert.equal(nodeView.stopEvent?.(event), true, `${testId} must not move the editor selection`);
    }
  } finally {
    nodeView?.destroy?.();
    editor.destroy();
    if (originalDocument) Object.defineProperty(globalThis, 'document', originalDocument);
    else Reflect.deleteProperty(globalThis, 'document');
  }
});

test('image controls follow editor editability changes without document transactions', () => {
  const editor = createEditor(makeBridge().bridge, {
    type: 'doc',
    content: [{ type: 'image', attrs: { assetId: IMAGE_ID, displayWidthPx: 500 } }, { type: 'paragraph' }],
  }, undefined, false);
  const fakeDocument = new TestDocument();
  const originalDocument = Object.getOwnPropertyDescriptor(globalThis, 'document');
  Object.defineProperty(globalThis, 'document', { configurable: true, value: fakeDocument });
  let nodeView: ReturnType<typeof createImageNodeView> | undefined;

  try {
    nodeView = createImageNodeView(editor, fakeDocument);
    const root = nodeView.dom as TestElement;
    const controls = root.children.find(child => child.className === 'rich-image-controls');
    const widthInput = findTestElement(root, 'rich-image-width')!;
    const resizeHandle = findTestElement(root, 'rich-image-resize-handle')!;
    const moveUp = findTestElement(root, 'rich-image-move-up')!;
    const moveDown = findTestElement(root, 'rich-image-move-down')!;
    const remove = findTestElement(root, 'rich-image-delete')!;
      const originalDocument = editor.state.doc;

    assert.equal(controls?.hidden, true);
      assert.equal(widthInput.value, '500');
    assert.equal(widthInput.disabled, true);
    assert.equal(resizeHandle.disabled, true);
    assert.equal(moveUp.disabled, true);
    assert.equal(moveDown.disabled, true);
    assert.equal(remove.disabled, true);

    editor.setEditable(true, false);
      refreshImageNodeViews(editor);
    assert.equal(controls?.hidden, false);
      assert.equal(widthInput.value, '500');
    assert.equal(widthInput.disabled, false);
    assert.equal(resizeHandle.disabled, false);
    assert.equal(moveUp.disabled, true);
    assert.equal(moveDown.disabled, false);
    assert.equal(remove.disabled, false);

    editor.setEditable(false, false);
      refreshImageNodeViews(editor);
    assert.equal(controls?.hidden, true);
      assert.equal(widthInput.value, '500');
    assert.equal(widthInput.disabled, true);
    assert.equal(resizeHandle.disabled, true);
    assert.equal(moveUp.disabled, true);
    assert.equal(moveDown.disabled, true);
    assert.equal(remove.disabled, true);

    nodeView.destroy?.();
    nodeView = undefined;
    editor.setEditable(true, false);
      refreshImageNodeViews(editor);
    assert.equal(controls?.hidden, true, 'destroyed NodeViews must unsubscribe from permission updates');
    assert.equal(widthInput.disabled, true);
      assert.equal(editor.state.doc, originalDocument, 'permission changes must not create document transactions');
  } finally {
    nodeView?.destroy?.();
    editor.destroy();
    if (originalDocument) Object.defineProperty(globalThis, 'document', originalDocument);
    else Reflect.deleteProperty(globalThis, 'document');
  }
});

test('move controls refresh when public editor transactions shift an unchanged image node', () => {
  const bridge = makeBridge();
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [{ type: 'image', attrs: { assetId: IMAGE_ID, displayWidthPx: 400 } }, { type: 'paragraph' }],
  });
  const fakeDocument = new TestDocument();
  const originalDocument = Object.getOwnPropertyDescriptor(globalThis, 'document');
  Object.defineProperty(globalThis, 'document', { configurable: true, value: fakeDocument });
  let nodeView: ReturnType<typeof createImageNodeView> | undefined;
  let invalidPosition = false;

  try {
    nodeView = createImageNodeView(editor, fakeDocument, () => invalidPosition ? undefined : imagePosition(editor));
    const root = nodeView.dom as TestElement;
    const moveUp = findTestElement(root, 'rich-image-move-up')!;
    const moveDown = findTestElement(root, 'rich-image-move-down')!;
    assert.equal(moveUp.disabled, true);
    assert.equal(moveDown.disabled, false);

    assert.equal(editor.commands.insertContentAt(0, { type: 'paragraph' }), true);
    assert.equal(imagePosition(editor), 2);
    assert.equal(moveUp.disabled, false);
    assert.equal(moveDown.disabled, false);

    assert.equal(editor.commands.deleteRange({ from: 0, to: 2 }), true);
    assert.equal(imagePosition(editor), 0);
    assert.equal(moveUp.disabled, true);
    assert.equal(editor.commands.deleteRange({ from: 1, to: 3 }), true);
    assert.equal(editor.state.doc.childCount, 1);
    assert.equal(moveDown.disabled, true);

    invalidPosition = true;
    assert.equal(editor.commands.insertContentAt(0, { type: 'paragraph' }), true);
    assert.equal(moveUp.disabled, true);
    assert.equal(moveDown.disabled, true);

    invalidPosition = false;
    nodeView.destroy?.();
    nodeView = undefined;
    assert.equal(editor.commands.insertContentAt(0, { type: 'paragraph' }), true);
    assert.equal(moveUp.disabled, true, 'destroyed NodeViews must unsubscribe from editor transactions');
  } finally {
    nodeView?.destroy?.();
    editor.destroy();
    if (originalDocument) Object.defineProperty(globalThis, 'document', originalDocument);
    else Reflect.deleteProperty(globalThis, 'document');
  }
});

test('a reused image transaction stages one file even when DataTransfer returns separate wrappers', async () => {
  const bridge = makeBridge();
  const editor = createEditor(bridge.bridge);
  const source = new Blob(['identical png bytes'], { type: 'image/png' });
  const secondWrapper = new Blob(['identical png bytes'], { type: 'image/png' });

  try {
    assert.equal(paste(editor, makeCustomPaste({
      files: [source],
      items: [{ kind: 'file', type: 'image/png', getAsFile: () => secondWrapper }],
    })), true);
    await bridge.wait();
    assert.equal(bridge.staged, source);
    assert.equal(fromEditorDocument(editor.getJSON(), false).filter(block => block.type === 'image').length, 1);
  } finally {
    editor.destroy();
  }
});

test('unsupported formats and remote HTML images report an error without creating image nodes', async () => {
  const errors: string[] = [];
  const bridge = makeBridge();
  const editor = createEditor(bridge.bridge, undefined, { reportError: message => errors.push(message) });

  try {
    assert.equal(paste(editor, makeImagePaste(new Blob(['gif'], { type: 'image/gif' }))), true);
    assert.equal(paste(editor, makeCustomPaste({ html: '<p><img src="https://example.invalid/photo.png"></p>' })), true);
    await bridge.wait();
    assert.equal(bridge.staged, undefined);
    assert.equal(imagePosition(editor), -1);
    assert.match(errors.join('\n'), /不支持此图片格式/);
    assert.match(errors.join('\n'), /没有可保存的本地文件/);
  } finally {
    editor.destroy();
  }
});

test('string clipboard image MIME types are rejected while ordinary text remains unhandled', () => {
  const errors: string[] = [];
  const bridge = makeBridge();
  const editor = createEditor(bridge.bridge, undefined, { reportError: message => errors.push(message) });

  try {
    for (const type of ['image/svg+xml', 'image/gif']) {
      assert.equal(paste(editor, makeCustomPaste({
        items: [{ kind: 'string', type, getAsFile: () => null }],
      })), true);
    }
    assert.equal(errors.filter(message => /不支持此图片格式/.test(message)).length, 2);
    assert.equal(paste(editor, makeCustomPaste({
      items: [{ kind: 'string', type: 'text/plain', getAsFile: () => null }],
      plainText: 'ordinary text',
    })), false);
    assert.equal(errors.length, 2);
    assert.equal(bridge.staged, undefined);
    assert.equal(imagePosition(editor), -1);
  } finally {
    editor.destroy();
  }
});

test('image width, movement, and deletion each form one undoable history action', () => {
  const bridge = makeBridge();
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [
      { type: 'paragraph', content: [{ type: 'text', text: 'before' }] },
      { type: 'image', attrs: { assetId: IMAGE_ID, displayWidthPx: 400 } },
      { type: 'paragraph', content: [{ type: 'text', text: 'after' }] },
    ],
  });

  try {
    const image = () => fromEditorDocument(editor.getJSON(), false).find(block => block.type === 'image');
    const position = imagePosition(editor);
    assert.equal(editor.commands.setImageDisplayWidth(position, 640), true);
    assert.deepEqual(image(), { type: 'image', assetId: IMAGE_ID, displayWidthPx: 640 });
    assert.equal(editor.commands.undo(), true);
    assert.deepEqual(image(), { type: 'image', assetId: IMAGE_ID, displayWidthPx: 400 });
    assert.equal(editor.commands.redo(), true);
    assert.deepEqual(image(), { type: 'image', assetId: IMAGE_ID, displayWidthPx: 640 });

    assert.equal(editor.commands.moveImageNode(imagePosition(editor), 'up'), true);
    assert.equal(fromEditorDocument(editor.getJSON(), false)[0]?.type, 'image');
    assert.equal(editor.commands.undo(), true);
    assert.equal(fromEditorDocument(editor.getJSON(), false)[1]?.type, 'image');
    assert.equal(editor.commands.redo(), true);
    assert.equal(fromEditorDocument(editor.getJSON(), false)[0]?.type, 'image');

    assert.equal(editor.commands.moveImageNode(imagePosition(editor), 'down'), true);
    assert.equal(fromEditorDocument(editor.getJSON(), false)[1]?.type, 'image');
    assert.equal(editor.commands.moveImageNode(imagePosition(editor), 'down'), true);
    const movedToEnd = fromEditorDocument(editor.getJSON(), false);
    assert.equal(movedToEnd.at(-2)?.type, 'image');
    assert.deepEqual(movedToEnd.at(-1), { type: 'paragraph', children: [] });
    assert.equal(editor.commands.undo(), true);
    assert.equal(fromEditorDocument(editor.getJSON(), false)[1]?.type, 'image');

    assert.equal(editor.commands.deleteImageNode(imagePosition(editor)), true);
    assert.equal(imagePosition(editor), -1);
    assert.equal(editor.commands.undo(), true);
    assert.notEqual(imagePosition(editor), -1);
  } finally {
    editor.destroy();
  }
});

test('image filters prevent wrapping an image in a list and parsing fake HTML asset ids', () => {
  const errors: string[] = [];
  const bridge = makeBridge();
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [{ type: 'image', attrs: { assetId: IMAGE_ID, displayWidthPx: 400 } }, { type: 'paragraph' }],
  }, { reportError: message => errors.push(message) });

  try {
    const original = editor.getJSON();
    const image = editor.state.doc.firstChild!;
    const listItem = editor.schema.nodes.listItem.create(null, [image]);
    const bulletList = editor.schema.nodes.bulletList.create(null, [listItem]);
    (editor.view as EditorView).dispatch(editor.state.tr.replaceWith(0, image.nodeSize, bulletList));
    assert.deepEqual(editor.getJSON(), original);
    assert.match(errors.join('\n'), /图片只能放在正文顶层/);

    assert.equal(editor.schema.nodes.image.spec.parseDOM?.length ?? 0, 0);
    assert.equal(editor.schema.nodes.image.spec.marks, '');
  } finally {
    editor.destroy();
  }
});

test('empty editor defaults and empty paragraph Enter do not synthesize invalid images', () => {
  const errors: string[] = [];
  const bridge = makeBridge();
  const editor = createEditor(bridge.bridge, {
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'text', text: 'clear me' }] }],
  }, { reportError: message => errors.push(message) });

  try {
    const defaultDoc = editor.schema.topNodeType.createAndFill();
    assert.ok(defaultDoc);
    assert.equal(defaultDoc.firstChild?.type.name, 'paragraph');

    assert.equal(editor.commands.selectAll(), true);
    assert.equal(editor.commands.deleteSelection(), true);
    assert.equal(errors.length, 0);

    assert.equal(editor.commands.setContent(defaultDoc.toJSON()), true);
    assert.equal(editor.state.doc.firstChild?.type.name, 'paragraph');
    editor.commands.setTextSelection(1);
    assert.equal(editor.commands.splitBlock(), true);
    assert.equal(errors.length, 0);
    assert.equal(editor.state.doc.firstChild?.type.name, 'paragraph');
  } finally {
    editor.destroy();
  }
});
