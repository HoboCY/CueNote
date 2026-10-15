import type { ExternalChangeProbe } from './workspace-api';

export interface ExternalChangeToken {
  sessionId: string | null;
  epoch: number;
  inputVersion: number;
}

export interface ExternalChangesCoordinatorOptions {
  getSessionId: () => string | null;
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

    const token: ExternalChangeToken = {
      sessionId: options.getSessionId(),
      epoch,
      inputVersion,
    };
    let operation!: Promise<void>;
    operation = (async () => {
      try {
        const result = await options.probe(token.sessionId);
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

  return { schedule, checkNow, noteInputChanged, sessionChanged, dispose, resume };
}
