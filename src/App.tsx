import { useEffect, useRef, useState, type FormEvent } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createSaveCoordinator, type SaveCoordinatorState } from './save-coordinator';
import { RichTextEditor, type RichTextSection } from './RichTextEditor';
import {
  workspaceApi,
  type NoteSession,
  type NoteContent,
  type RecoveryDraftSummary,
  type RecoveryList,
  type SaveReceipt,
  type SavedCopy,
  type ViewPreferences,
  type WorkspaceError,
  type WorkspaceView,
} from './workspace-api';

const desktopRuntime = isTauri();

type PendingTransition = {
  label: string;
  close?: boolean;
  run: () => Promise<void>;
};

type CurrentCopy = SavedCopy & { sessionId: string; content: NoteContent };
type SaveCoordinator = ReturnType<typeof createSaveCoordinator<NoteContent, SaveReceipt>>;
type EditorIssue = { message: string; initial: boolean } | null;
type EditorIssues = Record<RichTextSection, EditorIssue>;

const emptyEditorIssues: EditorIssues = { cue: null, content: null, summary: null };
type NotebookDialogState =
  | { mode: 'create' | 'rename'; notebookId?: string; name: string }
  | { mode: 'delete'; notebookId: string; name: string };
type TrashDialogState =
  | { mode: 'delete'; sessionId: string; noteId: string; title: string; error: string | null }
  | { mode: 'restore-target'; sessionId: string; noteId: string; title: string; targetNotebookId: string; error: string | null };

function cloneNoteContent(content: NoteContent): NoteContent {
  return structuredClone(content);
}

function sameContent(left: NoteContent | null, right: NoteContent | null) {
  return left !== null && right !== null && JSON.stringify(left) === JSON.stringify(right);
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

function formatModifiedAt(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? value
    : date.toLocaleString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' });
}

function noteTitle(note: { title: string }) {
  return note.title.trim() || '未命名笔记';
}

function App() {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [selectedNotebookId, setSelectedNotebookId] = useState<string | null>(null);
  const [isTrashView, setIsTrashView] = useState(false);
  const [notebookDialog, setNotebookDialog] = useState<NotebookDialogState | null>(null);
  const [notebookDialogError, setNotebookDialogError] = useState<string | null>(null);
  const [trashDialog, setTrashDialog] = useState<TrashDialogState | null>(null);
  const [isNotebookBusy, setIsNotebookBusy] = useState(false);
  const [workspace, setWorkspace] = useState<WorkspaceView | null>(null);
  const [session, setSession] = useState<NoteSession | null>(null);
  const [draft, setDraft] = useState<NoteContent | null>(null);
  const [savedContent, setSavedContent] = useState<NoteContent | null>(null);
  const [editorIssues, setEditorIssues] = useState<EditorIssues>({ ...emptyEditorIssues });
  const [isInitializing, setIsInitializing] = useState(desktopRuntime);
  const [isOperationBusy, setIsOperationBusy] = useState(false);
  const [saveState, setSaveState] = useState<SaveCoordinatorState>({
    phase: 'clean', sessionId: null, generation: 0, savedGeneration: 0, isDirty: false, error: null,
  });
  const [saveError, setSaveError] = useState<WorkspaceError | null>(null);
  const [error, setError] = useState<WorkspaceError | null>(null);
  const [viewPreferencesError, setViewPreferencesError] = useState<WorkspaceError | null>(null);
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
  const viewPreferencesRef = useRef<ViewPreferences>({ sidebarCollapsed: false, selectedNotebookId: null });
  const viewPreferencesInitializedRef = useRef(false);
  const viewPreferencesWriteQueueRef = useRef<Promise<void>>(Promise.resolve());
  const sessionRef = useRef<NoteSession | null>(null);
  const draftRef = useRef<NoteContent | null>(null);
  const editorIssuesRef = useRef<EditorIssues>({ ...emptyEditorIssues });
  const operationInProgressRef = useRef(false);
  const pendingTransitionRef = useRef<PendingTransition | null>(null);
  const transitionAttemptRef = useRef<PendingTransition | null>(null);
  const transitionWaitingForOperationRef = useRef(false);
  const activeTransitionRef = useRef<PendingTransition | null>(null);
  const isAbandoningRef = useRef(false);
  const isTrashViewRef = useRef(false);
  const trashMutationInProgressRef = useRef(false);
  const leaveIntentRef = useRef<ReturnType<SaveCoordinator['leave']> | null>(null);
  const lastSaveErrorRef = useRef<unknown | null>(null);
  const isComposingRef = useRef(false);
  const compositionWaitersRef = useRef<Array<() => void>>([]);
  const savingPauseRef = useRef<Promise<void>>(Promise.resolve());
  const closeRequestHandlerRef = useRef<() => void>(() => undefined);
  const flushCurrentRef = useRef<() => void>(() => undefined);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const trashDialogRef = useRef<HTMLDivElement | null>(null);
  const saveCoordinatorRef = useRef<SaveCoordinator | null>(null);

  const setTrashView = (value: boolean) => {
    isTrashViewRef.current = value;
    setIsTrashView(value);
  };

  const updateWorkspace = (value: WorkspaceView | null) => {
    workspaceRef.current = value;
    setWorkspace(value);
  };

  const persistViewPreferences = (next: ViewPreferences) => {
    viewPreferencesRef.current = next;
    setSidebarCollapsed(next.sidebarCollapsed);
    setSelectedNotebookId(next.selectedNotebookId);
    if (!desktopRuntime || !viewPreferencesInitializedRef.current) return;

    const write = viewPreferencesWriteQueueRef.current
      .catch(() => undefined)
      .then(() => workspaceApi.setViewPreferences(next.sidebarCollapsed, next.selectedNotebookId));
    viewPreferencesWriteQueueRef.current = write.then(() => {
      setViewPreferencesError(null);
    }).catch((cause: unknown) => {
      setViewPreferencesError(toWorkspaceError(cause));
    });
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
    const nextDraft = value ? cloneNoteContent(value.content) : null;
    const nextSaved = value ? cloneNoteContent(value.content) : null;
    draftRef.current = nextDraft;
    setDraft(nextDraft);
    setSavedContent(nextSaved);
    editorIssuesRef.current = { ...emptyEditorIssues };
    setEditorIssues({ ...emptyEditorIssues });
    setSaveError(null);
    setSafeCopyError(null);
    setCurrentCopy(null);
    saveCoordinatorRef.current?.reset(
      value?.sessionId ?? null,
      value ? cloneNoteContent(value.content) : null,
      Boolean(value && value.revision === null && !value.readOnlyReason && value.trashedAt === null),
    );
    const currentWorkspace = workspaceRef.current;
    if (currentWorkspace) updateWorkspace({ ...currentWorkspace, session: value });
  };

  const applyWorkspace = (value: WorkspaceView) => {
    const savedNotebookId = value.preferences.selectedNotebookId;
    const selectedNotebook = savedNotebookId && value.library?.notebooks.some(
      (notebook) => notebook.notebookId === savedNotebookId,
    ) ? savedNotebookId : null;
    const preferences = { ...value.preferences, selectedNotebookId: selectedNotebook };
    viewPreferencesRef.current = preferences;
    viewPreferencesInitializedRef.current = true;
    setSidebarCollapsed(preferences.sidebarCollapsed);
    setSelectedNotebookId(preferences.selectedNotebookId);
    updateWorkspace(value);
    updateSessionMetadata(value.session);
    const nextDraft = value.session ? cloneNoteContent(value.session.content) : null;
    const nextSaved = value.session ? cloneNoteContent(value.session.content) : null;
    draftRef.current = nextDraft;
    setDraft(nextDraft);
    setSavedContent(nextSaved);
    editorIssuesRef.current = { ...emptyEditorIssues };
    setEditorIssues({ ...emptyEditorIssues });
    setSaveError(null);
    setSafeCopyError(null);
    setCurrentCopy(null);
    saveCoordinatorRef.current?.reset(
      value.session?.sessionId ?? null,
      value.session ? cloneNoteContent(value.session.content) : null,
      Boolean(value.session && value.session.revision === null && !value.session.readOnlyReason && value.session.trashedAt === null),
    );
  };

  if (saveCoordinatorRef.current === null) {
    saveCoordinatorRef.current = createSaveCoordinator<NoteContent, SaveReceipt>({
      save: (sessionId, snapshot) => workspaceApi.save(sessionId, snapshot),
      onSaved: (snapshot, receipt) => {
        if (sessionRef.current?.sessionId !== snapshot.sessionId) return;
        updateSessionMetadata(receipt.session);
        const saved = cloneNoteContent(snapshot.value);
        setSavedContent(saved);
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

  const hasEditorIssue = () => Object.values(editorIssuesRef.current).some(Boolean);
  const hasInitialEditorIssue = () => Object.values(editorIssuesRef.current).some((issue) => issue?.initial);
  const hasBlockingEditorIssue = () => Object.values(editorIssuesRef.current).some((issue) => issue && !issue.initial);
  const pauseAutomaticSaving = () => {
    const pause = saveCoordinator.pauseAutomaticSavingAndWait();
    savingPauseRef.current = Promise.all([savingPauseRef.current, pause]).then(() => undefined);
    return savingPauseRef.current;
  };
  const resumeAutomaticSavingWhenSafe = () => {
    void savingPauseRef.current.then(() => {
      if (!isComposingRef.current && !hasEditorIssue() && !isAbandoningRef.current) {
        saveCoordinator.resumeAutomaticSaving();
      }
    });
  };
  const updateEditorIssue = (section: RichTextSection, message: string | null, initial = false) => {
    const next = { ...editorIssuesRef.current, [section]: message ? { message, initial } : null };
    editorIssuesRef.current = next;
    setEditorIssues(next);
    const firstIssue = Object.values(next).find(Boolean);
    if (firstIssue) {
      setError({ kind: 'editor_conversion', message: firstIssue.message });
      void pauseAutomaticSaving();
      return;
    }
    setError((current) => current?.kind === 'editor_conversion' ? null : current);
    resumeAutomaticSavingWhenSafe();
  };

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
    if (resumeAutomaticSaving) resumeAutomaticSavingWhenSafe();
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
      resumeAutomaticSavingWhenSafe();
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
      if (hasBlockingEditorIssue()) {
        setDialogMessage('编辑器内容尚无法转换为安全的保存快照。请撤销或修复该操作后再继续。');
        return;
      }
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
    if (hasBlockingEditorIssue()) {
      setDialogMessage('编辑器内容尚无法转换为安全的保存快照。请撤销或修复该操作后再继续。');
      return;
    }
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
    if (hasBlockingEditorIssue()) {
      setError({ kind: 'editor_conversion', message: '编辑器内容尚无法转换为安全的保存快照。请撤销或修复该操作后再继续。' });
      return;
    }
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
    if (!desktopRuntime || isInitializing || operationInProgressRef.current || hasEditorIssue() || !currentSession || currentSession.readOnlyReason || currentSession.trashedAt !== null) return;
    await waitForComposition();
    await saveCoordinator.flush();
  };
  flushCurrentRef.current = () => { void saveCurrent(); };

  const saveCurrentAs = async () => {
    await waitForComposition();
    const currentSession = sessionRef.current;
    const snapshot = draftRef.current;
    const unchangedReadOnlyCopy = Boolean(currentSession && (currentSession.readOnlyReason || hasInitialEditorIssue()));
    if (!desktopRuntime || isInitializing || operationInProgressRef.current || hasEditorIssue() && !unchangedReadOnlyCopy || !currentSession || !snapshot || currentSession.trashedAt !== null) return;
    const copyContent = cloneNoteContent(unchangedReadOnlyCopy ? currentSession.content : snapshot);
    operationInProgressRef.current = true;
    setIsOperationBusy(true);
    setSafeCopyError(null);
    try {
      const copy = await workspaceApi.saveCurrentAs(currentSession.sessionId, copyContent);
      if (copy) setCurrentCopy({ ...copy, sessionId: currentSession.sessionId, content: copyContent });
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
      if (isTrashViewRef.current) {
        setTrashView(false);
        persistViewPreferences({ ...viewPreferencesRef.current, selectedNotebookId: null });
      }
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
    if (!transition || hasEditorIssue() || isTransitionSaving || isOperationBusy || isSaving || isComposing || isInitializing) return;
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
    if (!desktopRuntime) return;
    setNotebookDialog(null);
    setTrashDialog(null);
    if (pendingTransitionRef.current?.close || activeTransitionRef.current?.close) return;
    if (pendingTransitionRef.current) clearPendingTransition();
    requestTransition({
      label: '关闭 CueNote',
      close: true,
      run: async () => {
        await viewPreferencesWriteQueueRef.current;
        await workspaceApi.completeClose();
      },
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
    if (!trashDialog) return;
    const dialog = trashDialogRef.current;
    dialog?.querySelector<HTMLButtonElement>('[data-trash-dialog-cancel]')?.focus();

    const handleDialogKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !trashMutationInProgressRef.current) {
        event.preventDefault();
        setTrashDialog(null);
        return;
      }
      if (event.key !== 'Tab' || !dialog) return;
      const controls = Array.from(dialog.querySelectorAll<HTMLElement>('button:not([disabled]), select:not([disabled])'));
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
  }, [trashDialog]);

  useEffect(() => {
    const handleSaveShortcut = (event: KeyboardEvent) => {
      if (event.isComposing || isComposingRef.current) return;
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
        event.preventDefault();
        if (desktopRuntime) flushCurrentRef.current();
      }
    };
    window.addEventListener('keydown', handleSaveShortcut);
    return () => window.removeEventListener('keydown', handleSaveShortcut);
  }, []);

  const updateDraft = (value: string) => {
    if (operationInProgressRef.current || isAbandoningRef.current) return;
    const current = draftRef.current;
    const currentSession = sessionRef.current;
    if (!current || !currentSession || isTrashViewRef.current || currentSession.readOnlyReason || currentSession.trashedAt !== null) return;
    const next = { ...current, title: value };
    draftRef.current = next;
    setDraft(next);
    if (!isComposingRef.current && !hasEditorIssue()) saveCoordinator.edit(currentSession.sessionId, cloneNoteContent(next));
  };

  const updateSectionDraft = (section: RichTextSection, blocks: NoteContent['sections'][RichTextSection]) => {
    if (operationInProgressRef.current || isAbandoningRef.current) return;
    const current = draftRef.current;
    const currentSession = sessionRef.current;
    if (!current || !currentSession || isTrashViewRef.current || currentSession.readOnlyReason || currentSession.trashedAt !== null) return;
    const next = cloneNoteContent({ ...current, sections: { ...current.sections, [section]: blocks } });
    draftRef.current = next;
    setDraft(next);
    if (!isComposingRef.current && !hasEditorIssue()) saveCoordinator.edit(currentSession.sessionId, cloneNoteContent(next));
  };

  const handleCompositionStart = () => {
    isComposingRef.current = true;
    setIsComposing(true);
    void pauseAutomaticSaving();
  };
  const resolveCompositionWaiters = () => {
    const waiters = compositionWaitersRef.current.splice(0);
    waiters.forEach((resolve) => resolve());
    resumeAutomaticSavingWhenSafe();
  };
  const handleCompositionEnd = (value: string) => {
    isComposingRef.current = false;
    setIsComposing(false);
    updateDraft(value);
    resolveCompositionWaiters();
  };
  const handleEditorCompositionEnd = (section: RichTextSection, blocks: NoteContent['sections'][RichTextSection] | null) => {
    isComposingRef.current = false;
    setIsComposing(false);
    if (blocks) updateSectionDraft(section, blocks);
    resolveCompositionWaiters();
  };

  const chooseLibrary = () => requestTransition({
    label: '更换笔记库',
    run: async () => {
      await viewPreferencesWriteQueueRef.current;
      const nextWorkspace = await workspaceApi.choose();
      if (nextWorkspace) applyWorkspace(nextWorkspace);
    },
  });

  const useDefaultLibrary = () => requestTransition({
    label: '切换到默认笔记库',
    run: async () => {
      await viewPreferencesWriteQueueRef.current;
      applyWorkspace(await workspaceApi.useDefault());
    },
  });

  const createNote = () => {
    const currentWorkspace = workspaceRef.current;
    const currentLibrary = currentWorkspace?.library;
    const creatingFromTrash = isTrashViewRef.current;
    const targetNotebookId = creatingFromTrash ? null : selectedNotebookId;
    const notebookName = targetNotebookId
      ? currentLibrary?.notebooks.find((notebook) => notebook.notebookId === targetNotebookId)?.name ?? '当前笔记本'
      : currentLibrary?.defaultNotebookName ?? '默认笔记本';
    requestTransition({
      label: `在“${notebookName}”中新建笔记`,
      run: async () => {
        if (creatingFromTrash) {
          setTrashView(false);
          persistViewPreferences({ ...viewPreferencesRef.current, selectedNotebookId: null });
        }
        applySession(await workspaceApi.newNote(targetNotebookId));
      },
    });
  };

  const selectNotebookView = (notebookId: string | null) => {
    const select = () => persistViewPreferences({ ...viewPreferencesRef.current, selectedNotebookId: notebookId });
    if (!isTrashViewRef.current) {
      select();
      return;
    }

    requestTransition({
      label: notebookId ? '返回普通笔记本' : '返回全部笔记',
      run: async () => {
        setTrashView(false);
        select();
      },
    });
  };

  const selectTrashView = () => {
    if (isTrashViewRef.current) return;
    requestTransition({
      label: '打开回收站',
      run: async () => setTrashView(true),
    });
  };

  const openNote = (noteId: string, title: string) => requestTransition({
    label: `打开“${title || '未命名笔记'}”`,
    run: async () => applySession(await workspaceApi.openNote(noteId)),
  });

  const applyLibraryView = (nextLibrary: NonNullable<WorkspaceView['library']>) => {
    const currentWorkspace = workspaceRef.current;
    if (currentWorkspace) updateWorkspace({ ...currentWorkspace, library: nextLibrary });
  };

  const trashCurrentNote = () => {
    const currentSession = sessionRef.current;
    if (!currentSession || currentSession.trashedAt !== null) return;
    requestTransition({
      label: `将“${noteTitle(currentSession.text)}”移入回收站`,
      run: async () => {
        const activeSession = sessionRef.current;
        if (!activeSession || activeSession.sessionId !== currentSession.sessionId) return;
        const receipt = await workspaceApi.trashNote(currentSession.sessionId);
        applyLibraryView(receipt.library);
        applySession(receipt.session);
        setTrashView(true);
      },
    });
  };

  const openTrashDeleteDialog = () => {
    const currentSession = sessionRef.current;
    if (!currentSession || currentSession.trashedAt === null || operationInProgressRef.current) return;
    setError(null);
    setTrashDialog({
      mode: 'delete',
      sessionId: currentSession.sessionId,
      noteId: currentSession.noteId,
      title: noteTitle(currentSession.text),
      error: null,
    });
  };

  const restoreTrashedNote = (targetNotebookId: string | null) => {
    const currentSession = sessionRef.current;
    const currentLibrary = workspaceRef.current?.library;
    if (!currentSession || currentSession.trashedAt === null || !currentLibrary) return;
    const sessionId = currentSession.sessionId;
    const title = noteTitle(currentSession.text);
    setTrashDialog((current) => current?.mode === 'restore-target' && current.sessionId === sessionId
      ? { ...current, error: null }
      : current);
    setError(null);
    requestTransition({
      label: `恢复“${title}”`,
      run: async () => {
        if (sessionRef.current?.sessionId !== sessionId) return;
        trashMutationInProgressRef.current = true;
        try {
          const receipt = await workspaceApi.restoreTrashedNote(sessionId, targetNotebookId);
          const currentWorkspace = workspaceRef.current;
          if (currentWorkspace) updateWorkspace({ ...currentWorkspace, library: receipt.library, session: receipt.session });
          applySession(receipt.session);
          setTrashView(false);
          const restoredNotebookId = targetNotebookId
            ?? receipt.session.notebookId
            ?? receipt.library.defaultNotebookId;
          persistViewPreferences({
            ...viewPreferencesRef.current,
            selectedNotebookId: restoredNotebookId === receipt.library.defaultNotebookId ? null : restoredNotebookId,
          });
          setTrashDialog(null);
        } catch (cause) {
          const operationError = toWorkspaceError(cause);
          setTrashDialog((current) => current?.mode === 'restore-target' && current.sessionId === sessionId
            ? { ...current, error: operationError.message }
            : current);
          setError(operationError);
        } finally {
          trashMutationInProgressRef.current = false;
        }
      },
    });
  };

  const openTrashRestore = () => {
    const currentSession = sessionRef.current;
    const currentLibrary = workspaceRef.current?.library;
    if (!currentSession || currentSession.trashedAt === null || !currentLibrary || operationInProgressRef.current) return;
    const originalNotebookId = currentSession.notebookId;
    const originalNotebookExists = originalNotebookId !== null
      && currentLibrary.notebooks.some((notebook) => notebook.notebookId === originalNotebookId);
    if (originalNotebookExists) {
      restoreTrashedNote(null);
      return;
    }

    setError(null);
    setTrashDialog({
      mode: 'restore-target',
      sessionId: currentSession.sessionId,
      noteId: currentSession.noteId,
      title: noteTitle(currentSession.text),
      targetNotebookId: currentLibrary.defaultNotebookId,
      error: null,
    });
  };

  const confirmTrashDelete = () => {
    const dialog = trashDialog;
    const currentSession = sessionRef.current;
    if (!dialog || dialog.mode !== 'delete' || operationInProgressRef.current) return;
    if (!currentSession || currentSession.sessionId !== dialog.sessionId || currentSession.trashedAt === null) {
      setTrashDialog({ ...dialog, error: '当前回收站笔记已变化，请关闭此窗口后重新选择。' });
      return;
    }

    setTrashDialog({ ...dialog, error: null });
    setError(null);
    requestTransition({
      label: `永久删除“${dialog.title}”`,
      run: async () => {
        if (sessionRef.current?.sessionId !== dialog.sessionId) return;
        trashMutationInProgressRef.current = true;
        try {
          const nextWorkspace = await workspaceApi.deleteTrashedNote(dialog.sessionId, true);
          applyWorkspace(nextWorkspace);
          setTrashView(true);
          setTrashDialog(null);
        } catch (cause) {
          const operationError = toWorkspaceError(cause);
          setTrashDialog((current) => current?.mode === 'delete' && current.sessionId === dialog.sessionId
            ? { ...current, error: operationError.message }
            : current);
          setError(operationError);
        } finally {
          trashMutationInProgressRef.current = false;
        }
      },
    });
  };

  const confirmTrashRestoreTarget = () => {
    if (trashDialog?.mode !== 'restore-target') return;
    const targetExists = workspaceRef.current?.library?.notebooks.some(
      (notebook) => notebook.notebookId === trashDialog.targetNotebookId,
    );
    if (!targetExists) {
      setTrashDialog({ ...trashDialog, error: '请选择一个仍存在的目标笔记本。' });
      return;
    }
    restoreTrashedNote(trashDialog.targetNotebookId);
  };

  const openNotebookDialog = (next: NotebookDialogState) => {
    setNotebookDialogError(null);
    setNotebookDialog(next);
  };

  const submitNotebookDialog = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const currentDialog = notebookDialog;
    const currentLibrary = workspaceRef.current?.library;
    if (!currentDialog || currentDialog.mode === 'delete' || !currentLibrary || isNotebookBusy || operationInProgressRef.current) return;
    const name = currentDialog.name.trim();
    if (!name) {
      setNotebookDialogError('请输入笔记本名称。');
      return;
    }

    setIsNotebookBusy(true);
    operationInProgressRef.current = true;
    setIsOperationBusy(true);
    setNotebookDialogError(null);
    try {
      const existingIds = new Set(currentLibrary.notebooks.map((notebook) => notebook.notebookId));
      const nextLibrary = currentDialog.mode === 'create'
        ? await workspaceApi.createNotebook(name)
        : await workspaceApi.renameNotebook(currentDialog.notebookId!, name);
      applyLibraryView(nextLibrary);
      if (currentDialog.mode === 'create') {
        const createdNotebook = nextLibrary.notebooks.find((notebook) => !notebook.isDefault && !existingIds.has(notebook.notebookId));
        if (createdNotebook) {
          persistViewPreferences({ ...viewPreferencesRef.current, selectedNotebookId: createdNotebook.notebookId });
        }
      }
      setNotebookDialog(null);
    } catch (cause) {
      const operationError = toWorkspaceError(cause);
      setNotebookDialogError(operationError.message);
      if (pendingTransitionRef.current?.close) setError(operationError);
    } finally {
      if (pendingTransitionRef.current?.close) setNotebookDialog(null);
      operationInProgressRef.current = false;
      setIsOperationBusy(false);
      setIsNotebookBusy(false);
    }
  };

  const deleteNotebook = async (notebookId: string) => {
    if (isNotebookBusy || operationInProgressRef.current) return;
    setIsNotebookBusy(true);
    operationInProgressRef.current = true;
    setIsOperationBusy(true);
    setNotebookDialogError(null);
    try {
      const nextLibrary = await workspaceApi.deleteNotebook(notebookId);
      applyLibraryView(nextLibrary);
      setNotebookDialog(null);
      if (viewPreferencesRef.current.selectedNotebookId === notebookId) {
        persistViewPreferences({ ...viewPreferencesRef.current, selectedNotebookId: null });
      }
    } catch (cause) {
      const operationError = toWorkspaceError(cause);
      setNotebookDialogError(operationError.message);
      if (pendingTransitionRef.current?.close) setError(operationError);
    } finally {
      if (pendingTransitionRef.current?.close) setNotebookDialog(null);
      operationInProgressRef.current = false;
      setIsOperationBusy(false);
      setIsNotebookBusy(false);
    }
  };

  const updateCurrentNoteOrganization = (notebookId: string, pinned: boolean, label: string) => requestTransition({
    label,
    run: async () => {
      const currentSession = sessionRef.current;
      if (!currentSession) return;
      const receipt = await workspaceApi.updateNoteOrganization(currentSession.sessionId, notebookId, pinned);
      const currentWorkspace = workspaceRef.current;
      updateSessionMetadata(receipt.session);
      if (currentWorkspace) updateWorkspace({ ...currentWorkspace, library: receipt.library, session: receipt.session });
    },
  });

  const moveCurrentNote = (notebookId: string) => {
    const currentSession = sessionRef.current;
    const currentLibrary = workspaceRef.current?.library;
    if (!currentSession || !currentLibrary) return;
    if ((currentSession.notebookId ?? currentLibrary.defaultNotebookId) === notebookId) return;
    const destination = currentLibrary.notebooks.find((notebook) => notebook.notebookId === notebookId)?.name ?? '目标笔记本';
    updateCurrentNoteOrganization(notebookId, currentSession.pinned, `移至“${destination}”`);
  };

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
  const trashNotes = library?.trashNotes ?? [];
  const notebooks = library?.notebooks ?? [];
  const selectedNotebook = notebooks.find((notebook) => notebook.notebookId === selectedNotebookId) ?? null;
  const visibleNotes = isTrashView
    ? trashNotes
    : selectedNotebook
      ? notes.filter((note) => note.notebookId === selectedNotebook.notebookId)
      : notes;
  const selectedViewName = isTrashView ? '回收站' : selectedNotebook?.name ?? '全部笔记';
  const isTrashSession = Boolean(session?.trashedAt);
  const sessionMatchesView = Boolean(session && isTrashSession === isTrashView);
  const displayedDraft = sessionMatchesView ? draft : null;
  const editorIssue = Object.values(editorIssues).find(Boolean) ?? null;
  const hasAnyEditorIssue = Boolean(editorIssue);
  const hasInitialIssue = Object.values(editorIssues).some((issue) => issue?.initial);
  const hasBlockingIssue = Object.values(editorIssues).some((issue) => issue !== null && !issue.initial);
  const displayedTitle = session?.readOnlyReason ? session.text.title : displayedDraft?.title;
  const sessionNotebookName = session
    ? isTrashSession
      ? session.notebookId === null
        ? '原笔记本未知'
        : notebooks.find((notebook) => notebook.notebookId === session.notebookId)?.name ?? '原笔记本已不存在'
      : notebooks.find((notebook) => notebook.notebookId === (session.notebookId ?? library?.defaultNotebookId))?.name
        ?? library?.defaultNotebookName
        ?? '默认笔记本'
    : null;
  const isReadOnly = Boolean(session?.readOnlyReason || session?.trashedAt);
  const isSaving = saveState.phase === 'saving';
  const saveFailed = saveState.phase === 'failed' || saveState.phase === 'conflict';
  const isDirty = Boolean(sessionMatchesView && session && draft && !isReadOnly && (
    isComposing || hasBlockingIssue || (saveState.sessionId === session.sessionId
      ? saveState.isDirty
      : session.revision === null || !sameContent(draft, savedContent))
  ));
  const navigationLocked = isInitializing || isOperationBusy || isNotebookBusy || pendingTransition !== null || trashDialog !== null;
  const editingLocked = !desktopRuntime || isInitializing || isOperationBusy || isAbandoning || !closeGuardReady || isTrashView || isTrashSession || !sessionMatchesView || hasAnyEditorIssue;
  const sectionCanEdit = (section: RichTextSection) => {
    const ownIssue = editorIssues[section];
    return desktopRuntime && !isInitializing && !isOperationBusy && !isAbandoning && closeGuardReady
      && !isTrashView && !isTrashSession && sessionMatchesView && !session?.readOnlyReason
      && (!hasAnyEditorIssue || Boolean(ownIssue && !ownIssue.initial && !hasInitialIssue));
  };
  const canDiscardAfterCopy = Boolean(currentCopy && session && draft
    && currentCopy.sessionId === session.sessionId
    && sameContent(currentCopy.content, session.readOnlyReason || hasInitialIssue ? session.content : draft));
  const runtimeStatus = !desktopRuntime
    ? '浏览器只读预览'
    : isInitializing
      ? '正在读取本地笔记库'
      : error && !workspace
        ? '桌面连接失败'
        : '桌面应用 · 本地笔记';
  const saveStatus = editorIssue?.initial
    ? '只读文字投影，原件保持不变'
    : editorIssue
      ? '内容无法安全转换，保存已暂停'
    : isSaving
    ? '正在保存…'
    : isComposing
      ? '正在输入，完成后自动保存'
    : saveState.phase === 'conflict'
      ? '保存冲突，请保留当前草稿'
      : saveFailed
      ? '保存失败，请保留当前内容'
    : session && !sessionMatchesView
      ? '当前视图未选中笔记'
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

  const renderSectionEditor = (section: RichTextSection) => {
    if (!session || !draft) return null;
    if (session.readOnlyReason) {
      const projection = session.text[section];
      return (
        <textarea
          className={`note-textarea rich-editor-projection${section === 'cue' ? ' cue-textarea' : section === 'summary' ? ' summary-textarea' : ' content-textarea'}`}
          data-testid={`rich-editor-input-${section}`}
          data-section={section}
          aria-label={`${section === 'cue' ? '线索区' : section === 'content' ? '内容区' : '总结区'}只读文字投影`}
          value={projection}
          readOnly
          rows={section === 'content' ? 14 : 6}
        />
      );
    }
    return (
      <RichTextEditor
        key={`${session.sessionId}:${section}`}
        sessionId={session.sessionId}
        section={section}
        blocks={draft.sections[section]}
        editable={sectionCanEdit(section)}
        readOnly={isReadOnly || hasInitialIssue}
        projectionText={session.text[section]}
        onChange={(blocks) => updateSectionDraft(section, blocks)}
        onError={(message, initial) => updateEditorIssue(section, message, initial)}
        onCompositionStart={handleCompositionStart}
        onCompositionEnd={(blocks) => handleEditorCompositionEnd(section, blocks)}
      />
    );
  };

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
            disabled={navigationLocked}
            onClick={() => persistViewPreferences({
              ...viewPreferencesRef.current,
              sidebarCollapsed: !sidebarCollapsed,
            })}
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
            <button
              className={`sidebar-current${!isTrashView && selectedNotebookId === null ? ' is-current' : ''}`}
              type="button"
              data-testid="all-notes-nav"
              aria-current={!isTrashView && selectedNotebookId === null ? 'page' : undefined}
              disabled={!library || navigationLocked}
              onClick={() => selectNotebookView(null)}
            >
              <span className="sidebar-item-icon" aria-hidden="true">⌂</span>
              <span>全部笔记</span>
              {library && <span className="sidebar-nav-count">{notes.length}</span>}
            </button>
            <button
              className={`sidebar-current trash-nav${isTrashView ? ' is-current' : ''}`}
              type="button"
              data-testid="trash-nav"
              aria-current={isTrashView ? 'page' : undefined}
              disabled={!library || navigationLocked}
              onClick={selectTrashView}
            >
              <span className="sidebar-item-icon" aria-hidden="true">↺</span>
              <span>回收站</span>
              {library && <span className="sidebar-nav-count">{trashNotes.length}</span>}
            </button>
            <div className="sidebar-notebook-heading">
              <div>
                <span className="sidebar-group-title">笔记本</span>
                <span className="sidebar-note-count">{notebooks.length} 个笔记本</span>
              </div>
              <button
                className="sidebar-add"
                type="button"
                aria-label="新建笔记本"
                title="新建笔记本"
                disabled={!desktopRuntime || !library || navigationLocked}
                onClick={() => openNotebookDialog({ mode: 'create', name: '' })}
              >＋</button>
            </div>
            {library ? notebooks.length > 0 ? (
              <ul className="sidebar-notebook-list" aria-label="笔记本">
                {notebooks.map((notebook) => {
                  const notebookNoteCount = notes.filter((note) => note.notebookId === notebook.notebookId).length;
                  const isSelectedNotebook = !isTrashView && selectedNotebookId === notebook.notebookId;
                  return (
                    <li className="sidebar-notebook-row" key={notebook.notebookId}>
                      <button
                        className={`sidebar-notebook-item${isSelectedNotebook ? ' is-current' : ''}`}
                        type="button"
                        data-notebook-id={notebook.notebookId}
                        aria-current={isSelectedNotebook ? 'page' : undefined}
                        disabled={navigationLocked}
                        onClick={() => selectNotebookView(notebook.notebookId)}
                        title={notebook.name}
                      >
                        <span className="sidebar-notebook-icon" aria-hidden="true">▣</span>
                        <span className="sidebar-notebook-name">{notebook.name}</span>
                        <span className="sidebar-nav-count">{notebookNoteCount}</span>
                      </button>
                      {notebook.isDefault ? (
                        <span className="sidebar-default-label">默认</span>
                      ) : (
                        <div className="sidebar-notebook-actions">
                          <button
                            className="notebook-action"
                            type="button"
                            aria-label={`重命名笔记本 ${notebook.name}`}
                            title="重命名"
                            data-notebook-id={notebook.notebookId}
                            disabled={navigationLocked}
                            onClick={() => openNotebookDialog({ mode: 'rename', notebookId: notebook.notebookId, name: notebook.name })}
                          >编辑</button>
                          <button
                            className="notebook-action notebook-delete-action"
                            type="button"
                            aria-label={`删除笔记本 ${notebook.name}`}
                            title="删除"
                            data-notebook-id={notebook.notebookId}
                            disabled={navigationLocked}
                            onClick={() => openNotebookDialog({ mode: 'delete', notebookId: notebook.notebookId, name: notebook.name })}
                          >删除</button>
                        </div>
                      )}
                    </li>
                  );
                })}
              </ul>
            ) : (
              <div className="sidebar-empty">笔记库没有可用笔记本</div>
            ) : (
              <div className="sidebar-empty">选择笔记库后开始整理</div>
            )}
            <div className="sidebar-notebook-heading">
              <div>
                <span className="sidebar-group-title">{isTrashView ? '回收站笔记' : '笔记列表'}</span>
                {library && <span className="sidebar-note-count">{visibleNotes.length}{!isTrashView && selectedNotebook ? ` / ${notes.length}` : ''} 篇笔记</span>}
              </div>
              <button
                className="sidebar-add"
                type="button"
                data-testid="create-note"
                aria-label={isTrashView
                  ? `在“${library?.defaultNotebookName ?? '默认笔记本'}”中新建笔记`
                  : `在${selectedNotebook ? `“${selectedNotebook.name}”` : `“${library?.defaultNotebookName ?? '默认笔记本'}”`}中新建笔记`}
                title="新建笔记"
                disabled={!desktopRuntime || !library || navigationLocked}
                onClick={createNote}
              >＋</button>
            </div>
            {library ? (
              visibleNotes.length > 0 ? (
                <ul className={`sidebar-note-list${isTrashView ? ' trash-note-list' : ''}`} aria-label={isTrashView ? '回收站笔记列表' : '笔记列表'} data-testid={isTrashView ? 'trash-note-list' : 'normal-note-list'}>
                  {visibleNotes.map((note) => (
                    <li key={note.noteId} data-testid={isTrashView ? 'trash-note-row' : undefined}>
                      <button
                        className={`sidebar-note-item${sessionMatchesView && session?.noteId === note.noteId ? ' is-current' : ''}`}
                        type="button"
                        data-testid={isTrashView ? 'trash-note-item' : 'normal-note-item'}
                        data-note-id={note.noteId}
                        aria-current={sessionMatchesView && session?.noteId === note.noteId ? 'page' : undefined}
                        disabled={navigationLocked}
                        onClick={() => openNote(note.noteId, note.title)}
                        title={`${noteTitle(note)} · ${formatModifiedAt(isTrashView ? note.trashedAt ?? note.modifiedAt : note.modifiedAt)}`}
                        aria-label={isTrashView
                          ? `打开回收站笔记：${noteTitle(note)}；移入回收站时间 ${formatModifiedAt(note.trashedAt ?? note.modifiedAt)}；${note.excerpt.trim() || '暂无内容摘要'}`
                          : `打开笔记：${noteTitle(note)}；${note.pinned ? '已置顶；' : ''}${formatModifiedAt(note.modifiedAt)}；${note.excerpt.trim() || '暂无内容摘要'}`}
                      >
                        <span className="sidebar-note-line">
                          {note.pinned && <span className="sidebar-pin" aria-label="已置顶" title="已置顶">◆</span>}
                          <span className="sidebar-note-title">{noteTitle(note)}</span>
                        </span>
                        <span className="sidebar-note-excerpt">{note.excerpt.trim() || '暂无内容摘要'}</span>
                        <time className="sidebar-note-time" dateTime={isTrashView ? note.trashedAt ?? note.modifiedAt : note.modifiedAt}>
                          {isTrashView ? '删除于 ' : ''}{formatModifiedAt(isTrashView ? note.trashedAt ?? note.modifiedAt : note.modifiedAt)}
                        </time>
                      </button>
                    </li>
                  ))}
                </ul>
              ) : (
                <div className="sidebar-empty">{isTrashView ? '回收站为空' : selectedNotebook ? `“${selectedNotebook.name}”里还没有笔记` : '这里还没有笔记'}</div>
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
              <strong>{library ? selectedViewName : '笔记库'}</strong>
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
                ? displayedTitle?.trim() || (sessionMatchesView ? '未命名笔记' : isTrashView ? '回收站' : '写下你的第一篇笔记')
                : '让每个想法，都有清晰的落点。'}
            </h1>
            <p>
              {desktopRuntime
                ? isTrashView
                  ? '回收站中的笔记只读；恢复后可继续编辑。'
                  : library
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
        {!notebookDialog && notebookDialogError && (
          <div className="notebook-operation-notice" role="alert">笔记本操作未完成：{notebookDialogError}</div>
        )}
        {viewPreferencesError && (
          <div className="workspace-notice" role="alert">
            侧栏和笔记本筛选偏好未能保存，当前界面仍可使用：{viewPreferencesError.message}
          </div>
        )}
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
        ) : !sessionMatchesView || !session || !draft ? (
          <section className="empty-workspace" data-testid={isTrashView ? 'trash-empty-view' : undefined}>
            <span className="empty-mark" aria-hidden="true">{isTrashView && trashNotes.length === 0 ? '✓' : '✳'}</span>
            {isTrashView ? (
              <>
                <h2>{trashNotes.length === 0 ? '回收站为空' : '选择一篇回收站笔记'}</h2>
                <p>{trashNotes.length === 0 ? '移入回收站的笔记会显示在左侧列表中。' : '回收站笔记只读，可恢复到原笔记本或永久删除。'}</p>
              </>
            ) : (
              <>
                <h2>{session ? '请选择一篇普通笔记' : '准备好记录了吗？'}</h2>
                <p>在“{selectedNotebook?.name ?? library.defaultNotebookName}”中创建一篇笔记，开始整理你的想法。</p>
                <button className="primary-button" type="button" data-testid="create-note-empty" disabled={navigationLocked} onClick={createNote}>新建笔记</button>
              </>
            )}
          </section>
        ) : (
          <article className={`note-paper${isTrashSession ? ' trash-note-paper' : ''}`} aria-label={isTrashSession ? '回收站笔记只读视图' : '康奈尔笔记编辑区'} data-testid={isTrashSession ? 'trash-note-view' : 'note-editor'}>
            <div className="paper-topline">
              <span>康奈尔笔记</span>
              <span className="paper-topline-right"><span className={`paper-dot${saveFailed || isDirty || hasAnyEditorIssue || session.revision === null ? ' is-unsaved' : ' is-saved'}`} />{saveStatus}</span>
            </div>
            <div className="note-organization" aria-label="当前笔记整理方式">
              <span className="note-notebook-context">当前笔记本：<strong>{sessionNotebookName}</strong></span>
              {desktopRuntime && library && !isTrashSession && (
                <div className="note-organization-controls">
                  <label className="note-move-control">
                    <span>移动到</span>
                    <select
                      aria-label="移动当前笔记到笔记本"
                      value={session.notebookId ?? library.defaultNotebookId}
                      disabled={navigationLocked}
                      onChange={(event) => moveCurrentNote(event.currentTarget.value)}
                    >
                      {notebooks.map((notebook) => (
                        <option key={notebook.notebookId} value={notebook.notebookId}>{notebook.name}</option>
                      ))}
                    </select>
                  </label>
                  <button
                    className={`pin-note-button${session.pinned ? ' is-pinned' : ''}`}
                    type="button"
                    aria-label={session.pinned ? '取消置顶当前笔记' : '置顶当前笔记'}
                    aria-pressed={session.pinned}
                    disabled={navigationLocked}
                    onClick={() => updateCurrentNoteOrganization(
                      session.notebookId ?? library.defaultNotebookId,
                      !session.pinned,
                      session.pinned ? '取消置顶这篇笔记' : '置顶这篇笔记',
                    )}
                  >
                    <span aria-hidden="true">◆</span>{session.pinned ? '已置顶' : '置顶'}
                  </button>
                  <button
                    className="quiet-button destructive-quiet move-to-trash-button"
                    type="button"
                    data-testid="move-to-trash"
                    disabled={navigationLocked}
                    onClick={trashCurrentNote}
                  >移入回收站</button>
                </div>
              )}
              {desktopRuntime && isTrashSession && (
                <div className="note-organization-controls trash-note-controls">
                  <button
                    className="quiet-button"
                    type="button"
                    data-testid="trash-restore"
                    disabled={navigationLocked}
                    onClick={openTrashRestore}
                  >恢复</button>
                  <button
                    className="quiet-button destructive-quiet"
                    type="button"
                    data-testid="trash-permanent-delete"
                    disabled={navigationLocked}
                    onClick={openTrashDeleteDialog}
                  >永久删除</button>
                </div>
              )}
            </div>
            {isReadOnly && (
              <div className="readonly-notice" role="status">
                <strong>这篇笔记以只读方式打开</strong>
                <p>{session.readOnlyReason}</p>
                {!isTrashSession && <span>当前版本暂不支持编辑其中的全部内容，原文件保持完整。</span>}
              </div>
            )}
            <section className="title-preview editable-title" aria-labelledby="note-title-label" data-testid="editor-section-title">
              <div className="title-copy">
                <span className="section-number">00</span>
                <div className="title-field-wrap">
                  <label className="field-caption" htmlFor="note-title-input" id="note-title-label">笔记标题</label>
                  <input
                    id="note-title-input"
                    className="title-input"
                    aria-label="笔记标题"
                    value={session.readOnlyReason ? session.text.title : draft.title}
                    placeholder="输入笔记标题"
                    readOnly={isReadOnly || hasAnyEditorIssue}
                    disabled={editingLocked}
                    onChange={(event) => updateDraft(event.currentTarget.value)}
                    onCompositionStart={handleCompositionStart}
                    onCompositionEnd={(event) => handleCompositionEnd(event.currentTarget.value)}
                  />
                </div>
              </div>
              <span className="coming-soon">{isReadOnly || hasInitialIssue ? '只读' : '富文本'}</span>
            </section>
            <div className="cornell-grid">
              <section className="cornell-panel cue-panel" aria-labelledby="cue-title" data-testid="editor-section-cue" data-section="cue">
                <div className="section-heading"><span className="section-number">01</span><h3 id="cue-title">线索区</h3></div>
                {renderSectionEditor('cue')}
                <span className="panel-note">用于回想与复习</span>
              </section>
              <section className="cornell-panel content-panel" aria-labelledby="content-title" data-testid="editor-section-content" data-section="content">
                <div className="section-heading"><span className="section-number">02</span><h3 id="content-title">内容区</h3><span className="content-hint">{isReadOnly ? '只读内容' : '主要记录区域'}</span></div>
                {renderSectionEditor('content')}
              </section>
            </div>
            <section className="summary-panel" aria-labelledby="summary-title" data-testid="editor-section-summary" data-section="summary">
              <div className="section-heading"><span className="section-number">03</span><h3 id="summary-title">总结区</h3><span className="content-hint">收束本篇要点</span></div>
              {renderSectionEditor('summary')}
            </section>
            <div className="note-actions">
              <div className="save-controls">
                <span className="save-hint">{isTrashSession
                  ? '回收站中的笔记为只读；恢复后可继续编辑。'
                  : session.readOnlyReason || hasInitialIssue
                    ? '这篇笔记包含当前编辑器无法安全转换的内容；文字投影只读，可另存完整副本。'
                  : hasAnyEditorIssue
                    ? '编辑内容尚无法转换，自动保存和切换操作已暂停。请撤销或修复当前编辑。'
                  : isReadOnly
                    ? '当前笔记只读显示。'
                  : '停止输入约 1 秒后自动保存；使用 Ctrl / ⌘ + S 可立即保存。'}</span>
                {!isTrashSession && (saveFailed && !isReadOnly || session.readOnlyReason || hasInitialIssue) && (
                  <button className="quiet-button safe-copy-button" type="button" disabled={isOperationBusy || hasAnyEditorIssue && !hasInitialIssue} onClick={() => void saveCurrentAs()}>
                    {session.readOnlyReason || hasInitialIssue ? '另存完整副本…' : '另存当前草稿…'}
                  </button>
                )}
                {currentCopy && (
                  <span className="safe-copy-path" role="status">
                    此次草稿已另存到：<code>{currentCopy.path}</code>{canDiscardAfterCopy
                      ? session.readOnlyReason || hasInitialIssue
                        ? '；此副本保留了当前只读原件的完整内容。'
                        : isDirty ? '；本机原稿仍未保存。' : '；本机原稿已保存。'
                      : '；此副本对应先前快照，本机原稿仍保留。'}
                  </span>
                )}
                {saveError && renderError(saveError)}
                {safeCopyError && renderError(safeCopyError)}
              </div>
              <button
                className="primary-button save-button"
                type="button"
                disabled={editingLocked || isSaving || isReadOnly || hasAnyEditorIssue || (!isDirty && !saveFailed)}
                onClick={() => void saveCurrent()}
              >
                {editorIssue?.initial ? '只读内容' : editorIssue ? '内容无法保存' : isSaving ? '正在保存…' : saveFailed ? '重试保存' : isDirty ? '立即保存' : session.revision === null ? '尚未保存' : '已保存到本地'}
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
            {notebookDialogError && <div className="notebook-dialog-error" role="alert">笔记本操作未完成：{notebookDialogError}</div>}
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
              {((saveFailed && !isReadOnly) || Boolean(session?.readOnlyReason || hasInitialIssue)) && (
                <button className="quiet-button" type="button" disabled={isOperationBusy || isWaitingForOperation || isInitializing} onClick={() => void saveCurrentAs()}>
                  {session?.readOnlyReason || hasInitialIssue ? '另存完整副本…' : '另存当前草稿…'}
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

      {notebookDialog && (
        <div className="dialog-backdrop notebook-dialog-backdrop">
          <div
            className="unsaved-dialog notebook-dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="notebook-dialog-title"
            aria-describedby="notebook-dialog-description"
          >
            {notebookDialog.mode === 'delete' ? (
              <>
                <span className="dialog-mark" aria-hidden="true">?</span>
                <h2 id="notebook-dialog-title">删除笔记本</h2>
                <p id="notebook-dialog-description">要删除“{notebookDialog.name}”吗？仅能删除没有正常笔记的笔记本。回收站笔记仍保留，恢复时需选择目标笔记本。</p>
                {notebookDialogError && <div className="notebook-dialog-error" role="alert">{notebookDialogError}</div>}
                <div className="dialog-actions">
                  <button className="quiet-button" type="button" disabled={isNotebookBusy} onClick={() => setNotebookDialog(null)}>取消</button>
                  <button className="quiet-button destructive-quiet" type="button" disabled={isNotebookBusy} onClick={() => void deleteNotebook(notebookDialog.notebookId)}>
                    {isNotebookBusy ? '正在删除…' : '删除笔记本'}
                  </button>
                </div>
              </>
            ) : (
              <form onSubmit={(event) => void submitNotebookDialog(event)}>
                <span className="dialog-mark" aria-hidden="true">✎</span>
                <h2 id="notebook-dialog-title">{notebookDialog.mode === 'create' ? '新建笔记本' : '重命名笔记本'}</h2>
                <p id="notebook-dialog-description">笔记本之间不嵌套，名称用于整理笔记。</p>
                <label className="notebook-name-field">
                  <span>笔记本名称</span>
                  <input
                    autoFocus
                    aria-label="笔记本名称"
                    maxLength={120}
                    value={notebookDialog.name}
                    disabled={isNotebookBusy}
                    onChange={(event) => setNotebookDialog({ ...notebookDialog, name: event.currentTarget.value })}
                  />
                </label>
                {notebookDialogError && <div className="notebook-dialog-error" role="alert">{notebookDialogError}</div>}
                <div className="dialog-actions">
                  <button className="quiet-button" type="button" disabled={isNotebookBusy} onClick={() => setNotebookDialog(null)}>取消</button>
                  <button className="primary-button" type="submit" disabled={isNotebookBusy || !notebookDialog.name.trim()}>
                    {isNotebookBusy ? '正在保存…' : notebookDialog.mode === 'create' ? '创建笔记本' : '保存名称'}
                  </button>
                </div>
              </form>
            )}
          </div>
        </div>
      )}

      {trashDialog && (
        <div className="dialog-backdrop trash-dialog-backdrop">
          <div
            className="unsaved-dialog trash-dialog"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="trash-dialog-title"
            aria-describedby="trash-dialog-description"
            ref={trashDialogRef}
            data-testid={trashDialog.mode === 'delete' ? 'trash-delete-dialog' : 'trash-restore-dialog'}
            data-note-id={trashDialog.noteId}
          >
            <span className="dialog-mark" aria-hidden="true">{trashDialog.mode === 'delete' ? '!' : '↺'}</span>
            {trashDialog.mode === 'delete' ? (
              <>
                <h2 id="trash-dialog-title">永久删除这篇笔记？</h2>
                <p id="trash-dialog-description">将从当前笔记库永久删除“{trashDialog.title}”及内含附件。此操作无法撤销；独立备份、历史与恢复副本不受影响。</p>
                {trashDialog.error && <div className="trash-dialog-error" role="alert" data-testid="trash-dialog-error">{trashDialog.error}</div>}
                <div className="dialog-actions">
                  <button
                    className="text-button"
                    type="button"
                    data-testid="trash-delete-cancel"
                    data-trash-dialog-cancel
                    disabled={isOperationBusy}
                    onClick={() => setTrashDialog(null)}
                  >取消</button>
                  <button
                    className="quiet-button destructive-quiet"
                    type="button"
                    data-testid="trash-delete-confirm"
                    disabled={isOperationBusy}
                    onClick={confirmTrashDelete}
                  >{isOperationBusy ? '正在永久删除…' : '永久删除'}</button>
                </div>
              </>
            ) : (
              <>
                <h2 id="trash-dialog-title">选择恢复目标</h2>
                <p id="trash-dialog-description">“{trashDialog.title}”的原笔记本已不存在。请选择一个目标笔记本。</p>
                <label className="trash-target-field">
                  <span>恢复到笔记本</span>
                  <select
                    aria-label="恢复目标笔记本"
                    data-testid="trash-restore-target-picker"
                    value={trashDialog.targetNotebookId}
                    disabled={isOperationBusy}
                    onChange={(event) => setTrashDialog({ ...trashDialog, targetNotebookId: event.currentTarget.value, error: null })}
                  >
                    {(library?.notebooks ?? []).map((notebook) => (
                      <option key={notebook.notebookId} value={notebook.notebookId}>{notebook.name}</option>
                    ))}
                  </select>
                </label>
                {trashDialog.error && <div className="trash-dialog-error" role="alert" data-testid="trash-dialog-error">{trashDialog.error}</div>}
                <div className="dialog-actions">
                  <button
                    className="text-button"
                    type="button"
                    data-testid="trash-restore-cancel"
                    data-trash-dialog-cancel
                    disabled={isOperationBusy}
                    onClick={() => setTrashDialog(null)}
                  >取消</button>
                  <button
                    className="primary-button"
                    type="button"
                    data-testid="trash-restore-confirm"
                    disabled={isOperationBusy || !trashDialog.targetNotebookId || !(library?.notebooks ?? []).some(
                      (notebook) => notebook.notebookId === trashDialog.targetNotebookId,
                    )}
                    onClick={confirmTrashRestoreTarget}
                  >{isOperationBusy ? '正在恢复…' : '恢复到所选笔记本'}</button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

export default App;
