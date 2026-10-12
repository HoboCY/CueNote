import { isTauri, invoke } from '@tauri-apps/api/core';
import StarterKit from '@tiptap/starter-kit';
import { useEditor, EditorContent, type Editor } from '@tiptap/react';
import type { JSONContent } from '@tiptap/core';
import type { EditorView } from '@tiptap/pm/view';
import { useCallback, useEffect, useMemo, useRef, useState, type ChangeEvent, type MouseEvent, type ReactNode } from 'react';
import { makeMindMap, makeSampleSnapshot } from './sample';
import { hydrateSnapshot, makePersistentSnapshot } from './persistence';
import { MindMapExtension, ResizableImage } from './extensions';
import type { JsonNode, NoteDocument, NoteSnapshot, PrototypeInfo, SaveResult } from './types';

const desktopMode = isTauri();

function messageFromError(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  return `${(bytes / 1024).toFixed(1)} KiB`;
}

const supportedImageTypes = new Set(['image/png', 'image/jpeg', 'image/webp', 'image/gif']);

function supportsLocalImage(file: File): boolean {
  return supportedImageTypes.has(file.type.toLowerCase());
}

function makePendingId(): string {
  return globalThis.crypto?.randomUUID?.() ?? ('pending-' + Math.random().toString(36).slice(2));
}

function insertionPointAfterCurrentParagraph(view: EditorView): number {
  const selection = view.state.selection;
  const $from = selection.$from;
  for (let depth = $from.depth; depth > 0; depth -= 1) {
    if ($from.node(depth).type.name === 'paragraph') return $from.after(depth);
  }
  for (let depth = $from.depth; depth > 0; depth -= 1) {
    if ($from.node(depth).isBlock) return $from.after(depth);
  }
  return Math.min(selection.to, view.state.doc.content.size);
}

function readImageAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => typeof reader.result === 'string'
      ? resolve(reader.result)
      : reject(new Error('图片读取结果为空。'));
    reader.onerror = () => reject(reader.error ?? new Error('读取图片失败。'));
    reader.readAsDataURL(file);
  });
}

function findPendingImage(view: EditorView, id: string): number | null {
  let found: number | null = null;
  view.state.doc.descendants((node, position) => {
    if (node.type.name === 'image' && node.attrs.pendingId === id) {
      found = position;
      return false;
    }
    return true;
  });
  return found;
}

function insertPendingImages(
  view: EditorView,
  files: File[],
  changePendingCount: (delta: number) => void,
  setStatus: (message: string) => void,
  statusNote?: string,
): void {
  const imageType = view.state.schema.nodes.image;
  if (!imageType) {
    setStatus('正文图片节点尚未就绪，请重试。');
    return;
  }

  const pending = files.map((file) => ({ file, id: makePendingId() }));
  const nodes = pending.map(({ file, id }) => imageType.create({
    src: '',
    alt: file.name,
    widthPercent: 75,
    pendingId: id,
  }));
  const position = insertionPointAfterCurrentParagraph(view);

  try {
    view.dispatch(view.state.tr.insert(position, nodes).scrollIntoView());
    view.focus();
  } catch (error) {
    setStatus('无法在当前段落后插入图片：' + String(error));
    return;
  }

  changePendingCount(pending.length);
  setStatus(statusNote ?? ('正在读取 ' + pending.length + ' 张本地图片；完成前保存会暂时禁用。'));

  for (const { file, id } of pending) {
    void readImageAsDataUrl(file).then((source) => {
      if (!view.dom.isConnected) return;
      const currentPosition = findPendingImage(view, id);
      if (currentPosition === null) return;
      const currentNode = view.state.doc.nodeAt(currentPosition);
      if (!currentNode) return;
      const transaction = view.state.tr
        .setNodeMarkup(currentPosition, undefined, { ...currentNode.attrs, src: source, pendingId: null })
        .setMeta('addToHistory', false)
        .scrollIntoView();
      view.dispatch(transaction);
      setStatus('本地图片已插入正文。');
    }).catch((error: unknown) => {
      if (view.dom.isConnected) {
        const currentPosition = findPendingImage(view, id);
        const currentNode = currentPosition === null ? null : view.state.doc.nodeAt(currentPosition);
        if (currentPosition !== null && currentNode) {
          view.dispatch(view.state.tr.delete(currentPosition, currentPosition + currentNode.nodeSize).setMeta('addToHistory', false));
        }
      }
      setStatus('图片读取失败：' + messageFromError(error));
    }).finally(() => changePendingCount(-1));
  }
}

function insertBlockAfterCurrentParagraph(editor: Editor, content: JSONContent): void {
  const position = insertionPointAfterCurrentParagraph(editor.view);
  editor.chain().focus().insertContentAt(position, content).run();
}

function isMissingScratchNote(error: unknown): boolean {
  return /^IO:\s*scratch note does not exist at(?:\s|$)/i.test(messageFromError(error).trim());
}
function markToolbarMouseDown(event: MouseEvent<HTMLButtonElement>) {
  event.preventDefault();
}

function ToolButton({
  label,
  active,
  disabled,
  onClick,
}: {
  label: string;
  active?: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className={active ? 'tool-button is-active' : 'tool-button'}
      aria-pressed={active}
      disabled={disabled}
      onMouseDown={markToolbarMouseDown}
      onClick={onClick}
    >
      {label}
    </button>
  );
}

function FormatToolbar({ editor, children }: { editor: Editor | null; children?: ReactNode }) {
  if (!editor) return <div className="editor-tools" aria-hidden="true" />;

  return (
    <div className="editor-tools" aria-label="文字格式工具">
      <ToolButton label="粗体" active={editor.isActive('bold')} onClick={() => editor.chain().focus().toggleBold().run()} />
      <ToolButton label="斜体" active={editor.isActive('italic')} onClick={() => editor.chain().focus().toggleItalic().run()} />
      <span className="tool-divider" aria-hidden="true" />
      <ToolButton
        label="小标题"
        active={editor.isActive('heading', { level: 2 })}
        onClick={() => editor.chain().focus().toggleHeading({ level: 2 }).run()}
      />
      <ToolButton label="项目符号" active={editor.isActive('bulletList')} onClick={() => editor.chain().focus().toggleBulletList().run()} />
      <ToolButton label="编号" active={editor.isActive('orderedList')} onClick={() => editor.chain().focus().toggleOrderedList().run()} />
      <span className="tool-divider" aria-hidden="true" />
      <ToolButton label="撤销" disabled={!editor.can().undo()} onClick={() => editor.chain().focus().undo().run()} />
      <ToolButton label="重做" disabled={!editor.can().redo()} onClick={() => editor.chain().focus().redo().run()} />
      {children}
    </div>
  );
}

function SectionLabel({ id, index, title, subtitle }: { id: string; index: string; title: string; subtitle: string }) {
  return (
    <div id={id} className="section-label">
      <span className="section-index">{index}</span>
      <span className="section-label-title">{title}</span>
      <span className="section-label-subtitle">{subtitle}</span>
    </div>
  );
}

export default function App() {
  const sample = useMemo(() => makeSampleSnapshot(), []);
  const sampleDocument = useMemo(() => hydrateSnapshot(sample), [sample]);
  const noteIdRef = useRef(sample.document.id);
  const titleRef = useRef(sample.document.title);
  const revisionRef = useRef<string | null>(null);
  const initialCreateAllowedRef = useRef(false);
  const editVersionRef = useRef(0);
  const savedVersionRef = useRef(0);
  const imageInputRef = useRef<HTMLInputElement>(null);
  const [title, setTitle] = useState(sample.document.title);
  const [currentSection, setCurrentSection] = useState('笔记标题');
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [editVersion, setEditVersion] = useState(0);
  const [savedEditVersion, setSavedEditVersion] = useState(0);
  const [revision, setRevision] = useState<string | null>(null);
  const [prototypeInfo, setPrototypeInfo] = useState<PrototypeInfo | null>(null);
  const [ready, setReady] = useState(!desktopMode);
  const [opening, setOpening] = useState(desktopMode);
  const [saving, setSaving] = useState(false);
  const [statusMessage, setStatusMessage] = useState(
    desktopMode ? '正在连接桌面笔记文件……' : '浏览器预览：只编辑当前内存，桌面文件保存不可用。',
  );
  const [lastSaveStats, setLastSaveStats] = useState<{ bytes: number; elapsedMs: number } | null>(null);
  const [pendingImages, setPendingImages] = useState(0);
  const [externalRevision, setExternalRevision] = useState<string | null>(null);
  const [diagnosticError, setDiagnosticError] = useState<string | null>(null);

  const isDirty = editVersion !== savedEditVersion;
  const markChanged = useCallback(() => {
    editVersionRef.current += 1;
    setEditVersion(editVersionRef.current);
  }, []);
  const markChangedRef = useRef(markChanged);
  markChangedRef.current = markChanged;
  const editorUpdate = useCallback(() => markChangedRef.current(), []);
  const changePendingCount = useCallback((delta: number) => {
    setPendingImages((count) => Math.max(0, count + delta));
  }, []);
  const focusCue = useCallback(() => setCurrentSection('线索区'), []);
  const focusContent = useCallback(() => setCurrentSection('内容区'), []);
  const focusSummary = useCallback(() => setCurrentSection('总结区'), []);

  const cueEditor = useEditor({
    extensions: [StarterKit],
    content: sampleDocument.cue as JSONContent,
    immediatelyRender: false,
    editorProps: { attributes: { class: 'tiptap rich-text cue-editor' } },
    onUpdate: editorUpdate,
    onFocus: focusCue,
  }, []);
  const contentEditor = useEditor({
    extensions: [StarterKit, ResizableImage, MindMapExtension],
    content: sampleDocument.content as JSONContent,
    immediatelyRender: false,
    editorProps: {
      attributes: { class: 'tiptap rich-text body-editor' },
      handlePaste: (view, event) => {
        const imageFiles = Array.from(event.clipboardData?.files ?? [])
          .filter((file) => file.type.toLowerCase().startsWith('image/'));
        if (!imageFiles.length) return false;

        const supported = imageFiles.filter(supportsLocalImage);
        const skipped = imageFiles.length - supported.length;
        event.preventDefault();
        if (!supported.length) {
          setStatusMessage('仅支持粘贴 PNG、JPEG、WebP 或 GIF 图片。');
          return true;
        }
        const note = skipped
          ? '正在读取 ' + supported.length + ' 张图片；已跳过 ' + skipped + ' 张不支持的图片。'
          : undefined;
        insertPendingImages(view, supported, changePendingCount, setStatusMessage, note);
        return true;
      },
    },
    onUpdate: editorUpdate,
    onFocus: focusContent,
  }, []);
  const summaryEditor = useEditor({
    extensions: [StarterKit],
    content: sampleDocument.summary as JSONContent,
    immediatelyRender: false,
    editorProps: { attributes: { class: 'tiptap rich-text summary-editor' } },
    onUpdate: editorUpdate,
    onFocus: focusSummary,
  }, []);

  const setLoadedDocument = useCallback((document: NoteDocument, loadedRevision: string) => {
    cueEditor?.commands.setContent(document.cue as JSONContent, { emitUpdate: false });
    contentEditor?.commands.setContent(document.content as JSONContent, { emitUpdate: false });
    summaryEditor?.commands.setContent(document.summary as JSONContent, { emitUpdate: false });
    noteIdRef.current = document.id;
    titleRef.current = document.title;
    setTitle(document.title);
    revisionRef.current = loadedRevision;
    initialCreateAllowedRef.current = false;
    setRevision(loadedRevision);
    editVersionRef.current = 0;
    savedVersionRef.current = 0;
    setEditVersion(0);
    setSavedEditVersion(0);
    setLastSaveStats(null);
    setExternalRevision(null);
    setDiagnosticError(null);
    setPendingImages(0);
  }, [contentEditor, cueEditor, summaryEditor]);

  useEffect(() => {
    if (!desktopMode || !cueEditor || !contentEditor || !summaryEditor) return;

    let active = true;
    async function openInitialNote() {
      setOpening(true);
      try {
        try {
          const info = await invoke<PrototypeInfo>('prototype_info');
          if (active) setPrototypeInfo(info);
        } catch (error) {
          if (active) setDiagnosticError(`读取样机路径失败：${messageFromError(error)}`);
        }

        let loaded: NoteSnapshot & { revision: string };
        try {
          loaded = await invoke<NoteSnapshot & { revision: string }>('prototype_open');
        } catch (error) {
          if (!active) return;
          if (!isMissingScratchNote(error)) throw error;
          initialCreateAllowedRef.current = true;
          revisionRef.current = null;
          setRevision(null);
          savedVersionRef.current = 0;
          editVersionRef.current = 1;
          setEditVersion(1);
          setSavedEditVersion(0);
          setReady(true);
          setStatusMessage('尚无本机临时笔记文件；已载入样例，首次保存将创建文件。');
          return;
        }
        if (!active) return;
        const document = hydrateSnapshot(loaded);
        setLoadedDocument(document, loaded.revision);
        setReady(true);
        setStatusMessage('已从桌面笔记文件打开；编辑后可手动保存。');
      } catch (error) {
        if (!active) return;
        setReady(false);
        setStatusMessage(`无法打开桌面笔记文件：${messageFromError(error)}`);
      } finally {
        if (active) setOpening(false);
      }
    }

    void openInitialNote();
    return () => { active = false; };
  }, [contentEditor, cueEditor, setLoadedDocument, summaryEditor]);

  async function saveNote() {
    if (!desktopMode || !ready || (revisionRef.current === null && !initialCreateAllowedRef.current) || saving || opening || pendingImages > 0) return;
    const cue = cueEditor;
    const content = contentEditor;
    const summary = summaryEditor;
    if (!cue || !content || !summary) return;

    const expectedRevision = revisionRef.current;
    const versionAtStart = editVersionRef.current;
    const document: NoteDocument = {
      schemaVersion: 1,
      id: noteIdRef.current,
      title: titleRef.current,
      cue: cue.getJSON() as JsonNode,
      content: content.getJSON() as JsonNode,
      summary: summary.getJSON() as JsonNode,
    };

    setSaving(true);
    setStatusMessage('正在保存当前草稿……');
    try {
      const snapshot = await makePersistentSnapshot(document);
      const result = await invoke<SaveResult>('prototype_save', {
        snapshot,
        expectedRevision,
      });
      revisionRef.current = result.revision;
      initialCreateAllowedRef.current = false;
      setRevision(result.revision);
      savedVersionRef.current = versionAtStart;
      setSavedEditVersion(versionAtStart);
      setLastSaveStats({ bytes: result.bytes, elapsedMs: result.elapsedMs });
      setStatusMessage(
        editVersionRef.current === versionAtStart
          ? '保存完成。'
          : '保存了开始操作时的草稿，保存期间的新改动仍未保存。',
      );
    } catch (error) {
      const detail = messageFromError(error);
      const conflict = /conflict|revision|版本/i.test(detail);
      setStatusMessage(
        conflict
          ? `保存冲突：桌面文件已被外部修改，本地草稿仍保留在当前页面。请重开后比较内容。`
          : `保存失败：${detail}。本地草稿仍保留在当前页面。`,
      );
    } finally {
      setSaving(false);
    }
  }

  async function reopenNote() {
    if (!desktopMode || !ready || revisionRef.current === null || saving || opening) return;
    if (isDirty && !window.confirm('重开会替换当前未保存的草稿。确定丢弃这些改动并重新读取桌面文件吗？')) return;
    if (!cueEditor || !contentEditor || !summaryEditor) return;

    setOpening(true);
    cueEditor.setEditable(false);
    contentEditor.setEditable(false);
    summaryEditor.setEditable(false);
    setStatusMessage('正在重新读取桌面笔记文件……');
    try {
      const loaded = await invoke<NoteSnapshot & { revision: string }>('prototype_open');
      const document = hydrateSnapshot(loaded);
      setLoadedDocument(document, loaded.revision);
      setReady(true);
      setStatusMessage('已从桌面笔记文件重新打开。');
    } catch (error) {
      setStatusMessage(`重开失败：${messageFromError(error)}。当前草稿仍保留。`);
    } finally {
      cueEditor.setEditable(true);
      contentEditor.setEditable(true);
      summaryEditor.setEditable(true);
      setOpening(false);
    }
  }

  async function simulateExternalUpdate() {
    if (!desktopMode || !ready || revisionRef.current === null || saving || opening) return;
    try {
      const result = await invoke<{ revision: string }>('prototype_simulate_external_update');
      setExternalRevision(result.revision);
      setStatusMessage('已模拟桌面文件外部改动；本地版本基线保持不变，下一次保存将触发冲突检查。');
    } catch (error) {
      setStatusMessage(`模拟外部改动失败：${messageFromError(error)}`);
    }
  }

  function onTitleChange(event: ChangeEvent<HTMLInputElement>) {
    const nextTitle = event.currentTarget.value;
    titleRef.current = nextTitle;
    setTitle(nextTitle);
    markChanged();
  }

  function insertImage(event: ChangeEvent<HTMLInputElement>) {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = '';
    if (!file || !contentEditor) return;
    if (!supportsLocalImage(file)) {
      setStatusMessage('仅支持 PNG、JPEG、WebP 或 GIF 图片。');
      return;
    }
    insertPendingImages(contentEditor.view, [file], changePendingCount, setStatusMessage);
  }

  function addMindMap() {
    if (!contentEditor) return;
    insertBlockAfterCurrentParagraph(contentEditor, {
      type: 'mindMap',
      attrs: { data: makeMindMap('新的思维导图') },
    });
  }

  function appendLongSample() {
    if (!contentEditor) return;
    const paragraphs = Array.from({ length: 24 }, (_, index) => ({
      type: 'paragraph',
      content: [{ type: 'text', text: `观察样例 ${index + 1}：把一次阅读中的问题、证据和可执行动作连在一起，再回到原文检查是否有遗漏。这个段落用于观察较长正文在连续编辑时的布局和响应。` }],
    }));
    contentEditor.chain().focus().insertContent(paragraphs).run();
    setStatusMessage('已在正文末尾加入较长示例；它仍是当前草稿，需要手动保存。');
  }

  const statusLabel = pendingImages > 0
    ? '正在载入图片'
    : desktopMode
      ? (isDirty ? '尚未保存' : ready ? '已保存' : '等待桌面文件')
      : (isDirty ? '内存草稿有改动' : '样例仅在内存中');
  const canUseDesktopFile = desktopMode && ready && revision !== null && !opening && !saving;
  const canReopenFile = canUseDesktopFile;
  const canSaveDesktop = desktopMode && ready && (revision !== null || initialCreateAllowedRef.current) && !opening && !saving && pendingImages === 0;

  return (
    <div className={sidebarOpen ? 'app-shell' : 'app-shell sidebar-collapsed'}>
      {sidebarOpen ? (
        <aside className="sidebar" aria-label="笔记导航">
          <div className="brand-row">
            <span className="brand-mark" aria-hidden="true">C</span>
            <span>CueNote</span>
            <button className="sidebar-toggle quiet-button" type="button" aria-label="收起侧栏" onClick={() => setSidebarOpen(false)}>收起</button>
          </div>
          <p className="sidebar-kicker">技术样机 · 单篇笔记</p>
          <div className="sidebar-group-title">当前笔记</div>
          <div className="sidebar-note-row" aria-current="page">
            <span className="sidebar-note-icon" aria-hidden="true">✎</span>
            <span className="sidebar-note-text">
              <strong>{title || '未命名笔记'}</strong>
              <small>{isDirty ? '有未保存改动' : '康奈尔笔记'}</small>
            </span>
          </div>
          <div className="sidebar-footer">只验证单篇编辑与文件往返</div>
        </aside>
      ) : null}

      <main className="workspace">
        <header className="workspace-header">
          <div className="header-leading">
            {!sidebarOpen ? (
              <button className="quiet-button" type="button" onClick={() => setSidebarOpen(true)}>显示侧栏</button>
            ) : null}
            <div className="breadcrumb"><span>我的笔记</span><span aria-hidden="true">/</span><strong>{title || '未命名笔记'}</strong></div>
          </div>
          <div className="header-actions">
            <span className={isDirty ? 'save-indicator is-dirty' : 'save-indicator'}>
              <span className="status-dot" aria-hidden="true" />
              {statusLabel}
            </span>
            <button
              type="button"
              className="button-secondary"
              disabled={!canReopenFile}
              onClick={() => void reopenNote()}
              title={desktopMode ? '重新读取桌面笔记文件' : '浏览器预览不能读取桌面笔记文件'}
            >
              重开文件
            </button>
            <button
              type="button"
              className="button-primary"
              disabled={!canSaveDesktop || !isDirty}
              onClick={() => void saveNote()}
              title={desktopMode ? '将当前草稿保存到桌面笔记文件' : '浏览器预览只编辑内存，无法保存桌面文件'}
            >
              {saving ? '保存中…' : '保存笔记'}
            </button>
          </div>
        </header>

        <section className="state-strip" aria-live="polite">
          <div className="state-summary">
            <span>当前区域 <strong>{currentSection}</strong></span>
            <span className="state-separator" aria-hidden="true">·</span>
            <span>{statusMessage}</span>
          </div>
          {lastSaveStats ? (
            <div className="save-metrics">
              <span>文件 {formatBytes(lastSaveStats.bytes)}</span>
              <span>耗时 {lastSaveStats.elapsedMs} ms</span>
            </div>
          ) : null}
        </section>

        <div className="paper-wrap">
          <article className="note-paper" aria-busy={!ready || opening}>
            {!sidebarOpen ? <div className="paper-sidebar-handle" /> : null}
            <div className="paper-topline">
              <span>康奈尔笔记</span>
              <span>{desktopMode ? '桌面文件 · 手动保存' : '浏览器内存预览'}</span>
            </div>
            <label className="visually-hidden" htmlFor="note-title">笔记标题</label>
            <input
              id="note-title"
              className="note-title"
              value={title}
              onChange={onTitleChange}
              onFocus={() => setCurrentSection('笔记标题')}
              placeholder="写下笔记标题"
              disabled={!ready || opening}
            />
            <p className="title-hint">把这篇记录的核心问题写在标题里</p>

            <div className="editor-layout">
              <section className="cue-panel" aria-labelledby="cue-heading">
                <SectionLabel id="cue-heading" index="01" title="线索区" subtitle="问题 · 关键词" />
                <FormatToolbar editor={cueEditor} />
                <div className="editor-surface cue-surface">
                  <EditorContent editor={cueEditor} />
                </div>
              </section>

              <section className="content-panel" aria-labelledby="content-heading">
                <SectionLabel id="content-heading" index="02" title="内容区" subtitle="文字 · 图片 · 思维导图" />
                <FormatToolbar editor={contentEditor}>
                  <span className="tool-divider" aria-hidden="true" />
                  <ToolButton
                    label="插入图片"
                    disabled={!contentEditor}
                    onClick={() => imageInputRef.current?.click()}
                  />
                  <ToolButton label="插入思维导图" disabled={!contentEditor} onClick={addMindMap} />
                </FormatToolbar>
                <input
                  ref={imageInputRef}
                  className="visually-hidden"
                  type="file"
                  accept="image/png,image/jpeg,image/webp,image/gif"
                  onChange={(event) => void insertImage(event)}
                  tabIndex={-1}
                />
                <div className="editor-surface content-surface">
                  <EditorContent editor={contentEditor} />
                </div>
              </section>
            </div>

            <section className="summary-panel" aria-labelledby="summary-heading">
              <SectionLabel id="summary-heading" index="03" title="总结区" subtitle="用自己的话收束本篇记录" />
              <FormatToolbar editor={summaryEditor} />
              <div className="editor-surface summary-surface">
                <EditorContent editor={summaryEditor} />
              </div>
            </section>

            {!ready || opening ? (
              <div className="loading-overlay">
                <div className="loading-card">
                  <span className="loading-spinner" aria-hidden="true" />
                  <strong>{opening ? '正在打开笔记文件' : '桌面文件暂不可用'}</strong>
                  <span>{statusMessage}</span>
                </div>
              </div>
            ) : null}
          </article>
        </div>

        <details className="diagnostics">
          <summary>开发诊断与样机操作</summary>
          <div className="diagnostic-content">
            <p>样机以手动保存验证正文、图片资源与思维导图的文件往返；这里显示运行位置和最近一次真实保存结果。</p>
            <div className="diagnostic-grid">
              <div><span>模式</span><strong>{desktopMode ? 'Tauri 桌面' : '浏览器内存预览'}</strong></div>
              <div><span>当前修订</span><strong>{revision ?? (initialCreateAllowedRef.current ? '尚未创建，首次保存时建立' : '未读取')}</strong></div>
              <div><span>笔记文件</span><strong>{prototypeInfo?.path ?? '桌面端运行时提供'}</strong></div>
              <div><span>恢复位置</span><strong>{prototypeInfo?.recoveryPath ?? '桌面端运行时提供'}</strong></div>
              {externalRevision ? <div><span>模拟的外部修订</span><strong>{externalRevision}</strong></div> : null}
            </div>
            {diagnosticError ? <p className="diagnostic-error">{diagnosticError}</p> : null}
            <div className="diagnostic-actions">
              <button type="button" className="button-secondary" disabled={!canUseDesktopFile} onClick={() => void simulateExternalUpdate()}>
                模拟外部改动
              </button>
              <button type="button" className="button-secondary" disabled={!contentEditor || !ready || opening} onClick={appendLongSample}>
                插入较长正文样例
              </button>
              {!desktopMode ? <span>浏览器预览没有桌面文件读写能力；保存与重开按钮保持禁用。</span> : null}
            </div>
          </div>
        </details>

        <footer className="page-footer">
          <span>{!desktopMode ? '浏览器预览只保留当前页面内存' : revision !== null ? '桌面样机文件位于本机临时目录' : '首次保存后会创建本机临时笔记文件'}</span>
          <span>单篇编辑技术样机</span>
        </footer>
      </main>
    </div>
  );
}
