import type {
  BackupProgress,
  LibraryRestoreAction,
  LibraryRestorePreview,
  LibraryRestoreReceipt,
  LibraryRestoreStatus,
  WorkspaceView,
} from './workspace-api';

export type LibraryRestoreOperation = 'previewing' | 'preparing' | 'restoring' | 'resuming'
  | 'cancelling' | 'refreshing';

export interface LibraryRestoreCoordinatorState {
  libraryPath: string | null;
  selectedBackupId: string | null;
  preview: LibraryRestorePreview | null;
  pending: LibraryRestoreStatus | null;
  allowed: boolean;
  operation: LibraryRestoreOperation | null;
  requestId: string | null;
  progress: BackupProgress | null;
  startedAt: number | null;
  elapsedSeconds: number;
  error: string | null;
  notice: string | null;
  receipt: LibraryRestoreReceipt | null;
}

export interface LibraryRestoreCoordinatorApi {
  previewLibraryRestore: (backupId: string, requestId: string, expectedLibraryPath: string) =>
    Promise<LibraryRestorePreview>;
  restoreLibraryFromBackup: (
    backupId: string,
    expectedManifestRevision: string,
    requestId: string,
    expectedLibraryPath: string,
  ) => Promise<LibraryRestoreReceipt>;
  resumeLibraryRestore: (
    action: LibraryRestoreAction,
    requestId: string,
    expectedLibraryPath: string,
  ) => Promise<LibraryRestoreReceipt>;
  get: () => Promise<WorkspaceView>;
  cancelBackup: () => Promise<void>;
}

export interface LibraryRestoreCoordinatorClock {
  now: () => number;
  setInterval: (callback: () => void, delay: number) => unknown;
  clearInterval: (handle: unknown) => void;
}

export interface LibraryRestoreCoordinatorOptions {
  api: LibraryRestoreCoordinatorApi;
  onState?: (state: LibraryRestoreCoordinatorState) => void;
  onWorkspace?: (view: WorkspaceView) => void;
  clock?: LibraryRestoreCoordinatorClock;
  createRequestId?: () => string;
}

export interface LibraryRestoreCoordinatorInput {
  libraryPath: string | null;
  backupId: string | null;
  pending: LibraryRestoreStatus | null;
  allowed: boolean;
}

export type RunWithSavedEditor = (
  run: () => Promise<LibraryRestoreReceipt>,
  resume: boolean,
) => Promise<LibraryRestoreReceipt | null>;

const defaultClock: LibraryRestoreCoordinatorClock = {
  now: () => Date.now(),
  setInterval: (callback, delay) => globalThis.setInterval(callback, delay),
  clearInterval: handle => globalThis.clearInterval(handle as number),
};

let fallbackRequestSequence = 0;
function defaultRequestId() {
  if (globalThis.crypto?.randomUUID) return globalThis.crypto.randomUUID();
  fallbackRequestSequence += 1;
  return `library-restore-${Date.now()}-${fallbackRequestSequence}`;
}

function errorMessage(cause: unknown) {
  if (cause && typeof cause === 'object' && 'message' in cause && typeof cause.message === 'string') {
    return cause.message;
  }
  return cause instanceof Error ? cause.message : String(cause);
}

function validProgress(value: unknown): value is BackupProgress {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return false;
  const progress = value as Record<string, unknown>;
  return typeof progress.requestId === 'string'
    && typeof progress.libraryPath === 'string'
    && typeof progress.phase === 'string'
    && Number.isSafeInteger(progress.checked) && (progress.checked as number) >= 0
    && Number.isSafeInteger(progress.total) && (progress.total as number) >= 0;
}

type HostRequest = {
  operation: LibraryRestoreOperation;
  requestId: string;
  libraryPath: string;
  generation: number;
  startedAt: number;
  cancellable: boolean;
  hostSettled: boolean;
  cancelSettlement: Promise<void> | null;
  cancelError: string | null;
};

const initialState: LibraryRestoreCoordinatorState = {
  libraryPath: null,
  selectedBackupId: null,
  preview: null,
  pending: null,
  allowed: false,
  operation: null,
  requestId: null,
  progress: null,
  startedAt: null,
  elapsedSeconds: 0,
  error: null,
  notice: null,
  receipt: null,
};

export function createLibraryRestoreCoordinator(options: LibraryRestoreCoordinatorOptions) {
  const clock = options.clock ?? defaultClock;
  const requestId = options.createRequestId ?? defaultRequestId;
  let state = { ...initialState };
  let mounted = true;
  let generation = 0;
  let selectionGeneration = 0;
  let activeRequest: HostRequest | null = null;
  let elapsedTimer: unknown = null;

  const emit = () => options.onState?.({ ...state });
  const update = (changes: Partial<LibraryRestoreCoordinatorState>) => {
    state = { ...state, ...changes };
    emit();
  };
  const clearElapsedTimer = () => {
    if (elapsedTimer === null) return;
    clock.clearInterval(elapsedTimer);
    elapsedTimer = null;
  };
  const updateElapsed = (job: HostRequest) => {
    if (activeRequest !== job || !mounted || job.generation !== generation) return;
    update({ elapsedSeconds: Math.max(0, Math.floor((clock.now() - job.startedAt) / 1_000)) });
  };
  const isCurrent = (expectedGeneration: number, expectedPath: string) =>
    mounted && generation === expectedGeneration && state.libraryPath === expectedPath;

  const runHostRequest = async <T>(
    operation: LibraryRestoreOperation,
    expectedPath: string,
    expectedGeneration: number,
    cancellable: boolean,
    invokeHost: (id: string) => Promise<T>,
    onHostResult?: (result: T) => void,
  ): Promise<T> => {
    if (activeRequest !== null || !isCurrent(expectedGeneration, expectedPath)) {
      throw new Error('笔记库已切换或宿主操作仍在结束，暂时无法启动整库恢复请求。');
    }
    const job: HostRequest = {
      operation,
      requestId: requestId(),
      libraryPath: expectedPath,
      generation: expectedGeneration,
      startedAt: clock.now(),
      cancellable,
      hostSettled: false,
      cancelSettlement: null,
      cancelError: null,
    };
    activeRequest = job;
    clearElapsedTimer();
    elapsedTimer = clock.setInterval(() => updateElapsed(job), 1_000);
    update({
      operation,
      requestId: job.requestId,
      progress: null,
      startedAt: job.startedAt,
      elapsedSeconds: 0,
      error: null,
      notice: null,
    });

    let result: T | undefined;
    let failure: unknown;
    let failed = false;
    try {
      result = await invokeHost(job.requestId);
      job.hostSettled = true;
      onHostResult?.(result);
    } catch (cause) {
      job.hostSettled = true;
      failed = true;
      failure = cause;
    }

    const cancelSettlement = job.cancelSettlement;
    if (cancelSettlement) await cancelSettlement;
    clearElapsedTimer();
    if (activeRequest === job) activeRequest = null;
    if (state.requestId === job.requestId) {
      update({ operation: null, requestId: null, progress: null, startedAt: null, elapsedSeconds: 0 });
    }
    if (failed) throw failure;
    if (job.cancelError && isCurrent(expectedGeneration, expectedPath)) {
      update({ notice: `恢复操作已返回；暂停请求未获确认：${job.cancelError}` });
    }
    return result as T;
  };

  const refreshAfterHostError = async (cause: unknown, expectedPath: string, expectedGeneration: number) => {
    const originalMessage = errorMessage(cause);
    if (!isCurrent(expectedGeneration, expectedPath)) return;
    update({ error: `整库恢复操作返回错误：${originalMessage}`, notice: '正在读取宿主中的恢复状态…' });
    try {
      const view = await runHostRequest(
        'refreshing',
        expectedPath,
        expectedGeneration,
        false,
        () => options.api.get(),
      );
      if (!isCurrent(expectedGeneration, expectedPath)) return;
      const viewPath = view.libraryRestore?.libraryPath ?? view.library?.path;
      if (viewPath === expectedPath) {
        update({
          pending: view.libraryRestore,
          error: `整库恢复操作返回错误：${originalMessage}`,
          notice: view.libraryRestore
            ? '已重新读取宿主状态；请按显示的继续或回退选项处理。'
            : '已重新读取宿主状态；当前笔记库没有待处理的整库恢复。',
        });
        try {
          options.onWorkspace?.(view);
        } catch (callbackError) {
          update({ notice: `宿主状态已读取，但界面更新回调异常：${errorMessage(callbackError)}` });
        }
      } else {
        update({
          error: `整库恢复操作返回错误：${originalMessage}`,
          notice: '宿主已返回状态，但笔记库位置不匹配；请重新选择当前笔记库后查看。',
        });
      }
    } catch (refreshError) {
      if (!isCurrent(expectedGeneration, expectedPath)) return;
      update({
        error: `整库恢复操作返回错误：${originalMessage}；读取宿主状态也失败：${errorMessage(refreshError)}`,
        notice: null,
      });
    }
  };

  const acceptReceipt = (receipt: LibraryRestoreReceipt, expectedPath: string, expectedGeneration: number) => {
    if (!isCurrent(expectedGeneration, expectedPath)) return;
    update({
      pending: null,
      preview: null,
      receipt,
      error: null,
      notice: receipt.message,
      operation: null,
      requestId: null,
      progress: null,
      startedAt: null,
      elapsedSeconds: 0,
    });
  };

  const runRestore = async (
    execute: (expectedPath: string, onReceipt: (receipt: LibraryRestoreReceipt) => void) =>
      Promise<LibraryRestoreReceipt>,
    runWithSavedEditor: RunWithSavedEditor,
    resume: boolean,
    expectedPath: string,
    expectedGeneration: number,
  ): Promise<boolean> => {
    let receiptFromHost: LibraryRestoreReceipt | null = null;
    let hostWasStarted = false;
    update({ operation: 'preparing', error: null, notice: null, receipt: null });
    try {
      const returnedReceipt = await runWithSavedEditor(async () => {
        if (!isCurrent(expectedGeneration, expectedPath) || activeRequest !== null) {
          throw new Error('笔记库已切换或宿主操作仍在结束，整库恢复未启动。');
        }
        hostWasStarted = true;
        const receipt = await execute(expectedPath, value => { receiptFromHost = value; });
        receiptFromHost = receipt;
        return receipt;
      }, resume);

      if (returnedReceipt) receiptFromHost = returnedReceipt;
      if (receiptFromHost) {
        acceptReceipt(receiptFromHost, expectedPath, expectedGeneration);
        return true;
      }
      if (hostWasStarted) {
        await refreshAfterHostError(new Error('恢复保护流程未返回完成回执。'), expectedPath, expectedGeneration);
      } else if (isCurrent(expectedGeneration, expectedPath)) {
        update({
          operation: null,
          notice: '当前编辑未能确认保存，整库恢复没有启动。',
          error: null,
        });
      }
      return false;
    } catch (cause) {
      if (receiptFromHost) {
        acceptReceipt(receiptFromHost, expectedPath, expectedGeneration);
        if (isCurrent(expectedGeneration, expectedPath)) {
          try {
            options.onWorkspace?.(receiptFromHost.workspace);
          } catch {
            update({ notice: `${receiptFromHost.message} 界面状态更新异常；宿主已返回完成回执。` });
          }
        }
        return true;
      }
      if (hostWasStarted) await refreshAfterHostError(cause, expectedPath, expectedGeneration);
      else if (isCurrent(expectedGeneration, expectedPath)) {
        update({ operation: null, error: errorMessage(cause), notice: '编辑器保护流程未完成，整库恢复没有启动。' });
      }
      return false;
    }
  };

  const synchronize = (input: LibraryRestoreCoordinatorInput) => {
    if (!mounted) return;
    const pathChanged = input.libraryPath !== state.libraryPath;
    const backupChanged = input.backupId !== state.selectedBackupId;
    const pendingStarted = input.pending !== null && input.pending.restoreId !== state.pending?.restoreId;
    if (pathChanged || backupChanged || pendingStarted) {
      generation += pathChanged ? 1 : 0;
      selectionGeneration += 1;
      update({
        libraryPath: input.libraryPath,
        selectedBackupId: input.backupId,
        preview: null,
        receipt: null,
        error: null,
        notice: null,
      });
    }
    update({ pending: input.pending, allowed: input.allowed });
  };

  const previewBackup = async (backupId: string): Promise<boolean> => {
    const expectedPath = state.libraryPath;
    if (!mounted || expectedPath === null || state.pending !== null || !state.allowed
      || activeRequest !== null || state.operation !== null) return false;
    if (state.selectedBackupId !== backupId) {
      selectionGeneration += 1;
      update({ selectedBackupId: backupId, preview: null, error: null, notice: null });
    }
    const selectedGeneration = selectionGeneration;
    const expectedGeneration = generation;
    const isSelectionCurrent = () => isCurrent(expectedGeneration, expectedPath)
      && selectionGeneration === selectedGeneration && state.selectedBackupId === backupId;
    try {
      const result = await runHostRequest(
        'previewing',
        expectedPath,
        expectedGeneration,
        false,
        id => options.api.previewLibraryRestore(backupId, id, expectedPath),
      );
      if (!isSelectionCurrent()) return false;
      if (result.backup.backupId !== backupId || !result.manifestRevision) {
        throw new Error('备份预校验返回的记录与所选备份不一致。');
      }
      update({ preview: result, error: null, notice: null });
      return true;
    } catch (cause) {
      if (isSelectionCurrent()) update({ preview: null, error: errorMessage(cause), notice: null });
      return false;
    }
  };

  const startRestore = async (runWithSavedEditor: RunWithSavedEditor): Promise<boolean> => {
    const expectedPath = state.libraryPath;
    const selectedBackupId = state.selectedBackupId;
    const selectedPreview = state.preview;
    if (!mounted || expectedPath === null || selectedBackupId === null || selectedPreview === null
      || selectedPreview.backup.backupId !== selectedBackupId || state.pending !== null || !state.allowed
      || activeRequest !== null || state.operation !== null) return false;
    const expectedGeneration = generation;
    return runRestore(
      async (path, captureReceipt) => runHostRequest(
        'restoring',
        path,
        expectedGeneration,
        true,
        id => options.api.restoreLibraryFromBackup(
          selectedBackupId,
          selectedPreview.manifestRevision,
          id,
          path,
        ),
        captureReceipt,
      ),
      runWithSavedEditor,
      false,
      expectedPath,
      expectedGeneration,
    );
  };

  const resumePending = async (
    action: LibraryRestoreAction,
    runWithSavedEditor: RunWithSavedEditor,
  ): Promise<boolean> => {
    const expectedPath = state.libraryPath;
    const pending = state.pending;
    if (!mounted || expectedPath === null || pending === null || pending.libraryPath !== expectedPath
      || !state.allowed || activeRequest !== null || state.operation !== null
      || (action === 'continue' ? !pending.canContinue : !pending.canRollBack)) return false;
    const expectedGeneration = generation;
    return runRestore(
      async (path, captureReceipt) => runHostRequest(
        pending.phase === 'rollingBack' && action === 'continue' ? 'resuming' : 'resuming',
        path,
        expectedGeneration,
        true,
        id => options.api.resumeLibraryRestore(action, id, path),
        captureReceipt,
      ),
      runWithSavedEditor,
      true,
      expectedPath,
      expectedGeneration,
    );
  };

  const cancel = (): boolean => {
    const job = activeRequest;
    if (!job || !job.cancellable || job.hostSettled || job.cancelSettlement !== null) return false;
    job.cancelSettlement = Promise.resolve().then(() => options.api.cancelBackup()).then(() => undefined).catch(cause => {
      job.cancelError = errorMessage(cause);
    }).finally(() => {
      if (activeRequest === job && !job.hostSettled) {
        const error = job.cancelError;
        job.cancelSettlement = null;
        if (error && isCurrent(job.generation, job.libraryPath)) {
          update({
            operation: job.operation,
            error,
            notice: '暂停请求未获确认；宿主恢复仍在处理中，可以再次请求暂停。',
          });
        } else if (isCurrent(job.generation, job.libraryPath)) {
          update({
            operation: 'cancelling',
            notice: '宿主已收到暂停请求；当前步骤完成后会显示继续或回退选项。',
          });
        }
      }
    });
    update({
      operation: 'cancelling',
      error: null,
      notice: '正在请求在安全边界暂停；这不会立即回滚已完成的文件。',
    });
    return true;
  };

  const handleProgress = (value: unknown) => {
    const job = activeRequest;
    if (!job || job.hostSettled || !validProgress(value)
      || value.requestId !== job.requestId || value.libraryPath !== job.libraryPath
      || !isCurrent(job.generation, job.libraryPath)) return;
    update({ progress: { ...value } });
  };

  const activate = () => { mounted = true; };
  const deactivate = () => {
    mounted = false;
    generation += 1;
    selectionGeneration += 1;
    clearElapsedTimer();
  };

  return {
    getState: () => ({ ...state }),
    activate,
    deactivate,
    synchronize,
    previewBackup,
    startRestore,
    resumePending,
    cancel,
    handleProgress,
  };
}
