import assert from 'node:assert/strict';
import test from 'node:test';
import { createSaveCoordinator } from '../src/save-coordinator.ts';

type Draft = { text: string };

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

async function settlePromises() {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
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

test('waits one second after the last edit before saving', async () => {
  const clock = new FakeClock();
  const writes: string[] = [];
  const coordinator = createSaveCoordinator<Draft>({
    save: async (_sessionId, snapshot) => { writes.push(snapshot.text); },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.reset('session-1', { text: 'saved' });

  coordinator.edit('session-1', { text: 'first' });
  await clock.advance(999);
  assert.deepEqual(writes, []);
  await clock.advance(1);
  assert.deepEqual(writes, ['first']);
  assert.equal(coordinator.getState().phase, 'saved');
  coordinator.dispose();
});

test('serializes writes and saves the newest snapshot after an in-flight write', async () => {
  const clock = new FakeClock();
  const first = deferred<void>();
  const writes: string[] = [];
  let active = 0;
  let maxActive = 0;
  const coordinator = createSaveCoordinator<Draft>({
    save: async (_sessionId, snapshot) => {
      writes.push(snapshot.text);
      active += 1;
      maxActive = Math.max(maxActive, active);
      if (snapshot.text === 'one') await first.promise;
      active -= 1;
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.reset('session-1', { text: 'saved' });

  coordinator.edit('session-1', { text: 'one' });
  await clock.advance(1000);
  coordinator.edit('session-1', { text: 'two' });
  assert.equal(coordinator.getState().phase, 'saving');
  assert.equal(coordinator.getState().isDirty, true);
  await clock.advance(1000);
  assert.deepEqual(writes, ['one']);

  first.resolve();
  await settlePromises();
  assert.deepEqual(writes, ['one', 'two']);
  assert.equal(maxActive, 1);
  assert.equal(await coordinator.flush(), true);
  assert.equal(coordinator.getState().isDirty, false);
  coordinator.dispose();
});

test('a stale save failure stays visible until the newer snapshot is saved', async () => {
  const clock = new FakeClock();
  const first = deferred<void>();
  const second = deferred<void>();
  const failure = { kind: 'disk', message: '第一次写入失败' };
  const writes: string[] = [];
  const coordinator = createSaveCoordinator<Draft>({
    save: async (_sessionId, snapshot) => {
      writes.push(snapshot.text);
      if (snapshot.text === 'one') return first.promise;
      return second.promise;
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.reset('session-1', { text: 'saved' });
  coordinator.edit('session-1', { text: 'one' });
  const leave = coordinator.leave();
  await settlePromises();

  coordinator.edit('session-1', { text: 'two' });
  await clock.advance(1000);
  first.reject(failure);
  await settlePromises();
  assert.deepEqual(writes, ['one', 'two']);
  assert.equal(coordinator.getState().phase, 'saving');
  assert.equal(coordinator.getState().isDirty, true);
  assert.equal(coordinator.getState().error, failure);

  second.resolve();
  assert.equal(await leave.result, true);
  assert.equal(coordinator.getState().phase, 'saved');
  assert.equal(coordinator.getState().error, null);
  coordinator.dispose();
});

test('a failed flush keeps the draft and reports conflict without auto retrying', async () => {
  const clock = new FakeClock();
  let writes = 0;
  const conflict = { kind: 'conflict', message: '版本已变化' };
  const coordinator = createSaveCoordinator<Draft>({
    save: async () => { writes += 1; throw conflict; },
    isConflict: (error) => error === conflict,
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.reset('session-1', { text: 'saved' });
  coordinator.edit('session-1', { text: 'draft' });

  const leave = coordinator.leave();
  assert.equal(await leave.result, false);
  assert.equal(coordinator.getState().phase, 'conflict');
  assert.equal(coordinator.getState().isDirty, true);
  await clock.advance(10_000);
  assert.equal(writes, 1);
  coordinator.dispose();
});

test('canceling a leave intent does not cancel the disk write', async () => {
  const write = deferred<void>();
  const coordinator = createSaveCoordinator<Draft>({
    save: async () => write.promise,
  });
  coordinator.reset('session-1', { text: 'saved' });
  coordinator.edit('session-1', { text: 'draft' });

  const leave = coordinator.leave();
  await settlePromises();
  leave.cancel();
  assert.equal(await leave.result, false);
  assert.equal(coordinator.getState().phase, 'saving');

  write.resolve();
  await settlePromises();
  assert.equal(coordinator.getState().phase, 'saved');
  assert.equal(coordinator.getState().isDirty, false);
  coordinator.dispose();
});

test('pausing automatic saves cancels the debounce and resumes only after an explicit resume', async () => {
  const clock = new FakeClock();
  const writes: string[] = [];
  const coordinator = createSaveCoordinator<Draft>({
    save: async (_sessionId, snapshot) => { writes.push(snapshot.text); },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.reset('session-1', { text: 'saved' });
  coordinator.edit('session-1', { text: 'draft' });

  await coordinator.pauseAutomaticSavingAndWait();
  await clock.advance(1000);
  assert.deepEqual(writes, []);
  assert.equal(coordinator.getState().isDirty, true);

  coordinator.resumeAutomaticSaving();
  await clock.advance(1000);
  assert.deepEqual(writes, ['draft']);
  assert.equal(coordinator.getState().isDirty, false);
  coordinator.dispose();
});

test('pausing while a write is active waits for it and suppresses queued automatic writes', async () => {
  const clock = new FakeClock();
  const first = deferred<void>();
  const writes: string[] = [];
  const coordinator = createSaveCoordinator<Draft>({
    save: async (_sessionId, snapshot) => {
      writes.push(snapshot.text);
      if (snapshot.text === 'first') await first.promise;
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.reset('session-1', { text: 'saved' });
  coordinator.edit('session-1', { text: 'first' });
  await clock.advance(1000);
  coordinator.edit('session-1', { text: 'newer' });

  let pauseFinished = false;
  const paused = coordinator.pauseAutomaticSavingAndWait().then(() => { pauseFinished = true; });
  await settlePromises();
  assert.equal(pauseFinished, false);
  first.resolve();
  await paused;
  await clock.advance(1000);

  assert.deepEqual(writes, ['first']);
  assert.equal(coordinator.getState().isDirty, true);
  coordinator.dispose();
});
