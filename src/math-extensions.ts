import { Extension, InputRule, type Editor, type Extensions, type NodeViewRenderer, type NodeViewRendererProps } from '@tiptap/core';
import { BlockMath, InlineMath } from '@tiptap/extension-mathematics';
import { Fragment, type Node as ProseMirrorNode, type NodeType } from '@tiptap/pm/model';
import { Plugin, PluginKey, Selection, type EditorState, type Transaction } from '@tiptap/pm/state';
import { AddMarkStep, AddNodeMarkStep, RemoveMarkStep, RemoveNodeMarkStep, type Step } from '@tiptap/pm/transform';
import { MathRenderer } from './math-renderer.ts';

const MAX_FALLBACK_SOURCE_LENGTH = 500;
const MAX_ERROR_HINT_LENGTH = 240;
const blockMathFenceKey = new PluginKey<number | null>('cueNoteStandaloneMathFence');
const activeFencePositions = new WeakMap<Editor, number>();

export type MathEditHandler = (node: ProseMirrorNode, position: number) => void;

function isEscaped(text: string, position: number): boolean {
  let backslashes = 0;
  for (let index = position - 1; index >= 0 && text[index] === '\\'; index -= 1) backslashes += 1;
  return backslashes % 2 === 1;
}

function findInlineMath(text: string) {
  const close = text.length - 1;
  if (close < 1 || text[close] !== '$' || text[close - 1] === '$' || isEscaped(text, close)) return null;

  let open = -1;
  for (let index = 0; index < close;) {
    if (text[index] !== '$' || isEscaped(text, index)) {
      index += 1;
      continue;
    }
    if (text[index + 1] === '$' || text[index - 1] === '$') {
      index += 1;
      continue;
    }
    if (open < 0) open = index;
    else open = -1;
    index += 1;
  }

  if (open < 0) return null;
  const latex = text.slice(open + 1, close);
  if (latex.includes('\n') || latex.includes('\r') || latex.trim().length === 0) return null;
  for (let index = 0; index < latex.length; index += 1) {
    if (latex[index] === '$' && !isEscaped(latex, index)) return null;
  }
  return { index: open, text: text.slice(open), data: { latex } };
}

function inlineMathIntersectsCodeSpan(text: string, from: number, to: number): boolean {
  let open: { start: number; length: number } | null = null;
  const spans: Array<{ from: number; to: number }> = [];

  for (let index = 0; index < text.length;) {
    if (text[index] !== '`' || isEscaped(text, index)) {
      index += 1;
      continue;
    }

    let runEnd = index + 1;
    while (text[runEnd] === '`') runEnd += 1;
    const runLength = runEnd - index;
    if (!open) {
      open = { start: index, length: runLength };
    } else if (open.length === runLength) {
      spans.push({ from: open.start, to: runEnd });
      open = null;
    }
    index = runEnd;
  }

  if (open) spans.push({ from: open.start, to: text.length });
  return spans.some(span => from < span.to && to > span.from);
}

function containsCodeMark(state: EditorState, from: number, to: number): boolean {
  let found = false;
  state.doc.nodesBetween(from, to, node => {
    if (node.isText && node.marks.some(mark => mark.type.spec.code === true || mark.type.name === 'code')) found = true;
  });
  return found;
}

function createInlineMathInputRule(type: NodeType): InputRule {
  return new InputRule({
    find: findInlineMath,
    handler: ({ state, range, match }) => {
      const latex = match.data?.latex;
      const input = typeof match.input === 'string' ? match.input : match[0];
      const matchStart = typeof match.input === 'string' && typeof match.index === 'number' ? match.index : 0;
      const matchEnd = matchStart + match[0].length;
      if (
        typeof latex !== 'string'
        || containsCodeMark(state, range.from, range.to)
        || inlineMathIntersectsCodeSpan(input, matchStart, matchEnd)
      ) return null;
      state.tr.replaceWith(range.from, range.to, type.create({ latex }));
    },
  });
}

function createMathNodeView(
  props: NodeViewRendererProps,
  editor: Editor,
  renderer: MathRenderer,
  displayMode: boolean,
  onEditMath?: MathEditHandler,
): ReturnType<NodeViewRenderer> {
  let node = props.node;
  let generation = 0;
  let destroyed = false;
  let cancelRender = () => {};
  const tagName = displayMode ? 'div' : 'span';
  const dom = document.createElement(tagName);
  const rendered = document.createElement(tagName);

  dom.className = displayMode ? 'cn-math cn-math-block' : 'cn-math cn-math-inline';
  dom.dataset.type = displayMode ? 'block-math' : 'inline-math';
  dom.dataset.mathState = 'pending';
  dom.contentEditable = 'false';
  rendered.className = 'cn-math-output';
  dom.appendChild(rendered);
  if (onEditMath) {
    dom.tabIndex = 0;
    dom.setAttribute('role', 'button');
    dom.setAttribute('aria-label', displayMode ? '编辑独立公式源码' : '编辑行内公式源码');
  }

  const sourcePreview = (latex: string): string => latex.length > MAX_FALLBACK_SOURCE_LENGTH
    ? `${latex.slice(0, MAX_FALLBACK_SOURCE_LENGTH)}…`
    : latex;

  const requestRender = (latex: string) => {
    generation += 1;
    const currentGeneration = generation;
    cancelRender();
    cancelRender = () => {};
    rendered.textContent = sourcePreview(latex);
    dom.dataset.mathState = 'pending';

    cancelRender = renderer.render(latex, displayMode, result => {
      if (destroyed || currentGeneration !== generation) return;
      if (result.html !== null) {
        rendered.innerHTML = result.html;
        dom.dataset.mathState = 'ready';
        return;
      }

      const hint = (result.error ?? '公式暂时无法排版；源码已保留。').slice(0, MAX_ERROR_HINT_LENGTH);
      rendered.textContent = `${sourcePreview(latex)}\n${hint}`;
      dom.dataset.mathState = 'error';
    });
  };

  const openSourceEditor = (event: Event) => {
    if (!onEditMath || !editor.isEditable) return;
    event.preventDefault();
    event.stopPropagation();
    const position = props.getPos();
    if (typeof position === 'number') onEditMath(node, position);
  };
  const handleClick = (event: Event) => openSourceEditor(event);
  const handleKeyDown = (event: Event) => {
    const key = (event as KeyboardEvent).key;
    if (key === 'Enter' || key === ' ' || key === 'Spacebar') openSourceEditor(event);
  };

  dom.addEventListener('click', handleClick);
  dom.addEventListener('keydown', handleKeyDown);
  requestRender(typeof node.attrs.latex === 'string' ? node.attrs.latex : '');

  return {
    dom,
    update(updatedNode) {
      if (updatedNode.type !== node.type) return false;
      const latex = typeof updatedNode.attrs.latex === 'string' ? updatedNode.attrs.latex : '';
      const changed = latex !== node.attrs.latex;
      node = updatedNode;
      if (changed) requestRender(latex);
      return true;
    },
    destroy() {
      destroyed = true;
      generation += 1;
      cancelRender();
      dom.removeEventListener('click', handleClick);
      dom.removeEventListener('keydown', handleKeyDown);
    },
  };
}

function plainParagraphText(node: ProseMirrorNode): string | null {
  if (node.type.name !== 'paragraph') return null;
  let text = '';
  let plain = true;
  node.forEach(child => {
    if (!child.isText || child.marks.length > 0) {
      plain = false;
      return;
    }
    text += child.text ?? '';
  });
  return plain ? text : null;
}

type FenceStateMeta = { type: 'open'; position: number } | { type: 'close' };

function isStandaloneFenceParagraph(node: ProseMirrorNode | null | undefined): boolean {
  return Boolean(node && plainParagraphText(node) === '$$');
}

function activeStandaloneFencePosition(editor: Editor): number | null {
  return activeFencePositions.get(editor) ?? null;
}

function locateActiveStandaloneBlockMathFence(editor: Editor, state: EditorState) {
  const { selection } = state;
  if (selection.$from.parent !== selection.$to.parent) return null;

  const openingPosition = activeStandaloneFencePosition(editor);
  if (openingPosition === null || !isStandaloneFenceParagraph(state.doc.nodeAt(openingPosition))) return null;

  const { $from } = selection;
  const parentDepth = $from.depth - 1;
  const parent = $from.node(parentDepth);
  const currentIndex = $from.index(parentDepth);
  const currentPosition = $from.before($from.depth);

  const opening = state.doc.resolve(openingPosition);
  if (opening.parent !== parent) return null;
  const openingIndex = opening.index();
  if (currentIndex <= openingIndex) return null;

  return { parent, openingIndex, openingPosition, currentIndex, currentPosition };
}

function inspectStandaloneBlockMathFence(editor: Editor, state: EditorState) {
  const located = locateActiveStandaloneBlockMathFence(editor, state);
  if (!located) return null;

  const { selection } = state;
  const { $from } = selection;
  const currentLine = plainParagraphText($from.parent);
  if (currentLine === null) return null;

  const closingMarkerAtCursor = selection.empty
    && $from.parentOffset === $from.parent.content.size
    && currentLine === '$$';
  if (!closingMarkerAtCursor) return { ...located, currentLine, closingMarkerAtCursor, canConvert: false, latex: '' };

  const body: string[] = [];
  let plainBody = true;
  for (let index = located.openingIndex + 1; index < located.currentIndex; index += 1) {
    const line = plainParagraphText(located.parent.child(index));
    if (line === null) {
      plainBody = false;
      continue;
    }
    body.push(line);
  }

  const latex = body.join('\n');
  return {
    ...located,
    currentLine,
    closingMarkerAtCursor,
    latex,
    canConvert: plainBody,
  };
}

function isInsideStandaloneBlockMath(editor: Editor, state: EditorState): boolean {
  return locateActiveStandaloneBlockMathFence(editor, state) !== null;
}

function openStandaloneBlockMathFence(editor: Editor): boolean {
  return editor.commands.command(({ state, tr }) => {
    const { selection } = state;
    const { $from } = selection;
    if (
      !selection.empty
      || $from.parent.type.name !== 'paragraph'
      || $from.parentOffset !== $from.parent.content.size
      || plainParagraphText($from.parent) !== '$$'
      || activeStandaloneFencePosition(editor) !== null
    ) return false;

    const openingPosition = $from.before($from.depth);
    tr.split($from.pos);
    const cursor = tr.mapping.map($from.pos, 1);
    tr.setSelection(Selection.near(tr.doc.resolve(cursor), 1));
    tr.setMeta(blockMathFenceKey, { type: 'open', position: openingPosition } satisfies FenceStateMeta);
    return true;
  });
}

function closeStandaloneBlockMathFence(editor: Editor): boolean {
  return editor.commands.command(({ state, tr }) => {
    const fence = inspectStandaloneBlockMathFence(editor, state);
    if (!fence?.closingMarkerAtCursor) return false;

    if (fence.canConvert) {
      const blockMathType = state.schema.nodes.blockMath;
      const paragraphType = state.schema.nodes.paragraph;
      const mathNode = blockMathType.create({ latex: fence.latex });
      const emptyParagraph = paragraphType.createAndFill();
      const replacement = emptyParagraph ? Fragment.fromArray([mathNode, emptyParagraph]) : null;
      if (replacement && fence.parent.canReplace(fence.openingIndex, fence.currentIndex + 1, replacement)) {
        const end = fence.currentPosition + state.selection.$from.parent.nodeSize;
        tr.replaceWith(fence.openingPosition, end, replacement);
        tr.setSelection(Selection.near(tr.doc.resolve(fence.openingPosition + mathNode.nodeSize + 1), 1));
        tr.setMeta(blockMathFenceKey, { type: 'close' } satisfies FenceStateMeta);
        return true;
      }
    }

    tr.split(state.selection.$from.pos);
    const cursor = tr.mapping.map(state.selection.$from.pos, 1);
    tr.setSelection(Selection.near(tr.doc.resolve(cursor), 1));
    tr.setMeta(blockMathFenceKey, { type: 'close' } satisfies FenceStateMeta);
    return true;
  });
}

function createBlockMathSourcePlugin(editor: Editor): Plugin {
  return new Plugin({
    props: {
      handleTextInput(view, from, to, text) {
        if (view.composing || !isInsideStandaloneBlockMath(editor, view.state)) return false;
        view.dispatch(view.state.tr.insertText(text, from, to));
        return true;
      },
    },
  });
}

function mathMarksAreAffected(transactions: readonly Transaction[], transactionIndex: number, stepIndex: number, step: Step): { from: number; to: number } | null {
  let from: number;
  let to: number;
  if (step instanceof AddNodeMarkStep || step instanceof RemoveNodeMarkStep) {
    from = step.pos;
    to = from + 1;
  } else if (step instanceof AddMarkStep || step instanceof RemoveMarkStep) {
    from = step.from;
    to = step.to;
  } else {
    return null;
  }
  for (const map of transactions[transactionIndex].mapping.maps.slice(stepIndex)) {
    from = map.map(from, -1);
    to = map.map(to, 1);
  }
  for (let index = transactionIndex + 1; index < transactions.length; index += 1) {
    for (const map of transactions[index].mapping.maps) {
      from = map.map(from, -1);
      to = map.map(to, 1);
    }
  }
  return { from, to };
}

export function createMathExtensions(onEditMath?: MathEditHandler): Extensions {
  const renderer = new MathRenderer();

  const inlineMath = InlineMath.extend({
    priority: 1000,
    marks: '',

    addInputRules() {
      return [createInlineMathInputRule(this.type)];
    },

    addNodeView() {
      return props => createMathNodeView(props, this.editor, renderer, false, onEditMath);
    },
  }).configure({ onClick: undefined, katexOptions: undefined });

  const blockMath = BlockMath.extend({
    priority: 1000,
    marks: '',

    addInputRules() {
      return [];
    },

    addKeyboardShortcuts() {
      return {
        Enter: ({ editor }) => {
          if (closeStandaloneBlockMathFence(editor)) return true;
          if (isInsideStandaloneBlockMath(editor, editor.state)) return editor.commands.splitBlock();
          return openStandaloneBlockMathFence(editor);
        },
      };
    },

    addProseMirrorPlugins() {
      return [createBlockMathSourcePlugin(this.editor)];
    },

    onTransaction({ transaction }) {
      const meta = transaction.getMeta(blockMathFenceKey) as FenceStateMeta | undefined;
      if (meta?.type === 'close') {
        activeFencePositions.delete(this.editor);
        return;
      }

      const previous = activeFencePositions.get(this.editor);
      const position = meta?.type === 'open'
        ? transaction.mapping.map(meta.position, 1)
        : previous === undefined
          ? null
          : transaction.mapping.map(previous, 1);
      if (position !== null && isStandaloneFenceParagraph(this.editor.state.doc.nodeAt(position))) {
        activeFencePositions.set(this.editor, position);
      } else {
        activeFencePositions.delete(this.editor);
      }
    },

    addNodeView() {
      return props => createMathNodeView(props, this.editor, renderer, true, onEditMath);
    },
  }).configure({ onClick: undefined, katexOptions: undefined });

  const protectMathNodesFromInlineMarks = Extension.create({
    name: 'cuenoteMathNodeMarkProtection',
    priority: 1000,

    addProseMirrorPlugins() {
      return [new Plugin({
        appendTransaction: (transactions, _oldState, newState) => {
          const ranges: Array<{ from: number; to: number }> = [];
          transactions.forEach((transaction, transactionIndex) => {
            transaction.steps.forEach((step, stepIndex) => {
              const range = mathMarksAreAffected(transactions, transactionIndex, stepIndex, step);
              if (range) ranges.push(range);
            });
          });
          if (ranges.length === 0) return null;

          const transaction = newState.tr;
          let changed = false;
          const mathType = newState.schema.nodes.inlineMath;
          newState.doc.descendants((node, position) => {
            if (node.type !== mathType || node.marks.length === 0) return;
            const end = position + node.nodeSize;
            if (!ranges.some(range => position < range.to && end > range.from)) return;
            transaction.setNodeMarkup(position, undefined, node.attrs, []);
            changed = true;
          });

          return changed ? transaction : null;
        },
      })];
    },

    onDestroy() {
      renderer.dispose();
    },
  });

  return [inlineMath, blockMath, protectMathNodesFromInlineMarks];
}
