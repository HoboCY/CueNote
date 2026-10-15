import assert from 'node:assert/strict';
import test from 'node:test';
import { createSearchCoordinator } from '../src/search-coordinator.ts';
import type { SearchResults } from '../src/workspace-api.ts';

class FakeClock {
  private nextId = 1;
  private now = 0;
  private readonly timers = new Map<number, { due: number; callback: () => void }>();

  setTimeout = (callback: () => void, delay: number) => {
    const id = this.nextId++;
    this.timers.set(id, { due: this.now + delay, callback });
    return id;
  };

  clearTimeout = (handle: unknown) => {
    if (typeof handle === 'number') this.timers.delete(handle);
  };

  async advance(milliseconds: number) {
    const end = this.now + milliseconds;
    while (true) {
      const next = [...this.timers.entries()]
        .filter(([, timer]) => timer.due <= end)
        .sort((left, right) => left[1].due - right[1].due)[0];
      if (!next) break;
      const [id, timer] = next;
      this.timers.delete(id);
      this.now = timer.due;
      timer.callback();
      await settlePromises();
    }
    this.now = end;
    await settlePromises();
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function settlePromises() {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
}

function emptyResults(
  query: string,
  libraryPath = 'library-a',
  notebookId: string | null = null,
  offset = 0,
  total = 0,
): SearchResults {
  return {
    libraryPath,
    query,
    notebookId,
    status: 'ready',
    hits: [],
    total,
    offset,
    indexedNotes: 0,
    incomplete: false,
    warnings: [],
  };
}

function pageResults(query: string, offset: number, total: number): SearchResults {
  const count = Math.max(0, Math.min(100, total - offset));
  return {
    ...emptyResults(query, 'library-a', null, offset, total),
    hits: Array.from({ length: count }, (_, index) => {
      const noteId = `note-${offset + index}`;
      return {
        note: {
          noteId,
          title: `Title ${noteId}`,
          modifiedAt: '2026-10-15T00:00:00Z',
          notebookId: null,
          pinned: false,
          excerpt: '',
          trashedAt: null,
        },
        section: 'content',
        snippet: { before: 'before ', matched: query, after: ' after' },
      };
    }),
  };
}

test('a late result cannot replace the newest search query', async () => {
  const clock = new FakeClock();
  const older = deferred<SearchResults>();
  const newer = deferred<SearchResults>();
  const coordinator = createSearchCoordinator({
    searchNotes: (query) => query === 'older' ? older.promise : newer.promise,
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.setLibraryPath('library-a');

  coordinator.setQuery('older');
  await clock.advance(100);
  coordinator.setQuery('newer');
  await clock.advance(100);
  newer.resolve(emptyResults('newer'));
  await settlePromises();
  older.resolve(emptyResults('older'));
  await settlePromises();

  assert.equal(coordinator.getState().query, 'newer');
  assert.equal(coordinator.getState().phase, 'ready');
  assert.equal(coordinator.getState().results?.query, 'newer');
  coordinator.dispose();
});

test('a library and notebook switch clears the old page before searching the new scope', async () => {
  const clock = new FakeClock();
  const oldLibrary = deferred<SearchResults>();
  const calls: Array<{ query: string; notebookId: string | null }> = [];
  const coordinator = createSearchCoordinator({
    searchNotes: (query, notebookId) => {
      calls.push({ query, notebookId });
      return notebookId === 'notebook-b'
        ? Promise.resolve(emptyResults(query, 'library-b', notebookId))
        : oldLibrary.promise;
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.setLibraryPath('library-a');
  coordinator.setQuery('cue');
  await clock.advance(100);

  coordinator.setScope('notebook-b');
  coordinator.setLibraryPath('library-b');
  assert.equal(coordinator.getState().phase, 'debouncing');
  assert.equal(coordinator.getState().results, null);
  await clock.advance(100);
  await settlePromises();
  oldLibrary.resolve(emptyResults('cue'));
  await settlePromises();

  assert.deepEqual(calls, [
    { query: 'cue', notebookId: null },
    { query: 'cue', notebookId: 'notebook-b' },
  ]);
  assert.equal(coordinator.getState().phase, 'ready');
  assert.equal(coordinator.getState().libraryPath, 'library-b');
  assert.equal(coordinator.getState().notebookId, 'notebook-b');
  assert.equal(coordinator.getState().results?.libraryPath, 'library-b');
  coordinator.dispose();
});

test('clearing a query prevents an in-flight result from restoring the results list', async () => {
  const clock = new FakeClock();
  const pending = deferred<SearchResults>();
  let calls = 0;
  const coordinator = createSearchCoordinator({
    searchNotes: () => { calls += 1; return pending.promise; },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.setLibraryPath('library-a');
  coordinator.setQuery('temporary');
  await clock.advance(100);

  coordinator.clear();
  pending.resolve(emptyResults('temporary'));
  await settlePromises();

  assert.equal(calls, 1);
  assert.equal(coordinator.getState().query, '');
  assert.equal(coordinator.getState().phase, 'idle');
  assert.equal(coordinator.getState().results, null);
  coordinator.dispose();
});

test('a stale search error cannot replace a newer successful page', async () => {
  const clock = new FakeClock();
  const earlier = deferred<SearchResults>();
  const coordinator = createSearchCoordinator({
    searchNotes: (query) => query === 'earlier' ? earlier.promise : Promise.resolve(emptyResults(query)),
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.setLibraryPath('library-a');
  coordinator.setQuery('earlier');
  await clock.advance(100);
  coordinator.setQuery('current');
  await clock.advance(100);
  await settlePromises();
  earlier.reject(new Error('late failure'));
  await settlePromises();

  assert.equal(coordinator.getState().query, 'current');
  assert.equal(coordinator.getState().phase, 'ready');
  assert.equal(coordinator.getState().error, null);
  assert.equal(coordinator.getState().results?.query, 'current');
  coordinator.dispose();
});

test('search pages advance, refresh the current page, and move back in the selected scope', async () => {
  const clock = new FakeClock();
  const offsets: number[] = [];
  const coordinator = createSearchCoordinator({
    searchNotes: (query, _notebookId, offset) => {
      offsets.push(offset);
      return Promise.resolve(pageResults(query, offset, 250));
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.setLibraryPath('library-a');
  coordinator.setQuery('formula');
  await clock.advance(100);
  coordinator.nextPage();
  await settlePromises();
  coordinator.nextPage();
  await settlePromises();
  coordinator.refresh();
  await settlePromises();
  coordinator.previousPage();
  await settlePromises();

  assert.deepEqual(offsets, [0, 100, 200, 200, 100]);
  assert.equal(coordinator.getState().results?.offset, 100);
  assert.equal(coordinator.getState().results?.hits.length, 100);
  coordinator.dispose();
});

test('whitespace-only input is empty while meaningful query whitespace stays exact', async () => {
  const clock = new FakeClock();
  const queries: string[] = [];
  const coordinator = createSearchCoordinator({
    searchNotes: (query) => {
      queries.push(query);
      return Promise.resolve(emptyResults(query));
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.setLibraryPath('library-a');
  coordinator.setQuery('   ');
  await clock.advance(100);
  coordinator.setQuery(' cue ');
  await clock.advance(100);

  assert.deepEqual(queries, [' cue ']);
  assert.equal(coordinator.getState().results?.query, ' cue ');
  coordinator.dispose();
});

test('refresh returns to the last valid page after results shrink', async () => {
  const clock = new FakeClock();
  const offsets: number[] = [];
  let total = 250;
  const coordinator = createSearchCoordinator({
    searchNotes: (query, _notebookId, offset) => {
      offsets.push(offset);
      return Promise.resolve(pageResults(query, offset, total));
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.setLibraryPath('library-a');
  coordinator.setQuery('note');
  await clock.advance(100);
  coordinator.nextPage();
  await settlePromises();
  coordinator.nextPage();
  await settlePromises();
  total = 120;
  coordinator.refresh();
  await settlePromises();

  assert.deepEqual(offsets, [0, 100, 200, 200, 100]);
  assert.equal(coordinator.getState().results?.offset, 100);
  assert.equal(coordinator.getState().results?.hits.length, 20);
  assert.equal(coordinator.getState().results?.total, 120);
  coordinator.dispose();
});

test('a workspace error object shows its message in the search state', async () => {
  const clock = new FakeClock();
  const coordinator = createSearchCoordinator({
    searchNotes: async () => { throw { kind: 'search', message: '搜索参数过长' }; },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.setLibraryPath('library-a');
  coordinator.setQuery('long');
  await clock.advance(100);

  assert.equal(coordinator.getState().phase, 'error');
  assert.equal(coordinator.getState().error, '搜索参数过长');
  coordinator.dispose();
});

test('search waits until an input composition ends before sending its final value', async () => {
  const clock = new FakeClock();
  const queries: string[] = [];
  const coordinator = createSearchCoordinator({
    searchNotes: (query) => {
      queries.push(query);
      return Promise.resolve(emptyResults(query));
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.setLibraryPath('library-a');
  coordinator.setEnabled(false);
  await clock.advance(200);
  assert.deepEqual(queries, []);

  coordinator.setQuery('组合完成');
  coordinator.setEnabled(true);
  await clock.advance(100);

  assert.deepEqual(queries, ['组合完成']);
  assert.equal(coordinator.getState().results?.query, '组合完成');
  coordinator.dispose();
});
