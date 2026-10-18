import { Extension, Node, mergeAttributes, type Editor, type Extensions, type NodeViewRendererProps } from '@tiptap/core';
import { closeHistory } from '@tiptap/pm/history';
import { Fragment, type Node as ProseMirrorNode } from '@tiptap/pm/model';
import { NodeSelection, Plugin, PluginKey, Selection, TextSelection, type EditorState, type Transaction } from '@tiptap/pm/state';
import type { EditorView } from '@tiptap/pm/view';
import type { JSONContent } from '@tiptap/core';
import type { AssetManifestEntry } from './generated/note-format.ts';
import { fromEditorDocument } from './editor-model.ts';
import type { ImageEditorBridge } from './image-session.ts';

const imagePastePriority = 1100;
const maximumPreflightRetries = 24;
const defaultViewportWidth = 800;
const destroyedEditors = new WeakSet<Editor>();
const observedEditors = new WeakSet<Editor>();
const imageControlRefreshers = new WeakMap<Editor, Set<() => void>>();

function registerImageControlRefresher(editor: Editor, refresh: () => void) {
  let refreshers = imageControlRefreshers.get(editor);
  if (!refreshers) {
    refreshers = new Set();
    imageControlRefreshers.set(editor, refreshers);
  }
  refreshers.add(refresh);

  let registered = true;
  return () => {
    if (!registered) return;
    registered = false;
    const current = imageControlRefreshers.get(editor);
    current?.delete(refresh);
    if (current?.size === 0) imageControlRefreshers.delete(editor);
  };
}

export function refreshImageNodeViews(editor: Editor) {
  for (const refresh of imageControlRefreshers.get(editor) ?? []) refresh();
}

function observeEditorDestruction(editor: Editor) {
  if (observedEditors.has(editor)) return;
  observedEditors.add(editor);
  editor.on('destroy', () => destroyedEditors.add(editor));
}

function editorWasDestroyed(editor: Editor) {
  observeEditorDestruction(editor);
  return destroyedEditors.has(editor);
}

export interface ImageEditorRuntime {
  getBridge?: () => ImageEditorBridge | undefined;
  canEdit?: () => boolean;
  isCurrent?: (editor: Editor) => boolean;
  reportError?: (message: string) => void;
  reportStatus?: (message: string) => void;
}

interface PasteTarget {
  position: number;
  blockStart: number;
  blockEnd: number;
  valid: boolean;
  nestedLabel: string | null;
}

type PasteTicketChange =
  | { type: 'add'; id: number; target: PasteTarget }
  | { type: 'remove'; id: number };

declare module '@tiptap/core' {
  interface Commands<ReturnType> {
    cuenoteImage: {
      setImageDisplayWidth: (position: number, width: number) => ReturnType;
      moveImageNode: (position: number, direction: 'up' | 'down') => ReturnType;
      deleteImageNode: (position: number) => ReturnType;
    };
  }
}

function errorMessage(cause: unknown): string {
  if (cause instanceof Error) return cause.message;
  if (typeof cause === 'object' && cause !== null && 'message' in cause) {
    const message = (cause as { message?: unknown }).message;
    if (typeof message === 'string' && message.trim().length > 0) return message;
  }
  return String(cause);
}

function bridgeFor(runtime: ImageEditorRuntime | undefined, fallback?: ImageEditorBridge) {
  return runtime?.getBridge?.() ?? fallback;
}

function editorIsCurrent(runtime: ImageEditorRuntime | undefined, editor: Editor, view: EditorView) {
  return !editorWasDestroyed(editor) && !view.isDestroyed && (runtime?.isCurrent?.(editor) ?? true);
}

function canEditImage(runtime: ImageEditorRuntime | undefined, editor: Editor) {
  return editor.isEditable && (runtime?.canEdit?.() ?? true);
}

function reportImageError(runtime: ImageEditorRuntime | undefined, cause: unknown) {
  runtime?.reportError?.(errorMessage(cause));
}

function findNestedLabel(selection: Editor['state']['selection']): string | null {
  for (let depth = 1; depth <= selection.$head.depth; depth += 1) {
    const name = selection.$head.node(depth).type.name;
    if (name === 'bulletList' || name === 'orderedList' || name === 'taskList') return '列表';
    if (name === 'blockquote') return '引用';
    if (name === 'table') return '表格';
  }
  return null;
}

function capturePasteTarget(view: EditorView): PasteTarget {
  const { $head } = view.state.selection;
  const blockStart = $head.depth > 0 ? $head.before(1) : $head.pos;
  const blockEnd = $head.depth > 0 ? $head.after(1) : $head.pos;
  return {
    position: blockEnd,
    blockStart,
    blockEnd,
    valid: true,
    nestedLabel: findNestedLabel(view.state.selection),
  };
}

function enumerateClipboardImages(clipboard: DataTransfer) {
  const scan = (candidates: Array<{ file: Blob | null; declaredType: string; name: string }>) => {
    const files: Blob[] = [];
    let containsImage = false;
    let missingImageFile = false;
    let unsupported = false;
    const consider = (file: Blob | null, declaredType: string, name: string) => {
      const type = (declaredType || file?.type || '').trim().toLowerCase();
      const extension = /\.([^.]+)$/.exec(name.toLowerCase())?.[1] ?? '';
      const looksLikeImage = type.startsWith('image/')
        || ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'tif', 'tiff'].includes(extension);
      if (!looksLikeImage) return;

      containsImage = true;
      const supportedType = type === 'image/png' || type === 'image/jpeg' || type === 'image/jpg';
      const supportedExtension = type.length === 0 && ['png', 'jpg', 'jpeg'].includes(extension);
      if (!supportedType && !supportedExtension) {
        unsupported = true;
        return;
      }
      if (!file) {
        missingImageFile = true;
        return;
      }

      files.push(file);
    };

    candidates.forEach(({ file, declaredType, name }) => consider(file, declaredType, name));
    return { files, containsImage, missingImageFile, unsupported };
  };

  const fileList = scan(Array.from(clipboard.files ?? []).map(file => ({
    file,
    declaredType: file.type,
    name: 'name' in file && typeof file.name === 'string' ? file.name : '',
  })));
  if (fileList.files.length > 0 || fileList.containsImage) return fileList;

  return scan(Array.from(clipboard.items ?? [])
    .filter(item => item.kind === 'file' || (item.kind === 'string' && item.type.trim().toLowerCase().startsWith('image/')))
    .map(item => ({ file: item.kind === 'file' ? item.getAsFile() : null, declaredType: item.type, name: '' })));
}

function clipboardContainsBodyText(clipboard: DataTransfer) {
  if (clipboard.getData('text/plain').length > 0) return true;
  const html = clipboard.getData('text/html');
  if (html.length === 0) return false;
  return html.replace(/<img\b[^>]*>/gi, '').replace(/<[^>]*>/g, '').trim().length > 0;
}

function viewportWidth(view: EditorView) {
  try {
    const width = Math.floor(view.dom.clientWidth || view.dom.parentElement?.clientWidth || defaultViewportWidth);
    return Math.max(1, width);
  } catch {
    return defaultViewportWidth;
  }
}

function imageDisplayWidth(view: EditorView, width: number) {
  return Math.min(width, viewportWidth(view));
}

function validateManifest(entry: AssetManifestEntry) {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(entry.id)) {
    throw new Error('图片服务返回了无效的图片标识，图片没有插入。');
  }
  if (entry.mediaType !== 'image/png' && entry.mediaType !== 'image/jpeg') {
    throw new Error('不支持此图片格式。请粘贴 PNG 或 JPEG 图片。');
  }
  if (!Number.isInteger(entry.width) || entry.width < 1 || entry.width > 0xffff_ffff
    || !Number.isInteger(entry.height) || entry.height < 1 || entry.height > 0xffff_ffff) {
    throw new Error('无法读取图片尺寸，图片没有插入。');
  }
  return entry;
}

function buildImageTransaction(
  editor: Editor,
  view: EditorView,
  key: PluginKey<Map<number, PasteTarget>>,
  ticketId: number,
  entries: Array<{ id: string; mediaType: string; width: number; height: number }>,
) {
  const target = key.getState(editor.state)?.get(ticketId);
  if (!target?.valid) throw new Error('图片插入已取消，因为目标内容块已被删除。');

  const imageType = editor.schema.nodes.image;
  const paragraphType = editor.schema.nodes.paragraph;
  if (!imageType || !paragraphType) throw new Error('图片节点不可用，图片没有插入。');

  const images = entries.map(entry => imageType.create({
    assetId: entry.id,
    displayWidthPx: imageDisplayWidth(view, entry.width),
  }));
  const fragment = Fragment.fromArray(images);
  const transaction = editor.state.tr.insert(target.position, fragment);
  if (transaction.doc.lastChild?.type === imageType) {
    const trailingParagraph = paragraphType.createAndFill();
    if (!trailingParagraph) throw new Error('无法创建图片后的编辑段落。');
    transaction.insert(transaction.doc.content.size, trailingParagraph);
  }

  const afterImages = target.position + fragment.size;
  const selectionPosition = Math.min(afterImages + 1, transaction.doc.content.size);
  transaction.setSelection(TextSelection.near(transaction.doc.resolve(selectionPosition), 1));
  return { transaction: transaction.scrollIntoView(), nestedLabel: target.nestedLabel };
}

function topLevelImageIndex(doc: ProseMirrorNode, position: number) {
  let offset = 0;
  for (let index = 0; index < doc.childCount; index += 1) {
    const child = doc.child(index);
    if (offset === position) return child.type.name === 'image' ? index : -1;
    offset += child.nodeSize;
  }
  return -1;
}

function moveImageTransaction(state: EditorState, position: number, direction: 'up' | 'down') {
  const index = topLevelImageIndex(state.doc, position);
  if (index < 0) return null;
  const adjacentIndex = index + (direction === 'up' ? -1 : 1);
  if (adjacentIndex < 0 || adjacentIndex >= state.doc.childCount) return null;

  const image = state.doc.child(index);
  const adjacent = state.doc.child(adjacentIndex);
  let adjacentPosition = 0;
  for (let childIndex = 0; childIndex < adjacentIndex; childIndex += 1) {
    adjacentPosition += state.doc.child(childIndex).nodeSize;
  }
  const insertPosition = direction === 'up' ? adjacentPosition : position + adjacent.nodeSize;
  const transaction = state.tr.delete(position, position + image.nodeSize).insert(insertPosition, image);
  if (transaction.doc.lastChild?.type.name === 'image') {
    const paragraph = state.schema.nodes.paragraph.createAndFill();
    if (!paragraph) return null;
    transaction.insert(transaction.doc.content.size, paragraph);
  }
  transaction.setSelection(NodeSelection.create(transaction.doc, insertPosition));
  return transaction.scrollIntoView();
}

function deleteImageTransaction(state: EditorState, position: number, imageType: ProseMirrorNode['type']) {
  if (topLevelImageIndex(state.doc, position) < 0) return null;
  const image = state.doc.nodeAt(position);
  if (!image || image.type !== imageType) return null;
  const transaction = state.tr.delete(position, position + image.nodeSize);
  transaction.setSelection(Selection.near(transaction.doc.resolve(position), 1));
  return transaction.scrollIntoView();
}

function dispatchImageTransaction(editor: Editor, transaction: Transaction) {
  editor.view.dispatch(closeHistory(editor.state.tr));
  editor.view.dispatch(closeHistory(transaction));
  editor.view.dispatch(closeHistory(editor.state.tr));
}

async function waitForComposition(editor: Editor, view: EditorView) {
  while (view.composing && !editorWasDestroyed(editor) && !view.isDestroyed) {
    await new Promise<void>(resolve => {
      let completed = false;
      const finish = () => {
        if (completed) return;
        completed = true;
        view.dom.removeEventListener('compositionend', finish);
        editor.off('destroy', finish);
        resolve();
      };
      view.dom.addEventListener('compositionend', finish, { once: true });
      editor.on('destroy', finish);
      if (!view.composing || view.isDestroyed || editorWasDestroyed(editor)) finish();
    });
    await new Promise<void>(resolve => setTimeout(resolve, 0));
  }
}

function createImageNodeView(
  props: NodeViewRendererProps,
  fallbackBridge: ImageEditorBridge | undefined,
  runtime: ImageEditorRuntime | undefined,
) {
  const { editor, node, getPos } = props;
  const root = document.createElement('figure');
  root.className = 'rich-image-block';
  root.setAttribute('data-testid', 'rich-image-block');
  root.setAttribute('contenteditable', 'false');
  root.draggable = false;

  const image = document.createElement('img');
  image.className = 'rich-image-preview';
  image.alt = '笔记图片';
  image.draggable = false;
  root.append(image);

  const controls = document.createElement('div');
  controls.className = 'rich-image-controls';
  controls.setAttribute('role', 'group');
  controls.setAttribute('aria-label', '图片操作');
  controls.hidden = !canEditImage(runtime, editor);

  const widthLabel = document.createElement('label');
  widthLabel.className = 'rich-image-width-control';
  widthLabel.textContent = '宽度';
  const widthInput = document.createElement('input');
  widthInput.type = 'number';
  widthInput.min = '1';
  widthInput.max = '4294967295';
  widthInput.step = '1';
  widthInput.setAttribute('data-testid', 'rich-image-width');
  widthInput.setAttribute('aria-label', '图片显示宽度（像素）');
  widthLabel.append(widthInput);
  controls.append(widthLabel);

  const resizeHandle = document.createElement('input');
  resizeHandle.type = 'range';
  resizeHandle.min = '1';
  resizeHandle.setAttribute('data-testid', 'rich-image-resize-handle');
  resizeHandle.setAttribute('aria-label', '拖动调整图片宽度');
  controls.append(resizeHandle);

  const actionButton = (action: 'move-up' | 'move-down' | 'delete', label: string) => {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'rich-image-action';
    button.setAttribute('data-testid', `rich-image-${action}`);
    button.setAttribute('aria-label', label);
    button.textContent = label;
    controls.append(button);
    return button;
  };
  const moveUp = actionButton('move-up', '上移');
  const moveDown = actionButton('move-down', '下移');
  const remove = actionButton('delete', '删除');
  root.append(controls);

  const error = document.createElement('figcaption');
  error.className = 'rich-image-error';
  error.setAttribute('data-testid', 'rich-image-error');
  error.setAttribute('role', 'alert');
  error.hidden = true;
  root.append(error);

  const retry = document.createElement('button');
  retry.type = 'button';
  retry.className = 'rich-image-retry';
  retry.setAttribute('data-testid', 'rich-image-retry');
  retry.textContent = '重试读取图片';
  retry.hidden = true;
  root.append(retry);

  let currentNode = node;
  let blobUrl: string | null = null;
  let destroyed = false;
  let loadSequence = 0;
  let displayedRequest = 0;
  let hasAttemptedLoad = false;
  let visibilityObserver: IntersectionObserver | null = null;
  observeEditorDestruction(editor);

  const currentPosition = () => {
    try {
      const position = getPos();
      return typeof position === 'number' ? position : null;
    } catch {
      return null;
    }
  };

  const currentTopLevelPosition = () => {
    const position = currentPosition();
    if (position === null || !Number.isInteger(position) || position < 0) return null;
    const doc = editor.state.doc;
    if (position + currentNode.nodeSize > doc.content.size) return null;
    return position;
  };

  const updateControls = () => {
    const editable = canEditImage(runtime, editor);
    const position = currentTopLevelPosition();
    const docSize = editor.state.doc.content.size;
    controls.hidden = !editable;
    widthInput.disabled = !editable;
    resizeHandle.disabled = !editable;
    moveUp.disabled = !editable || position === null || position <= 0;
    moveDown.disabled = !editable || position === null || position + currentNode.nodeSize >= docSize;
    remove.disabled = !editable;
  };

  const refreshMoveControls = ({ transaction }: { transaction: Transaction }) => {
    if (transaction.docChanged) updateControls();
  };
  editor.on('transaction', refreshMoveControls);
  const unregisterImageControlRefresher = registerImageControlRefresher(editor, updateControls);

  const runImageCommand = (command: () => boolean) => {
    if (!canEditImage(runtime, editor)) return;
    try {
      command();
    } catch (cause) {
      reportImageError(runtime, cause);
    }
  };

  const commitWidth = (rawWidth: string) => {
    const width = Number(rawWidth);
    const position = currentPosition();
    if (position === null || !Number.isInteger(width) || width < 1 || width > 0xffff_ffff) {
      widthInput.value = String(currentNode.attrs.displayWidthPx);
      resizeHandle.value = widthInput.value;
      return;
    }
    runImageCommand(() => editor.commands.setImageDisplayWidth(position, width));
  };

  const stopMouseDown = (event: Event) => event.preventDefault();
  widthInput.addEventListener('change', () => commitWidth(widthInput.value));
  resizeHandle.addEventListener('input', () => {
    image.style.width = `${Number(resizeHandle.value)}px`;
  });
  resizeHandle.addEventListener('change', () => commitWidth(resizeHandle.value));

  const bindAction = (button: HTMLButtonElement, command: (position: number) => boolean) => {
    button.addEventListener('mousedown', stopMouseDown);
    button.addEventListener('click', event => {
      event.preventDefault();
      event.stopPropagation();
      const position = currentPosition();
      if (position !== null) runImageCommand(() => command(position));
    });
  };
  bindAction(moveUp, position => editor.commands.moveImageNode(position, 'up'));
  bindAction(moveDown, position => editor.commands.moveImageNode(position, 'down'));
  bindAction(remove, position => {
    const deleted = editor.commands.deleteImageNode(position);
    // The focused width control or delete button disappears with this NodeView.
    // Return keyboard shortcuts to the body after the successful transaction.
    if (deleted) editor.view.focus();
    return deleted;
  });

  const setMessage = (message: string | null) => {
    error.textContent = message ?? '';
    error.hidden = message === null;
    retry.hidden = message === null;
  };

  const revokeBlobUrl = () => {
    if (!blobUrl) return;
    URL.revokeObjectURL(blobUrl);
    blobUrl = null;
    image.removeAttribute('src');
  };

  const applyWidth = () => {
    root.setAttribute('data-asset-id', String(currentNode.attrs.assetId));
    const width = Number(currentNode.attrs.displayWidthPx);
    image.style.width = `${width}px`;
    image.style.maxWidth = '100%';
    image.style.height = 'auto';
    resizeHandle.max = String(Math.min(0xffff_ffff, Math.max(width, image.naturalWidth || 0, width * 2)));
    widthInput.value = String(width);
    resizeHandle.value = String(width);
    updateControls();
  };

  const loadAsset = async () => {
    if (destroyed) return;
    hasAttemptedLoad = true;
    const request = ++loadSequence;
    const assetId = String(currentNode.attrs.assetId);
    setMessage(null);
    try {
      const bridge = bridgeFor(runtime, fallbackBridge);
      if (!bridge) throw new Error('图片服务当前不可用。');
      const blob = await bridge.read(assetId);
      if (destroyed || request !== loadSequence || editorWasDestroyed(editor)) return;
      const nextUrl = URL.createObjectURL(blob);
      revokeBlobUrl();
      blobUrl = nextUrl;
      displayedRequest = request;
      image.src = nextUrl;
    } catch (cause) {
      if (destroyed || request !== loadSequence || editorWasDestroyed(editor)) return;
      const message = `图片读取失败：${errorMessage(cause)}`;
      setMessage(message);
      runtime?.reportError?.(message);
    }
  };

  const stopObservingVisibility = () => {
    visibilityObserver?.disconnect();
    visibilityObserver = null;
  };

  const requestAssetLoad = () => {
    if (destroyed) return;
    stopObservingVisibility();
    void loadAsset();
  };

  retry.addEventListener('mousedown', event => event.preventDefault());
  retry.addEventListener('click', event => {
    event.preventDefault();
    event.stopPropagation();
    requestAssetLoad();
  });

  applyWidth();
  image.addEventListener('load', updateControls);
  image.addEventListener('error', () => {
    if (destroyed || displayedRequest !== loadSequence) return;
    const message = '图片预览失败：浏览器无法解码或显示此图片。';
    setMessage(message);
    runtime?.reportError?.(message);
  });
  const IntersectionObserverConstructor = root.ownerDocument.defaultView?.IntersectionObserver;
  if (IntersectionObserverConstructor) {
    visibilityObserver = new IntersectionObserverConstructor(entries => {
      if (entries.some(entry => entry.isIntersecting)) requestAssetLoad();
    }, { rootMargin: '200px' });
    visibilityObserver.observe(root);
  } else {
    requestAssetLoad();
  }

  return {
    dom: root,
    update(nextNode: ProseMirrorNode) {
      if (nextNode.type !== currentNode.type) return false;
      const assetChanged = nextNode.attrs.assetId !== currentNode.attrs.assetId;
      currentNode = nextNode;
      applyWidth();
      if (assetChanged) {
        loadSequence += 1;
        revokeBlobUrl();
        if (hasAttemptedLoad) requestAssetLoad();
      }
      return true;
    },
    stopEvent(event: Event) {
      const target = event.target;
      const ElementNode = root.ownerDocument.defaultView?.Node;
      return target !== null && ElementNode !== undefined && target instanceof ElementNode && root.contains(target);
    },
    ignoreMutation() { return true; },
    destroy() {
      destroyed = true;
      editor.off('transaction', refreshMoveControls);
      unregisterImageControlRefresher();
      loadSequence += 1;
      stopObservingVisibility();
      image.removeEventListener('load', updateControls);
      if (root.isConnected && root.ownerDocument.defaultView) {
        image.removeAttribute('src');
      }
      revokeBlobUrl();
    },
  };
}

function createImageNode(bridge: ImageEditorBridge | undefined, runtime: ImageEditorRuntime | undefined): Extensions[number] {
  return Node.create({
    name: 'image',
    group: 'block',
    atom: true,
    selectable: true,
    draggable: false,
    isolating: true,
    marks: '',

    addAttributes() {
      return {
        assetId: { default: null },
        displayWidthPx: { default: null },
      };
    },

    parseHTML() {
      return [];
    },

    renderHTML({ HTMLAttributes }) {
      return ['figure', mergeAttributes({ 'data-note-image': '' }, HTMLAttributes)];
    },

    addCommands() {
      return {
        setImageDisplayWidth: (position: number, width: number) => ({ editor, tr, dispatch }) => {
          if (!canEditImage(runtime, editor)) return false;
          if (!Number.isInteger(width) || width < 1 || width > 0xffff_ffff) return false;
          if (topLevelImageIndex(editor.state.doc, position) < 0) return false;
          const node = editor.state.doc.nodeAt(position);
          if (!node || node.type !== this.type || node.attrs.displayWidthPx === width) return false;
          if (!dispatch) return true;
          const transaction = editor.state.tr.setNodeMarkup(position, this.type, {
            ...node.attrs,
            displayWidthPx: width,
          });
          tr.setMeta('preventDispatch', true);
          dispatchImageTransaction(editor, transaction);
          return true;
        },
        moveImageNode: (position: number, direction: 'up' | 'down') => ({ editor, tr, dispatch }) => {
          if (!canEditImage(runtime, editor)) return false;
          const transaction = moveImageTransaction(editor.state, position, direction);
          if (!transaction) return false;
          if (!dispatch) return true;
          tr.setMeta('preventDispatch', true);
          dispatchImageTransaction(editor, transaction);
          return true;
        },
        deleteImageNode: (position: number) => ({ editor, tr, dispatch }) => {
          if (!canEditImage(runtime, editor)) return false;
          const transaction = deleteImageTransaction(editor.state, position, this.type);
          if (!transaction) return false;
          if (!dispatch) return true;
          tr.setMeta('preventDispatch', true);
          dispatchImageTransaction(editor, transaction);
          return true;
        },
      };
    },

    addNodeView() {
      return props => createImageNodeView(props, bridge, runtime);
    },
  });
}

function createImagePasteExtension(bridge: ImageEditorBridge | undefined, runtime: ImageEditorRuntime | undefined): Extensions[number] {
  const key = new PluginKey<Map<number, PasteTarget>>('cueNoteImagePasteTargets');
  let nextTicketId = 0;

  return Extension.create({
    name: 'cuenoteImagePaste',
    priority: imagePastePriority + 1,

    addProseMirrorPlugins() {
      const editor = this.editor;
      return [new Plugin<Map<number, PasteTarget>>({
        key,
        state: {
          init: () => new Map(),
          apply(transaction, previous) {
            const next = new Map<number, PasteTarget>();
            for (const [id, target] of previous) {
              const mapped = transaction.mapping.mapResult(target.position, 1);
              const mappedStart = transaction.mapping.mapResult(target.blockStart, 1);
              const mappedEnd = transaction.mapping.mapResult(target.blockEnd, -1);
              next.set(id, {
                ...target,
                position: mapped.pos,
                blockStart: mappedStart.pos,
                blockEnd: mappedEnd.pos,
                valid: target.valid && !mapped.deleted && !mapped.deletedAcross
                  && !mappedStart.deleted && !mappedEnd.deleted,
              });
            }
            const change = transaction.getMeta(key) as PasteTicketChange | undefined;
            if (change?.type === 'add') next.set(change.id, change.target);
            if (change?.type === 'remove') next.delete(change.id);
            return next;
          },
        },
        filterTransaction(transaction) {
          let nestedImage = false;
          let invalidImage = false;
          transaction.doc.descendants((node, _position, parent) => {
            if (node.type.name === 'image') {
              if (parent?.type.name !== 'doc') {
                nestedImage = true;
                return false;
              }
              if (node.marks.length > 0
                || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(node.attrs.assetId)
                || !Number.isInteger(node.attrs.displayWidthPx)
                || node.attrs.displayWidthPx < 1 || node.attrs.displayWidthPx > 0xffff_ffff) {
                invalidImage = true;
                return false;
              }
            }
            return !nestedImage && !invalidImage;
          });
          if (nestedImage) runtime?.reportError?.('图片只能放在正文顶层，无法移入列表、引用或表格中。');
          if (invalidImage) runtime?.reportError?.('图片数据无效，已阻止此操作，原笔记没有改变。');
          return !nestedImage && !invalidImage;
        },
        props: {
          handlePaste(view, event) {
            const clipboard = event.clipboardData;
            if (!clipboard) return false;

            const images = enumerateClipboardImages(clipboard);
            const html = clipboard.getData('text/html');
            const hasHtmlImage = /<img\b/i.test(html);
            const hasBodyText = clipboardContainsBodyText(clipboard);

            if (images.unsupported) {
              runtime?.reportError?.('暂不支持此图片格式，请使用 PNG 或 JPEG 图片。图片没有插入。');
              return !hasBodyText;
            }
            if (images.containsImage && images.missingImageFile) {
              runtime?.reportError?.('无法从剪贴板读取图片文件。请重新复制 PNG 或 JPEG 图片。');
              return !hasBodyText;
            }
            if (images.files.length === 0) {
              if (hasHtmlImage) {
                runtime?.reportError?.('剪贴板中的网页图片没有可保存的本地文件，请直接复制图片数据后重试。');
                return !hasBodyText;
              }
              return false;
            }

            if (!canEditImage(runtime, editor)) return !hasBodyText;
            const activeBridge = bridgeFor(runtime, bridge);
            if (!activeBridge) {
              runtime?.reportError?.('图片服务当前不可用，图片没有插入。');
              return !hasBodyText;
            }
            try {
              if (!activeBridge.canStart()) {
                runtime?.reportError?.('笔记正在切换或关闭，暂时不能开始新的图片粘贴。');
                return !hasBodyText;
              }
            } catch (cause) {
              reportImageError(runtime, cause);
              return !hasBodyText;
            }

            observeEditorDestruction(editor);
            const id = ++nextTicketId;
            const target = capturePasteTarget(view);
            view.dispatch(view.state.tr.setMeta(key, { type: 'add', id, target } satisfies PasteTicketChange));

            const isCurrent = () => editorIsCurrent(runtime, editor, view);
            const operation = Promise.resolve().then(async () => {
              if (!isCurrent()) return;

              const entries: Array<{ id: string; mediaType: string; width: number; height: number }> = [];
              for (const file of images.files) {
                const entry = validateManifest(await activeBridge.stage(file));
                if (!isCurrent()) return;
                entries.push(entry);
              }

              for (let attempt = 0; attempt < maximumPreflightRetries; attempt += 1) {
                if (!isCurrent()) return;
                await waitForComposition(editor, view);
                if (!isCurrent()) return;

                const currentDocument = editor.state.doc;
                const candidate = buildImageTransaction(editor, view, key, id, entries);
                const candidateBlocks = fromEditorDocument(candidate.transaction.doc.toJSON() as JSONContent, false);
                const commitIfCurrent = await activeBridge.preflight(candidateBlocks);
                if (!isCurrent()) return;

                if (view.composing) {
                  await waitForComposition(editor, view);
                  if (!isCurrent()) return;
                  continue;
                }
                if (editor.state.doc !== currentDocument || !commitIfCurrent()) {
                  await new Promise<void>(resolve => setTimeout(resolve, 0));
                  if (!isCurrent()) return;
                  continue;
                }

                if (!canEditImage(runtime, editor)) return;
                view.dispatch(closeHistory(view.state.tr));
                view.dispatch(candidate.transaction);
                view.dispatch(closeHistory(view.state.tr));
                if (candidate.nestedLabel) {
                  runtime?.reportStatus?.(`图片已插入${candidate.nestedLabel}这一顶层内容块之后。`);
                }
                return;
              }

              throw new Error('图片插入期间草稿持续变化，请稍后重试。');
            }).finally(() => {
              if (!editorWasDestroyed(editor) && !view.isDestroyed) {
                view.dispatch(view.state.tr.setMeta(key, { type: 'remove', id } satisfies PasteTicketChange));
              }
            });

            try {
              activeBridge.track(operation);
            } catch (cause) {
              reportImageError(runtime, cause);
            }
            void operation.catch(cause => reportImageError(runtime, cause));

            // A file-only paste is consumed here. If the clipboard also has text,
            // ProseMirror's ordinary paste path remains responsible for that text.
            return !hasBodyText;
          },
        },
      })];
    },
  });
}

export function createImageExtensions(
  bridge?: ImageEditorBridge,
  runtime?: ImageEditorRuntime,
): Extensions {
  return [createImageNode(bridge, runtime), createImagePasteExtension(bridge, runtime)];
}
