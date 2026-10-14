import type { Block } from './generated/note-format.ts';

/** Session-bound operations for validating and flushing a live mind map. */
export interface MindMapEditorBridge {
  canStart(): boolean;
  /** Validate the complete live draft and expire the predicate when it changes. */
  preflight(contentBlocks: Block[]): Promise<() => boolean>;
  /** Register immediately so switching, saving, and closing wait for this work. */
  track(operation: Promise<void>): void;
  /** Register a flush for an expanded map, including text not yet blurred. */
  registerFlush(settle: () => Promise<void>): () => void;
}

export interface MindMapTextInputQueue {
  input(): Promise<void>;
  compositionStart(): void;
  compositionEnd(): Promise<void>;
  settle(): Promise<void>;
  isComposing(): boolean;
}

/** Coordinate the final DOM input event with IME compositionend and editor flushes. */
export function createMindMapTextInputQueue(
  validate: (force: boolean) => Promise<void>,
  defer: (callback: () => void) => void = callback => { setTimeout(callback, 0); },
  onCompositionSettled: () => void = () => undefined,
): MindMapTextInputQueue {
  type CompositionCycle = {
    composing: boolean;
    finalizing: boolean;
    finalInputSeen: boolean;
    finalInputTask: Promise<void> | null;
    endSignal: Promise<void>;
    resolveEndSignal: (() => void) | null;
    endTask: Promise<void> | null;
  };
  let activeCycle: CompositionCycle | null = null;
  const pendingCycles = new Set<CompositionCycle>();

  return {
    input() {
      const cycle = activeCycle;
      if (cycle?.composing) return Promise.resolve();
      if (cycle?.finalizing) {
        cycle.finalInputSeen = true;
        cycle.finalInputTask = validate(true);
        return cycle.finalInputTask;
      }
      return validate(false);
    },
    compositionStart() {
      if (activeCycle?.composing) return;
      const cycle: CompositionCycle = {
        composing: true,
        finalizing: false,
        finalInputSeen: false,
        finalInputTask: null,
        endSignal: Promise.resolve(),
        resolveEndSignal: null,
        endTask: null,
      };
      cycle.endSignal = new Promise<void>(resolve => { cycle.resolveEndSignal = resolve; });
      activeCycle = cycle;
      pendingCycles.add(cycle);
    },
    compositionEnd() {
      const cycle = activeCycle;
      if (!cycle?.composing) return cycle?.endTask ?? Promise.resolve();
      cycle.composing = false;
      cycle.finalizing = true;
      cycle.resolveEndSignal?.();
      cycle.resolveEndSignal = null;
      cycle.endTask = new Promise<void>((resolve, reject) => {
        defer(() => {
          let task: Promise<void>;
          try {
            task = cycle.finalInputSeen ? cycle.finalInputTask ?? Promise.resolve() : validate(true);
          } catch (cause) {
            task = Promise.reject(cause);
          }
          const finish = () => {
            cycle.finalizing = false;
            pendingCycles.delete(cycle);
            if (activeCycle === cycle) activeCycle = null;
            if (pendingCycles.size === 0) onCompositionSettled();
          };
          void task.then(() => {
            try {
              finish();
              resolve();
            } catch (cause) {
              reject(cause);
            }
          }, cause => {
            try {
              finish();
            } catch (settleCause) {
              reject(settleCause);
              return;
            }
            reject(cause);
          });
        });
      });
      return cycle.endTask;
    },
    async settle() {
      while (pendingCycles.size > 0) {
        const cycles = [...pendingCycles];
        await Promise.all(cycles.map(cycle => cycle.composing
          ? cycle.endSignal
          : cycle.endTask ?? Promise.resolve()));
      }
    },
    isComposing: () => [...pendingCycles].some(cycle => cycle.composing || cycle.finalizing),
  };
}
