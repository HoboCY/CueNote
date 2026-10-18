import type { ExternalChangeProbe } from './workspace-api';

export interface ExternalChangeToken {
  sessionId: string | null;
  epoch: number;
  inputVersion: number;
}

export interface ExternalChangesCoordinatorOptions {
  getSessionId: () => string | null;
  /** Defer background fingerprint checks while a foreground save is active. */
  canProbe?: () => boolean;
  probe: (sessionId: string | null) => Promise<ExternalChangeProbe>;
  onChanged: (
    result: ExternalChangeProbe,
    token: ExternalChangeToken,
    isCurrent: () => boolean,
  ) => void | Promise<void>;
  onProbeResult?: (result: ExternalChangeProbe) => void;
  onError?: (
    error: unknown,
    token: ExternalChangeToken,
    isCurrent: () => boolean,
  ) => void | Promise<void>;
  debounceMs?: number;
  setTimeout?: (callback: () => void, delay: number) => unknown;
  clearTimeout?: (handle: unknown) => void;
}

const defaultSetTimeout = (callback: () => void, delay: number) => globalThis.setTimeout(callback, delay);
const defaultClearTimeout = (handle: unknown) => globalThis.clearTimeout(handle as number);

export function createExternalChangesCoordinator(options: ExternalChangesCoordinatorOptions) {
  const debounceMs = options.debounceMs ?? 250;
  const setTimer = options.setTimeout ?? defaultSetTimeout;
  const clearTimer = options.clearTimeout ?? defaultClearTimeout;

  let timer: unknown = null;
  let epoch = 0;
  let inputVersion = 0;
  let active: Promise<void> | null = null;
  let queued = false;
  let disposed = false;

  const cancelTimer = () => {
    if (timer === null) return;
    clearTimer(timer);
    timer = null;
  };

  const isCurrent = (token: ExternalChangeToken) => !disposed
    && token.epoch === epoch
    && token.inputVersion === inputVersion
    && token.sessionId === options.getSessionId();

  const schedule = () => {
    if (disposed) return;
    cancelTimer();
    timer = setTimer(() => {
      timer = null;
      void checkNow();
    }, debounceMs);
  };

  const checkNow = (): Promise<void> => {
    if (disposed) return Promise.resolve();
    if (active) {
      queued = true;
      return active;
    }
    if (options.canProbe?.() === false) {
      schedule();
      return Promise.resolve();
    }

    const token: ExternalChangeToken = {
      sessionId: options.getSessionId(),
      epoch,
      inputVersion,
    };
    let operation!: Promise<void>;
    operation = (async () => {
      try {
        const result = await options.probe(token.sessionId);
        if (result.status === 'deferred') {
          // Keep the notification even if the session/input changed while the
          // host yielded its scan to a save. The retry captures the latest token.
          if (!disposed) queued = true;
          return;
        }
        if (!isCurrent(token) || result.sessionId !== token.sessionId) return;
        if (result.status !== 'unchanged') {
          await options.onChanged(result, token, () => isCurrent(token));
        } else {
          options.onProbeResult?.(result);
        }
      } catch (error) {
        if (isCurrent(token)) {
          try {
            await options.onError?.(error, token, () => isCurrent(token));
          } catch {
            // Error handlers own presenting failures; keep the polling loop settled.
          }
        }
      } finally {
        if (active === operation) active = null;
        if (queued && !disposed) {
          queued = false;
          schedule();
        }
      }
    })();
    active = operation;
    return operation;
  };

  const noteInputChanged = () => {
    inputVersion += 1;
    if (active) queued = true;
  };

  // A retry that waited behind a save can be due just as that save completes.
  // Retain the notification, but start its normal quiet interval again so it
  // does not immediately queue another full scan ahead of the next edit/save.
  const afterSave = () => {
    if (timer !== null) schedule();
  };

  // A library save the host confirmed published must request its own full
  // probe: the receipt may project only the committed note, so no watcher
  // notification is guaranteed. Unlike afterSave this always queues a scan,
  // including when the UI session went stale and onSaved was skipped. Repeated
  // saves coalesce through the debounce, and canProbe still defers the scan
  // until foreground saves settle.
  const librarySaved = () => {
    schedule();
  };

  const sessionChanged = () => {
    epoch += 1;
    inputVersion = 0;
    if (active) queued = true;
  };

  const dispose = () => {
    disposed = true;
    queued = false;
    epoch += 1;
    cancelTimer();
  };

  const resume = () => { disposed = false; };

  return { schedule, checkNow, noteInputChanged, afterSave, librarySaved, sessionChanged, dispose, resume };
}
