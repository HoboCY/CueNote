export type SavePhase = 'clean' | 'dirty' | 'saving' | 'saved' | 'failed' | 'conflict';

export interface SaveCoordinatorState {
  phase: SavePhase;
  sessionId: string | null;
  generation: number;
  savedGeneration: number;
  isDirty: boolean;
  error: unknown | null;
}

export interface SaveSnapshot<T> {
  sessionId: string;
  generation: number;
  value: T;
}

export interface SaveCoordinatorOptions<T, R = void> {
  save: (sessionId: string, snapshot: T) => Promise<R>;
  onSaved?: (snapshot: SaveSnapshot<T>, result: R) => void;
  onState?: (state: SaveCoordinatorState) => void;
  isConflict?: (error: unknown) => boolean;
  debounceMs?: number;
  setTimeout?: (callback: () => void, delay: number) => unknown;
  clearTimeout?: (handle: unknown) => void;
}

export interface LeaveIntent {
  result: Promise<boolean>;
  cancel: () => void;
}

type SaveOutcome = { ok: true; generation: number } | { ok: false; generation: number };

type InFlight = {
  sessionId: string;
  generation: number;
  promise: Promise<SaveOutcome>;
};

const defaultSetTimeout = (callback: () => void, delay: number) => globalThis.setTimeout(callback, delay);
const defaultClearTimeout = (handle: unknown) => globalThis.clearTimeout(handle as number);

export function createSaveCoordinator<T, R = void>(options: SaveCoordinatorOptions<T, R>) {
  const debounceMs = options.debounceMs ?? 1000;
  const setTimer = options.setTimeout ?? defaultSetTimeout;
  const clearTimer = options.clearTimeout ?? defaultClearTimeout;

  let sessionId: string | null = null;
  let latest: T | null = null;
  let hasLatest = false;
  let generation = 0;
  let savedGeneration = 0;
  let phase: SavePhase = 'clean';
  let error: unknown | null = null;
  let failedGeneration: number | null = null;
  let timer: unknown = null;
  let inFlight: InFlight | null = null;
  let flushPromise: Promise<boolean> | null = null;
  let automaticSaveRequested = false;
  let automaticSavingPaused = false;
  let disposed = false;

  const getState = (): SaveCoordinatorState => ({
    phase,
    sessionId,
    generation,
    savedGeneration,
    isDirty: hasLatest && generation > savedGeneration,
    error,
  });

  const emit = () => {
    options.onState?.(getState());
  };

  const setState = (nextPhase: SavePhase, nextError: unknown | null = error) => {
    phase = nextPhase;
    error = nextError;
    emit();
  };

  const cancelTimer = () => {
    if (timer === null) return;
    clearTimer(timer);
    timer = null;
  };

  let startSave: (version: number, targetSessionId: string, snapshot: T) => Promise<SaveOutcome>;

  const runAutomaticSave = () => {
    if (disposed || automaticSavingPaused || !sessionId || !hasLatest || generation <= savedGeneration) return;
    if (inFlight) {
      automaticSaveRequested = true;
      return;
    }
    if (failedGeneration === generation) return;
    void startSave(generation, sessionId, latest as T);
  };

  const scheduleAutomaticSave = () => {
    cancelTimer();
    if (automaticSavingPaused) return;
    timer = setTimer(() => {
      timer = null;
      runAutomaticSave();
    }, debounceMs);
  };

  startSave = (version, targetSessionId, snapshot) => {
    if (inFlight) return inFlight.promise;
    if (disposed) return Promise.resolve({ ok: false, generation: version });

    setState('saving');
    let promise: Promise<SaveOutcome>;
    promise = Promise.resolve()
      .then(() => options.save(targetSessionId, snapshot))
      .then((result) => {
        if (sessionId === targetSessionId && !disposed) {
          savedGeneration = Math.max(savedGeneration, version);
          try {
            options.onSaved?.({ sessionId: targetSessionId, generation: version, value: snapshot }, result);
          } catch {
            // A UI callback cannot turn a completed disk write into a failed save.
          }
          if (generation === version) {
            failedGeneration = null;
            setState('saved', null);
          } else if (error !== null) {
            setState(phase, error);
          } else {
            setState('dirty');
          }
        }
        return { ok: true, generation: version } as const;
      })
      .catch((cause: unknown) => {
        if (sessionId === targetSessionId && !disposed) {
          failedGeneration = version;
          const isConflict = options.isConflict?.(cause) ?? false;
          setState(isConflict ? 'conflict' : 'failed', cause);
        }
        return { ok: false, generation: version } as const;
      })
      .finally(() => {
        if (inFlight?.promise === promise) inFlight = null;
        if (!disposed && !automaticSavingPaused && automaticSaveRequested) {
          automaticSaveRequested = false;
          if (hasLatest && generation > savedGeneration && failedGeneration !== generation) {
            queueMicrotask(runAutomaticSave);
          }
        }
      });

    inFlight = { sessionId: targetSessionId, generation: version, promise };
    return promise;
  };

  const flushUntilCurrent = async (retryGeneration: number | null): Promise<boolean> => {
    let explicitRetryUsed = false;
    while (!disposed) {
      if (!sessionId || !hasLatest || generation <= savedGeneration) return true;

      if (inFlight) {
        const outcome = await inFlight.promise;
        if (!outcome.ok && outcome.generation === generation) return false;
        continue;
      }

      const targetGeneration = generation;
      if (failedGeneration === targetGeneration) {
        if (retryGeneration !== targetGeneration || explicitRetryUsed) return false;
        explicitRetryUsed = true;
      }

      const targetSessionId = sessionId;
      const snapshot = latest as T;
      const outcome = await startSave(targetGeneration, targetSessionId, snapshot);
      if (!outcome.ok && outcome.generation === generation) return false;
    }
    return false;
  };

  const flush = (): Promise<boolean> => {
    cancelTimer();
    if (disposed) return Promise.resolve(false);
    if (flushPromise) return flushPromise;

    const retryGeneration = !inFlight && failedGeneration === generation ? generation : null;
    const currentFlush = flushUntilCurrent(retryGeneration);
    let wrappedFlush: Promise<boolean>;
    wrappedFlush = currentFlush.finally(() => {
      if (flushPromise === wrappedFlush) flushPromise = null;
    });
    flushPromise = wrappedFlush;
    return wrappedFlush;
  };

  const reset = (nextSessionId: string | null, snapshot: T | null, initiallyDirty = false) => {
    cancelTimer();
    sessionId = nextSessionId;
    latest = snapshot;
    hasLatest = nextSessionId !== null && snapshot !== null;
    generation = initiallyDirty && nextSessionId !== null && snapshot !== null ? 1 : 0;
    savedGeneration = 0;
    failedGeneration = null;
    automaticSaveRequested = false;
    automaticSavingPaused = false;
    setState(!hasLatest ? 'clean' : initiallyDirty ? 'dirty' : 'saved', null);
  };

  const edit = (targetSessionId: string, snapshot: T): boolean => {
    if (disposed || !sessionId || targetSessionId !== sessionId) return false;
    latest = snapshot;
    hasLatest = true;
    generation += 1;
    if (phase !== 'failed' && phase !== 'conflict' && phase !== 'saving') setState('dirty');
    else emit();
    if (!automaticSavingPaused) scheduleAutomaticSave();
    return true;
  };

  const leave = (): LeaveIntent => {
    let cancelled = false;
    let settled = false;
    let settle!: (value: boolean) => void;
    const result = new Promise<boolean>((resolve) => { settle = resolve; });
    void flush().then((saved) => {
      if (settled) return;
      settled = true;
      settle(!cancelled && saved);
    });
    return {
      result,
      cancel: () => {
        if (settled) return;
        cancelled = true;
        settled = true;
        settle(false);
      },
    };
  };

  const dispose = () => {
    cancelTimer();
    disposed = true;
    automaticSaveRequested = false;
    automaticSavingPaused = true;
  };

  const resume = () => { disposed = false; };

  const pauseAutomaticSavingAndWait = async () => {
    automaticSavingPaused = true;
    cancelTimer();
    automaticSaveRequested = false;
    const activeWrite = inFlight?.promise;
    if (activeWrite) await activeWrite;
    cancelTimer();
    automaticSaveRequested = false;
  };

  const resumeAutomaticSaving = () => {
    automaticSavingPaused = false;
    if (hasLatest && generation > savedGeneration && failedGeneration !== generation) {
      scheduleAutomaticSave();
    }
  };

  return { reset, edit, flush, leave, getState, pauseAutomaticSavingAndWait, resumeAutomaticSaving, dispose, resume };
}
