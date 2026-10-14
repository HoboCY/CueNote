import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type MouseEvent } from 'react';
import type { Editor, JSONContent } from '@tiptap/core';
import { EditorContent, useEditor, useEditorState } from '@tiptap/react';
import { closeHistory } from '@tiptap/pm/history';
import type { Node as ProseMirrorNode } from '@tiptap/pm/model';
import { NodeSelection, TextSelection, type Selection, type SelectionBookmark, type Transaction } from '@tiptap/pm/state';
import 'katex/dist/katex.min.css';
import type { Block } from './generated/note-format';
import { CODE_LANGUAGES } from './code-languages';
import { fromEditorDocument, toEditorDocument } from './editor-model';
import { createEditorExtensions, TABLE_HEADER_NORMALIZATION_META } from './editor-extensions';

export type RichTextSection = 'cue' | 'content' | 'summary';

interface RichTextEditorProps {
  sessionId: string;
  section: RichTextSection;
  blocks: Block[];
  editable: boolean;
  readOnly: boolean;
  projectionText: string;
  onChange: (blocks: Block[]) => void;
  onError: (message: string | null, initial: boolean) => void;
  onCompositionStart: () => void;
  onCompositionEnd: (blocks: Block[] | null) => void;
}

interface ConversionResult {
  document: JSONContent | null;
  error: string | null;
}

function errorMessage(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

const initialConversionMessage = '无法安全转换当前内容，已停止编辑并显示只读文字投影。';
const editConversionMessage = '暂时无法保存此编辑内容，请撤销刚才的操作或修复后再继续。';
const customCodeLanguageOption = '__cuenote_custom_language__';
type MathNodeName = 'inlineMath' | 'blockMath';

type MathFormTarget =
  | { mode: 'insert'; nodeName: MathNodeName; editor: Editor; bookmark: SelectionBookmark }
  | { mode: 'edit'; nodeName: MathNodeName; editor: Editor; position: number; node: ProseMirrorNode };

function isMathNodeName(value: string): value is MathNodeName {
  return value === 'inlineMath' || value === 'blockMath';
}

function canInsertMath(editor: Editor, selection: Selection, nodeName: MathNodeName) {
  if (selectionTouchesNode(selection, new Set(['codeBlock']))) return false;
  if (selectionTouchesMark(selection, 'code')) return false;
  if (nodeName === 'blockMath' && selectionTouchesNode(selection, new Set(['tableCell', 'tableHeader']))) return false;
  try {
    const nodeType = editor.schema.nodes[nodeName];
    if (!nodeType) return false;
    const formula = nodeType.create({ latex: '' });
    const transaction = editor.state.tr.setSelection(selection);
    transaction.replaceSelectionWith(formula, false);
    return transaction.docChanged && transactionContainsNode(transaction, formula);
  } catch {
    return false;
  }
}

function selectionTouchesNode(selection: Selection, names: ReadonlySet<string>) {
  for (const range of selection.ranges) {
    for (const position of [range.$from, range.$to]) {
      for (let depth = position.depth; depth > 0; depth -= 1) {
        if (names.has(position.node(depth).type.name)) return true;
      }
    }
    if (range.$from.pos < range.$to.pos) {
      let found = false;
      range.$from.doc.nodesBetween(range.$from.pos, range.$to.pos, node => {
        if (names.has(node.type.name)) {
          found = true;
          return false;
        }
        return !found;
      });
      if (found) return true;
    }
  }
  return false;
}

function selectionTouchesMark(selection: Selection, markName: string) {
  for (const range of selection.ranges) {
    for (const position of [range.$from, range.$to]) {
      if (position.marks().some(mark => mark.type.name === markName)) return true;
    }
    if (range.$from.pos < range.$to.pos) {
      let found = false;
      range.$from.doc.nodesBetween(range.$from.pos, range.$to.pos, node => {
        if (node.marks.some(mark => mark.type.name === markName)) {
          found = true;
          return false;
        }
        return !found;
      });
      if (found) return true;
    }
  }
  return false;
}

function transactionContainsNode(transaction: Transaction, expected: ProseMirrorNode) {
  let found = false;
  transaction.doc.descendants(node => {
    if (node === expected) {
      found = true;
      return false;
    }
    return !found;
  });
  return found;
}

function insertMathNode(editor: Editor, selection: Selection, nodeName: MathNodeName, latex: string) {
  const nodeType = editor.schema.nodes[nodeName];
  if (!nodeType) throw new Error('公式节点不可用。');
  const formula = nodeType.create({ latex });
  const transaction = editor.state.tr.setSelection(selection);
  transaction.replaceSelectionWith(formula, false);
  if (!transaction.docChanged || !transactionContainsNode(transaction, formula)) throw new Error('当前位置无法插入此类公式。');
  return transaction;
}

function makeInitialDocument(blocks: Block[], simple: boolean): ConversionResult {
  try {
    return { document: toEditorDocument(blocks, simple), error: null };
  } catch (cause) {
    return { document: null, error: errorMessage(cause) };
  }
}

function validHref(value: string) {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const candidate = /^[a-z][a-z0-9+.-]*:/i.test(trimmed) ? trimmed : `https://${trimmed}`;
  try {
    const parsed = new URL(candidate);
    return ['http:', 'https:', 'mailto:'].includes(parsed.protocol) ? parsed.href : null;
  } catch {
    return null;
  }
}

function actionTestId(action: string, section: RichTextSection) {
  return `rich-toolbar-${action}-${section}`;
}

export function RichTextEditor({
  sessionId,
  section,
  blocks,
  editable,
  readOnly,
  projectionText,
  onChange,
  onError,
  onCompositionStart,
  onCompositionEnd,
}: RichTextEditorProps) {
  const simple = section !== 'content';
  const [initial] = useState(() => makeInitialDocument(blocks, simple));
  const [conversionError, setConversionError] = useState(initial.error);
  const [linkOpen, setLinkOpen] = useState(false);
  const [linkLabel, setLinkLabel] = useState('');
  const [linkHref, setLinkHref] = useState('');
  const [linkError, setLinkError] = useState<string | null>(null);
  const [codeLanguageDraft, setCodeLanguageDraft] = useState('');
  const [mathFormOpen, setMathFormOpen] = useState(false);
  const [mathSource, setMathSource] = useState('');
  const [mathTargetError, setMathTargetError] = useState<string | null>(null);
  const [mathTargetInvalid, setMathTargetInvalid] = useState(false);
  const linkInputRef = useRef<HTMLInputElement | null>(null);
  const mathDialogRef = useRef<HTMLDialogElement | null>(null);
  const mathInputRef = useRef<HTMLTextAreaElement | null>(null);
  const mathTargetRef = useRef<MathFormTarget | null>(null);
  const mathApplyingRef = useRef(false);
  const currentEditorRef = useRef<Editor | null>(null);
  const mathPermissionsRef = useRef({ editable, readOnly, simple, conversionError });
  mathPermissionsRef.current = { editable, readOnly, simple, conversionError };
  const savedSelectionRef = useRef<{ from: number; to: number } | null>(null);
  const lastReportedErrorRef = useRef<string | null | undefined>(undefined);
  const compositionSequenceRef = useRef(0);

  const openMathFromNode = useCallback((node: ProseMirrorNode, position: number) => {
    const currentEditor = currentEditorRef.current;
    const permissions = mathPermissionsRef.current;
    if (!currentEditor || !currentEditor.view.dom.isConnected || !permissions.editable
      || permissions.readOnly || permissions.simple || permissions.conversionError) return;
    const currentNode = currentEditor.state.doc.nodeAt(position);
    if (!isMathNodeName(node.type.name) || currentNode !== node) return;

    mathTargetRef.current = { mode: 'edit', nodeName: node.type.name, editor: currentEditor, position, node };
    setMathSource(typeof node.attrs.latex === 'string' ? node.attrs.latex : String(node.attrs.latex ?? ''));
    setMathTargetError(null);
    setMathTargetInvalid(false);
    setMathFormOpen(true);
  }, []);

  const extensions = useMemo(() => createEditorExtensions(simple, openMathFromNode), [simple, openMathFromNode]);
  const notifyError = useCallback((message: string | null, isInitial: boolean) => {
    setConversionError(message);
    if (lastReportedErrorRef.current === message) return;
    lastReportedErrorRef.current = message;
    onError(message, isInitial);
  }, [onError]);

  useEffect(() => {
    if (initial.error) notifyError(initialConversionMessage, true);
  }, [initial.error, notifyError]);

  const editor = useEditor({
    extensions,
    content: initial.document ?? undefined,
    editable: editable && initial.document !== null,
    immediatelyRender: false,
    editorProps: {
      attributes: {
        class: `rich-editor-prose${simple ? ' is-simple' : ''}`,
        'data-testid': `rich-editor-input-${section}`,
        'aria-label': `${section === 'cue' ? '线索区' : section === 'content' ? '内容区' : '总结区'}编辑区`,
        'data-section': section,
        spellcheck: 'true',
      },
    },
    onUpdate: ({ editor: updatedEditor }) => {
      try {
        const next = fromEditorDocument(updatedEditor.getJSON(), simple);
        notifyError(null, false);
        onChange(next);
      } catch (cause) {
        notifyError(editConversionMessage, false);
      }
    },
  }, []);

  useEffect(() => {
    if (!editor) return;
    currentEditorRef.current = editor;
    return () => {
      if (currentEditorRef.current === editor) currentEditorRef.current = null;
    };
  }, [editor]);

  useEffect(() => {
    if (!editor) return;
    const mapMathTarget = ({ transaction }: { transaction: Transaction }) => {
      if (mathApplyingRef.current) return;
      const target = mathTargetRef.current;
      if (!target || target.editor !== editor) return;
      if (target.mode === 'insert') {
        target.bookmark = target.bookmark.map(transaction.mapping);
        return;
      }
      if (!transaction.docChanged) return;
      const mapped = transaction.mapping.mapResult(target.position, 1);
      const currentNode = editor.state.doc.nodeAt(mapped.pos);
      if (mapped.deleted || currentNode !== target.node) {
        setMathTargetInvalid(true);
        setMathTargetError('此公式在源码窗口打开期间发生了变化。请取消后重新打开，避免覆盖其他内容。');
        return;
      }
      target.position = mapped.pos;
    };
    editor.on('transaction', mapMathTarget);
    return () => {
      editor.off('transaction', mapMathTarget);
    };
  }, [editor]);

  const selectionState = useEditorState({
    editor,
    selector: ({ editor: current }) => {
      if (!current) return null;
      const hasTable = !simple && current.extensionManager.extensions.some((extension) => extension.name === 'table');
      const selection = current.state.selection;
      const selectedMath = selection instanceof NodeSelection && isMathNodeName(selection.node.type.name)
        ? selection.node.type.name
        : null;
      return {
        bold: current.isActive('bold'),
        italic: current.isActive('italic'),
        strike: current.isActive('strike'),
        inlineCode: current.isActive('code'),
        codeBlock: current.isActive('codeBlock'),
        codeLanguage: current.getAttributes('codeBlock').language ?? null,
        paragraph: current.isActive('paragraph'),
        heading1: current.isActive('heading', { level: 1 }),
        heading2: current.isActive('heading', { level: 2 }),
        heading3: current.isActive('heading', { level: 3 }),
        bulletList: current.isActive('bulletList'),
        orderedList: current.isActive('orderedList'),
        taskList: current.isActive('taskList'),
        blockquote: current.isActive('blockquote'),
        link: current.isActive('link'),
        selectedMath,
        table: hasTable && current.isActive('table'),
        canAddRow: hasTable && current.can().addRowAfter(),
        canDeleteRow: hasTable && current.can().deleteRow(),
        canAddColumn: hasTable && current.can().addColumnAfter(),
        canDeleteColumn: hasTable && current.can().deleteColumn(),
        canDeleteTable: hasTable && current.can().deleteTable(),
      };
    },
  });

  useEffect(() => {
    setCodeLanguageDraft(selectionState?.codeLanguage ?? '');
  }, [selectionState?.codeBlock, selectionState?.codeLanguage]);

  useEffect(() => {
    if (editor) editor.setEditable(editable && initial.document !== null, false);
  }, [editor, editable, initial.document]);

  useEffect(() => {
    if (!linkOpen) return;
    linkInputRef.current?.focus();
  }, [linkOpen]);

  const preventToolbarFocus = (event: MouseEvent<HTMLButtonElement>) => event.preventDefault();
  const synchronizeTextSelection = () => {
    if (!editor) return;
    const { view, state } = editor;
    // Keyboard selection can reach the DOM before selectionchange updates
    // ProseMirror. Read it before a toolbar command consumes the old caret.
    // Node and table-cell selections keep their editor-specific semantics.
    if (view.composing || !(state.selection instanceof TextSelection)) return;
    const selection = view.dom.ownerDocument.getSelection();
    if (!selection?.anchorNode || !selection.focusNode
      || !view.dom.contains(selection.anchorNode) || !view.dom.contains(selection.focusNode)) return;
    const anchor = view.posAtDOM(selection.anchorNode, selection.anchorOffset);
    const head = view.posAtDOM(selection.focusNode, selection.focusOffset);
    if (anchor < 0 || head < 0 || anchor > state.doc.content.size || head > state.doc.content.size) return;
    const current = TextSelection.between(state.doc.resolve(anchor), state.doc.resolve(head));
    if (!current.eq(state.selection)) view.dispatch(state.tr.setSelection(current));
  };
  const captureEditorSelection = () => {
    if (!editor) return;
    synchronizeTextSelection();
    const { selection } = editor.state;
    if (selection instanceof TextSelection) {
      savedSelectionRef.current = { from: selection.from, to: selection.to };
    } else {
      // A whole-block selection must not restore a prior text selection.
      savedSelectionRef.current = null;
    }
  };
  const run = (command: () => boolean) => {
    if (!editor || !editable || conversionError) return;
    synchronizeTextSelection();
    command();
  };
  const applyCodeLanguage = (language: string | null) => {
    if (!editor || !editable || conversionError || !editor.isActive('codeBlock')) return;
    const savedSelection = savedSelectionRef.current;
    const chain = editor.chain();
    if (savedSelection) chain.setTextSelection(savedSelection);
    // Language selection is one undoable action, separate from nearby typing.
    chain.command(({ tr }) => {
      closeHistory(tr);
      return true;
    }).updateAttributes('codeBlock', { language }).focus().run();
    editor.view.dispatch(closeHistory(editor.state.tr));
    savedSelectionRef.current = null;
  };
  const restoreEditorSelection = () => {
    if (!editor || !savedSelectionRef.current) return;
    const { from, to } = savedSelectionRef.current;
    editor.chain().setTextSelection({ from, to }).focus().run();
  };
  const openMathInsert = (nodeName: MathNodeName) => {
    if (!editor || !editable || readOnly || conversionError || simple) return;
    synchronizeTextSelection();
    const selection = editor.state.selection;
    if (!canInsertMath(editor, selection, nodeName)) return;
    mathTargetRef.current = { mode: 'insert', nodeName, editor, bookmark: selection.getBookmark() };
    setMathSource('');
    setMathTargetError(null);
    setMathTargetInvalid(false);
    setMathFormOpen(true);
  };
  const openSelectedMath = () => {
    if (!editor || !editable || readOnly || conversionError || simple) return;
    const selection = editor.state.selection;
    if (!(selection instanceof NodeSelection) || !isMathNodeName(selection.node.type.name)) return;
    openMathFromNode(selection.node, selection.from);
  };
  const closeMathForm = (restoreSelection: boolean) => {
    const target = mathTargetRef.current;
    mathTargetRef.current = null;
    setMathFormOpen(false);
    setMathTargetError(null);
    setMathTargetInvalid(false);
    if (mathDialogRef.current?.open) mathDialogRef.current.close();
    requestAnimationFrame(() => {
      const currentEditor = currentEditorRef.current;
      if (!currentEditor || !currentEditor.view.dom.isConnected) return;
      if (restoreSelection && target && target.editor === currentEditor) {
        try {
          if (target.mode === 'insert') {
            currentEditor.view.dispatch(currentEditor.state.tr.setSelection(target.bookmark.resolve(currentEditor.state.doc)));
          } else if (currentEditor.state.doc.nodeAt(target.position) === target.node) {
            const selection = NodeSelection.create(currentEditor.state.doc, target.position);
            currentEditor.view.dispatch(currentEditor.state.tr.setSelection(selection));
          }
        } catch {
          // A canceled form never changes the document; focus still returns if its selection cannot be restored.
        }
      }
      currentEditor.view.focus();
    });
  };
  const applyMathSource = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const currentEditor = currentEditorRef.current;
    const target = mathTargetRef.current;
    if (!editor || currentEditor !== editor || !editor.view.dom.isConnected || !target || target.editor !== editor
      || !editable || readOnly || conversionError || simple || mathTargetInvalid) return;

    try {
      let transaction;
      if (target.mode === 'insert') {
        const selection = target.bookmark.resolve(editor.state.doc);
        if (!canInsertMath(editor, selection, target.nodeName)) throw new Error('当前位置无法插入此类公式。');
        transaction = insertMathNode(editor, selection, target.nodeName, mathSource);
      } else {
        const currentNode = editor.state.doc.nodeAt(target.position);
        if (currentNode !== target.node || currentNode.type.name !== target.nodeName) {
          setMathTargetInvalid(true);
          setMathTargetError('此公式在源码窗口打开期间发生了变化。请取消后重新打开，避免覆盖其他内容。');
          return;
        }
        if (currentNode.attrs.latex === mathSource) {
          closeMathForm(false);
          return;
        }
        transaction = editor.state.tr;
        transaction.setNodeMarkup(target.position, currentNode.type, { ...currentNode.attrs, latex: mathSource }, currentNode.marks);
        transaction.setSelection(NodeSelection.create(transaction.doc, target.position));
      }

      if (!transaction.docChanged) {
        if (transaction.selectionSet) editor.view.dispatch(transaction);
        closeMathForm(false);
        return;
      }
      closeHistory(transaction);
      mathApplyingRef.current = true;
      try {
        editor.view.dispatch(transaction);
        editor.view.dispatch(closeHistory(editor.state.tr));
      } finally {
        mathApplyingRef.current = false;
      }
      closeMathForm(false);
    } catch (cause) {
      setMathTargetError(errorMessage(cause));
    }
  };

  useEffect(() => {
    if (!mathFormOpen) return;
    const dialog = mathDialogRef.current;
    if (!dialog) return;
    if (!dialog.open) dialog.showModal();
    mathInputRef.current?.focus();
  }, [mathFormOpen]);

  const openLinkForm = () => {
    if (!editor || !editable || conversionError) return;
    synchronizeTextSelection();
    const { from, to } = editor.state.selection;
    savedSelectionRef.current = { from, to };
    setLinkLabel(editor.state.doc.textBetween(from, to, ' '));
    setLinkHref(editor.getAttributes('link').href ?? '');
    setLinkError(null);
    setLinkOpen(true);
  };
  const cancelLinkForm = () => {
    setLinkOpen(false);
    requestAnimationFrame(restoreEditorSelection);
  };
  const submitLink = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!editor || !savedSelectionRef.current) return;
    const href = validHref(linkHref);
    if (!href) {
      setLinkError('请输入有效的 HTTP、HTTPS 或 mailto 链接。');
      return;
    }
    const label = linkLabel.trim();
    if (!label) {
      setLinkError('请输入链接文字。');
      return;
    }
    const { from, to } = savedSelectionRef.current;
    const linkText: JSONContent = { type: 'text', text: label, marks: [{ type: 'link', attrs: { href } }] };
    editor.chain().setTextSelection({ from, to }).insertContent(linkText).focus().run();
    setLinkOpen(false);
    setLinkError(null);
  };

  const tool = (
    action: string,
    label: string,
    command: () => boolean,
    active = false,
    disabled = false,
  ) => (
    <button
      key={action}
      type="button"
      className={`rich-editor-tool${active ? ' is-active' : ''}`}
      data-testid={actionTestId(action, section)}
      aria-label={label}
      aria-pressed={active}
      title={label}
      disabled={!editable || Boolean(conversionError) || disabled}
      onMouseDown={preventToolbarFocus}
      onClick={() => run(command)}
    >{label}</button>
  );

  const onCompositionStartFromEditor = () => {
    compositionSequenceRef.current += 1;
    onCompositionStart();
  };
  const onCompositionEndDeferred = () => {
    if (!editor) return;
    const sequence = compositionSequenceRef.current;
    requestAnimationFrame(() => {
      if (sequence !== compositionSequenceRef.current) return;
      try {
        const next = fromEditorDocument(editor.getJSON(), simple);
        notifyError(null, false);
        onCompositionEnd(next);
      } catch (cause) {
        notifyError(editConversionMessage, false);
        onCompositionEnd(null);
      }
    });
  };

  if (!initial.document) {
    return (
      <div className="rich-editor-fallback" data-testid={`rich-editor-${section}`} data-section={section} data-session-id={sessionId}>
        <textarea
          className={`note-textarea rich-editor-projection${section === 'cue' ? ' cue-textarea' : section === 'summary' ? ' summary-textarea' : ' content-textarea'}`}
          data-testid={`rich-editor-input-${section}`}
          aria-label={`${section === 'cue' ? '线索区' : section === 'content' ? '内容区' : '总结区'}只读文字投影`}
          value={projectionText}
          readOnly
          rows={section === 'content' ? 14 : 6}
        />
      </div>
    );
  }

  const active = selectionState ?? {
    bold: false, italic: false, strike: false, inlineCode: false, codeBlock: false, codeLanguage: null, paragraph: false,
    heading1: false, heading2: false, heading3: false,
    bulletList: false, orderedList: false, taskList: false,
    blockquote: false, link: false, selectedMath: null, table: false,
    canAddRow: false, canDeleteRow: false, canAddColumn: false,
    canDeleteColumn: false, canDeleteTable: false,
  };
  const formattingDisabled = active.inlineCode || active.codeBlock || Boolean(active.selectedMath);
  const knownCodeLanguage = CODE_LANGUAGES.some(language => language.value === active.codeLanguage);
  const codeLanguageSelectValue = active.codeLanguage === null
    ? ''
    : knownCodeLanguage ? active.codeLanguage : customCodeLanguageOption;
  const canAddInlineMath = Boolean(editor && canInsertMath(editor, editor.state.selection, 'inlineMath'));
  const canAddBlockMath = Boolean(editor && canInsertMath(editor, editor.state.selection, 'blockMath'));
  const currentMathTarget = mathTargetRef.current;

  return (
    <div
      className={`rich-editor${simple ? ' is-simple' : ''}${readOnly ? ' is-readonly' : ''}`}
      data-testid={`rich-editor-${section}`}
      data-section={section}
      data-session-id={sessionId}
    >
      {!readOnly && (
        <div className="rich-editor-toolbar" role="toolbar" aria-label={`${section === 'cue' ? '线索区' : section === 'content' ? '内容区' : '总结区'}格式工具`}>
          {tool('paragraph', '段落', () => editor?.chain().focus().setParagraph().run() ?? false, active.paragraph, formattingDisabled)}
          {tool('heading-1', '一级标题', () => editor?.chain().focus().toggleHeading({ level: 1 }).run() ?? false, active.heading1, formattingDisabled)}
          {tool('heading-2', '二级标题', () => editor?.chain().focus().toggleHeading({ level: 2 }).run() ?? false, active.heading2, formattingDisabled)}
          {tool('heading-3', '三级标题', () => editor?.chain().focus().toggleHeading({ level: 3 }).run() ?? false, active.heading3, formattingDisabled)}
          {tool('bold', '加粗', () => editor?.chain().focus().toggleBold().run() ?? false, active.bold, formattingDisabled)}
          {!simple && tool('italic', '斜体', () => editor?.chain().focus().toggleItalic().run() ?? false, active.italic, formattingDisabled)}
          {!simple && tool('strike', '删除线', () => editor?.chain().focus().toggleStrike().run() ?? false, active.strike, formattingDisabled)}
          {tool('bullet-list', '项目符号列表', () => editor?.chain().focus().toggleBulletList().run() ?? false, active.bulletList, formattingDisabled)}
          {tool('ordered-list', '编号列表', () => editor?.chain().focus().toggleOrderedList().run() ?? false, active.orderedList, formattingDisabled)}
          {tool('task-list', '任务列表', () => editor?.chain().focus().toggleTaskList().run() ?? false, active.taskList, formattingDisabled)}
          {!simple && tool('blockquote', '引用', () => editor?.chain().focus().toggleBlockquote().run() ?? false, active.blockquote, formattingDisabled)}
          {!simple && tool('code', '行内代码', () => editor?.chain().focus().toggleCode().run() ?? false, active.inlineCode, active.codeBlock)}
          {!simple && tool('code-block', '代码块', () => editor?.chain().focus().toggleCodeBlock().run() ?? false, active.codeBlock, active.inlineCode)}
          {!simple && (
            <button
              type="button"
              className={`rich-editor-tool${active.link ? ' is-active' : ''}`}
              data-testid={actionTestId('link', section)}
              aria-label="插入链接"
              aria-pressed={active.link}
              title="插入链接"
              disabled={!editable || Boolean(conversionError) || formattingDisabled}
              onMouseDown={preventToolbarFocus}
              onClick={openLinkForm}
            >插入链接</button>
          )}
          {!simple && (
            <>
              <button
                type="button"
                className="rich-editor-tool"
                data-testid="rich-toolbar-math-inline-content"
                aria-label="插入行内公式"
                title="插入行内公式"
                disabled={!editable || Boolean(conversionError) || !canAddInlineMath}
                onMouseDown={preventToolbarFocus}
                onClick={() => openMathInsert('inlineMath')}
              >插入行内公式</button>
              <button
                type="button"
                className="rich-editor-tool"
                data-testid="rich-toolbar-math-block-content"
                aria-label="插入公式块"
                title={canAddBlockMath ? '插入公式块' : '当前位置无法插入独立公式块'}
                disabled={!editable || Boolean(conversionError) || !canAddBlockMath}
                onMouseDown={preventToolbarFocus}
                onClick={() => openMathInsert('blockMath')}
              >插入公式块</button>
              <button
                type="button"
                className={`rich-editor-tool${active.selectedMath ? ' is-active' : ''}`}
                data-testid="rich-toolbar-math-edit-content"
                aria-label="编辑所选公式源码"
                aria-pressed={Boolean(active.selectedMath)}
                title="编辑所选公式源码"
                disabled={!editable || Boolean(conversionError) || !active.selectedMath}
                onMouseDown={preventToolbarFocus}
                onClick={openSelectedMath}
              >编辑所选公式</button>
            </>
          )}
          {!simple && tool('horizontal-rule', '分隔线', () => editor?.chain().focus().setHorizontalRule().run() ?? false, false, formattingDisabled)}
          {!simple && tool('insert-table', '插入表格', () => editor?.chain().focus().insertTable({ rows: 2, cols: 2, withHeaderRow: true }).run() ?? false, active.table, formattingDisabled)}
          {!simple && active.table && (
            <>
              {tool('add-row-before', '上方插行', () => editor?.chain().focus().addRowBefore().command(({ tr }) => {
                tr.setMeta(TABLE_HEADER_NORMALIZATION_META, true);
                return true;
              }).run() ?? false, false, !active.canAddRow)}
              {tool('add-row-after', '下方插行', () => editor?.chain().focus().addRowAfter().run() ?? false, false, !active.canAddRow)}
              {tool('add-column-before', '左侧插列', () => editor?.chain().focus().addColumnBefore().run() ?? false, false, !active.canAddColumn)}
              {tool('add-column-after', '右侧插列', () => editor?.chain().focus().addColumnAfter().run() ?? false, false, !active.canAddColumn)}
              {tool('delete-row', '删除行', () => editor?.chain().focus().deleteRow().command(({ tr }) => {
                tr.setMeta(TABLE_HEADER_NORMALIZATION_META, true);
                return true;
              }).run() ?? false, false, !active.canDeleteRow)}
              {tool('delete-column', '删除列', () => editor?.chain().focus().deleteColumn().run() ?? false, false, !active.canDeleteColumn)}
              {tool('delete-table', '删除表格', () => editor?.chain().focus().deleteTable().run() ?? false, false, !active.canDeleteTable)}
            </>
          )}
          {!simple && active.codeBlock && (
            <div className="rich-editor-code-controls" role="group" aria-label="代码块语言和退出操作">
              <label className="code-language-control">
                <span>代码语言</span>
                <select
                  data-testid="code-language-select-content"
                  aria-label="代码语言"
                  value={codeLanguageSelectValue}
                  disabled={!editable || Boolean(conversionError)}
                  onMouseDown={captureEditorSelection}
                  onFocus={captureEditorSelection}
                  onChange={event => {
                    const value = event.currentTarget.value;
                    if (value === customCodeLanguageOption) return;
                    const language = value || null;
                    setCodeLanguageDraft(language ?? '');
                    applyCodeLanguage(language);
                  }}
                >
                  <option value="">纯文本</option>
                  {CODE_LANGUAGES.map(language => <option key={language.value} value={language.value}>{language.label}</option>)}
                  {!knownCodeLanguage && active.codeLanguage !== null && (
                    <option value={customCodeLanguageOption}>保留原标记：{active.codeLanguage}</option>
                  )}
                </select>
              </label>
              <label className="code-language-control code-language-custom">
                <span>自定义标记</span>
                <input
                  data-testid="code-language-input-content"
                  aria-label="自定义语言标记"
                  value={codeLanguageDraft}
                  disabled={!editable || Boolean(conversionError)}
                  onMouseDown={captureEditorSelection}
                  onFocus={captureEditorSelection}
                  onChange={event => setCodeLanguageDraft(event.currentTarget.value)}
                  onKeyDown={event => {
                    if (event.key === 'Enter') {
                      event.preventDefault();
                      applyCodeLanguage(codeLanguageDraft === '' ? null : codeLanguageDraft);
                    }
                  }}
                />
              </label>
              <button
                className="rich-editor-tool is-primary"
                data-testid="code-language-apply-content"
                type="button"
                disabled={!editable || Boolean(conversionError)}
                onMouseDown={event => {
                  captureEditorSelection();
                  preventToolbarFocus(event);
                }}
                onClick={() => applyCodeLanguage(codeLanguageDraft === '' ? null : codeLanguageDraft)}
              >应用标记</button>
              {tool('exit-code', '退出代码块', () => editor?.chain().focus().exitCode().run() ?? false)}
            </div>
          )}
        </div>
      )}

      {linkOpen && !simple && !readOnly && (
        <form className="rich-link-form" data-testid={`rich-link-form-${section}`} onSubmit={submitLink}>
          <label>
            <span>链接文字</span>
            <input
              data-testid={`rich-link-label-${section}`}
              aria-label="链接文字"
              value={linkLabel}
              onChange={(event) => setLinkLabel(event.currentTarget.value)}
            />
          </label>
          <label>
            <span>链接地址</span>
            <input
              ref={linkInputRef}
              data-testid={`rich-link-url-${section}`}
              aria-label="链接地址"
              type="text"
              inputMode="url"
              placeholder="https://example.com"
              value={linkHref}
              onChange={(event) => setLinkHref(event.currentTarget.value)}
            />
          </label>
          {linkError && <span className="rich-link-error" role="alert">{linkError}</span>}
          <div className="rich-link-actions">
            <button className="rich-editor-tool is-primary" data-testid={`rich-link-submit-${section}`} type="submit">应用链接</button>
            <button className="rich-editor-tool" data-testid={`rich-link-cancel-${section}`} type="button" onMouseDown={preventToolbarFocus} onClick={cancelLinkForm}>取消</button>
          </div>
        </form>
      )}

      {!readOnly && (
        <p className="markdown-editor-help" data-testid={`markdown-help-${section}`}>
          {simple
            ? 'Markdown：# / ## / ### 标题，**加粗**，- 列表，1. 编号，- [ ] 待办；Ctrl/⌘+B 加粗。'
            : active.codeBlock
              ? '代码块会保留缩进与空行；按 Ctrl/⌘+Enter，或点击“退出代码块”继续编辑普通文字。'
              : 'Markdown：# / ## / ### 标题，**加粗**，*斜体*，~~删除线~~，- 列表，1. 编号，- [ ] 待办，> 引用，--- 分隔线，[文字](https://…) 链接；反引号包住行内代码；输入 ```语言 后按空格或回车进入代码块，按 Ctrl/⌘+Enter 或点击“退出代码块”退出；行内公式写为 $...$，公式块使用独占行 $$ 包围源码；Ctrl/⌘+B 加粗，Ctrl/⌘+I 斜体。'}
        </p>
      )}

      {conversionError && !initial.error && (
        <div className="rich-editor-error" role="alert" data-testid={`rich-editor-error-${section}`}>
          {conversionError}
        </div>
      )}

      {mathFormOpen && !simple && (
        <dialog
          ref={mathDialogRef}
          className="math-source-dialog"
          aria-labelledby="math-source-title-content"
          onCancel={(event) => {
            event.preventDefault();
            closeMathForm(true);
          }}
        >
          <form className="math-source-form" data-testid="math-source-form-content" onSubmit={applyMathSource}>
            <h2 id="math-source-title-content">{currentMathTarget?.mode === 'edit' ? '编辑公式源码' : '插入公式'}</h2>
            <p className="math-source-type">
              {currentMathTarget?.nodeName === 'blockMath' ? '公式块' : '行内公式'}
              {currentMathTarget?.mode === 'edit' ? ' · 类型保持不变' : ''}
            </p>
            <label className="math-source-field">
              <span>LaTeX 源码</span>
              <textarea
                ref={mathInputRef}
                data-testid="math-source-input-content"
                aria-label="LaTeX 源码"
                aria-describedby="math-source-hint-content"
                value={mathSource}
                readOnly={!editable || readOnly || Boolean(conversionError)}
                rows={9}
                spellCheck={false}
                wrap="soft"
                onChange={(event) => setMathSource(event.currentTarget.value)}
              />
            </label>
            <p id="math-source-hint-content" className="math-source-hint">
              行内公式使用 $...$；公式块使用独占行 $$ 包围源码。源码按原样保存，包括空格、空行和空源码。
            </p>
            {mathTargetError && <p className="math-source-error" role="alert">{mathTargetError}</p>}
            <div className="math-source-actions">
              <button
                className="rich-editor-tool is-primary"
                data-testid="math-source-apply-content"
                type="submit"
                disabled={!editable || readOnly || Boolean(conversionError) || mathTargetInvalid || !currentMathTarget}
              >{currentMathTarget?.mode === 'edit' ? '应用修改' : '插入公式'}</button>
              <button
                className="rich-editor-tool"
                data-testid="math-source-cancel-content"
                type="button"
                onClick={() => closeMathForm(true)}
              >取消</button>
            </div>
          </form>
        </dialog>
      )}

      <div className="rich-editor-surface" onCompositionStart={onCompositionStartFromEditor} onCompositionEnd={onCompositionEndDeferred}>
        <EditorContent editor={editor} />
      </div>
    </div>
  );
}
