import type {
  BackupCatalog,
  BackupOutcome,
  BackupProgress,
  BackupSummary,
  LibraryView,
} from './workspace-api';

export const BACKUP_CHANGE_DEBOUNCE_MS = 5_000;
export const BACKUP_FALLBACK_INTERVAL_MS = 60_000;

export type BackupOperation = 'preparing' | 'automatic' | 'manual' | 'listing' | 'cancelling' | 'choosing';

export interface BackupCoordinatorState {
  libraryPath: string | null;
  catalog: BackupCatalog | null;
  catalogLoading: boolean;
  operation: BackupOperation | null;
  progress: BackupProgress | null;
  startedAt: number | null;
  elapsedSeconds: number;
  error: string | null;
  notice: string | null;
}

export interface BackupCoordinatorApi {
  runBackup: (kind: 'automatic' | 'manual', requestId: string, expectedLibraryPath: string) => Promise<BackupOutcome>;
  listBackups: (requestId: string, expectedLibraryPath: string) => Promise<BackupCatalog>;
  chooseBackupDirectory: (requestId: string, expectedLibraryPath: string) => Promise<BackupCatalog | null>;
  cancelBackup: () => Promise<void>;
}

export interface BackupCoordinatorClock {
  now: () => number;
  setTimeout: (callback: () => void, delay: number) => unknown;
  clearTimeout: (handle: unknown) => void;
  setInterval: (callback: () => void, delay: number) => unknown;
  clearInterval: (handle: unknown) => void;
}

export interface BackupCoordinatorOptions {
  api: BackupCoordinatorApi;
  onState?: (state: BackupCoordinatorState) => void;
  clock?: BackupCoordinatorClock;
  createRequestId?: () => string;
}

export interface BackupCoordinatorInput {
  libraryPath: string | null;
  changeToken: unknown;
  automaticAllowed: boolean;
}

const defaultClock: BackupCoordinatorClock = {
  now: () => Date.now(),
  setTimeout: (callback, delay) => globalThis.setTimeout(callback, delay),
  clearTimeout: handle => globalThis.clearTimeout(handle as number),
  setInterval: (callback, delay) => globalThis.setInterval(callback, delay),
  clearInterval: handle => globalThis.clearInterval(handle as number),
};

let fallbackRequestSequence = 0;
function defaultRequestId() {
  if (globalThis.crypto?.randomUUID) return globalThis.crypto.randomUUID();
  fallbackRequestSequence += 1;
  return `backup-${Date.now()}-${fallbackRequestSequence}`;
}

function errorMessage(cause: unknown) {
  if (cause && typeof cause === 'object' && 'message' in cause && typeof cause.message === 'string') {
    return cause.message;
  }
  return cause instanceof Error ? cause.message : String(cause);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isBackupCancelled(cause: unknown) {
  return isRecord(cause) && cause.kind === 'backupCancelled';
}

function validateBackupSummary(value: unknown): BackupSummary {
  if (!isRecord(value)
    || typeof value.backupId !== 'string'
    || (value.kind !== 'automatic' && value.kind !== 'manual')
    || typeof value.createdAt !== 'string'
    || typeof value.localDate !== 'string'
    || typeof value.path !== 'string'
    || !Number.isSafeInteger(value.noteCount) || (value.noteCount as number) < 0
    || !Number.isSafeInteger(value.notebookCount) || (value.notebookCount as number) < 0) {
    throw new Error('备份列表返回了无效的备份记录。');
  }
  return {
    backupId: value.backupId,
    kind: value.kind,
    createdAt: value.createdAt,
    localDate: value.localDate,
    path: value.path,
    noteCount: value.noteCount as number,
    notebookCount: value.notebookCount as number,
  };
}

export function validateBackupCatalog(value: unknown, expectedLibraryPath: string): BackupCatalog {
  if (!isRecord(value)
    || value.libraryPath !== expectedLibraryPath
    || typeof value.directory !== 'string'
    || !Array.isArray(value.backups)
    || !Array.isArray(value.warnings)
    || !value.warnings.every(warning => typeof warning === 'string')) {
    throw new Error('备份列表与当前笔记库不一致，已拒绝显示。');
  }
  return {
    libraryPath: value.libraryPath,
    directory: value.directory,
    backups: value.backups.map(validateBackupSummary),
    warnings: [...value.warnings],
  };
}

function validateBackupOutcome(value: unknown, expectedLibraryPath: string): BackupOutcome {
  if (!isRecord(value) || typeof value.message !== 'string') {
    throw new Error('备份操作返回了无效结果。');
  }
  return {
    catalog: validateBackupCatalog(value.catalog, expectedLibraryPath),
    created: value.created === null ? null : validateBackupSummary(value.created),
    message: value.message,
  };
}

function libraryChangeKey(value: unknown): unknown {
  if (!isRecord(value) || typeof value.path !== 'string' || !Array.isArray(value.notes)) return value;
  const library = value as unknown as LibraryView;
  const noteKey = (notes: LibraryView['notes']) => notes.map(note => [
    note.noteId,
    note.title,
    note.modifiedAt,
    note.notebookId,
    note.pinned,
    note.trashedAt,
    note.excerpt,
  ]);
  return JSON.stringify([
    library.path,
    library.defaultNotebookId,
    library.defaultNotebookName,
    library.notebooks.map(notebook => [notebook.notebookId, notebook.name, notebook.isDefault]),
    noteKey(library.notes),
    noteKey(library.trashNotes),
  ]);
}

function validProgress(value: unknown): value is BackupProgress {
  if (!isRecord(value)
    || typeof value.requestId !== 'string'
    || typeof value.libraryPath !== 'string'
    || typeof value.phase !== 'string'
    || !Number.isSafeInteger(value.checked) || (value.checked as number) < 0
    || !Number.isSafeInteger(value.total) || (value.total as number) < 0) return false;
  return true;
}

type ActiveJob = {
  kind: 'automatic' | 'manual';
  requestId: string;
  libraryPath: string;
  generation: number;
  startedAt: number;
  cancellationRequested: boolean;
  cancellationRequestInFlight: boolean;
  cancellationAcknowledged: boolean;
  cancellationSettlement: Promise<void> | null;
  suppressedChange: boolean;
};

type ManualPreparation = {
  requestId: string;
  libraryPath: string;
  generation: number;
  suppressedChange: boolean;
};

type PendingPicker = { libraryPath: string; generation: number; requestId: string };
type CatalogRequest = {
  libraryPath: string;
  generation: number;
  sequence: number;
  requestId: string;
  startedAt: number;
  cancellationRequested: boolean;
  cancellationRequestInFlight: boolean;
  cancellationAcknowledged: boolean;
  cancellationSettlement: Promise<void> | null;
};
type CancelReason = 'user' | 'library-change' | 'unmount';

const idleState = (libraryPath: string | null): BackupCoordinatorState => ({
  libraryPath,
  catalog: null,
  catalogLoading: false,
  operation: null,
  progress: null,
  startedAt: null,
  elapsedSeconds: 0,
  error: null,
  notice: null,
});

export function createBackupCoordinator(options: BackupCoordinatorOptions) {
  const clock = options.clock ?? defaultClock;
  const requestId = options.createRequestId ?? defaultRequestId;
  let state = idleState(null);
  let mounted = false;
  let libraryPath: string | null = null;
  let generation = 0;
  let lastChangeKey: unknown;
  let hasChangeKey = false;
  let automaticAllowed = false;
  let pendingAutomatic = false;
  let automaticNotBefore = 0;
  let automaticTimer: unknown = null;
  let fallbackTimer: unknown = null;
  let elapsedTimer: unknown = null;
  let activeJob: ActiveJob | null = null;
  let catalogInFlight: CatalogRequest | null = null;
  let pendingCatalogRefresh = false;
  let manualPreparation: ManualPreparation | null = null;
  let pendingPicker: PendingPicker | null = null;
  let catalogRequest = 0;

  const emit = () => {
    if (mounted) options.onState?.({ ...state });
  };
  const update = (patch: Partial<BackupCoordinatorState>) => {
    state = { ...state, ...patch };
    emit();
  };
  const clearAutomaticTimer = () => {
    if (automaticTimer === null) return;
    clock.clearTimeout(automaticTimer);
    automaticTimer = null;
  };
  const clearFallbackTimer = () => {
    if (fallbackTimer === null) return;
    clock.clearInterval(fallbackTimer);
    fallbackTimer = null;
  };
  const clearElapsedTimer = () => {
    if (elapsedTimer === null) return;
    clock.clearInterval(elapsedTimer);
    elapsedTimer = null;
  };
  const canRunAutomatic = () => mounted
    && automaticAllowed
    && libraryPath !== null
    && activeJob === null
    && catalogInFlight === null
    && manualPreparation === null
    && pendingPicker === null;

  const canScheduleAutomatic = () => mounted
    && automaticAllowed
    && libraryPath !== null
    && manualPreparation === null
    && activeJob?.kind !== 'manual';

  const scheduleAutomatic = (resetDebounce: boolean) => {
    if (!pendingAutomatic) return;
    if (!canScheduleAutomatic()) {
      clearAutomaticTimer();
      return;
    }
    if (resetDebounce) clearAutomaticTimer();
    if (automaticTimer !== null) return;
    const debounceDelay = resetDebounce ? BACKUP_CHANGE_DEBOUNCE_MS : 0;
    const retryDelay = Math.max(0, automaticNotBefore - clock.now());
    automaticTimer = clock.setTimeout(() => {
      automaticTimer = null;
      if (!pendingAutomatic) return;
      if (!canRunAutomatic()) return;
      pendingAutomatic = false;
      void runBackup('automatic');
    }, Math.max(debounceDelay, retryDelay));
  };

  const startFallbackTimer = () => {
    clearFallbackTimer();
    if (!mounted || libraryPath === null) return;
    fallbackTimer = clock.setInterval(() => {
      if (manualPreparation !== null || activeJob?.kind === 'manual') return;
      if (activeJob?.kind === 'automatic') return;
      pendingAutomatic = true;
      scheduleAutomatic(false);
    }, BACKUP_FALLBACK_INTERVAL_MS);
  };

  const isCurrentJob = (job: ActiveJob) => mounted
    && activeJob === job
    && libraryPath === job.libraryPath
    && generation === job.generation;

  const updateElapsed = (job: ActiveJob) => {
    if (!isCurrentJob(job)) return;
    update({ elapsedSeconds: Math.max(0, Math.floor((clock.now() - job.startedAt) / 1_000)) });
  };

  const isCurrentCatalogRequest = (request: CatalogRequest) => mounted
    && catalogInFlight === request
    && libraryPath === request.libraryPath
    && generation === request.generation
    && catalogRequest === request.sequence;

  const updateCatalogElapsed = (request: CatalogRequest) => {
    if (!isCurrentCatalogRequest(request)) return;
    update({ elapsedSeconds: Math.max(0, Math.floor((clock.now() - request.startedAt) / 1_000)) });
  };

  const runBackup = async (kind: 'automatic' | 'manual'): Promise<boolean> => {
    const expectedPath = libraryPath;
    if (!mounted || expectedPath === null || activeJob !== null || catalogInFlight !== null
      || pendingPicker !== null || manualPreparation !== null) return false;
    const job: ActiveJob = {
      kind,
      requestId: requestId(),
      libraryPath: expectedPath,
      generation,
      startedAt: clock.now(),
      cancellationRequested: false,
      cancellationRequestInFlight: false,
      cancellationAcknowledged: false,
      cancellationSettlement: null,
      suppressedChange: false,
    };
    activeJob = job;
    if (kind === 'manual') {
      pendingAutomatic = false;
      clearAutomaticTimer();
      automaticNotBefore = 0;
    }
    update({
      operation: kind,
      progress: null,
      startedAt: job.startedAt,
      elapsedSeconds: 0,
      error: null,
      notice: null,
    });
    elapsedTimer = clock.setInterval(() => updateElapsed(job), 1_000);

    try {
      const outcome = validateBackupOutcome(
        await options.api.runBackup(kind, job.requestId, job.libraryPath),
        job.libraryPath,
      );
      if (job.cancellationSettlement) await job.cancellationSettlement;
      if (!isCurrentJob(job)) return false;
      update({ catalog: outcome.catalog, catalogLoading: false, error: null, notice: outcome.message });
      return true;
    } catch (cause) {
      if (job.cancellationSettlement) await job.cancellationSettlement;
      if (!isCurrentJob(job)) return false;
      const message = errorMessage(cause);
      if (job.cancellationAcknowledged && isBackupCancelled(cause)) {
        update({ error: null, notice: '备份已取消。' });
      } else if (kind === 'automatic') {
        pendingAutomatic = true;
        automaticNotBefore = clock.now() + BACKUP_FALLBACK_INTERVAL_MS;
        update({ error: message, notice: `自动备份失败：${message}。稍后会重试。` });
      } else {
        update({ error: message, notice: `手动备份失败：${message}` });
      }
      return false;
    } finally {
      // Cancellation is a global host command. Both the old operation and its
      // cancel request must settle before a new operation can own this slot.
      if (job.cancellationSettlement) await job.cancellationSettlement;
      if (activeJob === job) {
        clearElapsedTimer();
        activeJob = null;
        const stillCurrentLibrary = mounted && libraryPath === job.libraryPath && generation === job.generation;
        if (stillCurrentLibrary) {
          update({ operation: null, progress: null, startedAt: null, elapsedSeconds: 0 });
        }
        if (mounted && pendingCatalogRefresh) {
          pendingCatalogRefresh = false;
          void refresh();
        }
        if (mounted && kind === 'automatic') scheduleAutomatic(false);
        if (mounted && kind === 'manual') automaticNotBefore = 0;
      }
    }
  };

  const cancelJob = (job: ActiveJob, reason: CancelReason) => {
    if (job.cancellationRequestInFlight) return;
    if (job.cancellationRequested && reason !== 'user') return;
    job.cancellationRequested = true;
    job.cancellationRequestInFlight = true;
    if (reason === 'user' && mounted && libraryPath === job.libraryPath && generation === job.generation) {
      pendingAutomatic = false;
      clearAutomaticTimer();
      update({ operation: 'cancelling', progress: null, error: null, notice: null });
    }
    job.cancellationSettlement = options.api.cancelBackup().then(() => {
      job.cancellationRequestInFlight = false;
      job.cancellationAcknowledged = true;
    }).catch(cause => {
      job.cancellationRequestInFlight = false;
      job.cancellationRequested = false;
      if (activeJob === job && mounted && libraryPath === job.libraryPath && generation === job.generation) {
        update({
          operation: job.kind,
          error: errorMessage(cause),
          notice: '取消请求未获确认，备份任务仍在处理中。',
        });
      }
    });
  };

  const cancelCatalogRequest = (request: CatalogRequest, reason: CancelReason) => {
    if (request.cancellationRequestInFlight) return;
    if (request.cancellationRequested && reason !== 'user') return;
    request.cancellationRequested = true;
    request.cancellationRequestInFlight = true;
    if (reason === 'user' && isCurrentCatalogRequest(request)) {
      pendingAutomatic = false;
      clearAutomaticTimer();
      update({ operation: 'cancelling', error: null, notice: null });
    }
    request.cancellationSettlement = options.api.cancelBackup().then(() => {
      request.cancellationRequestInFlight = false;
      request.cancellationAcknowledged = true;
    }).catch(cause => {
      request.cancellationRequestInFlight = false;
      request.cancellationRequested = false;
      if (isCurrentCatalogRequest(request)) {
        update({
          operation: 'listing',
          error: errorMessage(cause),
          notice: '取消请求未获确认，备份列表仍在校验。',
        });
      }
    });
  };

  const refresh = async (): Promise<boolean> => {
    const expectedPath = libraryPath;
    if (!mounted || expectedPath === null) return false;
    if (activeJob !== null || pendingPicker !== null || manualPreparation !== null) {
      pendingCatalogRefresh = true;
      update({ catalogLoading: true });
      return false;
    }
    if (catalogInFlight !== null) {
      if (catalogInFlight.libraryPath !== expectedPath || catalogInFlight.generation !== generation) {
        pendingCatalogRefresh = true;
        update({ catalogLoading: true });
      }
      return false;
    }
    const requestGeneration = generation;
    const request: CatalogRequest = {
      libraryPath: expectedPath,
      generation: requestGeneration,
      sequence: ++catalogRequest,
      requestId: requestId(),
      startedAt: clock.now(),
      cancellationRequested: false,
      cancellationRequestInFlight: false,
      cancellationAcknowledged: false,
      cancellationSettlement: null,
    };
    catalogInFlight = request;
    update({
      catalogLoading: true,
      operation: 'listing',
      progress: null,
      startedAt: request.startedAt,
      elapsedSeconds: 0,
      error: null,
      notice: null,
    });
    elapsedTimer = clock.setInterval(() => updateCatalogElapsed(request), 1_000);
    try {
      const catalog = validateBackupCatalog(
        await options.api.listBackups(request.requestId, expectedPath),
        expectedPath,
      );
      if (request.cancellationSettlement) await request.cancellationSettlement;
      if (!isCurrentCatalogRequest(request)) return false;
      update({ catalog, catalogLoading: false, error: null, notice: null });
      return true;
    } catch (cause) {
      if (request.cancellationSettlement) await request.cancellationSettlement;
      if (!isCurrentCatalogRequest(request)) return false;
      if (request.cancellationAcknowledged && isBackupCancelled(cause)) {
        update({ catalogLoading: false, error: null, notice: '备份列表检查已取消。' });
      } else {
        update({ catalogLoading: false, error: errorMessage(cause), notice: null });
      }
      return false;
    } finally {
      if (request.cancellationSettlement) await request.cancellationSettlement;
      if (catalogInFlight === request) {
        clearElapsedTimer();
        catalogInFlight = null;
        if (mounted && generation === request.generation && libraryPath === request.libraryPath) {
          update({ operation: null, progress: null, startedAt: null, elapsedSeconds: 0 });
        }
      }
      if (mounted && pendingCatalogRefresh) {
        pendingCatalogRefresh = false;
        void refresh();
      } else if (mounted) {
        scheduleAutomatic(false);
      }
    }
  };

  const changeDirectory = async (): Promise<boolean> => {
    const expectedPath = libraryPath;
    if (!mounted || expectedPath === null || activeJob !== null || catalogInFlight !== null
      || manualPreparation !== null || pendingPicker !== null) return false;
    const picker: PendingPicker = { libraryPath: expectedPath, generation, requestId: requestId() };
    pendingPicker = picker;
    update({ operation: 'choosing', error: null, notice: null });
    try {
      const result = await options.api.chooseBackupDirectory(picker.requestId, expectedPath);
      if (!mounted || pendingPicker !== picker || generation !== picker.generation || libraryPath !== expectedPath) return false;
      if (result !== null) {
        const catalog = validateBackupCatalog(result, expectedPath);
        update({ catalog, notice: '备份位置已更新，原目录的备份仍保留；切回原目录可查看。', error: null });
      }
      return true;
    } catch (cause) {
      if (mounted && pendingPicker === picker && generation === picker.generation && libraryPath === expectedPath) {
        update({ error: errorMessage(cause) });
      }
      return false;
    } finally {
      if (pendingPicker === picker) {
        pendingPicker = null;
        if (mounted && generation === picker.generation && libraryPath === expectedPath) update({ operation: null });
        if (mounted && pendingCatalogRefresh) {
          pendingCatalogRefresh = false;
          void refresh();
        } else if (mounted) scheduleAutomatic(false);
      }
    }
  };

  const createManualBackup = async (beforeBackup: () => Promise<boolean>): Promise<boolean> => {
    const expectedPath = libraryPath;
    if (!mounted || expectedPath === null || activeJob !== null || catalogInFlight !== null
      || pendingPicker !== null || manualPreparation !== null) return false;
    const preparation: ManualPreparation = {
      requestId: requestId(),
      libraryPath: expectedPath,
      generation,
      suppressedChange: false,
    };
    manualPreparation = preparation;
    pendingAutomatic = false;
    clearAutomaticTimer();
    update({ operation: 'preparing', progress: null, error: null, notice: '正在保存当前编辑，然后创建备份。' });
    let ready = false;
    try {
      ready = await beforeBackup();
    } catch (cause) {
      if (manualPreparation !== preparation) return false;
      ready = false;
      update({ error: errorMessage(cause) });
    }
    if (manualPreparation !== preparation) return false;
    manualPreparation = null;
    if (!mounted || generation !== preparation.generation || libraryPath !== expectedPath) return false;
    if (!ready) {
      if (preparation.suppressedChange) pendingAutomatic = true;
      update({
        operation: null,
        startedAt: null,
        elapsedSeconds: 0,
        notice: '当前编辑未能确认保存，已跳过备份。',
      });
      if (pendingAutomatic && automaticAllowed) scheduleAutomatic(false);
      return false;
    }
    return runBackup('manual');
  };

  const synchronize = (input: BackupCoordinatorInput) => {
    if (!mounted) return;
    const nextKey = libraryChangeKey(input.changeToken);
    const changedLibrary = input.libraryPath !== libraryPath;
    const previousAllowed = automaticAllowed;
    automaticAllowed = input.automaticAllowed;

    if (changedLibrary) {
      const oldJob = activeJob;
      const oldCatalogRequest = catalogInFlight;
      generation += 1;
      catalogRequest += 1;
      clearAutomaticTimer();
      clearFallbackTimer();
      libraryPath = input.libraryPath;
      lastChangeKey = nextKey;
      hasChangeKey = true;
      pendingAutomatic = libraryPath !== null;
      automaticNotBefore = 0;
      manualPreparation = null;
      state = idleState(libraryPath);
      emit();
      if (oldJob) cancelJob(oldJob, 'library-change');
      if (oldCatalogRequest) cancelCatalogRequest(oldCatalogRequest, 'library-change');
      if (libraryPath !== null) {
        startFallbackTimer();
        void refresh();
        scheduleAutomatic(true);
      }
      return;
    }

    const changed = !hasChangeKey || !Object.is(lastChangeKey, nextKey);
    lastChangeKey = nextKey;
    hasChangeKey = true;
    if (changed) {
      if (manualPreparation) manualPreparation.suppressedChange = true;
      if (activeJob?.kind === 'manual') activeJob.suppressedChange = true;
      if (manualPreparation === null && activeJob?.kind !== 'manual') {
        pendingAutomatic = libraryPath !== null;
      }
    }
    if (!automaticAllowed) {
      clearAutomaticTimer();
      return;
    }
    if (changed) scheduleAutomatic(true);
    else if (!previousAllowed && pendingAutomatic) scheduleAutomatic(true);
  };

  const handleProgress = (value: unknown) => {
    if (!validProgress(value)) return;
    const job = activeJob;
    if (job && !job.cancellationRequested && isCurrentJob(job)
      && value.requestId === job.requestId && value.libraryPath === job.libraryPath) {
      update({ progress: { ...value } });
      return;
    }
    const request = catalogInFlight;
    if (!request || request.cancellationRequested || !isCurrentCatalogRequest(request)
      || value.requestId !== request.requestId || value.libraryPath !== request.libraryPath) return;
    update({ progress: { ...value } });
  };

  const activate = () => {
    if (mounted) return;
    mounted = true;
    if (libraryPath !== null) startFallbackTimer();
    if (pendingAutomatic) scheduleAutomatic(false);
  };

  const deactivate = () => {
    if (!mounted) return;
    mounted = false;
    generation += 1;
    clearAutomaticTimer();
    clearFallbackTimer();
    clearElapsedTimer();
    manualPreparation = null;
    pendingPicker = null;
    const job = activeJob;
    const request = catalogInFlight;
    if (job) cancelJob(job, 'unmount');
    if (request) cancelCatalogRequest(request, 'unmount');
  };

  return {
    getState: () => ({ ...state }),
    activate,
    deactivate,
    synchronize,
    refresh,
    changeDirectory,
    createManualBackup,
    cancel: () => {
      if (activeJob) cancelJob(activeJob, 'user');
      else if (catalogInFlight && isCurrentCatalogRequest(catalogInFlight)) cancelCatalogRequest(catalogInFlight, 'user');
      else return false;
      return true;
    },
    handleProgress,
  };
}
