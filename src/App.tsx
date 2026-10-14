import { useEffect, useRef, useState } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createSaveCoordinator, type SaveCoordinatorState } from './save-coordinator';
import {
  workspaceApi,
  type NoteSession,
  type PlainNoteText,
  type RecoveryDraftSummary,
  type RecoveryList,
  type SaveReceipt,
  type SavedCopy,
  type WorkspaceError,
  type WorkspaceView,
} from './workspace-api';

const desktopRuntime = isTauri();

type PendingTransition = {
  label: string;
  close?: boolean;
  run: () => Promise<void>;
};

type CurrentCopy = SavedCopy & { sessionId: string; text: PlainNoteText };
type SaveCoordinator = ReturnType<typeof createSaveCoordinator<PlainNoteText, SaveReceipt>>;

function sameText(left: PlainNoteText | null, right: PlainNoteText | null) {
  return left !== null && right !== null
    && left.title === right.title
    && left.cue === right.cue
    && left.content === right.content
    && left.summary === right.summary;
}

function toWorkspaceError(error: unknown): WorkspaceError {
  if (error && typeof error === 'object') {
    const value = error as Partial<WorkspaceError>;
    if (typeof value.message === 'string') {
      return {
        kind: typeof value.kind === 'string' ? value.kind : 'workspace',
        message: value.message,
        recoveryPath: typeof value.recoveryPath === 'string' ? value.recoveryPath : null,
        recoveryError: typeof value.recoveryError === 'string' ? value.recoveryError : null,
      };
    }
  }

  return {
    kind: 'workspace',
    message: error instanceof Error ? error.message : String(error),
  };
}

function App() {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [workspace, setWorkspace] = useState<WorkspaceView | null>(null);
  const [session, setSession] = useState<NoteSession | null>(null);
  const [draft, setDraft] = useState<PlainNoteText | null>(null);
  const [savedText, setSavedText] = useState<PlainNoteText | null>(null);
  const [isInitializing, setIsInitializing] = useState(desktopRuntime);
  const [isOperationBusy, setIsOperationBusy] = useState(false);
  const [saveState, setSaveState] = useState<SaveCoordinatorState>({
    phase: 'clean', sessionId: null, generation: 0, savedGeneration: 0, isDirty: false, error: null,
  });
  const [saveError, setSaveError] = useState<WorkspaceError | null>(null);
  const [error, setError] = useState<WorkspaceError | null>(null);
  const [safeCopyError, setSafeCopyError] = useState<WorkspaceError | null>(null);
  const [currentCopy, setCurrentCopy] = useState<CurrentCopy | null>(null);
  const [recoveryList, setRecoveryList] = useState<RecoveryList | null>(null);
  const [recoveryLoading, setRecoveryLoading] = useState(false);
  const [recoveryError, setRecoveryError] = useState<WorkspaceError | null>(null);
  const [recoveryCopyPaths, setRecoveryCopyPaths] = useState<Record<string, string>>({});
  const [closeGuardReady, setCloseGuardReady] = useState(!desktopRuntime);
  const [isComposing, setIsComposing] = useState(false);
  const [pendingTransition, setPendingTransition] = useState<PendingTransition | null>(null);
  const [dialogMessage, setDialogMessage] = useState<string | null>(null);
  const [isTransitionSaving, setIsTransitionSaving] = useState(false);
  const [isWaitingForOperation, setIsWaitingForOperation] = useState(false);
  const [isAbandoning, setIsAbandoning] = useState(false);

  const workspaceRef = useRef<WorkspaceView | null>(null);
  const sessionRef = useRef<NoteSession | null>(null);
  const draftRef = useRef<PlainNoteText | null>(null);
  const savedTextRef = useRef<PlainNoteText | null>(null);
  const operationInProgressRef = useRef(false);
  const pendingTransitionRef = useRef<PendingTransition | null>(null);
  const transitionAttemptRef = useRef<PendingTransition | null>(null);
  const transitionWaitingForOperationRef = useRef(false);
  const activeTransitionRef = useRef<PendingTransition | null>(null);
  const isAbandoningRef = useRef(false);
  const leaveIntentRef = useRef<ReturnType<SaveCoordinator['leave']> | null>(null);
  const lastSaveErrorRef = useRef<unknown | null>(null);
  const isComposingRef = useRef(false);
  const compositionWaitersRef = useRef<Array<() => void>>([]);
  const closeRequestHandlerRef = useRef<() => void>(() => undefined);
  const flushCurrentRef = useRef<() => void>(() => undefined);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const saveCoordinatorRef = useRef<SaveCoordinator | null>(null);

  const updateWorkspace = (value: WorkspaceView | null) => {
    workspaceRef.current = value;
    setWorkspace(value);
  };

  const updateSessionMetadata = (value: NoteSession | null) => {
    sessionRef.current = value;
    setSession(value);
  };

  const refreshRecoveryDrafts = async () => {
    setRecoveryLoading(true);
    try {
      setRecoveryList(await workspaceApi.listRecoveryDrafts());
      setRecoveryError(null);
    } catch (cause) {
      setRecoveryError(toWorkspaceError(cause));
    } finally {
      setRecoveryLoading(false);
    }
  };

  const applySession = (value: NoteSession | null) => {
    updateSessionMetadata(value);
    draftRef.current = value?.text ?? null;
    savedTextRef.current = value?.text ?? null;
    setDraft(value?.text ?? null);
    setSavedText(value?.text ?? null);
    setSaveError(null);
    setSafeCopyError(null);
    setCurrentCopy(null);
    saveCoordinatorRef.current?.reset(
      value?.sessionId ?? null,
      value?.text ?? null,
      Boolean(value && value.revision === null && !value.readOnlyReason),
    );
    const currentWorkspace = workspaceRef.current;
    if (currentWorkspace) updateWorkspace({ ...currentWorkspace, session: value });
  };

  const applyWorkspace = (value: WorkspaceView) => {
    updateWorkspace(value);
    updateSessionMetadata(value.session);
    draftRef.current = value.session?.text ?? null;
    savedTextRef.current = value.session?.text ?? null;
    setDraft(value.session?.text ?? null);
    setSavedText(value.session?.text ?? null);
    setSaveError(null);
    setSafeCopyError(null);
    setCurrentCopy(null);
    saveCoordinatorRef.current?.reset(
      value.session?.sessionId ?? null,
      value.session?.text ?? null,
      Boolean(value.session && value.session.revision === null && !value.session.readOnlyReason),
    );
  };

  if (saveCoordinatorRef.current === null) {
    saveCoordinatorRef.current = createSaveCoordinator<PlainNoteText, SaveReceipt>({
      save: (sessionId, snapshot) => workspaceApi.save(sessionId, snapshot),
      onSaved: (snapshot, receipt) => {
        if (sessionRef.current?.sessionId !== snapshot.sessionId) return;
        updateSessionMetadata(receipt.session);
        savedTextRef.current = receipt.session.text;
        setSavedText(receipt.session.text);
        const currentWorkspace = workspaceRef.current;
        if (currentWorkspace) {
          updateWorkspace({ ...currentWorkspace, library: receipt.library, session: receipt.session });
        }
      },
      onState: (nextState) => {
        setSaveState(nextState);
        if (nextState.error !== null) {
          setSaveError(toWorkspaceError(nextState.error));
          if (lastSaveErrorRef.current !== nextState.error) {
            lastSaveErrorRef.current = nextState.error;
            void refreshRecoveryDrafts();
          }
        } else if (nextState.phase === 'saved' && !nextState.isDirty) {
          lastSaveErrorRef.current = null;
          setSaveError(null);
        }
      },
      isConflict: (cause) => {
        if (!cause || typeof cause !== 'object') return false;
        const kind = (cause as Partial<WorkspaceError>).kind;
        return kind === 'conflict' || kind === 'version_conflict' || kind === 'revision_conflict';
      },
    });
  }

  const saveCoordinator = saveCoordinatorRef.current!;

  const clearPendingTransition = (resumeAutomaticSaving = true) => {
    leaveIntentRef.current?.cancel();
    leaveIntentRef.current = null;
    pendingTransitionRef.current = null;
    transitionAttemptRef.current = null;
    transitionWaitingForOperationRef.current = false;
    setPendingTransition(null);
    setDialogMessage(null);
    setIsTransitionSaving(false);
    setIsWaitingForOperation(false);
    if (resumeAutomaticSaving && !isAbandoningRef.current) saveCoordinator.resumeAutomaticSaving();
  };

  const executeTransition = async (transition: PendingTransition) => {
    if (operationInProgressRef.current) return;
    activeTransitionRef.current = transition;
    operationInProgressRef.current = true;
    setIsOperationBusy(true);
    setError(null);
    try {
      await transition.run();
    } catch (cause) {
      const operationError = toWorkspaceError(cause);
      setError(operationError);
      if (transition.close) {
        pendingTransitionRef.current = transition;
        setPendingTransition(transition);
        setDialogMessage('关闭请求未完成。当前笔记仍留在本页；可以重试关闭或取消。');
      }
    } finally {
      operationInProgressRef.current = false;
      if (activeTransitionRef.current === transition) activeTransitionRef.current = null;
      setIsOperationBusy(false);
      saveCoordinator.resumeAutomaticSaving();
    }
  };

  const waitForComposition = () => {
    if (!isComposingRef.current) return Promise.resolve();
    return new Promise<void>((resolve) => compositionWaitersRef.current.push(resolve));
  };

  const attemptTransitionAfterSave = async (transition: PendingTransition) => {
    if (transitionAttemptRef.current) return;
    transitionAttemptRef.current = transition;
    setIsTransitionSaving(true);
    setDialogMessage(`正在保存最新修改，完成后继续${transition.label}。可以取消此次操作。`);
    try {
      await waitForComposition();
      if (pendingTransitionRef.current !== transition || transitionAttemptRef.current !== transition) return;
      const intent = saveCoordinator.leave();
      leaveIntentRef.current = intent;
      const saved = await intent.result;
      if (leaveIntentRef.current === intent) leaveIntentRef.current = null;
      if (pendingTransitionRef.current !== transition || transitionAttemptRef.current !== transition) return;
      if (saved) {
        clearPendingTransition(false);
        void executeTransition(transition);
        return;
      }
      const state = saveCoordinator.getState();
      setDialogMessage(state.phase === 'conflict'
        ? '原文件已发生变化，草稿仍留在当前页。请检查冲突、重试或另存草稿后再继续。'
        : '自动保存没有完成，草稿仍留在当前页。可以重试、另存草稿，或取消此次操作。');
    } finally {
      if (transitionAttemptRef.current === transition) {
        transitionAttemptRef.current = null;
        setIsTransitionSaving(false);
      }
    }
  };

  const beginTransition = (transition: PendingTransition) => {
    if (transitionAttemptRef.current || operationInProgressRef.current) return;
    const state = saveCoordinator.getState();
    if (isComposingRef.current || state.isDirty || state.phase === 'saving') {
      void attemptTransitionAfterSave(transition);
      return;
    }
    clearPendingTransition(false);
    void executeTransition(transition);
  };

  const requestTransition = (transition: PendingTransition, queueWhileBusy = false) => {
    if (pendingTransitionRef.current) return;
    if (isInitializing || operationInProgressRef.current) {
      if (!queueWhileBusy) return;
      pendingTransitionRef.current = transition;
      transitionWaitingForOperationRef.current = true;
      setPendingTransition(transition);
      setIsWaitingForOperation(true);
      setDialogMessage(isInitializing
        ? `正在初始化本机工作区，完成后会继续${transition.label}。`
        : `正在完成当前操作，完成后会继续${transition.label}。`);
      return;
    }
    const state = saveCoordinator.getState();
    if (isComposingRef.current || state.isDirty || state.phase === 'saving') {
      pendingTransitionRef.current = transition;
      setPendingTransition(transition);
      void attemptTransitionAfterSave(transition);
      return;
    }
    void executeTransition(transition);
  };

  useEffect(() => {
    const transition = pendingTransition;
    if (!transition || !transitionWaitingForOperationRef.current || isInitializing || isOperationBusy || operationInProgressRef.current) return;
    transitionWaitingForOperationRef.current = false;
    setIsWaitingForOperation(false);
    beginTransition(transition);
  }, [pendingTransition, isInitializing, isOperationBusy]);

  const saveCurrent = async () => {
    const currentSession = sessionRef.current;
    if (!desktopRuntime || isInitializing || operationInProgressRef.current || !currentSession || currentSession.readOnlyReason) return;
    await waitForComposition();
    await saveCoordinator.flush();
  };
  flushCurrentRef.current = () => { void saveCurrent(); };

  const saveCurrentAs = async () => {
    await waitForComposition();
    const currentSession = sessionRef.current;
    const snapshot = draftRef.current;
    if (!desktopRuntime || isInitializing || operationInProgressRef.current || !currentSession || !snapshot || currentSession.readOnlyReason) return;
    const copyText = { ...snapshot };
    operationInProgressRef.current = true;
    setIsOperationBusy(true);
    setSafeCopyError(null);
    try {
      const copy = await workspaceApi.saveCurrentAs(currentSession.sessionId, copyText);
      if (copy) setCurrentCopy({ ...copy, sessionId: currentSession.sessionId, text: copyText });
    } catch (cause) {
      setSafeCopyError(toWorkspaceError(cause));
    } finally {
      operationInProgressRef.current = false;
      setIsOperationBusy(false);
    }
  };

  const saveRecoveryAs = async (draft: RecoveryDraftSummary) => {
    if (operationInProgressRef.current) return;
    operationInProgressRef.current = true;
    setIsOperationBusy(true);
    setError(null);
    try {
      const copy = await workspaceApi.saveRecoveryAs(draft.recoveryId);
      if (copy) setRecoveryCopyPaths((current) => ({ ...current, [draft.recoveryId]: copy.path }));
    } catch (cause) {
      setError(toWorkspaceError(cause));
    } finally {
      operationInProgressRef.current = false;
      setIsOperationBusy(false);
    }
  };

  const restoreRecovery = (draft: RecoveryDraftSummary) => requestTransition({
    label: `恢复“${draft.title || '未命名笔记'}”为新笔记`,
    run: async () => {
      const receipt = await workspaceApi.restoreRecoveryAsCopy(draft.recoveryId);
      const currentWorkspace = workspaceRef.current;
      if (currentWorkspace) {
        updateWorkspace({ ...currentWorkspace, library: receipt.library, session: receipt.session });
      }
      applySession(receipt.session);
      await refreshRecoveryDrafts();
    },
  });

  const discardAndContinue = async () => {
    const transition = pendingTransitionRef.current;
    if (!transition || isTransitionSaving || isOperationBusy || isSaving || isComposing || isInitializing) return;
    isAbandoningRef.current = true;
    operationInProgressRef.current = true;
    setIsAbandoning(true);
    setIsOperationBusy(true);
    setDialogMessage('正在停止自动保存并等待当前磁盘写入。可以取消此次操作。');
    try {
      await saveCoordinator.pauseAutomaticSavingAndWait();
      if (pendingTransitionRef.current !== transition) return;
      clearPendingTransition(false);
      isAbandoningRef.current = false;
      operationInProgressRef.current = false;
      setIsAbandoning(false);
      setIsOperationBusy(false);
      await executeTransition(transition);
    } finally {
      isAbandoningRef.current = false;
      operationInProgressRef.current = false;
      setIsAbandoning(false);
      setIsOperationBusy(false);
      if (pendingTransitionRef.current !== transition) saveCoordinator.resumeAutomaticSaving();
    }
  };

  const cancelPendingTransition = () => clearPendingTransition();

  const requestClose = () => {
    if (!desktopRuntime || pendingTransitionRef.current?.close || activeTransitionRef.current?.close) return;
    if (pendingTransitionRef.current) clearPendingTransition();
    requestTransition({
      label: '关闭 CueNote',
      close: true,
      run: async () => workspaceApi.completeClose(),
    }, true);
  };
  closeRequestHandlerRef.current = requestClose;

  useEffect(() => {
    if (!desktopRuntime) return;
    let active = true;

    void workspaceApi.get()
      .then((view) => {
        if (active) applyWorkspace(view);
      })
      .catch((cause: unknown) => {
        if (active) setError(toWorkspaceError(cause));
      })
      .finally(() => {
        if (active) setIsInitializing(false);
      });
    void refreshRecoveryDrafts();

    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!desktopRuntime || isInitializing) return;
    let active = true;
    let unlisten: (() => void) | null = null;
    setCloseGuardReady(false);

    void listen('cuenote:close-requested', () => {
      if (active) closeRequestHandlerRef.current();
    })
      .then(async (stopListening) => {
        if (!active) {
          stopListening();
          return;
        }
        unlisten = stopListening;
        await workspaceApi.activateCloseGuard();
        if (active) setCloseGuardReady(true);
      })
      .catch((cause: unknown) => {
        if (active) setError(toWorkspaceError(cause));
      });

    return () => {
      active = false;
      setCloseGuardReady(false);
      unlisten?.();
    };
  }, [isInitializing]);

  useEffect(() => {
    saveCoordinator.resume();
    return () => saveCoordinator.dispose();
  }, [saveCoordinator]);

  useEffect(() => {
    if (!pendingTransition) return;
    const dialog = dialogRef.current;
    dialog?.querySelector<HTMLButtonElement>('[data-dialog-cancel]')?.focus();

    const handleDialogKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        clearPendingTransition();
        return;
      }
      if (event.key !== 'Tab' || !dialog) return;
      const controls = Array.from(dialog.querySelectorAll<HTMLButtonElement>('button:not([disabled])'));
      if (controls.length === 0) return;
      const first = controls[0];
      const last = controls[controls.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };

    window.addEventListener('keydown', handleDialogKeyDown);
    return () => window.removeEventListener('keydown', handleDialogKeyDown);
  }, [pendingTransition]);

  useEffect(() => {
    const handleSaveShortcut = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
        event.preventDefault();
        if (desktopRuntime) flushCurrentRef.current();
      }
    };
    window.addEventListener('keydown', handleSaveShortcut);
    return () => window.removeEventListener('keydown', handleSaveShortcut);
  }, []);

  const updateDraft = (field: keyof PlainNoteText, value: string) => {
    if (operationInProgressRef.current || isAbandoningRef.current) return;
    const current = draftRef.current;
    const currentSession = sessionRef.current;
    if (!current || !currentSession) return;
    const next = { ...current, [field]: value };
    draftRef.current = next;
    setDraft(next);
    if (!isComposingRef.current) saveCoordinator.edit(currentSession.sessionId, next);
  };

  const handleCompositionStart = () => {
    isComposingRef.current = true;
    setIsComposing(true);
  };
  const handleCompositionEnd = (field: keyof PlainNoteText, value: string) => {
    isComposingRef.current = false;
    setIsComposing(false);
    updateDraft(field, value);
    const waiters = compositionWaitersRef.current.splice(0);
    waiters.forEach((resolve) => resolve());
  };

  const chooseLibrary = () => requestTransition({
    label: '更换笔记库',
    run: async () => {
      const nextWorkspace = await workspaceApi.choose();
      if (nextWorkspace) applyWorkspace(nextWorkspace);
    },
  });

  const useDefaultLibrary = () => requestTransition({
    label: '切换到默认笔记库',
    run: async () => applyWorkspace(await workspaceApi.useDefault()),
  });

  const createNote = () => requestTransition({
    label: '新建笔记',
    run: async () => applySession(await workspaceApi.newNote()),
  });

  const openNote = (noteId: string, title: string) => requestTransition({
    label: `打开“${title || '未命名笔记'}”`,
    run: async () => applySession(await workspaceApi.openNote(noteId)),
  });

  const openLibraryFolder = async () => {
    if (isInitializing || operationInProgressRef.current) return;
    operationInProgressRef.current = true;
    setIsOperationBusy(true);
    setError(null);
    try {
      await workspaceApi.openFolder();
    } catch (cause) {
      setError(toWorkspaceError(cause));
    } finally {
      operationInProgressRef.current = false;
      setIsOperationBusy(false);
    }
  };

  const library = workspace?.library ?? null;
  const notes = library?.notes ?? [];
  const isReadOnly = Boolean(session?.readOnlyReason);
  const isSaving = saveState.phase === 'saving';
  const saveFailed = saveState.phase === 'failed' || saveState.phase === 'conflict';
  const isDirty = Boolean(session && draft && !isReadOnly && (
    isComposing || (saveState.sessionId === session.sessionId
      ? saveState.isDirty
      : session.revision === null || !sameText(draft, savedText))
  ));
  const navigationLocked = isInitializing || isOperationBusy || pendingTransition !== null;
  const editingLocked = !desktopRuntime || isInitializing || isOperationBusy || isAbandoning || !closeGuardReady;
  const canDiscardAfterCopy = Boolean(currentCopy && session && draft
    && currentCopy.sessionId === session.sessionId && sameText(currentCopy.text, draft));
  const runtimeStatus = !desktopRuntime
    ? '浏览器只读预览'
    : isInitializing
      ? '正在读取本地笔记库'
      : error && !workspace
        ? '桌面连接失败'
        : '桌面应用 · 本地笔记';
  const saveStatus = isSaving
    ? '正在保存…'
    : isComposing
      ? '正在输入，完成后自动保存'
    : saveState.phase === 'conflict'
      ? '保存冲突，请保留当前草稿'
      : saveFailed
      ? '保存失败，请保留当前内容'
    : isReadOnly
      ? '只读预览'
      : !session
        ? '未选择笔记'
        : isDirty
          ? '有未保存的修改'
          : session.revision === null
            ? '尚未保存到本地'
            : '已保存到本地';

  const renderError = (value: WorkspaceError) => (
    <div className="error-notice" role="alert">
      <strong>操作未完成</strong>
      <p>{value.message}</p>
      {value.recoveryPath && (
        <p className="recovery-detail">恢复位置：<code>{value.recoveryPath}</code></p>
      )}
      {value.recoveryError && (
        <p className="recovery-detail">恢复副本也未能写入：{value.recoveryError}</p>
      )}
    </div>
  );

  return (
    <div className={`app-shell${sidebarCollapsed ? ' sidebar-collapsed' : ''}`}>
      <aside className="sidebar" aria-label="CueNote 工作区">
        <div className="brand-row">
          <div className="brand-mark" aria-hidden="true">C</div>
          {!sidebarCollapsed && (
            <div className="brand-copy">
              <strong>CueNote</strong>
              <span>把想法写清楚</span>
            </div>
          )}
          <button
            className="sidebar-toggle"
            type="button"
            aria-label={sidebarCollapsed ? '展开侧栏' : '收起侧栏'}
            aria-expanded={!sidebarCollapsed}
            title={sidebarCollapsed ? '展开侧栏' : '收起侧栏'}
            onClick={() => setSidebarCollapsed((collapsed) => !collapsed)}
          >
            <span aria-hidden="true">{sidebarCollapsed ? '›' : '‹'}</span>
          </button>
        </div>

        {!sidebarCollapsed && (
          <>
            <div className="sidebar-kicker">
              <span className={`status-dot${desktopRuntime ? ' is-local' : ''}`} aria-hidden="true" />
              <span>{desktopRuntime ? '本机工作区' : '浏览器预览'}</span>
            </div>
            <p className="sidebar-group-title">工作区</p>
            <div className="sidebar-current" aria-current="page">
              <span className="sidebar-item-icon" aria-hidden="true">⌂</span>
              <span>全部笔记</span>
            </div>
            <div className="sidebar-notebook-heading">
              <div>
                <span className="sidebar-group-title">笔记列表</span>
                {library && <span className="sidebar-note-count">{notes.length} 篇笔记</span>}
              </div>
              <button
                className="sidebar-add"
                type="button"
                aria-label="新建笔记"
                title="新建笔记"
                disabled={!desktopRuntime || !library || navigationLocked}
                onClick={createNote}
              >＋</button>
            </div>
            {library ? (
              notes.length > 0 ? (
                <ul className="sidebar-note-list" aria-label="笔记列表">
                  {notes.map((note) => (
                    <li key={note.noteId}>
                      <button
                        className={`sidebar-note-item${session?.noteId === note.noteId ? ' is-current' : ''}`}
                        type="button"
                        aria-current={session?.noteId === note.noteId ? 'page' : undefined}
                        disabled={navigationLocked}
                        onClick={() => openNote(note.noteId, note.title)}
                        title={note.title || '未命名笔记'}
                      >
                        <span className="sidebar-note-bullet" aria-hidden="true" />
                        <span className="sidebar-note-title">{note.title || '未命名笔记'}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              ) : (
                <div className="sidebar-empty">这里还没有笔记</div>
              )
            ) : (
              <div className="sidebar-empty">选择笔记库后开始记录</div>
            )}
            <div className="sidebar-footer">
              <span className="sidebar-footer-rule" />
              <span>笔记保存在本机</span>
            </div>
          </>
        )}
      </aside>

      <main className="workspace">
        <header className="workspace-header">
          <div className="header-leading">
            {sidebarCollapsed && <span className="compact-mark" aria-hidden="true">C</span>}
            <div className="breadcrumb">
              <span>工作台</span>
              <span className="breadcrumb-divider" aria-hidden="true">/</span>
              <strong>{library ? '全部笔记' : '笔记库'}</strong>
            </div>
          </div>
          <div className="header-actions">
            <span className={`runtime-badge${desktopRuntime ? ' is-desktop' : ' is-browser'}`}>
              <span className="status-dot" aria-hidden="true" />
              {runtimeStatus}
            </span>
            {workspace?.appVersion && <span className="version-badge">v{workspace.appVersion}</span>}
          </div>
        </header>

        <section className="preview-heading" aria-labelledby="page-title">
          <div className="heading-copy">
            <div className="eyebrow"><span>CueNote</span><span className="eyebrow-line" />康奈尔笔记</div>
            <h1 id="page-title">
              {desktopRuntime
                ? draft?.title.trim() || (session ? '未命名笔记' : '写下你的第一篇笔记')
                : '让每个想法，都有清晰的落点。'}
            </h1>
            <p>
              {desktopRuntime
                ? library
                  ? '在自己的笔记库中记录与回顾。'
                  : '先选择笔记库，笔记会保存在你控制的本机目录中。'
                : '这是界面预览，不会读取、创建或保存笔记。'}
            </p>
          </div>
          {desktopRuntime && (
            <div className="library-actions" aria-label="笔记库操作">
              {library && (
                <button className="quiet-button" type="button" disabled={navigationLocked} onClick={useDefaultLibrary}>
                  使用默认笔记库
                </button>
              )}
              <button className="quiet-button" type="button" disabled={navigationLocked} onClick={chooseLibrary}>
                {library ? '更换笔记库' : '选择笔记库'}
              </button>
              {library && (
                <button className="quiet-button" type="button" disabled={isInitializing || isOperationBusy} onClick={openLibraryFolder}>
                  打开笔记库目录
                </button>
              )}
            </div>
          )}
          {!desktopRuntime && (
            <div className="preview-seal" aria-label="只读预览">
              <span>FIRST</span>
              <strong>预览</strong>
              <span>BUILD</span>
            </div>
          )}
        </section>

        <section className="runtime-panel" aria-live="polite">
          <span className={`runtime-indicator${error ? ' has-error' : isInitializing ? '' : ' is-ready'}`} aria-hidden="true" />
          <div>
            <strong>{runtimeStatus}</strong>
            <p>
              {!desktopRuntime
                ? '浏览器预览不连接本机文件，也不会模拟保存。请在桌面应用中选择笔记库。'
                : isInitializing
                  ? '正在读取上次使用的笔记库。'
                  : !closeGuardReady
                    ? '正在启用关闭保护，完成前暂不能编辑笔记。'
                  : error && !workspace
                    ? '无法读取工作区。请重试，或重新打开桌面应用。'
                    : library
                      ? `笔记库位置：${library.path}`
                      : workspace?.defaultLibraryPath
                        ? `默认位置：${workspace.defaultLibraryPath}`
                        : '选择一个笔记库，开始保存本机笔记。'}
            </p>
          </div>
          <span className="runtime-readonly">{desktopRuntime ? saveStatus : '只读'}</span>
        </section>

        {!pendingTransition && error && renderError(error)}
        {workspace?.notice && <div className="workspace-notice" role="status">{workspace.notice}</div>}
        {library?.warnings.map((warning, index) => (
          <div className="workspace-notice" role="status" key={`${index}-${warning}`}>{warning}</div>
        ))}

        {desktopRuntime && (
          <section className="recovery-panel" aria-labelledby="recovery-panel-title">
            <div className="recovery-panel-heading">
              <div>
                <span className="eyebrow">本机恢复区</span>
                <h2 id="recovery-panel-title">可恢复草稿</h2>
              </div>
              <button className="text-button" type="button" disabled={recoveryLoading} onClick={() => void refreshRecoveryDrafts()}>
                {recoveryLoading ? '正在读取…' : '刷新'}
              </button>
            </div>
            {recoveryError && renderError(recoveryError)}
            {recoveryLoading && !recoveryList && <p className="recovery-empty">正在读取恢复区。</p>}
            {recoveryList?.warnings.map((warning, index) => (
              <div className="workspace-notice" role="status" key={`${index}-${warning}`}>{warning}</div>
            ))}
            {recoveryList?.drafts.length ? (
              <ul className="recovery-list">
                {recoveryList.drafts.map((recoveryDraft) => (
                  <li className="recovery-item" key={recoveryDraft.recoveryId}>
                    <div className="recovery-copy">
                      <strong>{recoveryDraft.title || '未命名草稿'}</strong>
                      <span>草稿保存时间：{Number.isNaN(Date.parse(recoveryDraft.savedAt))
                        ? recoveryDraft.savedAt
                        : new Date(recoveryDraft.savedAt).toLocaleString()}</span>
                    </div>
                    <div className="recovery-actions">
                      <button
                        className="quiet-button"
                        type="button"
                        disabled={!library || navigationLocked}
                        onClick={() => restoreRecovery(recoveryDraft)}
                      >恢复为新笔记</button>
                      <button
                        className="quiet-button"
                        type="button"
                        disabled={isOperationBusy}
                        onClick={() => void saveRecoveryAs(recoveryDraft)}
                      >另存恢复副本…</button>
                    </div>
                    {recoveryCopyPaths[recoveryDraft.recoveryId] && (
                      <p className="recovery-saved-path" role="status">
                        已将完整恢复副本另存到：<code>{recoveryCopyPaths[recoveryDraft.recoveryId]}</code>
                      </p>
                    )}
                  </li>
                ))}
              </ul>
            ) : recoveryList && recoveryList.drafts.length === 0 ? (
              <p className="recovery-empty">当前没有可恢复草稿。恢复区无法读取时会在这里明确提示，不会当作空列表。</p>
            ) : null}
          </section>
        )}

        {!desktopRuntime ? (
          <article className="note-paper browser-preview" aria-label="康奈尔笔记布局只读预览">
            <div className="paper-topline">
              <span>康奈尔笔记布局</span>
              <span className="paper-topline-right"><span className="paper-dot" />浏览器只读预览</span>
            </div>
            <section className="title-preview" aria-labelledby="preview-title-label">
              <div className="title-copy">
                <span className="section-number">00</span>
                <div><h2 id="preview-title-label">笔记标题</h2></div>
              </div>
              <span className="coming-soon">桌面端编辑</span>
            </section>
            <div className="cornell-grid">
              <section className="cornell-panel cue-panel" aria-labelledby="preview-cue-title">
                <div className="section-heading"><span className="section-number">01</span><h3 id="preview-cue-title">线索区</h3></div>
                <div className="readonly-preview-field">线索文字将在桌面应用中显示</div>
                <span className="panel-note">用于回想与复习</span>
              </section>
              <section className="cornell-panel content-panel" aria-labelledby="preview-content-title">
                <div className="section-heading"><span className="section-number">02</span><h3 id="preview-content-title">内容区</h3><span className="content-hint">主要记录区域</span></div>
                <div className="readonly-preview-field content-preview-field">笔记正文将在桌面应用中显示</div>
              </section>
            </div>
            <section className="summary-panel" aria-labelledby="preview-summary-title">
              <div className="section-heading"><span className="section-number">03</span><h3 id="preview-summary-title">总结区</h3></div>
              <div className="readonly-preview-field summary-preview-field">总结文字将在桌面应用中显示</div>
            </section>
          </article>
        ) : isInitializing ? (
          <section className="empty-workspace" aria-live="polite"><span className="loading-mark" aria-hidden="true" /><h2>正在读取笔记库</h2><p>请稍候，正在打开上次使用的位置。</p></section>
        ) : error && !workspace ? (
          <section className="empty-workspace"><h2>暂时无法打开工作区</h2><p>请重试，或选择一个笔记库继续。</p><div className="empty-actions"><button className="primary-button" type="button" onClick={() => { setError(null); setIsInitializing(true); void workspaceApi.get().then(applyWorkspace).catch((cause: unknown) => setError(toWorkspaceError(cause))).finally(() => setIsInitializing(false)); }}>重试</button><button className="quiet-button" type="button" onClick={chooseLibrary}>选择笔记库</button></div></section>
        ) : !library ? (
          <section className="empty-workspace">
            <span className="empty-mark" aria-hidden="true">✳</span>
            <h2>选择你的笔记库</h2>
            <p>笔记库存放在本机目录中。选择一个已有位置，或使用 CueNote 的默认位置。</p>
            {workspace?.defaultLibraryPath && <p className="path-preview">默认位置：<code>{workspace.defaultLibraryPath}</code></p>}
            <div className="empty-actions">
              <button className="primary-button" type="button" disabled={navigationLocked} onClick={useDefaultLibrary}>使用默认位置</button>
              <button className="quiet-button" type="button" disabled={navigationLocked} onClick={chooseLibrary}>选择已有笔记库</button>
            </div>
          </section>
        ) : !session || !draft ? (
          <section className="empty-workspace">
            <span className="empty-mark" aria-hidden="true">✳</span>
            <h2>准备好记录了吗？</h2>
            <p>在“{library.defaultNotebookName}”中创建一篇笔记，开始整理你的想法。</p>
            <button className="primary-button" type="button" disabled={navigationLocked} onClick={createNote}>新建笔记</button>
          </section>
        ) : (
          <article className="note-paper" aria-label="康奈尔笔记编辑区">
            <div className="paper-topline">
              <span>康奈尔笔记</span>
              <span className="paper-topline-right"><span className={`paper-dot${saveFailed || isDirty || session.revision === null ? ' is-unsaved' : ' is-saved'}`} />{saveStatus}</span>
            </div>
            {isReadOnly && (
              <div className="readonly-notice" role="status">
                <strong>这篇笔记以只读方式打开</strong>
                <p>{session.readOnlyReason}</p>
                <span>当前版本暂不支持编辑其中的全部内容，原文件保持完整。</span>
              </div>
            )}
            <section className="title-preview editable-title" aria-labelledby="note-title-label">
              <div className="title-copy">
                <span className="section-number">00</span>
                <div className="title-field-wrap">
                  <label className="field-caption" htmlFor="note-title-input" id="note-title-label">笔记标题</label>
                  <input
                    id="note-title-input"
                    className="title-input"
                    aria-label="笔记标题"
                    value={draft.title}
                    placeholder="输入笔记标题"
                    readOnly={isReadOnly}
                    disabled={editingLocked}
                    onChange={(event) => updateDraft('title', event.currentTarget.value)}
                    onCompositionStart={handleCompositionStart}
                    onCompositionEnd={(event) => handleCompositionEnd('title', event.currentTarget.value)}
                  />
                </div>
              </div>
              <span className="coming-soon">{isReadOnly ? '只读' : '纯文字'}</span>
            </section>
            <div className="cornell-grid">
              <section className="cornell-panel cue-panel" aria-labelledby="cue-title">
                <div className="section-heading"><span className="section-number">01</span><h3 id="cue-title">线索区</h3></div>
                <textarea
                  aria-labelledby="cue-title"
                  className="note-textarea cue-textarea"
                  value={draft.cue}
                  placeholder="记录问题、关键词与回顾提示"
                  readOnly={isReadOnly}
                  disabled={editingLocked}
                  onChange={(event) => updateDraft('cue', event.currentTarget.value)}
                  onCompositionStart={handleCompositionStart}
                  onCompositionEnd={(event) => handleCompositionEnd('cue', event.currentTarget.value)}
                />
                <span className="panel-note">用于回想与复习</span>
              </section>
              <section className="cornell-panel content-panel" aria-labelledby="content-title">
                <div className="section-heading"><span className="section-number">02</span><h3 id="content-title">内容区</h3><span className="content-hint">纯文字记录</span></div>
                <textarea
                  aria-labelledby="content-title"
                  className="note-textarea content-textarea"
                  value={draft.content}
                  placeholder="从这里开始记录……"
                  readOnly={isReadOnly}
                  disabled={editingLocked}
                  onChange={(event) => updateDraft('content', event.currentTarget.value)}
                  onCompositionStart={handleCompositionStart}
                  onCompositionEnd={(event) => handleCompositionEnd('content', event.currentTarget.value)}
                />
              </section>
            </div>
            <section className="summary-panel" aria-labelledby="summary-title">
              <div className="section-heading"><span className="section-number">03</span><h3 id="summary-title">总结区</h3><span className="content-hint">收束本篇要点</span></div>
              <textarea
                aria-labelledby="summary-title"
                className="note-textarea summary-textarea"
                value={draft.summary}
                placeholder="复述重点，留下自己的结论"
                readOnly={isReadOnly}
                disabled={editingLocked}
                onChange={(event) => updateDraft('summary', event.currentTarget.value)}
                onCompositionStart={handleCompositionStart}
                onCompositionEnd={(event) => handleCompositionEnd('summary', event.currentTarget.value)}
              />
            </section>
            <div className="note-actions">
              <div className="save-controls">
                <span className="save-hint">{isReadOnly
                  ? '当前显示可阅读的文字内容。'
                  : '停止输入约 1 秒后自动保存；使用 Ctrl / ⌘ + S 可立即保存。'}</span>
                {saveFailed && !isReadOnly && (
                  <button className="quiet-button safe-copy-button" type="button" disabled={isOperationBusy} onClick={() => void saveCurrentAs()}>
                    另存当前草稿…
                  </button>
                )}
                {currentCopy && (
                  <span className="safe-copy-path" role="status">
                    此次草稿已另存到：<code>{currentCopy.path}</code>{canDiscardAfterCopy
                      ? isDirty ? '；本机原稿仍未保存。' : '；本机原稿已保存。'
                      : '；此副本对应先前快照，本机原稿仍保留。'}
                  </span>
                )}
                {saveError && renderError(saveError)}
                {safeCopyError && renderError(safeCopyError)}
              </div>
              <button
                className="primary-button save-button"
                type="button"
                disabled={editingLocked || isSaving || isReadOnly || (!isDirty && !saveFailed)}
                onClick={() => void saveCurrent()}
              >
                {isSaving ? '正在保存…' : saveFailed ? '重试保存' : isDirty ? '立即保存' : session.revision === null ? '尚未保存' : '已保存到本地'}
              </button>
            </div>
          </article>
        )}

        <footer className="page-footer">
          <span>CueNote · 康奈尔笔记</span>
          <span>{desktopRuntime ? '自动保存到本机' : '浏览器只读预览 · 不会保存笔记'}</span>
        </footer>
      </main>

      {pendingTransition && (
        <div className="dialog-backdrop">
          <div
            className="unsaved-dialog"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="unsaved-dialog-title"
            aria-describedby="unsaved-dialog-description"
            ref={dialogRef}
          >
            <span className="dialog-mark" aria-hidden="true">!</span>
            <h2 id="unsaved-dialog-title">{pendingTransition.close ? '关闭前保存当前笔记' : '切换前保存当前笔记'}</h2>
            <p id="unsaved-dialog-description">
              {dialogMessage ?? (isWaitingForOperation
                ? `正在等待当前操作完成，随后继续${pendingTransition.label}。`
                : `正在处理${pendingTransition.label}。草稿会留在当前页，直到本机保存成功。`)}
            </p>
            {saveError && renderError(saveError)}
            {safeCopyError && renderError(safeCopyError)}
            {currentCopy && (
              <p className="safe-copy-path" role="status">
                此次草稿已另存到：<code>{currentCopy.path}</code>。这不代表笔记库原件已保存。
              </p>
            )}
            {error && renderError(error)}
            <div className="dialog-actions">
              <button
                className="primary-button"
                type="button"
                disabled={isTransitionSaving || isOperationBusy || isWaitingForOperation || isInitializing || isReadOnly || !session}
                onClick={() => void attemptTransitionAfterSave(pendingTransition)}
              >
                {isWaitingForOperation ? '等待当前操作…' : isTransitionSaving ? '正在保存…' : pendingTransition.close ? '重试保存并关闭' : '重试保存并继续'}
              </button>
              {saveFailed && !isReadOnly && (
                <button className="quiet-button" type="button" disabled={isOperationBusy || isWaitingForOperation || isInitializing} onClick={() => void saveCurrentAs()}>
                  另存当前草稿…
                </button>
              )}
              {session && !isReadOnly && (isDirty || saveFailed) && (
                <button
                  className="quiet-button destructive-quiet"
                  type="button"
                  disabled={isTransitionSaving || isOperationBusy || isSaving || isComposing || isWaitingForOperation || isInitializing}
                  onClick={() => void discardAndContinue()}
                >
                  {isAbandoning
                    ? '正在等待磁盘写入…'
                    : `${canDiscardAfterCopy ? '已另存当前快照；' : ''}放弃本机修改并${pendingTransition.close ? '关闭' : '继续'}`}
                </button>
              )}
              <button className="text-button" type="button" data-dialog-cancel onClick={cancelPendingTransition}>
                {pendingTransition.close ? '取消关闭' : '取消切换'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default App;
