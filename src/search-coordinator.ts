import type { SearchResults } from './workspace-api';

export const SEARCH_PAGE_SIZE = 100;

export type SearchPhase = 'idle' | 'debouncing' | 'loading' | 'ready' | 'building' | 'unavailable' | 'error';

export interface SearchCoordinatorState {
  query: string;
  libraryPath: string | null;
  notebookId: string | null;
  phase: SearchPhase;
  buildingSource: 'search' | 'progress' | null;
  results: SearchResults | null;
  error: string | null;
}

export interface SearchCoordinatorOptions {
  searchNotes: (query: string, notebookId: string | null, offset: number) => Promise<SearchResults>;
  onState?: (state: SearchCoordinatorState) => void;
  debounceMs?: number;
  setTimeout?: (callback: () => void, delay: number) => unknown;
  clearTimeout?: (handle: unknown) => void;
}

const defaultSetTimeout = (callback: () => void, delay: number) => globalThis.setTimeout(callback, delay);
const defaultClearTimeout = (handle: unknown) => globalThis.clearTimeout(handle as number);
const hasSearchText = (query: string) => query.length > 0 && /\S/u.test(query);
const errorMessage = (cause: unknown) => {
  if (cause && typeof cause === 'object' && 'message' in cause && typeof cause.message === 'string') {
    return cause.message;
  }
  return cause instanceof Error ? cause.message : String(cause);
};

export function createSearchCoordinator(options: SearchCoordinatorOptions) {
  const debounceMs = options.debounceMs ?? 100;
  const setTimer = options.setTimeout ?? defaultSetTimeout;
  const clearTimer = options.clearTimeout ?? defaultClearTimeout;

  let state: SearchCoordinatorState = {
    query: '',
    libraryPath: null,
    notebookId: null,
    phase: 'idle',
    buildingSource: null,
    results: null,
    error: null,
  };
  let timer: unknown = null;
  let generation = 0;
  let enabled = true;
  let disposed = false;
  let offset = 0;

  const emit = () => options.onState?.({ ...state });
  const isCurrent = (requestGeneration: number) => !disposed && requestGeneration === generation;
  const cancelTimer = () => {
    if (timer === null) return;
    clearTimer(timer);
    timer = null;
  };
  const clearResults = (phase: SearchPhase) => {
    state = { ...state, phase, buildingSource: null, results: null, error: null };
  };

  const canSearch = () => enabled && state.libraryPath !== null && hasSearchText(state.query);

  const schedule = () => {
    cancelTimer();
    generation += 1;
    offset = 0;
    if (!canSearch()) {
      clearResults('idle');
      emit();
      return;
    }
    clearResults('debouncing');
    emit();
    const scheduledGeneration = generation;
    timer = setTimer(() => {
      timer = null;
      if (isCurrent(scheduledGeneration)) loadPage(0, scheduledGeneration);
    }, debounceMs);
  };

  const loadPage = (pageOffset: number, requestGeneration = generation) => {
    if (!canSearch() || !isCurrent(requestGeneration)) return;
    const requestQuery = state.query;
    const requestLibraryPath = state.libraryPath;
    const requestNotebookId = state.notebookId;
    offset = pageOffset;
    state = { ...state, phase: 'loading', buildingSource: null, results: null, error: null };
    emit();

    void (async () => {
      try {
        const results = await options.searchNotes(requestQuery, requestNotebookId, pageOffset);
        if (!isCurrent(requestGeneration)) return;
        if (results.libraryPath !== requestLibraryPath
          || results.query !== requestQuery
          || results.notebookId !== requestNotebookId
          || results.offset !== pageOffset) {
          state = { ...state, phase: 'error', buildingSource: null, results: null, error: '搜索结果与当前笔记库或筛选范围不一致，请重试。' };
          emit();
          return;
        }
        if (results.status === 'building') {
          state = { ...state, phase: 'building', buildingSource: 'search', results: null, error: null };
        } else if (results.status === 'unavailable') {
          state = { ...state, phase: 'unavailable', buildingSource: null, results: null, error: null };
        } else {
          if (pageOffset > 0 && pageOffset >= results.total) {
            const lastValidOffset = Math.floor((results.total - 1) / SEARCH_PAGE_SIZE) * SEARCH_PAGE_SIZE;
            const nextGeneration = generation + 1;
            generation = nextGeneration;
            loadPage(Math.max(0, lastValidOffset), nextGeneration);
            return;
          }
          state = { ...state, phase: 'ready', results, error: null };
        }
        emit();
      } catch (cause) {
        if (!isCurrent(requestGeneration)) return;
        state = {
          ...state,
          phase: 'error',
          buildingSource: null,
          results: null,
          error: errorMessage(cause),
        };
        emit();
      }
    })();
  };

  const refresh = () => {
    cancelTimer();
    generation += 1;
    offset = state.results?.offset ?? offset;
    if (!canSearch()) {
      clearResults('idle');
      emit();
      return;
    }
    loadPage(offset, generation);
  };

  const setQuery = (query: string) => {
    if (query === state.query) return;
    state = { ...state, query };
    schedule();
  };

  const setScope = (notebookId: string | null) => {
    if (notebookId === state.notebookId) return;
    state = { ...state, notebookId };
    schedule();
  };

  const setLibraryPath = (libraryPath: string | null) => {
    if (libraryPath === state.libraryPath) return;
    state = { ...state, libraryPath };
    schedule();
  };

  const setEnabled = (value: boolean) => {
    if (value === enabled) return;
    enabled = value;
    schedule();
  };

  const clear = () => {
    if (state.query === '' && state.phase === 'idle') return;
    state = { ...state, query: '' };
    schedule();
  };

  const invalidate = () => {
    cancelTimer();
    generation += 1;
    offset = 0;
    clearResults('idle');
    emit();
  };

  const markBuilding = (libraryPath: string) => {
    if (libraryPath !== state.libraryPath || !hasSearchText(state.query) || !enabled) return;
    cancelTimer();
    generation += 1;
    clearResults('building');
    state = { ...state, buildingSource: 'progress' };
    emit();
  };

  const nextPage = () => {
    const current = state.results;
    if (state.phase !== 'ready' || !current || current.offset + SEARCH_PAGE_SIZE >= current.total) return;
    cancelTimer();
    generation += 1;
    loadPage(current.offset + SEARCH_PAGE_SIZE, generation);
  };

  const previousPage = () => {
    const current = state.results;
    if (state.phase !== 'ready' || !current || current.offset === 0) return;
    cancelTimer();
    generation += 1;
    loadPage(Math.max(0, current.offset - SEARCH_PAGE_SIZE), generation);
  };

  const dispose = () => {
    disposed = true;
    generation += 1;
    cancelTimer();
  };

  return {
    getState: () => ({ ...state }),
    setQuery,
    setScope,
    setLibraryPath,
    setEnabled,
    clear,
    invalidate,
    markBuilding,
    refresh,
    nextPage,
    previousPage,
    dispose,
  };
}
