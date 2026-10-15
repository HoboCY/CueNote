import assert from 'node:assert/strict';
import test from 'node:test';
import { createBackupCoordinator } from '../src/backup-coordinator.ts';
import type { BackupCatalog, BackupOutcome } from '../src/workspace-api.ts';

class FakeClock {
  private nextId = 1;
  private currentTime = 0;
  private readonly timers = new Map<number, { due: number; callback: () => void; interval: number | null }>();

  now = () => this.currentTime;

  setTimeout = (callback: () => void, delay: number) => this.add(callback, delay, null);
  setInterval = (callback: () => void, delay: number) => this.add(callback, delay, delay);

  clearTimeout = (handle: unknown) => this.remove(handle);
  clearInterval = (handle: unknown) => this.remove(handle);

  private add(callback: () => void, delay: number, interval: number | null) {
    const id = this.nextId++;
    this.timers.set(id, { due: this.currentTime + delay, callback, interval });
    return id;
  }

  private remove(handle: unknown) {
    if (typeof handle === 'number') this.timers.delete(handle);
  }

  async advance(milliseconds: number) {
    const end = this.currentTime + milliseconds;
    while (true) {
      const next = [...this.timers.entries()]
        .filter(([, timer]) => timer.due <= end)
        .sort((left, right) => left[1].due - right[1].due)[0];
      if (!next) break;
      const [id, timer] = next;
      this.currentTime = timer.due;
      if (timer.interval === null) this.timers.delete(id);
      else timer.due += timer.interval;
      timer.callback();
      await settlePromises();
    }
    this.currentTime = end;
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

function emptyCatalog(libraryPath: string): BackupCatalog {
  return { libraryPath, directory: 'D:\\CueNote Backups', backups: [], warnings: [] };
}

function outcome(libraryPath: string, message: string): BackupOutcome {
  return { catalog: emptyCatalog(libraryPath), created: null, message };
}

function libraryToken(modifiedAt = '2026-10-15T00:00:00Z') {
  return {
    path: 'library-a',
    defaultNotebookName: 'Notes',
    defaultNotebookId: 'default',
    notebooks: [],
    notes: [{
      noteId: 'note-1', title: 'One', modifiedAt, notebookId: null, pinned: false, excerpt: '', trashedAt: null,
    }],
    trashNotes: [],
    warnings: [],
  };
}

for (const oldOperation of ['backup', 'catalog'] as const) {
  test(`a finished ${oldOperation} keeps the next library queued until its cancellation settles`, async () => {
    const clock = new FakeClock();
    const oldBackup = deferred<BackupOutcome>();
    const oldCatalog = deferred<BackupCatalog>();
    const cancelAck = deferred<void>();
    const listedPaths: string[] = [];
    const coordinator = createBackupCoordinator({
      api: {
        runBackup: (_kind, _requestId, path) => path === 'library-a'
          ? oldBackup.promise : Promise.resolve(outcome(path, '没有新增内容')),
        listBackups: (_requestId, path) => {
          listedPaths.push(path);
          return oldOperation === 'catalog' && path === 'library-a'
            ? oldCatalog.promise : Promise.resolve(emptyCatalog(path));
        },
        chooseBackupDirectory: async () => null,
        cancelBackup: () => cancelAck.promise,
      },
      clock,
    });
    coordinator.activate();
    coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a', automaticAllowed: true });
    await clock.advance(5_000);
    assert.equal(coordinator.cancel(), true);
    coordinator.synchronize({ libraryPath: 'library-b', changeToken: 'b', automaticAllowed: true });

    if (oldOperation === 'backup') oldBackup.resolve(outcome('library-a', '旧任务已完成'));
    else oldCatalog.resolve(emptyCatalog('library-a'));
    await settlePromises();
    assert.deepEqual(listedPaths, ['library-a'], 'a late global cancellation must not reach a new host operation');
    assert.equal(coordinator.getState().catalog, null);

    cancelAck.resolve();
    await settlePromises();
    assert.deepEqual(listedPaths, ['library-a', 'library-b']);
    assert.equal(coordinator.getState().catalog?.libraryPath, 'library-b');
    coordinator.deactivate();
  });
}

test('a backup cancellation result waits for acknowledgement before reporting its final state', async () => {
  const clock = new FakeClock();
  const backup = deferred<BackupOutcome>();
  const cancelAck = deferred<void>();
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: () => backup.promise,
      listBackups: async (_requestId, path) => emptyCatalog(path),
      chooseBackupDirectory: async () => null,
      cancelBackup: () => cancelAck.promise,
    },
    clock,
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a', automaticAllowed: true });
  await clock.advance(5_000);
  coordinator.cancel();
  backup.reject({ kind: 'backupCancelled', message: '备份已取消' });
  await settlePromises();
  assert.equal(coordinator.getState().operation, 'cancelling');
  cancelAck.resolve();
  await settlePromises();
  assert.equal(coordinator.getState().operation, null);
  assert.equal(coordinator.getState().error, null);
  assert.equal(coordinator.getState().notice, '备份已取消。');
  coordinator.deactivate();
});

test('a late backup completion from the previous library cannot replace the current catalog', async () => {
  const clock = new FakeClock();
  const oldRun = deferred<BackupOutcome>();
  const paths: string[] = [];
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: (_kind, _requestId, path) => {
        paths.push(path);
        return path === 'library-a' ? oldRun.promise : Promise.resolve(outcome(path, '没有新增内容'));
      },
      listBackups: async (_requestId, path) => emptyCatalog(path),
      chooseBackupDirectory: async () => null,
      cancelBackup: async () => undefined,
    },
    clock,
    createRequestId: () => 'request',
  });

  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a1', automaticAllowed: true });
  await clock.advance(5_000);
  assert.deepEqual(paths, ['library-a']);

  coordinator.synchronize({ libraryPath: 'library-b', changeToken: 'b1', automaticAllowed: true });
  await settlePromises();
  await clock.advance(5_000);
  assert.equal(coordinator.getState().libraryPath, 'library-b');
  assert.equal(coordinator.getState().catalog, null);

  oldRun.resolve(outcome('library-a', '旧库备份已完成'));
  await settlePromises();
  await clock.advance(0);
  await settlePromises();
  assert.equal(coordinator.getState().libraryPath, 'library-b');
  assert.equal(coordinator.getState().catalog?.libraryPath, 'library-b');
  assert.notEqual(coordinator.getState().notice, '旧库备份已完成');
  coordinator.deactivate();
});

test('automatic backups coalesce library changes and wait until editing is clean', async () => {
  const clock = new FakeClock();
  const paths: string[] = [];
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: async (_kind, _requestId, path) => {
        paths.push(path);
        return outcome(path, '没有新增内容');
      },
      listBackups: async (_requestId, path) => emptyCatalog(path),
      chooseBackupDirectory: async () => null,
      cancelBackup: async () => undefined,
    },
    clock,
    createRequestId: () => 'request',
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: libraryToken(), automaticAllowed: true });
  await clock.advance(3_000);
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: libraryToken('2026-10-15T00:00:05Z'), automaticAllowed: true });
  await clock.advance(4_999);
  assert.deepEqual(paths, []);
  await clock.advance(1);
  assert.deepEqual(paths, ['library-a']);

  coordinator.synchronize({ libraryPath: 'library-a', changeToken: libraryToken('2026-10-15T00:00:10Z'), automaticAllowed: false });
  await clock.advance(10_000);
  assert.deepEqual(paths, ['library-a']);
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: libraryToken('2026-10-15T00:00:10Z'), automaticAllowed: true });
  await clock.advance(4_999);
  assert.deepEqual(paths, ['library-a']);
  await clock.advance(1);
  assert.deepEqual(paths, ['library-a', 'library-a']);
  coordinator.deactivate();
});

test('equivalent refreshed views do not restart the debounce and the periodic fallback still runs', async () => {
  const clock = new FakeClock();
  let runCount = 0;
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: async (_kind, _requestId, path) => {
        runCount += 1;
        return outcome(path, '没有新增内容');
      },
      listBackups: async (_requestId, path) => emptyCatalog(path),
      chooseBackupDirectory: async () => null,
      cancelBackup: async () => undefined,
    },
    clock,
    createRequestId: () => 'request',
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: libraryToken(), automaticAllowed: true });
  await clock.advance(4_000);
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: libraryToken(), automaticAllowed: true });
  await clock.advance(1_000);
  assert.equal(runCount, 1);
  await clock.advance(5_000);
  assert.equal(runCount, 1);

  for (let index = 0; index < 5; index += 1) {
    await clock.advance(10_000);
    coordinator.synchronize({ libraryPath: 'library-a', changeToken: libraryToken(), automaticAllowed: true });
  }
  await clock.advance(10_000);
  assert.equal(runCount, 2);
  coordinator.deactivate();
});

test('catalog requests serialize and hold automatic backup until the current library is listed', async () => {
  const clock = new FakeClock();
  const oldCatalog = deferred<BackupCatalog>();
  const listedPaths: string[] = [];
  const backupPaths: string[] = [];
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: async (_kind, _requestId, path) => {
        backupPaths.push(path);
        return outcome(path, '没有新增内容');
      },
      listBackups: (_requestId, path) => {
        listedPaths.push(path);
        return path === 'library-a' ? oldCatalog.promise : Promise.resolve(emptyCatalog(path));
      },
      chooseBackupDirectory: async () => null,
      cancelBackup: async () => undefined,
    },
    clock,
    createRequestId: () => 'request',
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a1', automaticAllowed: true });
  await clock.advance(5_000);
  assert.deepEqual(backupPaths, []);

  coordinator.synchronize({ libraryPath: 'library-b', changeToken: 'b1', automaticAllowed: true });
  await settlePromises();
  assert.deepEqual(listedPaths, ['library-a']);
  assert.deepEqual(backupPaths, []);

  oldCatalog.resolve(emptyCatalog('library-a'));
  await settlePromises();
  assert.deepEqual(listedPaths, ['library-a', 'library-b']);
  await clock.advance(5_000);
  assert.deepEqual(backupPaths, ['library-b']);
  coordinator.deactivate();
});

test('a failed manual save check skips backup creation', async () => {
  const clock = new FakeClock();
  let runCount = 0;
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: async (_kind, _requestId, path) => {
        runCount += 1;
        return outcome(path, '没有新增内容');
      },
      listBackups: async (_requestId, path) => emptyCatalog(path),
      chooseBackupDirectory: async () => null,
      cancelBackup: async () => undefined,
    },
    clock,
    createRequestId: () => 'request',
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a1', automaticAllowed: true });
  await settlePromises();

  assert.equal(await coordinator.createManualBackup(async () => false), false);
  await clock.advance(5_000);
  assert.equal(runCount, 0);
  assert.match(coordinator.getState().notice ?? '', /跳过备份/);
  assert.equal(coordinator.getState().operation, null);
  coordinator.deactivate();
});

test('a settled cancellation releases the active slot so a later automatic backup can run', async () => {
  const clock = new FakeClock();
  const firstRun = deferred<BackupOutcome>();
  const secondRun = deferred<BackupOutcome>();
  const paths: string[] = [];
  let cancelCount = 0;
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: (_kind, _requestId, path) => {
        paths.push(path);
        return paths.length === 1 ? firstRun.promise : secondRun.promise;
      },
      listBackups: async (_requestId, path) => emptyCatalog(path),
      chooseBackupDirectory: async () => null,
      cancelBackup: async () => { cancelCount += 1; },
    },
    clock,
    createRequestId: () => `request-${paths.length}`,
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a1', automaticAllowed: true });
  await clock.advance(5_000);
  assert.equal(coordinator.getState().operation, 'automatic');

  assert.equal(coordinator.cancel(), true);
  await settlePromises();
  assert.equal(cancelCount, 1);
  assert.equal(coordinator.getState().operation, 'cancelling');
  await clock.advance(5_000);
  assert.deepEqual(paths, ['library-a']);

  firstRun.resolve(outcome('library-a', '备份已取消。'));
  await settlePromises();
  assert.equal(coordinator.getState().operation, null);

  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a2', automaticAllowed: true });
  await clock.advance(5_000);
  assert.deepEqual(paths, ['library-a', 'library-a']);
  await clock.advance(3_000);
  assert.equal(coordinator.getState().elapsedSeconds, 3);
  await clock.advance(1_000);
  assert.equal(coordinator.getState().elapsedSeconds, 4);

  secondRun.resolve(outcome('library-a', '没有新增内容'));
  await settlePromises();
  assert.equal(coordinator.getState().operation, null);
  coordinator.deactivate();
});

test('cancel acknowledgement keeps the host slot until the backup result settles', async () => {
  const clock = new FakeClock();
  const firstRun = deferred<BackupOutcome>();
  let runCount = 0;
  let listCount = 0;
  const created = {
    backupId: 'backup-1',
    kind: 'automatic' as const,
    createdAt: '2026-10-15T00:00:05Z',
    localDate: '2026-10-15',
    path: 'D:\\CueNote Backups\\backup-1.zip',
    noteCount: 2,
    notebookCount: 1,
  };
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: async (_kind, _requestId, path) => {
        runCount += 1;
        return runCount === 1
          ? firstRun.promise
          : outcome(path, '没有新增内容');
      },
      listBackups: async (_requestId, path) => {
        listCount += 1;
        return emptyCatalog(path);
      },
      chooseBackupDirectory: async () => null,
      cancelBackup: async () => undefined,
    },
    clock,
    createRequestId: () => `request-${runCount}`,
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a1', automaticAllowed: true });
  await clock.advance(5_000);
  assert.equal(runCount, 1);

  assert.equal(coordinator.cancel(), true);
  await settlePromises();
  assert.equal(coordinator.getState().operation, 'cancelling');
  assert.equal(listCount, 1);
  await clock.advance(5_000);
  assert.equal(runCount, 1);

  firstRun.resolve({
    catalog: { ...emptyCatalog('library-a'), backups: [created] },
    created,
    message: '备份已创建。',
  });
  await settlePromises();
  assert.equal(coordinator.getState().operation, null);
  assert.equal(coordinator.getState().catalog?.backups[0]?.backupId, 'backup-1');
  assert.equal(coordinator.getState().notice, '备份已创建。');

  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a2', automaticAllowed: true });
  await clock.advance(5_000);
  assert.equal(runCount, 2);
  coordinator.deactivate();
});

test('a real backup error after cancel acknowledgement remains an error', async () => {
  const clock = new FakeClock();
  const pendingRun = deferred<BackupOutcome>();
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: () => pendingRun.promise,
      listBackups: async (_requestId, path) => emptyCatalog(path),
      chooseBackupDirectory: async () => null,
      cancelBackup: async () => undefined,
    },
    clock,
    createRequestId: () => 'request',
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a1', automaticAllowed: false });
  await settlePromises();

  const manualRun = coordinator.createManualBackup(async () => true);
  await settlePromises();
  assert.equal(coordinator.getState().operation, 'manual');
  assert.equal(coordinator.cancel(), true);
  await settlePromises();
  pendingRun.reject({ kind: 'permissionDenied', message: '没有写入权限。' });
  assert.equal(await manualRun, false);

  assert.equal(coordinator.getState().error, '没有写入权限。');
  assert.match(coordinator.getState().notice ?? '', /手动备份失败/);
  assert.doesNotMatch(coordinator.getState().notice ?? '', /已取消/);
  coordinator.deactivate();
});

test('a confirmed backupCancelled result is shown as cancellation', async () => {
  const clock = new FakeClock();
  const pendingRun = deferred<BackupOutcome>();
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: () => pendingRun.promise,
      listBackups: async (_requestId, path) => emptyCatalog(path),
      chooseBackupDirectory: async () => null,
      cancelBackup: async () => undefined,
    },
    clock,
    createRequestId: () => 'request',
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a1', automaticAllowed: false });
  await settlePromises();

  const manualRun = coordinator.createManualBackup(async () => true);
  await settlePromises();
  assert.equal(coordinator.cancel(), true);
  await settlePromises();
  pendingRun.reject({ kind: 'backupCancelled', message: '任务已取消。' });
  assert.equal(await manualRun, false);

  assert.equal(coordinator.getState().error, null);
  assert.equal(coordinator.getState().notice, '备份已取消。');
  coordinator.deactivate();
});

test('catalog validation reports matching progress and cancellation retains the host slot until settlement', async () => {
  const clock = new FakeClock();
  const pendingList = deferred<BackupCatalog>();
  const cancelAck = deferred<void>();
  let listRequestId = '';
  let listCount = 0;
  let backupCount = 0;
  const coordinator = createBackupCoordinator({
    api: {
      runBackup: async (_kind, _requestId, path) => {
        backupCount += 1;
        return outcome(path, '没有新增内容');
      },
      listBackups: (requestId, _path) => {
        listRequestId = requestId;
        listCount += 1;
        return pendingList.promise;
      },
      chooseBackupDirectory: async () => null,
      cancelBackup: () => cancelAck.promise,
    },
    clock,
    createRequestId: () => 'catalog-request',
  });
  coordinator.activate();
  coordinator.synchronize({ libraryPath: 'library-a', changeToken: 'a1', automaticAllowed: false });
  await settlePromises();

  assert.equal(coordinator.getState().operation, 'listing');
  coordinator.handleProgress({
    requestId: 'old-request', libraryPath: 'library-a', phase: 'checkingSource', checked: 4, total: 8,
  });
  assert.equal(coordinator.getState().progress, null);
  coordinator.handleProgress({
    requestId: listRequestId, libraryPath: 'library-b', phase: 'checkingSource', checked: 4, total: 8,
  });
  assert.equal(coordinator.getState().progress, null);
  coordinator.handleProgress({
    requestId: listRequestId, libraryPath: 'library-a', phase: 'checkingSource', checked: 4, total: 8,
  });
  assert.deepEqual(coordinator.getState().progress, {
    requestId: listRequestId, libraryPath: 'library-a', phase: 'checkingSource', checked: 4, total: 8,
  });
  await clock.advance(3_000);
  assert.equal(coordinator.getState().elapsedSeconds, 3);

  assert.equal(coordinator.cancel(), true);
  assert.equal(coordinator.getState().operation, 'cancelling');
  assert.equal(await coordinator.refresh(), false);
  assert.equal(await coordinator.createManualBackup(async () => true), false);
  assert.equal(listCount, 1);
  assert.equal(backupCount, 0);
  cancelAck.resolve();
  await settlePromises();
  assert.equal(coordinator.getState().operation, 'cancelling');

  pendingList.reject({ kind: 'backupCancelled', message: '取消成功。' });
  await settlePromises();
  assert.equal(coordinator.getState().operation, null);
  assert.equal(coordinator.getState().notice, '备份列表检查已取消。');
  assert.equal(coordinator.getState().error, null);
  assert.equal(await coordinator.createManualBackup(async () => true), true);
  assert.equal(backupCount, 1);
  coordinator.deactivate();
});
