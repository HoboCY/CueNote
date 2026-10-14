import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type MouseEvent } from 'react';
import type { JSONContent } from '@tiptap/core';
import { EditorContent, useEditor, useEditorState } from '@tiptap/react';
import { TextSelection } from '@tiptap/pm/state';
import type { Block } from './generated/note-format';
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
  const linkInputRef = useRef<HTMLInputElement | null>(null);
  const savedSelectionRef = useRef<{ from: number; to: number } | null>(null);
  const lastReportedErrorRef = useRef<string | null | undefined>(undefined);
  const compositionSequenceRef = useRef(0);

  const extensions = useMemo(() => createEditorExtensions(simple), [simple]);
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

  const selectionState = useEditorState({
    editor,
    selector: ({ editor: current }) => {
      if (!current) return null;
      const hasTable = !simple && current.extensionManager.extensions.some((extension) => extension.name === 'table');
      return {
        bold: current.isActive('bold'),
        italic: current.isActive('italic'),
        strike: current.isActive('strike'),
        paragraph: current.isActive('paragraph'),
        heading1: current.isActive('heading', { level: 1 }),
        heading2: current.isActive('heading', { level: 2 }),
        heading3: current.isActive('heading', { level: 3 }),
        bulletList: current.isActive('bulletList'),
        orderedList: current.isActive('orderedList'),
        taskList: current.isActive('taskList'),
        blockquote: current.isActive('blockquote'),
        link: current.isActive('link'),
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
  const run = (command: () => boolean) => {
    if (!editor || !editable || conversionError) return;
    synchronizeTextSelection();
    command();
  };
  const restoreEditorSelection = () => {
    if (!editor || !savedSelectionRef.current) return;
    const { from, to } = savedSelectionRef.current;
    editor.chain().setTextSelection({ from, to }).focus().run();
  };
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
    bold: false, italic: false, strike: false, paragraph: false,
    heading1: false, heading2: false, heading3: false,
    bulletList: false, orderedList: false, taskList: false,
    blockquote: false, link: false, table: false,
    canAddRow: false, canDeleteRow: false, canAddColumn: false,
    canDeleteColumn: false, canDeleteTable: false,
  };

  return (
    <div
      className={`rich-editor${simple ? ' is-simple' : ''}${readOnly ? ' is-readonly' : ''}`}
      data-testid={`rich-editor-${section}`}
      data-section={section}
      data-session-id={sessionId}
    >
      {!readOnly && (
        <div className="rich-editor-toolbar" role="toolbar" aria-label={`${section === 'cue' ? '线索区' : section === 'content' ? '内容区' : '总结区'}格式工具`}>
          {tool('paragraph', '段落', () => editor?.chain().focus().setParagraph().run() ?? false, active.paragraph)}
          {tool('heading-1', '一级标题', () => editor?.chain().focus().toggleHeading({ level: 1 }).run() ?? false, active.heading1)}
          {tool('heading-2', '二级标题', () => editor?.chain().focus().toggleHeading({ level: 2 }).run() ?? false, active.heading2)}
          {tool('heading-3', '三级标题', () => editor?.chain().focus().toggleHeading({ level: 3 }).run() ?? false, active.heading3)}
          {tool('bold', '加粗', () => editor?.chain().focus().toggleBold().run() ?? false, active.bold)}
              {!simple && tool('italic', '斜体', () => editor?.chain().focus().toggleItalic().run() ?? false, active.italic)}
          {!simple && tool('strike', '删除线', () => editor?.chain().focus().toggleStrike().run() ?? false, active.strike)}
          {tool('bullet-list', '项目符号列表', () => editor?.chain().focus().toggleBulletList().run() ?? false, active.bulletList)}
          {tool('ordered-list', '编号列表', () => editor?.chain().focus().toggleOrderedList().run() ?? false, active.orderedList)}
          {tool('task-list', '任务列表', () => editor?.chain().focus().toggleTaskList().run() ?? false, active.taskList)}
          {!simple && tool('blockquote', '引用', () => editor?.chain().focus().toggleBlockquote().run() ?? false, active.blockquote)}
          {!simple && (
            <button
              type="button"
              className={`rich-editor-tool${active.link ? ' is-active' : ''}`}
              data-testid={actionTestId('link', section)}
              aria-label="插入链接"
              aria-pressed={active.link}
              title="插入链接"
              disabled={!editable || Boolean(conversionError)}
              onMouseDown={preventToolbarFocus}
              onClick={openLinkForm}
            >插入链接</button>
          )}
          {!simple && tool('horizontal-rule', '分隔线', () => editor?.chain().focus().setHorizontalRule().run() ?? false)}
          {!simple && tool('insert-table', '插入表格', () => editor?.chain().focus().insertTable({ rows: 2, cols: 2, withHeaderRow: true }).run() ?? false, active.table)}
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
            : 'Markdown：# / ## / ### 标题，**加粗**，*斜体*，~~删除线~~，- 列表，1. 编号，- [ ] 待办，> 引用，--- 分隔线，[文字](https://…) 链接；Ctrl/⌘+B 加粗，Ctrl/⌘+I 斜体。'}
        </p>
      )}

      {conversionError && !initial.error && (
        <div className="rich-editor-error" role="alert" data-testid={`rich-editor-error-${section}`}>
          {conversionError}
        </div>
      )}

      <div className="rich-editor-surface" onCompositionStart={onCompositionStartFromEditor} onCompositionEnd={onCompositionEndDeferred}>
        <EditorContent editor={editor} />
      </div>
    </div>
  );
}
