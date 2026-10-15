import { useEffect, useRef, useState, type FormEvent } from 'react';
import { flushSync } from 'react-dom';
import { isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createSaveCoordinator, type SaveCoordinatorState } from './save-coordinator';
import { createExternalChangesCoordinator, type ExternalChangeToken } from './external-changes';
import { continueFromVerifiedCopyOrProtect } from './conflict-transition';
import { RichTextEditor, type RichTextSection } from './RichTextEditor';
import type { ImageEditorBridge } from './image-session';
import type { MindMapEditorBridge } from './mindmap-session';
import {
  workspaceApi,
  type NoteSession,
  type NoteContent,
  type ExternalChangeProbe,
  type ExternalChangeReconciliation,
  type RecoveryDraftSummary,
  type RecoveryList,
  type PreservedVersionList,
  type PreservedVersionSummary,
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
type ActivePreservedPreview = PreservedVersionSummary & {
  sessionId: string;
  returnNoteId: string | null;
};
type SaveCoordinator = ReturnType<typeof createSaveCoordinator<NoteContent, SaveReceipt>>;
type ExternalChangesCoordinator = ReturnType<typeof createExternalChangesCoordinator>;
type EditorIssue = { message: string; initial: boolean } | null;
type EditorIssues = Record<RichTextSection, EditorIssue>;
type LibraryNotebook = NonNullable<WorkspaceView['library']>['notebooks'][number];

type NotebookDisplay = { name: string; shortId: string | null; tooltip: string };

function notebookDisplay(notebooks: LibraryNotebook[], notebook: LibraryNotebook): NotebookDisplay {
  const normalizedName = notebook.name.trim().toLowerCase();
  const sameName = notebooks.filter(candidate => candidate.name.trim().toLowerCase() === normalizedName);
  if (sameName.length < 2) return { name: notebook.name, shortId: null, tooltip: notebook.name };

  const otherIds = notebooks.filter(candidate => candidate.notebookId !== notebook.notebookId)
    .map(candidate => candidate.notebookId);
  let prefixLength = Math.min(8, notebook.notebookId.length);
  while (prefixLength < notebook.notebookId.length
    && otherIds.some(id => id.startsWith(notebook.notebookId.slice(0, prefixLength)))) {
    prefixLength += 1;
  }
  return {
    name: notebook.name,
    shortId: notebook.notebookId.slice(0, prefixLength),
    tooltip: `笔记本完整 ID：${notebook.notebookId}`,
  };
}

function notebookDisplayLabel(notebooks: LibraryNotebook[], notebook: LibraryNotebook) {
  const display = notebookDisplay(notebooks, notebook);
  return display.shortId ? `${display.name} · ${display.shortId}` : display.name;
}

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

function preservedVersionReason(version: PreservedVersionSummary) {
  const reason = version.reason.trim();
  const internalReason = /^([a-z]+(?:-[a-z]+)+)(?::\s*(.*))?$/s.exec(reason);
  if (!internalReason) return reason || (version.conflict ? '保留的冲突版本' : '已保存的历史版本');
  if (internalReason[2]) return internalReason[2];
  switch (internalReason[1]) {
    case 'confirmed-publication': return '已保存的版本';
    case 'recovered-publication': return '重启后找回的已保存版本';
    case 'publication-baseline': return '保存前的版本';
    default: return version.conflict ? '保存冲突时保留的版本' : '已保存的历史版本';
  }
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
  const [metadataReadOnlySessionId, setMetadataReadOnlySessionId] = useState<string | null>(null);
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
  const [preservedVersions, setPreservedVersions] = useState<PreservedVersionList | null>(null);
  const [preservedVersionsOpen, setPreservedVersionsOpen] = useState(false);
  const [preservedVersionsLoading, setPreservedVersionsLoading] = useState(false);
  const [preservedVersionsError, setPreservedVersionsError] = useState<WorkspaceError | null>(null);
  const [preservedCopyPaths, setPreservedCopyPaths] = useState<Record<string, string>>({});
  const [previewingVersionId, setPreviewingVersionId] = useState<string | null>(null);
  const [preservedVersionActionBusy, setPreservedVersionActionBusy] = useState(false);
  const [activePreservedPreview, setActivePreservedPreview] = useState<ActivePreservedPreview | null>(null);
  const [externalConflict, setExternalConflict] = useState(false);
  const [externalConflictKind, setExternalConflictKind] = useState<'conflict' | 'missing' | 'unreadable' | null>(null);
  const [externalMessage, setExternalMessage] = useState<string | null>(null);
  const [isReconcilingExternalChange, setIsReconcilingExternalChange] = useState(false);
  const [closeGuardReady, setCloseGuardReady] = useState(!desktopRuntime);
  const [isComposing, setIsComposing] = useState(false);
  const [pendingEditorOperationCount, setPendingEditorOperationCount] = useState(0);
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
  const metadataReadOnlySessionIdRef = useRef<string | null>(null);
  const draftRef = useRef<NoteContent | null>(null);
  const editorIssuesRef = useRef<EditorIssues>({ ...emptyEditorIssues });
  const operationInProgressRef = useRef(false);
  const manualSaveInProgressRef = useRef(false);
  const pendingTransitionRef = useRef<PendingTransition | null>(null);
  const transitionAttemptRef = useRef<PendingTransition | null>(null);
  const transitionWaitingForOperationRef = useRef(false);
  const activeTransitionRef = useRef<PendingTransition | null>(null);
  const isAbandoningRef = useRef(false);
  const isTrashViewRef = useRef(false);
  const externalConflictRef = useRef(false);
  const externalConflictProtectedRef = useRef(false);
  const externalReconcilePendingRef = useRef(false);
  const activePreservedPreviewRef = useRef<ActivePreservedPreview | null>(null);
  const trashMutationInProgressRef = useRef(false);
  const leaveIntentRef = useRef<ReturnType<SaveCoordinator['leave']> | null>(null);
  const lastSaveErrorRef = useRef<unknown | null>(null);
  const isComposingRef = useRef(false);
  const compositionWaitersRef = useRef<Array<() => void>>([]);
  const pendingEditorOperationsRef = useRef(new Map<Promise<void>, Promise<WorkspaceError | null>>());
  const editorFlushersRef = useRef(new Map<() => Promise<void>, string>());
  const pendingMetadataReadOnlyProbeSessionIdRef = useRef<string | null>(null);
  const savingPauseRef = useRef<Promise<void>>(Promise.resolve());
  const closeRequestHandlerRef = useRef<() => void>(() => undefined);
  const flushCurrentRef = useRef<() => void>(() => undefined);
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const trashDialogRef = useRef<HTMLDivElement | null>(null);
  const saveCoordinatorRef = useRef<SaveCoordinator | null>(null);
  const externalChangesCoordinatorRef = useRef<ExternalChangesCoordinator | null>(null);
  const externalChangeHandlerRef = useRef<(
    result: ExternalChangeProbe,
    token: ExternalChangeToken,
    isCurrent: () => boolean,
  ) => Promise<void>>(async () => undefined);

  const hasUnsettledEditorWork = (sessionId: string) => isComposingRef.current
    || pendingEditorOperationsRef.current.size > 0
    || [...editorFlushersRef.current.values()].some(registeredSessionId => registeredSessionId === sessionId);

  const scheduleDeferredMetadataReadOnlyProbe = () => {
    const deferredSessionId = pendingMetadataReadOnlyProbeSessionIdRef.current;
    if (!deferredSessionId) return;
    if (sessionRef.current?.sessionId !== deferredSessionId) {
      pendingMetadataReadOnlyProbeSessionIdRef.current = null;
      return;
    }
    if (hasUnsettledEditorWork(deferredSessionId)) return;

    pendingMetadataReadOnlyProbeSessionIdRef.current = null;
    externalChangesCoordinatorRef.current?.schedule();
  };

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

  const markMetadataReadOnlySession = (sessionId: string | null) => {
    metadataReadOnlySessionIdRef.current = sessionId;
    setMetadataReadOnlySessionId(sessionId);
  };

  const updateSessionMetadata = (value: NoteSession | null) => {
    if ((sessionRef.current?.sessionId ?? null) !== (value?.sessionId ?? null)) {
      externalChangesCoordinatorRef.current?.sessionChanged();
      pendingMetadataReadOnlyProbeSessionIdRef.current = null;
      if (metadataReadOnlySessionIdRef.current !== null) markMetadataReadOnlySession(null);
    }
    sessionRef.current = value;
    setSession(value);
  };

  const clearExternalConflict = () => {
    externalConflictRef.current = false;
    externalConflictProtectedRef.current = false;
    setExternalConflict(false);
    setExternalConflictKind(null);
    setExternalMessage(null);
  };

  const markActivePreservedPreview = (value: ActivePreservedPreview | null) => {
    activePreservedPreviewRef.current = value;
    setActivePreservedPreview(value);
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

  const refreshPreservedVersions = async () => {
    if (!desktopRuntime) return;
    setPreservedVersionsLoading(true);
    try {
      setPreservedVersions(await workspaceApi.listPreservedVersions());
      setPreservedVersionsError(null);
    } catch (cause) {
      setPreservedVersionsError(toWorkspaceError(cause));
    } finally {
      setPreservedVersionsLoading(false);
    }
  };

  const applySession = (value: NoteSession | null) => {
    pendingMetadataReadOnlyProbeSessionIdRef.current = null;
    markMetadataReadOnlySession(null);
    updateSessionMetadata(value);
    clearExternalConflict();
    markActivePreservedPreview(null);
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
    pendingMetadataReadOnlyProbeSessionIdRef.current = null;
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
    markMetadataReadOnlySession(null);
    updateSessionMetadata(value.session);
    clearExternalConflict();
    markActivePreservedPreview(null);
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
        if (nextState.phase === 'conflict' && !externalConflictRef.current) {
          externalConflictRef.current = true;
          setExternalConflict(true);
          setExternalConflictKind('conflict');
          setExternalMessage('保存时发现磁盘版本已变化。当前草稿仍保留，请查看历史与冲突后选择下一步。');
          setPreservedVersionsOpen(true);
          void saveCoordinatorRef.current?.pauseAutomaticSavingAndWait();
          void refreshPreservedVersions();
          externalChangesCoordinatorRef.current?.schedule();
        }
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
      if (!isComposingRef.current && !hasEditorIssue() && !isAbandoningRef.current && !externalConflictRef.current
        && !externalReconcilePendingRef.current && !operationInProgressRef.current
        && !pendingTransitionRef.current && !transitionAttemptRef.current && !activeTransitionRef.current) {
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

  const waitForEditorOperations = async () => {
    while (pendingEditorOperationsRef.current.size > 0) {
      const results = await Promise.all([...pendingEditorOperationsRef.current.values()]);
      const failure = results.find((result) => result !== null);
      if (failure) throw failure;
    }
  };

  const settleEditors = async () => {
    const sessionId = sessionRef.current?.sessionId;
    for (;;) {
      await waitForComposition();
      if (sessionRef.current?.sessionId !== sessionId) return;
      for (const [settle, registeredSessionId] of editorFlushersRef.current) {
        if (registeredSessionId === sessionId) await settle();
        if (sessionRef.current?.sessionId !== sessionId) return;
      }
      await waitForEditorOperations();
      // A second composition may have begun while an earlier candidate was
      // being validated. Do not let that earlier completion release the leave.
      if (!isComposingRef.current && pendingEditorOperationsRef.current.size === 0) return;
    }
  };

  const updateLibraryFromExternalResult = (value: WorkspaceView['library'] | null) => {
    const currentWorkspace = workspaceRef.current;
    if (value && currentWorkspace) updateWorkspace({ ...currentWorkspace, library: value });
  };

  const blockExternalSaving = (
    kind: 'conflict' | 'missing' | 'unreadable',
    message: string,
  ) => {
    externalConflictRef.current = true;
    setExternalConflict(true);
    setExternalConflictKind(kind);
    setExternalMessage(message);
    setPreservedVersionsOpen(true);
    void pauseAutomaticSaving();
    void refreshPreservedVersions();
  };

  const handleExternalProbeResult = (result: ExternalChangeProbe) => {
    updateLibraryFromExternalResult(result.library);
    const currentSession = sessionRef.current;
    if (!currentSession || result.sessionId !== currentSession.sessionId) return;
    // Conflict protection owns the draft and the explicit leave decision. A
    // restored file must not turn that state into a metadata-only read lock.
    if (externalConflictRef.current) return;

    const isMetadataReadOnlySession = metadataReadOnlySessionIdRef.current === currentSession.sessionId;
    const isProtectedPreview = currentSession.trashedAt !== null
      || activePreservedPreviewRef.current?.sessionId === currentSession.sessionId;
    const becomesMetadataReadOnly = currentSession.readOnlyReason === null
      && result.sessionReadOnlyReason !== null
      && !isProtectedPreview;
    if (becomesMetadataReadOnly && hasUnsettledEditorWork(currentSession.sessionId)) {
      pendingMetadataReadOnlyProbeSessionIdRef.current = currentSession.sessionId;
      return;
    }
    if (pendingMetadataReadOnlyProbeSessionIdRef.current === currentSession.sessionId) {
      pendingMetadataReadOnlyProbeSessionIdRef.current = null;
    }
    if (result.sessionReadOnlyReason === null && isMetadataReadOnlySession) {
      markMetadataReadOnlySession(null);
    } else if (becomesMetadataReadOnly) {
      markMetadataReadOnlySession(currentSession.sessionId);
    }
    if (currentSession.readOnlyReason === result.sessionReadOnlyReason) return;

    const nextSession = { ...currentSession, readOnlyReason: result.sessionReadOnlyReason };
    updateSessionMetadata(nextSession);
    const currentWorkspace = workspaceRef.current;
    if (currentWorkspace) updateWorkspace({ ...currentWorkspace, session: nextSession });
  };

  const handleExternalProbeError = async (
    cause: unknown,
    token: ExternalChangeToken,
    isCurrent: () => boolean,
  ) => {
    const operationError = toWorkspaceError(cause);
    await externalChangeHandlerRef.current({
      sessionId: token.sessionId,
      status: 'unreadable',
      revision: null,
      sessionReadOnlyReason: null,
      message: operationError.message,
      library: null,
    }, token, isCurrent);
  };

  const handleExternalChange = async (
    result: ExternalChangeProbe,
    _token: ExternalChangeToken,
    isCurrent: () => boolean,
  ) => {
    if (!isCurrent()) return;
    updateLibraryFromExternalResult(result.library);
    const initialSession = sessionRef.current;
    if (!initialSession || result.sessionId !== initialSession.sessionId) {
      void refreshPreservedVersions();
      return;
    }
    if (externalConflictRef.current && externalConflictProtectedRef.current) return;
    const hasOtherOperation = () => operationInProgressRef.current
      || manualSaveInProgressRef.current
      || Boolean(activeTransitionRef.current)
      || Boolean(pendingTransitionRef.current)
      || Boolean(transitionAttemptRef.current)
      || isTransitionSaving
      || isOperationBusy
      || isInitializing
      || isAbandoningRef.current;
    const queueProbe = () => externalChangesCoordinatorRef.current?.schedule();
    if (hasOtherOperation()) {
      queueProbe();
      return;
    }

    externalReconcilePendingRef.current = true;
    setIsReconcilingExternalChange(true);
    let ownsOperationLock = false;
    const conflictKind = result.status === 'missing'
      ? 'missing'
      : result.status === 'unreadable'
        ? 'unreadable'
        : 'conflict';
    try {
      await pauseAutomaticSaving();
      if (!isCurrent()) {
        queueProbe();
        resumeAutomaticSavingWhenSafe();
        return;
      }
      await settleEditors();
      if (!isCurrent() || sessionRef.current?.sessionId !== initialSession.sessionId) {
        queueProbe();
        resumeAutomaticSavingWhenSafe();
        return;
      }
      if (hasOtherOperation()) {
        queueProbe();
        return;
      }

      operationInProgressRef.current = true;
      ownsOperationLock = true;
      flushSync(() => setIsOperationBusy(true));
      const confirmedSession = sessionRef.current;
      if (!confirmedSession || confirmedSession.sessionId !== initialSession.sessionId) {
        queueProbe();
        return;
      }
      const finalContent = draftRef.current;
      if (!finalContent) {
        blockExternalSaving(conflictKind, '无法读取当前编辑器草稿。现有内容仍保留，自动保存已暂停。');
        return;
      }
      const reconciliation = await workspaceApi.reconcileExternalChanges(
        confirmedSession.sessionId,
        cloneNoteContent(finalContent),
      );
      updateLibraryFromExternalResult(reconciliation.library);
      if (reconciliation.status === 'refreshed' && reconciliation.session) {
        const needsProtection = !sameContent(finalContent, confirmedSession.content);
        if (needsProtection) {
          blockExternalSaving(conflictKind, reconciliation.message
            ?? '当前草稿与打开时的内容不同，无法确认它已进入保留版本区。草稿仍留在此处，自动保存已暂停。');
        } else {
          applySession(reconciliation.session);
          void refreshPreservedVersions();
        }
      } else if (reconciliation.status === 'conflict') {
        externalConflictProtectedRef.current = true;
        blockExternalSaving(conflictKind, reconciliation.message
          ?? '磁盘版本与当前草稿冲突。原文件、当前版本和草稿均已保留；请查看历史与冲突后选择下一步。');
      } else if (reconciliation.status === 'unchanged') {
        const needsProtection = !sameContent(finalContent, confirmedSession.content);
        if (needsProtection) blockExternalSaving(conflictKind, reconciliation.message
          ?? '当前文件无法安全读取。草稿仍保留在此处，自动保存已暂停。');
        else clearExternalConflict();
      } else {
        blockExternalSaving(conflictKind, reconciliation.message
          ?? '无法安全读取最新版本。草稿仍保留在此处，自动保存已暂停。');
      }
    } catch (cause) {
      const operationError = toWorkspaceError(cause);
      setError(operationError);
      const probeFailure = result.status === 'unreadable' && result.message
        ? `外部检查失败：${result.message}；`
        : '';
      blockExternalSaving(conflictKind, `${probeFailure}协调磁盘版本失败：${operationError.message} 草稿仍保留在此处，自动保存已暂停。`);
    } finally {
      if (ownsOperationLock) {
        operationInProgressRef.current = false;
        flushSync(() => setIsOperationBusy(false));
      }
      externalReconcilePendingRef.current = false;
      setIsReconcilingExternalChange(false);
      if (!externalConflictRef.current) resumeAutomaticSavingWhenSafe();
    }
  };

  externalChangeHandlerRef.current = handleExternalChange;
  if (externalChangesCoordinatorRef.current === null) {
    externalChangesCoordinatorRef.current = createExternalChangesCoordinator({
      getSessionId: () => sessionRef.current?.sessionId ?? null,
      probe: (sessionId) => workspaceApi.probeExternalChanges(sessionId),
      onChanged: (result, token, isCurrent) => externalChangeHandlerRef.current(result, token, isCurrent),
      onProbeResult: handleExternalProbeResult,
      onError: handleExternalProbeError,
    });
  }
  const externalChangesCoordinator = externalChangesCoordinatorRef.current;

  const hasExpandedEditor = () => [...editorFlushersRef.current.values()]
    .some(sessionId => sessionId === sessionRef.current?.sessionId);

  const attemptTransitionAfterSave = async (transition: PendingTransition) => {
    if (externalConflictRef.current) {
      setDialogMessage(externalConflictProtectedRef.current
        ? '当前笔记有未解决的外部版本冲突。草稿和原文件都已保留，请先查看历史与冲突并选择下一步。'
        : '当前笔记有未解决的外部版本冲突。草稿仍在本页，自动保存已暂停；请选择一个操作来保护草稿后再继续。');
      return;
    }
    if (transitionAttemptRef.current) return;
    transitionAttemptRef.current = transition;
    setIsTransitionSaving(true);
    setDialogMessage(`正在保存最新修改，完成后继续${transition.label}。可以取消此次操作。`);
    try {
      await settleEditors();
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
    } catch (cause) {
      if (pendingTransitionRef.current === transition) {
        setError(toWorkspaceError(cause));
        setDialogMessage('内容编辑尚未完成，当前草稿仍留在本页。请检查提示后重试保存或取消此次操作。');
      }
    } finally {
      if (transitionAttemptRef.current === transition) {
        transitionAttemptRef.current = null;
        setIsTransitionSaving(false);
      }
    }
  };

  const beginTransition = (transition: PendingTransition) => {
    if (externalConflictRef.current) {
      setDialogMessage(externalConflictProtectedRef.current
        ? '草稿和磁盘原件已保留。请选择打开最新版本、恢复为新笔记或取消切换。'
        : '当前草稿仍在本页。请选择一个操作先保护草稿，再继续切换。');
      return;
    }
    if (hasBlockingEditorIssue()) {
      setDialogMessage('编辑器内容尚无法转换为安全的保存快照。请撤销或修复该操作后再继续。');
      return;
    }
    if (transitionAttemptRef.current || operationInProgressRef.current) return;
    const state = saveCoordinator.getState();
    if (isComposingRef.current || pendingEditorOperationsRef.current.size > 0 || hasExpandedEditor() || state.isDirty || state.phase === 'saving') {
      void attemptTransitionAfterSave(transition);
      return;
    }
    clearPendingTransition(false);
    void executeTransition(transition);
  };

  const requestTransition = (transition: PendingTransition, queueWhileBusy = false) => {
    if (pendingTransitionRef.current) return;
    if (externalReconcilePendingRef.current) {
      if (!queueWhileBusy) return;
      pendingTransitionRef.current = transition;
      transitionWaitingForOperationRef.current = true;
      setPendingTransition(transition);
      setIsWaitingForOperation(true);
      setDialogMessage(`正在检查磁盘版本，完成后继续${transition.label}。`);
      return;
    }
    if (externalConflictRef.current) {
      pendingTransitionRef.current = transition;
      setPendingTransition(transition);
      setDialogMessage(externalConflictProtectedRef.current
        ? '外部版本与当前草稿存在冲突。草稿和磁盘原件均已保留；请先查看历史与冲突，或明确选择继续此操作。'
        : '外部版本与当前草稿存在冲突。当前草稿仍在本页；请先保护草稿，再继续此操作。');
      return;
    }
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
    if (isComposingRef.current || pendingEditorOperationsRef.current.size > 0 || hasExpandedEditor() || state.isDirty || state.phase === 'saving') {
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
    if (!desktopRuntime || isInitializing || operationInProgressRef.current || externalReconcilePendingRef.current
      || manualSaveInProgressRef.current || externalConflictRef.current || hasEditorIssue()
      || !currentSession || currentSession.readOnlyReason || currentSession.trashedAt !== null) return;
    manualSaveInProgressRef.current = true;
    try {
      await settleEditors();
      if (sessionRef.current?.sessionId !== currentSession.sessionId || operationInProgressRef.current
        || externalReconcilePendingRef.current || externalConflictRef.current) return;
      await saveCoordinator.flush();
    } catch (cause) {
      setError(toWorkspaceError(cause));
    } finally {
      manualSaveInProgressRef.current = false;
    }
  };
  flushCurrentRef.current = () => { void saveCurrent(); };

  const saveCurrentAs = async () => {
    if (externalReconcilePendingRef.current) return;
    try {
      await settleEditors();
    } catch (cause) {
      setSafeCopyError(toWorkspaceError(cause));
      return;
    }
    const currentSession = sessionRef.current;
    const snapshot = draftRef.current;
    const metadataReadOnly = Boolean(currentSession
      && metadataReadOnlySessionIdRef.current === currentSession.sessionId);
    const unchangedReadOnlyCopy = Boolean(currentSession && (
      currentSession.readOnlyReason && !metadataReadOnly || hasInitialEditorIssue()
    ));
    if (!desktopRuntime || isInitializing || operationInProgressRef.current || externalReconcilePendingRef.current
      || hasEditorIssue() && !unchangedReadOnlyCopy || !currentSession || !snapshot || currentSession.trashedAt !== null) return;
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
    if (operationInProgressRef.current || externalReconcilePendingRef.current) return;
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

  const restoreRecovery = (draft: RecoveryDraftSummary) => {
    if (workspaceRef.current?.library?.defaultNotebookId === null) return;
    return requestTransition({
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
  };

  const discardAndContinue = async () => {
    const transition = pendingTransitionRef.current;
    if (!transition || externalReconcilePendingRef.current || hasEditorIssue() || isTransitionSaving
      || isOperationBusy || isSaving || isComposing || isInitializing) return;
    if (externalConflictRef.current) {
      try {
        await settleEditors();
      } catch (cause) {
        setError(toWorkspaceError(cause));
        setDialogMessage('编辑器内容尚未完成整理，草稿仍留在本页。请修复内容后重试。');
        return;
      }
      if (pendingTransitionRef.current !== transition || isComposingRef.current) return;
    }
    isAbandoningRef.current = true;
    operationInProgressRef.current = true;
    setIsAbandoning(true);
    flushSync(() => setIsOperationBusy(true));
    setDialogMessage(externalConflictRef.current
      ? '正在保留最新草稿并等待当前磁盘写入。'
      : '正在停止自动保存并等待当前磁盘写入。可以取消此次操作。');
    try {
      await saveCoordinator.pauseAutomaticSavingAndWait();
      if (pendingTransitionRef.current !== transition) return;
      if (externalConflictRef.current) {
        const currentSession = sessionRef.current;
        const currentDraft = draftRef.current;
        if (!currentSession || !currentDraft) {
          setDialogMessage('当前草稿不可用，磁盘原件仍保留；请先检查历史与冲突。');
          return;
        }
        const protectedKind = externalConflictKind ?? 'conflict';
        const needsDraftProtection = !sameContent(currentDraft, currentSession.content);
        const synchronizeCancelledRefresh = (result: ExternalChangeReconciliation | undefined) => {
          if (!result || result.status !== 'refreshed' || !result.session || needsDraftProtection
            || sessionRef.current?.sessionId !== currentSession.sessionId
            || !draftRef.current || !sameContent(draftRef.current, currentDraft)) return;
          updateLibraryFromExternalResult(result.library);
          applySession(result.session);
          void refreshPreservedVersions();
        };
        const transitionChoice = await continueFromVerifiedCopyOrProtect(
          { sessionId: currentSession.sessionId, content: currentDraft },
          currentCopy,
          sameContent,
          () => pendingTransitionRef.current === transition
            && sessionRef.current?.sessionId === currentSession.sessionId,
          (snapshot) => workspaceApi.reconcileExternalChanges(
            snapshot.sessionId,
            cloneNoteContent(snapshot.content),
          ),
        );
        if (transitionChoice.kind === 'cancelled'
          || pendingTransitionRef.current !== transition
          || sessionRef.current?.sessionId !== currentSession.sessionId) {
          const completedReconciliation = transitionChoice.kind === 'protect' || transitionChoice.kind === 'cancelled'
            ? transitionChoice.result
            : undefined;
          // The host may have refreshed this same frozen note even when a new
          // transition replaced the cancelled one. Sync its session without
          // clearing or executing either pending navigation here.
          synchronizeCancelledRefresh(completedReconciliation);
          return;
        }
        if (transitionChoice.kind === 'continue') {
          setDialogMessage('已核对已另存副本与当前冻结草稿一致；继续操作不会标记笔记库原件已保存。');
        } else if (transitionChoice.kind === 'protect') {
          const reconciliation = transitionChoice.result;
          updateLibraryFromExternalResult(reconciliation.library);
          if (reconciliation.status === 'conflict') {
            externalConflictProtectedRef.current = true;
            blockExternalSaving(protectedKind, reconciliation.message
              ?? '最新草稿已保留为冲突版本。可以继续打开所选内容。');
          } else if (reconciliation.status === 'refreshed' && reconciliation.session) {
            if (needsDraftProtection) {
              blockExternalSaving(protectedKind, reconciliation.message
                ?? '无法确认当前草稿已进入保留版本区，因此未执行切换。');
              setDialogMessage('当前草稿尚未确认已受到保护。可另存当前内容为副本，或稍后重试。');
              return;
            }
            // This transition will replace the active session immediately after the
            // protected draft is reconciled, so the refreshed base is not shown.
            void refreshPreservedVersions();
          } else if (reconciliation.status === 'unchanged') {
            if (needsDraftProtection) {
              blockExternalSaving(protectedKind, reconciliation.message
                ?? '无法确认当前草稿已进入保留版本区，因此未执行切换。');
              setDialogMessage('当前草稿尚未确认已受到保护。可另存当前内容为副本，或稍后重试。');
              return;
            }
            void refreshPreservedVersions();
          }
        }
      }
      clearPendingTransition(false);
      isAbandoningRef.current = false;
      operationInProgressRef.current = false;
      setIsAbandoning(false);
      setIsOperationBusy(false);
      await executeTransition(transition);
    } catch (cause) {
      if (pendingTransitionRef.current === transition) {
        const operationError = toWorkspaceError(cause);
        setError(operationError);
        setDialogMessage(`无法安全继续：${operationError.message} 草稿和已保留版本仍在当前工作区。`);
      }
    } finally {
      isAbandoningRef.current = false;
      operationInProgressRef.current = false;
      setIsAbandoning(false);
      setIsOperationBusy(false);
      if (pendingTransitionRef.current !== transition && !externalConflictRef.current) saveCoordinator.resumeAutomaticSaving();
    }
  };

  const cancelPendingTransition = () => clearPendingTransition();

  const openLatestExternalVersion = () => {
    const currentSession = sessionRef.current;
    if (!currentSession || !externalConflictRef.current || externalReconcilePendingRef.current) return;
    const transition: PendingTransition = {
      label: '打开磁盘上的最新版本',
      run: async () => applySession(await workspaceApi.openNote(currentSession.noteId)),
    };
    pendingTransitionRef.current = transition;
    setPendingTransition(transition);
    setDialogMessage('继续前会再次保护当前草稿，然后读取磁盘上的最新版本。');
    void discardAndContinue();
  };

  const previewPreservedVersion = async (version: PreservedVersionSummary) => {
    if (previewingVersionId !== null || externalReconcilePendingRef.current) return;
    setPreservedVersionsError(null);
    const transition: PendingTransition = {
      label: `预览“${version.title || '未命名笔记'}”的保留版本`,
      run: async () => {
        const returnNoteId = sessionRef.current?.noteId ?? null;
        setPreviewingVersionId(version.versionId);
        try {
          const preview = await workspaceApi.openPreservedVersion(version.versionId);
          if (isTrashViewRef.current) setTrashView(false);
          applySession(preview);
          markActivePreservedPreview({
            ...version,
            sessionId: preview.sessionId,
            returnNoteId,
          });
          void refreshPreservedVersions();
        } finally {
          setPreviewingVersionId(null);
        }
      },
    };
    if (externalConflictRef.current) {
      pendingTransitionRef.current = transition;
      setPendingTransition(transition);
      setDialogMessage('继续前会再次保护当前草稿，然后在主编辑区只读预览所选版本。');
      void discardAndContinue();
      return;
    }
    requestTransition(transition);
  };

  const restorePreservedVersion = (version: PreservedVersionSummary) => {
    if (externalReconcilePendingRef.current) return;
    if (workspaceRef.current?.library?.defaultNotebookId === null) return;
    const transition: PendingTransition = {
      label: `恢复“${version.title || '未命名笔记'}”为新笔记`,
      run: async () => {
        const receipt = await workspaceApi.restorePreservedAsCopy(version.versionId);
        if (isTrashViewRef.current) {
          setTrashView(false);
          persistViewPreferences({ ...viewPreferencesRef.current, selectedNotebookId: null });
        }
        const currentWorkspace = workspaceRef.current;
        if (currentWorkspace) updateWorkspace({ ...currentWorkspace, library: receipt.library, session: receipt.session });
        applySession(receipt.session);
        await refreshPreservedVersions();
      },
    };
    if (externalConflictRef.current) {
      pendingTransitionRef.current = transition;
      setPendingTransition(transition);
      setDialogMessage('继续前会再次保护当前草稿，并将所选版本恢复为独立新笔记。原保留版本会继续留存。');
      void discardAndContinue();
      return;
    }
    requestTransition(transition);
  };

  const savePreservedVersionAs = async (version: PreservedVersionSummary) => {
    if (operationInProgressRef.current || externalReconcilePendingRef.current || isComposingRef.current) return;
    try {
      await settleEditors();
    } catch (cause) {
      setPreservedVersionsError(toWorkspaceError(cause));
      return;
    }
    if (operationInProgressRef.current || externalReconcilePendingRef.current || isComposingRef.current) return;
    operationInProgressRef.current = true;
    flushSync(() => setIsOperationBusy(true));
    setPreservedVersionsError(null);
    try {
      const copy = await workspaceApi.savePreservedAs(version.versionId);
      if (copy) setPreservedCopyPaths((current) => ({ ...current, [version.versionId]: copy.path }));
    } catch (cause) {
      setPreservedVersionsError(toWorkspaceError(cause));
    } finally {
      operationInProgressRef.current = false;
      setIsOperationBusy(false);
    }
  };

  const markPreservedVersionHandled = async (version: PreservedVersionSummary) => {
    if (preservedVersionActionBusy || externalReconcilePendingRef.current) return;
    setPreservedVersionActionBusy(true);
    setPreservedVersionsError(null);
    try {
      await workspaceApi.resolvePreservedVersion(version.versionId);
      await refreshPreservedVersions();
    } catch (cause) {
      setPreservedVersionsError(toWorkspaceError(cause));
    } finally {
      setPreservedVersionActionBusy(false);
    }
  };

  const openPreviewDestination = (noteId: string | null) => {
    if (!noteId) return;
    const library = workspaceRef.current?.library;
    const note = library?.notes.find((item) => item.noteId === noteId)
      ?? library?.trashNotes.find((item) => item.noteId === noteId);
    if (note) {
      setTrashView(note.trashedAt !== null);
      openNote(note.noteId, note.title);
    }
  };

  const closePreservedPreview = () => {
    const preview = activePreservedPreviewRef.current;
    if (!preview) return;
    const library = workspaceRef.current?.library;
    const returnNote = preview.returnNoteId
      ? library?.notes.find((note) => note.noteId === preview.returnNoteId)
        ?? library?.trashNotes.find((note) => note.noteId === preview.returnNoteId)
      : null;
    const sourceNote = library?.notes.find((note) => note.noteId === preview.noteId)
      ?? library?.trashNotes.find((note) => note.noteId === preview.noteId);
    const target = returnNote ?? sourceNote;
    if (target) openPreviewDestination(target.noteId);
    else setError({ kind: 'workspace', message: '此保留版本的原笔记已不在当前库中。请从侧栏打开另一篇笔记，或将版本恢复为新笔记。' });
  };

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
    void refreshPreservedVersions();

    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    if (!desktopRuntime || isInitializing) return;
    let active = true;
    let unlisten: (() => void) | null = null;
    let unlistenFilesChanged: (() => void) | null = null;
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

    const externalChanges = externalChangesCoordinatorRef.current;
    const scheduleExternalProbe = () => externalChanges?.schedule();
    const handleVisibilityChange = () => {
      if (document.visibilityState === 'visible') scheduleExternalProbe();
    };
    window.addEventListener('focus', scheduleExternalProbe);
    document.addEventListener('visibilitychange', handleVisibilityChange);
    void listen('cuenote:files-changed', scheduleExternalProbe)
      .then((stopListening) => {
        if (!active) {
          stopListening();
          return;
        }
        unlistenFilesChanged = stopListening;
      })
      .catch((cause: unknown) => {
        if (active) setError(toWorkspaceError(cause));
      });
    externalChanges?.schedule();

    return () => {
      active = false;
      setCloseGuardReady(false);
      unlisten?.();
      unlistenFilesChanged?.();
      window.removeEventListener('focus', scheduleExternalProbe);
      document.removeEventListener('visibilitychange', handleVisibilityChange);
    };
  }, [isInitializing]);

  useEffect(() => {
    saveCoordinator.resume();
    return () => saveCoordinator.dispose();
  }, [saveCoordinator]);

  useEffect(() => {
    externalChangesCoordinator.resume();
    return () => externalChangesCoordinator.dispose();
  }, [externalChangesCoordinator]);

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
    if (!current || !currentSession || isTrashViewRef.current || currentSession.readOnlyReason || currentSession.trashedAt !== null
      || activePreservedPreviewRef.current?.sessionId === currentSession.sessionId) return;
    const next = { ...current, title: value };
    externalConflictProtectedRef.current = false;
    externalChangesCoordinator.noteInputChanged();
    draftRef.current = next;
    setDraft(next);
    if (!isComposingRef.current && !hasEditorIssue()) saveCoordinator.edit(currentSession.sessionId, cloneNoteContent(next));
  };

  const updateSectionDraft = (section: RichTextSection, blocks: NoteContent['sections'][RichTextSection]) => {
    if (operationInProgressRef.current || isAbandoningRef.current) return;
    const current = draftRef.current;
    const currentSession = sessionRef.current;
    if (!current || !currentSession || isTrashViewRef.current || currentSession.readOnlyReason || currentSession.trashedAt !== null
      || activePreservedPreviewRef.current?.sessionId === currentSession.sessionId) return;
    const next = cloneNoteContent({ ...current, sections: { ...current.sections, [section]: blocks } });
    externalConflictProtectedRef.current = false;
    externalChangesCoordinator.noteInputChanged();
    draftRef.current = next;
    setDraft(next);
    if (!isComposingRef.current && !hasEditorIssue()) saveCoordinator.edit(currentSession.sessionId, cloneNoteContent(next));
  };

  const trackEditorOperation = (operation: Promise<void>) => {
    const settled = operation.then(() => null, (cause: unknown) => toWorkspaceError(cause));
    pendingEditorOperationsRef.current.set(operation, settled);
    setPendingEditorOperationCount(pendingEditorOperationsRef.current.size);
    void settled.then(() => {
      pendingEditorOperationsRef.current.delete(operation);
      setPendingEditorOperationCount(pendingEditorOperationsRef.current.size);
      scheduleDeferredMetadataReadOnlyProbe();
    });
  };

  const imageBridgeForSession = (sessionId: string): ImageEditorBridge => {
    const assertSession = () => {
      const current = sessionRef.current;
      if (!current || current.sessionId !== sessionId) throw new Error('笔记已切换，请在当前笔记重新粘贴图片。');
      return current;
    };
    const assertEditable = () => {
      const current = assertSession();
      if (current.readOnlyReason || current.trashedAt !== null || activePreservedPreviewRef.current?.sessionId === sessionId || isTrashViewRef.current
        || operationInProgressRef.current || isAbandoningRef.current || hasEditorIssue()) {
        throw new Error('当前笔记暂时无法插入图片。已有内容保持不变。');
      }
    };
    return {
      canStart: () => {
        try {
          assertEditable();
          return desktopRuntime && !pendingTransitionRef.current;
        } catch {
          return false;
        }
      },
      stage: async (file) => {
        assertEditable();
        if (file.size > 512 * 1024 * 1024) throw new Error('图片超过笔记文件的展开大小上限，未插入图片。');
        const bytes = new Uint8Array(await file.arrayBuffer());
        assertEditable();
        const asset = await workspaceApi.stageImage(sessionId, bytes);
        assertEditable();
        return asset;
      },
      read: async (assetId) => {
        assertSession();
        const image = await workspaceApi.readImage(sessionId, assetId);
        assertSession();
        return image;
      },
      preflight: async (contentBlocks) => {
        assertEditable();
        const basis = draftRef.current;
        if (!basis) throw new Error('当前笔记草稿不可用。');
        // A map may be open while a previously started image paste completes.
        // Its latest tree lives in the draft until it is collapsed into Tiptap.
        const draftMaps = new Map(basis.sections.content.flatMap(block => block.type === 'mindMap' ? [[block.mapId, block] as const] : []));
        const currentBlocks = contentBlocks.map(block => block.type === 'mindMap' ? draftMaps.get(block.mapId) ?? block : block);
        const candidate = cloneNoteContent({ ...basis, sections: { ...basis.sections, content: currentBlocks } });
        try {
          await workspaceApi.preflightImageInsert(sessionId, candidate);
        } catch (cause) {
          // A rejected old draft says nothing about a newer one, for example
          // when the user removed another image while the size check ran.
          if (sessionRef.current?.sessionId === sessionId && draftRef.current !== basis) return () => false;
          throw cause;
        }
        return () => sessionRef.current?.sessionId === sessionId && draftRef.current === basis
          && !operationInProgressRef.current && !isAbandoningRef.current && !hasEditorIssue();
      },
      track: trackEditorOperation,
    };
  };

  const mindMapBridgeForSession = (sessionId: string): MindMapEditorBridge => {
    const assertEditable = () => {
      const current = sessionRef.current;
      if (!current || current.sessionId !== sessionId || current.readOnlyReason
        || activePreservedPreviewRef.current?.sessionId === sessionId
        || current.trashedAt !== null || isTrashViewRef.current || operationInProgressRef.current
        || isAbandoningRef.current || hasEditorIssue()) {
        throw new Error('当前笔记暂时无法编辑导图。已有内容保持不变。');
      }
    };
    return {
      canStart: () => {
        try {
          assertEditable();
          return desktopRuntime && !pendingTransitionRef.current;
        } catch {
          return false;
        }
      },
      preflight: async contentBlocks => {
        // An operation begun before a close/switch request may still finish.
        // The transition waits for registered editors and tracked operations.
        assertEditable();
        const basis = draftRef.current;
        if (!basis) throw new Error('当前笔记草稿不可用。');
        const candidate = cloneNoteContent({ ...basis, sections: { ...basis.sections, content: contentBlocks } });
        try {
          await workspaceApi.preflightNoteContent(sessionId, candidate);
        } catch (cause) {
          if (sessionRef.current?.sessionId === sessionId && draftRef.current !== basis) return () => false;
          throw cause;
        }
        return () => sessionRef.current?.sessionId === sessionId && draftRef.current === basis
          && !operationInProgressRef.current && !isAbandoningRef.current && !hasEditorIssue();
      },
      track: trackEditorOperation,
      registerFlush: settle => {
        editorFlushersRef.current.set(settle, sessionId);
        return () => {
          editorFlushersRef.current.delete(settle);
          scheduleDeferredMetadataReadOnlyProbe();
        };
      },
    };
  };

  const handleCompositionStart = () => {
    externalConflictProtectedRef.current = false;
    externalChangesCoordinator.noteInputChanged();
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
    scheduleDeferredMetadataReadOnlyProbe();
  };
  const handleEditorCompositionEnd = (section: RichTextSection, blocks: NoteContent['sections'][RichTextSection] | null) => {
    isComposingRef.current = false;
    setIsComposing(false);
    if (blocks) updateSectionDraft(section, blocks);
    resolveCompositionWaiters();
    scheduleDeferredMetadataReadOnlyProbe();
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

  const initializeSelectedLibrary = () => requestTransition({
    label: '初始化当前空笔记库',
    run: async () => applyWorkspace(await workspaceApi.initializeSelectedLibrary()),
  });

  const createNote = () => {
    const currentWorkspace = workspaceRef.current;
    const currentLibrary = currentWorkspace?.library;
    const creatingFromTrash = isTrashViewRef.current;
    const targetNotebookId = creatingFromTrash ? null : selectedNotebookId;
    if (!currentLibrary || (targetNotebookId === null && currentLibrary.defaultNotebookId === null)) return;
    const targetNotebook = targetNotebookId
      ? currentLibrary.notebooks.find((notebook) => notebook.notebookId === targetNotebookId)
      : currentLibrary.defaultNotebookId
        ? currentLibrary.notebooks.find((notebook) => notebook.notebookId === currentLibrary.defaultNotebookId)
        : null;
    const notebookName = targetNotebook
      ? notebookDisplayLabel(currentLibrary.notebooks, targetNotebook)
      : currentLibrary.defaultNotebookName ?? '默认笔记本';
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
    if (!currentSession || currentSession.trashedAt !== null
      || activePreservedPreviewRef.current?.sessionId === currentSession.sessionId) return;
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
    const targetNotebookId = currentLibrary.defaultNotebookId
      ?? currentLibrary.notebooks[0]?.notebookId;
    if (!targetNotebookId) {
      setError({
        kind: 'library_metadata_pending',
        message: '当前无法匹配笔记本信息，暂时不能选择恢复目标。请检查笔记库提示，文件或元数据变化后重新检查。',
      });
      return;
    }

    setError(null);
    setTrashDialog({
      mode: 'restore-target',
      sessionId: currentSession.sessionId,
      noteId: currentSession.noteId,
      title: noteTitle(currentSession.text),
      targetNotebookId,
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
    if (!currentDialog || currentDialog.mode === 'delete' || !currentLibrary || isNotebookBusy
      || operationInProgressRef.current || externalReconcilePendingRef.current) return;
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
    if (isNotebookBusy || operationInProgressRef.current || externalReconcilePendingRef.current) return;
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
      if (!currentSession || activePreservedPreviewRef.current?.sessionId === currentSession.sessionId) return;
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
    const destinationNotebook = currentLibrary.notebooks.find((notebook) => notebook.notebookId === notebookId);
    const destination = destinationNotebook
      ? notebookDisplayLabel(currentLibrary.notebooks, destinationNotebook)
      : '目标笔记本';
    updateCurrentNoteOrganization(notebookId, currentSession.pinned, `移至“${destination}”`);
  };

  const openLibraryFolder = async () => {
    if (isInitializing || operationInProgressRef.current || externalReconcilePendingRef.current) return;
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
  const defaultNotebook = library?.defaultNotebookId
    ? notebooks.find((notebook) => notebook.notebookId === library.defaultNotebookId) ?? null
    : null;
  const defaultNotebookDisplayName = defaultNotebook
    ? notebookDisplayLabel(notebooks, defaultNotebook)
    : library?.defaultNotebookName ?? '默认笔记本';
  const canCreateNote = Boolean(library && (isTrashView
    ? library.defaultNotebookId !== null
    : selectedNotebook !== null || library.defaultNotebookId !== null));
  const visibleNotes = isTrashView
    ? trashNotes
    : selectedNotebook
      ? notes.filter((note) => note.notebookId === selectedNotebook.notebookId)
      : notes;
  const selectedViewName = isTrashView ? '回收站' : selectedNotebook
    ? notebookDisplayLabel(notebooks, selectedNotebook)
    : '全部笔记';
  const isTrashSession = Boolean(session?.trashedAt);
  const sessionNotebookId = session?.notebookId ?? null;
  const sessionNotebook = sessionNotebookId === null
    ? null
    : notebooks.find((notebook) => notebook.notebookId === sessionNotebookId) ?? null;
  const sessionAttributionNotebook = sessionNotebook;
  const sessionNotebookDisplay = sessionAttributionNotebook
    ? notebookDisplay(notebooks, sessionAttributionNotebook)
    : null;
  const sessionAttributionNotebookLabel = sessionAttributionNotebook
    ? notebookDisplayLabel(notebooks, sessionAttributionNotebook)
    : null;
  const sessionOrganizationNotebookId = sessionNotebook?.notebookId ?? null;
  const canRestoreTrashedSession = Boolean(session && isTrashSession && notebooks.length > 0);
  const sessionMatchesView = Boolean(session && isTrashSession === isTrashView);
  const displayedDraft = sessionMatchesView ? draft : null;
  const editorIssue = Object.values(editorIssues).find(Boolean) ?? null;
  const hasAnyEditorIssue = Boolean(editorIssue);
  const hasInitialIssue = Object.values(editorIssues).some((issue) => issue?.initial);
  const hasBlockingIssue = Object.values(editorIssues).some((issue) => issue !== null && !issue.initial);
  const isActivePreservedPreview = Boolean(activePreservedPreview && session?.sessionId === activePreservedPreview.sessionId);
  const isMetadataReadOnlySession = Boolean(session
    && metadataReadOnlySessionId === session.sessionId
    && session.readOnlyReason);
  const usesStoredReadOnlyProjection = Boolean(session?.readOnlyReason
    && !isTrashSession && !isActivePreservedPreview && !isMetadataReadOnlySession);
  const displayedTitle = usesStoredReadOnlyProjection || isActivePreservedPreview || isTrashSession
    ? session?.text.title
    : displayedDraft?.title;
  const sessionNotebookName = !session
    ? null
    : isTrashSession
      ? sessionNotebookId === null
        ? '原笔记本未知'
        : sessionAttributionNotebookLabel ?? '原笔记本信息待补齐（归属已保留）'
      : sessionNotebookId !== null
        ? sessionAttributionNotebookLabel ?? '笔记本信息待补齐（归属已保留）'
        : '笔记本归属缺失';
  const isReadOnly = Boolean(session?.readOnlyReason || session?.trashedAt || isActivePreservedPreview);
  const isSaving = saveState.phase === 'saving';
  const saveFailed = saveState.phase === 'failed' || saveState.phase === 'conflict';
  const isDirty = Boolean(sessionMatchesView && session && draft && (!isReadOnly || isMetadataReadOnlySession) && (
    isComposing || hasBlockingIssue || (saveState.sessionId === session.sessionId
      ? saveState.isDirty
      : session.revision === null || !sameContent(draft, savedContent))
  ));
  const navigationLocked = isInitializing || isOperationBusy || isReconcilingExternalChange || isNotebookBusy || pendingTransition !== null || trashDialog !== null;
  const editingLocked = !desktopRuntime || isInitializing || isOperationBusy || isAbandoning || !closeGuardReady || isTrashView || isTrashSession || !sessionMatchesView || hasAnyEditorIssue;
  const sectionCanEdit = (section: RichTextSection) => {
    const ownIssue = editorIssues[section];
    return desktopRuntime && !isInitializing && !isOperationBusy && !isAbandoning && closeGuardReady
      && !isTrashView && !isTrashSession && sessionMatchesView && !isReadOnly
      && (!hasAnyEditorIssue || Boolean(ownIssue && !ownIssue.initial && !hasInitialIssue));
  };
  const canDiscardAfterCopy = Boolean(currentCopy && session && draft
    && currentCopy.sessionId === session.sessionId
    && sameContent(currentCopy.content, usesStoredReadOnlyProjection || hasInitialIssue ? session.content : draft));
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
    : isReconcilingExternalChange
      ? '正在整理编辑器并检查磁盘版本'
    : externalConflict
      ? !externalConflictProtectedRef.current
        ? '保存已暂停；当前草稿仍在本页，尚未确认独立保存'
        : externalConflictKind === 'missing'
          ? '文件已移除，草稿已保存到本机恢复区'
          : externalConflictKind === 'unreadable'
            ? '无法读取文件，草稿已保存到本机恢复区'
            : '外部版本冲突，草稿已保存到本机恢复区'
    : pendingEditorOperationCount > 0
      ? '正在处理内容，完成后自动保存'
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
    : isMetadataReadOnlySession
      ? isDirty ? '本机修改未保存；笔记库信息待核对' : '笔记库信息待核对；当前只读'
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
    if (usesStoredReadOnlyProjection) {
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
        imageBridge={section === 'content' ? imageBridgeForSession(session.sessionId) : undefined}
        mindMapBridge={section === 'content' ? mindMapBridgeForSession(session.sessionId) : undefined}
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
                  const display = notebookDisplay(notebooks, notebook);
                  const displayLabel = display.shortId ? `${display.name} · ${display.shortId}` : display.name;
                  return (
                    <li className="sidebar-notebook-row" key={notebook.notebookId}>
                      <button
                        className={`sidebar-notebook-item${isSelectedNotebook ? ' is-current' : ''}`}
                        type="button"
                        data-notebook-id={notebook.notebookId}
                        aria-current={isSelectedNotebook ? 'page' : undefined}
                        disabled={navigationLocked}
                        onClick={() => selectNotebookView(notebook.notebookId)}
                        title={display.shortId ? display.tooltip : notebook.name}
                        aria-label={`选择笔记本 ${displayLabel}`}
                      >
                        <span className="sidebar-notebook-icon" aria-hidden="true">▣</span>
                        <span className="sidebar-notebook-name">{notebook.name}</span>
                        {display.shortId && <span className="sidebar-notebook-id">{display.shortId}</span>}
                        <span className="sidebar-nav-count">{notebookNoteCount}</span>
                      </button>
                      {notebook.isDefault ? (
                        <span className="sidebar-default-label">默认</span>
                      ) : (
                        <div className="sidebar-notebook-actions">
                          <button
                            className="notebook-action"
                            type="button"
                            aria-label={`重命名笔记本 ${displayLabel}`}
                            title="重命名"
                            data-notebook-id={notebook.notebookId}
                            disabled={navigationLocked}
                            onClick={() => openNotebookDialog({ mode: 'rename', notebookId: notebook.notebookId, name: notebook.name })}
                          >编辑</button>
                          <button
                            className="notebook-action notebook-delete-action"
                            type="button"
                            aria-label={`删除笔记本 ${displayLabel}`}
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
              <div className="sidebar-empty">{library.defaultNotebookId === null
                ? '笔记本信息待核对；现有笔记仍显示在全部笔记中。'
                : '笔记库没有可用笔记本'}</div>
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
                aria-label={!canCreateNote
                  ? '笔记本信息补齐后可新建笔记'
                  : isTrashView
                  ? `在“${defaultNotebookDisplayName}”中新建笔记`
                  : `在${selectedNotebook ? `“${notebookDisplayLabel(notebooks, selectedNotebook)}”` : `“${defaultNotebookDisplayName}”`}中新建笔记`}
                title={library?.defaultNotebookId === null && !canCreateNote
                  ? '笔记本信息补齐或初始化后可新建笔记'
                  : '新建笔记'}
                disabled={!desktopRuntime || !canCreateNote || navigationLocked}
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
                <button className="quiet-button" type="button" disabled={isInitializing || isOperationBusy || isReconcilingExternalChange} onClick={openLibraryFolder}>
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
        {desktopRuntime && library && (
          <div className="workspace-notice library-sync-guidance" role="status">
            <span>OneDrive 提示：Windows 与 macOS 使用同一笔记库文件，请将整个目录设为“始终保留在此设备上”。CueNote 本机保存完成，不代表 OneDrive 云端同步完成。</span>
            <button
              className="text-button"
              type="button"
              title="文件或元数据变化后，重新检查当前笔记库"
              disabled={navigationLocked}
              onClick={() => void externalChangesCoordinator.checkNow()}
            >检查文件更新</button>
          </div>
        )}
        {desktopRuntime && library?.defaultNotebookId === null && (
          <div className="workspace-notice library-metadata-pending" role="status" data-testid="library-metadata-pending">
            <div>
              <strong>笔记本信息待核对</strong>
              <p>{isTrashView
                ? '现有笔记可在“全部笔记”中查看，原归属引用保持不变。回收站视图中的新建和依赖默认笔记本的恢复暂不可用。'
                : selectedNotebook
                  ? `现有笔记可在“全部笔记”中查看，原归属引用保持不变。仍可在已选笔记本“${selectedNotebook.name}”中新建；依赖默认笔记本的恢复暂不可用。`
                  : '现有笔记可在“全部笔记”中查看，原归属引用保持不变。未选具体笔记本时，新建笔记和依赖默认笔记本的恢复暂不可用。'}笔记本信息可能仍在同步，也可能存在无效或冲突元数据；请查看已有警告，文件或元数据变化后可检查更新。只有确认当前目录确为空时，才初始化默认笔记本；已有内容或无效、冲突元数据会由应用拒绝初始化。</p>
            </div>
            <button
              className="quiet-button"
              type="button"
              data-testid="initialize-empty-library"
              title="仅用于确认为空的笔记库；含已有笔记或其他内容的目录会被拒绝"
              disabled={navigationLocked}
              onClick={initializeSelectedLibrary}
            >初始化当前空笔记库</button>
          </div>
        )}

        {desktopRuntime && (
          <section className="recovery-panel" aria-labelledby="recovery-panel-title">
            <div className="recovery-panel-heading">
              <div>
                <span className="eyebrow">本机恢复区</span>
                <h2 id="recovery-panel-title">可恢复草稿</h2>
              </div>
              <div className="preserved-panel-heading-actions">
                <button
                  className={`quiet-button${preservedVersionsOpen ? ' is-active' : ''}`}
                  type="button"
                  aria-expanded={preservedVersionsOpen}
                  aria-controls="preserved-versions-panel"
                  onClick={() => {
                    const open = !preservedVersionsOpen;
                    setPreservedVersionsOpen(open);
                    if (open) void refreshPreservedVersions();
                  }}
                >历史与冲突{preservedVersions?.versions.length ? ` · ${preservedVersions.versions.length}` : ''}</button>
                <button className="text-button" type="button" disabled={recoveryLoading} onClick={() => void refreshRecoveryDrafts()}>
                  {recoveryLoading ? '正在读取…' : '刷新草稿'}
                </button>
              </div>
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
                        disabled={!library || library.defaultNotebookId === null || navigationLocked}
                        title={library?.defaultNotebookId === null ? '笔记本信息补齐后才能恢复为新笔记' : undefined}
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
            {preservedVersionsOpen && (
              <section className="preserved-versions-panel" id="preserved-versions-panel" aria-labelledby="preserved-versions-title">
                <div className="preserved-versions-heading">
                  <div>
                    <span className="eyebrow">已保存版本与冲突副本</span>
                    <h3 id="preserved-versions-title">历史与冲突</h3>
                  </div>
                  <button
                    className="text-button"
                    type="button"
                    disabled={preservedVersionsLoading}
                    onClick={() => void refreshPreservedVersions()}
                  >{preservedVersionsLoading ? '正在读取…' : '刷新版本'}</button>
                </div>
                {externalConflict && (
                  <div className="external-conflict-notice" role="status">
                    <strong>{externalConflictKind === 'missing' ? '笔记文件已移除' : externalConflictKind === 'unreadable' ? '无法读取笔记文件' : '磁盘版本与草稿冲突'}</strong>
                    <p>{externalMessage ?? '草稿仍保留在当前页面，自动保存和切换已暂停。'}</p>
                    {externalConflictKind === 'conflict' && (
                      <button className="quiet-button" type="button" disabled={isOperationBusy || isReconcilingExternalChange || isComposing} onClick={openLatestExternalVersion}>
                        打开磁盘最新版本…
                      </button>
                    )}
                  </div>
                )}
                {preservedVersionsError && renderError(preservedVersionsError)}
                {preservedVersionsLoading && !preservedVersions && <p className="recovery-empty">正在读取保留版本。</p>}
                {preservedVersions?.warnings.map((warning, index) => (
                  <div className="workspace-notice" role="status" key={`${index}-${warning}`}>{warning}</div>
                ))}
                {preservedVersions?.versions.length ? (
                  <ul className="recovery-list preserved-version-list">
                    {preservedVersions.versions.map((version) => (
                      <li className="recovery-item preserved-version-item" key={version.versionId}>
                        <div className="recovery-copy">
                          <strong>{version.title || '未命名笔记'}{version.conflict ? <span className="preserved-conflict-tag">冲突副本</span> : null}</strong>
                          <span>{preservedVersionReason(version)} · {formatModifiedAt(version.savedAt)}</span>
                          <span>保存来源：{version.origin} · 源版本：{version.sourceRevision}</span>
                        </div>
                        <div className="recovery-actions">
                          <button className="quiet-button" type="button" disabled={previewingVersionId !== null || navigationLocked || isTransitionSaving} onClick={() => void previewPreservedVersion(version)}>{previewingVersionId === version.versionId ? '正在读取…' : '预览'}</button>
                          <button
                            className="quiet-button"
                            type="button"
                            disabled={!library || library.defaultNotebookId === null || navigationLocked}
                            title={library?.defaultNotebookId === null ? '笔记本信息补齐后才能恢复为新笔记' : undefined}
                            onClick={() => restorePreservedVersion(version)}
                          >恢复为新笔记</button>
                          <button className="quiet-button" type="button" disabled={isOperationBusy || isReconcilingExternalChange || isComposing} onClick={() => void savePreservedVersionAs(version)}>另存为…</button>
                          <button className="text-button" type="button" disabled={preservedVersionActionBusy || isReconcilingExternalChange} onClick={() => void markPreservedVersionHandled(version)}>{preservedVersionActionBusy ? '正在处理…' : '标记已处理'}</button>
                        </div>
                        {preservedCopyPaths[version.versionId] && (
                          <p className="recovery-saved-path" role="status">已另存完整版本：<code>{preservedCopyPaths[version.versionId]}</code></p>
                        )}
                      </li>
                    ))}
                  </ul>
                ) : preservedVersions && preservedVersions.versions.length === 0 ? (
                  <p className="recovery-empty">当前没有历史版本或冲突副本。</p>
                ) : null}
              </section>
            )}
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
                <p>{canCreateNote
                  ? `在“${selectedNotebook ? notebookDisplayLabel(notebooks, selectedNotebook) : defaultNotebookDisplayName}”中创建一篇笔记，开始整理你的想法。`
                  : '笔记本信息待补齐。现有笔记仍在全部笔记中；文件或元数据变化后检查更新，或在确认目录为空时初始化。'}</p>
                <button
                  className="primary-button"
                  type="button"
                  data-testid="create-note-empty"
                  disabled={!canCreateNote || navigationLocked}
                  title={!canCreateNote ? '笔记本信息补齐或初始化后可新建笔记' : undefined}
                  onClick={createNote}
                >新建笔记</button>
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
              <span className="note-notebook-context">当前笔记本：<strong title={sessionNotebookDisplay?.shortId ? sessionNotebookDisplay.tooltip : undefined}>{sessionNotebookName}</strong></span>
              {desktopRuntime && library && !isTrashSession && (
                <div className="note-organization-controls">
                  <label className="note-move-control">
                    <span>移动到</span>
                    <select
                      aria-label="移动当前笔记到笔记本"
                      value={sessionOrganizationNotebookId ?? ''}
                      disabled={navigationLocked || isReadOnly || sessionOrganizationNotebookId === null}
                      onChange={(event) => moveCurrentNote(event.currentTarget.value)}
                    >
                      {sessionOrganizationNotebookId === null && (
                        <option value="" disabled>笔记本信息待补齐</option>
                      )}
                      {notebooks.map((notebook) => {
                        const display = notebookDisplay(notebooks, notebook);
                        return (
                          <option key={notebook.notebookId} value={notebook.notebookId} title={display.shortId ? display.tooltip : undefined}>
                            {display.shortId ? `${display.name} · ${display.shortId}` : display.name}
                          </option>
                        );
                      })}
                    </select>
                  </label>
                  <button
                    className={`pin-note-button${session.pinned ? ' is-pinned' : ''}`}
                    type="button"
                    aria-label={session.pinned ? '取消置顶当前笔记' : '置顶当前笔记'}
                    aria-pressed={session.pinned}
                    disabled={navigationLocked || isReadOnly || sessionOrganizationNotebookId === null}
                    title={sessionOrganizationNotebookId === null ? '笔记本信息补齐后可更改置顶状态' : undefined}
                    onClick={() => {
                      if (sessionOrganizationNotebookId === null) return;
                      updateCurrentNoteOrganization(
                        sessionOrganizationNotebookId,
                        !session.pinned,
                        session.pinned ? '取消置顶这篇笔记' : '置顶这篇笔记',
                      );
                    }}
                  >
                    <span aria-hidden="true">◆</span>{session.pinned ? '已置顶' : '置顶'}
                  </button>
                  <button
                    className="quiet-button destructive-quiet move-to-trash-button"
                    type="button"
                    data-testid="move-to-trash"
                    disabled={navigationLocked || isReadOnly}
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
                    disabled={navigationLocked || !canRestoreTrashedSession}
                    title={!canRestoreTrashedSession ? '至少一个可用笔记本的信息补齐后才能恢复' : undefined}
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
              <div className={`readonly-notice${isActivePreservedPreview ? ' preserved-preview-notice' : ''}`} role="status">
                <strong>{isActivePreservedPreview ? '正在主编辑区只读预览保留版本' : '这篇笔记以只读方式打开'}</strong>
                <p>{isActivePreservedPreview
                  ? `${activePreservedPreview?.title || '未命名笔记'} · ${activePreservedPreview ? preservedVersionReason(activePreservedPreview) : ''} · ${activePreservedPreview ? formatModifiedAt(activePreservedPreview.savedAt) : ''}。此版本不会覆盖笔记库原件。`
                  : session.readOnlyReason}</p>
                {isMetadataReadOnlySession
                  ? <span>当前草稿仍保留并显示在本页；笔记库信息核对完成前暂不能编辑或保存。</span>
                  : !isTrashSession && !isActivePreservedPreview && <span>当前版本暂不支持编辑其中的全部内容；原文件保持完整。</span>}
                {isActivePreservedPreview && (
                  <div className="preserved-preview-actions">
                    <button className="quiet-button" type="button" disabled={navigationLocked} onClick={closePreservedPreview}>关闭预览</button>
                    {(library?.notes.some((note) => note.noteId === activePreservedPreview?.noteId)
                      || library?.trashNotes.some((note) => note.noteId === activePreservedPreview?.noteId)) && (
                      <button className="quiet-button" type="button" disabled={navigationLocked} onClick={() => openPreviewDestination(activePreservedPreview?.noteId ?? null)}>打开笔记库中的原笔记</button>
                    )}
                  </div>
                )}
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
                    value={usesStoredReadOnlyProjection || isActivePreservedPreview || isTrashSession ? session.text.title : draft.title}
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
                  : isActivePreservedPreview
                    ? '当前为完整保留版本的只读预览，可恢复为新笔记或另存完整副本。'
                  : isMetadataReadOnlySession
                    ? '笔记库信息待核对，当前笔记只读；本页草稿仍保留，请检查文件变化后重试。'
                  : session.readOnlyReason || hasInitialIssue
                    ? '这篇笔记包含当前编辑器无法安全转换的内容；文字投影只读，可另存完整副本。'
                  : hasAnyEditorIssue
                    ? '编辑内容尚无法转换，自动保存和切换操作已暂停。请撤销或修复当前编辑。'
                  : isReadOnly
                    ? '当前笔记只读显示。'
                  : '停止输入约 1 秒后自动保存；使用 Ctrl / ⌘ + S 可立即保存。'}</span>
                {!isTrashSession && (externalConflict || saveFailed && !isReadOnly || session.readOnlyReason || hasInitialIssue) && (
                  <button className="quiet-button safe-copy-button" type="button" disabled={isOperationBusy || isReconcilingExternalChange || hasAnyEditorIssue && !hasInitialIssue} onClick={() => void saveCurrentAs()}>
                    {usesStoredReadOnlyProjection || hasInitialIssue ? '另存完整副本…' : '另存当前草稿…'}
                  </button>
                )}
                {currentCopy && (
                  <span className="safe-copy-path" role="status">
                    此次草稿已另存到：<code>{currentCopy.path}</code>{canDiscardAfterCopy
                      ? usesStoredReadOnlyProjection || hasInitialIssue
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
                disabled={editingLocked || isReconcilingExternalChange || isSaving || isReadOnly || externalConflict || hasAnyEditorIssue || (!isDirty && !saveFailed)}
                onClick={() => void saveCurrent()}
              >
                {externalConflict ? '先处理外部版本' : editorIssue?.initial ? '只读内容' : editorIssue ? '内容无法保存' : isSaving ? '正在保存…' : saveFailed ? '重试保存' : isDirty ? '立即保存' : session.revision === null ? '尚未保存' : '已保存到本地'}
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
            className={`unsaved-dialog${externalConflict ? ' external-conflict-dialog' : ''}`}
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
            {externalConflict && (
              <section className="conflict-dialog-versions" aria-label="受保护的历史与冲突版本">
                <p>{externalConflictProtectedRef.current
                  ? '草稿和磁盘原件均已保留。可预览保留版本、恢复为新笔记，或明确打开磁盘最新版本。'
                  : '当前草稿仍留在本页面。正在尝试保护草稿；也可先将当前内容另存为副本。'}</p>
                {preservedVersions?.versions.slice(0, 3).map((version) => (
                  <div className="conflict-dialog-version" key={version.versionId}>
                    <span><strong>{version.title || '未命名笔记'}</strong>{version.conflict ? ' · 冲突副本' : ' · 历史版本'}</span>
                    <div className="recovery-actions">
                      <button className="quiet-button" type="button" disabled={previewingVersionId !== null || isOperationBusy || isTransitionSaving || isReconcilingExternalChange} onClick={() => void previewPreservedVersion(version)}>{previewingVersionId === version.versionId ? '正在读取…' : '预览'}</button>
                      <button className="quiet-button" type="button" disabled={!library || isOperationBusy || isTransitionSaving || isComposing || isReconcilingExternalChange} onClick={() => restorePreservedVersion(version)}>恢复为新笔记</button>
                    </div>
                  </div>
                ))}
                {externalConflictKind === 'conflict' && (
                  <button className="quiet-button" type="button" disabled={isOperationBusy || isReconcilingExternalChange || isComposing} onClick={openLatestExternalVersion}>
                    打开磁盘最新版本…
                  </button>
                )}
              </section>
            )}
            {notebookDialogError && <div className="notebook-dialog-error" role="alert">笔记本操作未完成：{notebookDialogError}</div>}
            {error && renderError(error)}
            <div className="dialog-actions">
              {!externalConflict && (
                <button
                  className="primary-button"
                  type="button"
                  disabled={isTransitionSaving || isOperationBusy || isWaitingForOperation || isInitializing || isReadOnly || !session}
                  onClick={() => void attemptTransitionAfterSave(pendingTransition)}
                >
                  {isWaitingForOperation ? '等待当前操作…' : isTransitionSaving ? '正在保存…' : pendingTransition.close ? '重试保存并关闭' : '重试保存并继续'}
                </button>
              )}
              {(externalConflict || (saveFailed && !isReadOnly) || Boolean(session?.readOnlyReason || hasInitialIssue)) && (
                <button className="quiet-button" type="button" disabled={isOperationBusy || isReconcilingExternalChange || isWaitingForOperation || isInitializing} onClick={() => void saveCurrentAs()}>
                  {usesStoredReadOnlyProjection || hasInitialIssue ? '另存完整副本…' : '另存当前草稿…'}
                </button>
              )}
              {session && (externalConflict || !isReadOnly && (isDirty || saveFailed)) && (
                <button
                  className="quiet-button destructive-quiet"
                  type="button"
                  disabled={isTransitionSaving || isOperationBusy || isReconcilingExternalChange || isSaving || isComposing || isWaitingForOperation || isInitializing}
                  onClick={() => void discardAndContinue()}
                >
                  {isAbandoning
                    ? '正在等待磁盘写入…'
                    : externalConflict
                      ? externalConflictProtectedRef.current
                        ? `使用已保护草稿并${pendingTransition.close ? '关闭' : '继续'}`
                        : canDiscardAfterCopy
                          ? `已另存当前快照；${pendingTransition.close ? '关闭' : '继续'}`
                          : `先保护当前草稿并${pendingTransition.close ? '关闭' : '继续'}`
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
                  <button className="quiet-button destructive-quiet" type="button" disabled={isNotebookBusy || isReconcilingExternalChange} onClick={() => void deleteNotebook(notebookDialog.notebookId)}>
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
                    disabled={isNotebookBusy || isReconcilingExternalChange}
                    onChange={(event) => setNotebookDialog({ ...notebookDialog, name: event.currentTarget.value })}
                  />
                </label>
                {notebookDialogError && <div className="notebook-dialog-error" role="alert">{notebookDialogError}</div>}
                <div className="dialog-actions">
                  <button className="quiet-button" type="button" disabled={isNotebookBusy} onClick={() => setNotebookDialog(null)}>取消</button>
                  <button className="primary-button" type="submit" disabled={isNotebookBusy || isReconcilingExternalChange || !notebookDialog.name.trim()}>
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
                <p id="trash-dialog-description">“{trashDialog.title}”的原笔记本信息暂无法匹配，归属记录仍保留。请选择一个目标笔记本。</p>
                <label className="trash-target-field">
                  <span>恢复到笔记本</span>
                  <select
                    aria-label="恢复目标笔记本"
                    data-testid="trash-restore-target-picker"
                    value={trashDialog.targetNotebookId}
                    disabled={isOperationBusy}
                    onChange={(event) => setTrashDialog({ ...trashDialog, targetNotebookId: event.currentTarget.value, error: null })}
                  >
                    {(library?.notebooks ?? []).map((notebook) => {
                      const display = notebookDisplay(library?.notebooks ?? [], notebook);
                      return (
                        <option key={notebook.notebookId} value={notebook.notebookId} title={display.shortId ? display.tooltip : undefined}>
                          {display.shortId ? `${display.name} · ${display.shortId}` : display.name}
                        </option>
                      );
                    })}
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
