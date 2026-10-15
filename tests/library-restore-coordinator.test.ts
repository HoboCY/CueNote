import assert from 'node:assert/strict';
import test from 'node:test';
import { createLibraryRestoreCoordinator } from '../src/library-restore-coordinator.ts';
import type {
  BackupSummary,
  LibraryRestorePreview,
  LibraryRestoreReceipt,
  LibraryRestoreStatus,
  WorkspaceView,
} from '../src/workspace-api.ts';

const libraryPath = 'D:\\CueNote\\Library';

function backupSummary(backupId: string): BackupSummary {
  return {
    backupId,
    kind: 'manual',
    createdAt: '2026-10-15T01:02:03Z',
    localDate: '2026-10-15',
    path: `D:\\CueNote Backups\\${backupId}.cuenote-backup`,
    noteCount: 3,
    notebookCount: 2,
  };
}

function workspace(libraryRestore: WorkspaceView['libraryRestore'] = null): WorkspaceView {
  return {
    appVersion: '0.1.0',
    defaultLibraryPath: libraryPath,
    library: {
      path: libraryPath,
      defaultNotebookName: 'Notes',
      defaultNotebookId: 'default',
      notebooks: [],
      notes: [],
      trashNotes: [],
      warnings: [],
    },
    session: null,
    notice: null,
    preferences: { sidebarCollapsed: false, selectedNotebookId: null },
    libraryRestore,
  };
}

function receipt(): LibraryRestoreReceipt {
  return {
    workspace: workspace(),
    preRestoreBackup: backupSummary('pre-restore'),
    message: '整个笔记库已恢复。',
  };
}

function preview(backupId: string, manifestRevision = 'manifest-r7'): LibraryRestorePreview {
  return { backup: backupSummary(backupId), manifestRevision };
}

function pendingStatus(): LibraryRestoreStatus {
  return {
    restoreId: 'restore-pending',
    libraryPath,
    sourceBackupId: 'backup-1',
    preRestoreBackup: backupSummary('pre-restore'),
    phase: 'paused',
    completedFiles: 1,
    totalFiles: 4,
    canContinue: true,
    canRollBack: true,
    message: '恢复已在安全边界暂停。',
  };
}

function clock() {
  let now = 0;
  return {
    now: () => now,
    setTimeout: (callback: () => void, _delay: number) => callback,
    clearTimeout: (_handle: unknown) => undefined,
    setInterval: (callback: () => void, _delay: number) => callback,
    clearInterval: (_handle: unknown) => undefined,
    advance: (milliseconds: number) => { now += milliseconds; },
  };
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

test('a confirmed restore uses the frozen backup and manifest after the saved-editor gate', async () => {
  const order: string[] = [];
  const restoreArguments: string[] = [];
  const coordinator = createLibraryRestoreCoordinator({
    api: {
      previewLibraryRestore: async backupId => {
        order.push('preview');
        return preview(backupId);
      },
      restoreLibraryFromBackup: async (backupId, manifestRevision, _requestId, expectedPath) => {
        order.push('host restore');
        restoreArguments.push(backupId, manifestRevision, expectedPath);
        return receipt();
      },
      resumeLibraryRestore: async () => receipt(),
      get: async () => workspace(),
      cancelBackup: async () => undefined,
    },
    clock: clock(),
    createRequestId: () => 'restore-request-1',
  });
  coordinator.synchronize({ libraryPath, pending: null, allowed: true });

  assert.equal(await coordinator.previewBackup('backup-picked-before-preview'), true);
  const restored = await coordinator.startRestore(async (run, resume) => {
    order.push(resume ? 'resume gate' : 'save gate');
    return run();
  });

  assert.deepEqual(order, ['preview', 'save gate', 'host restore']);
  assert.deepEqual(restoreArguments, ['backup-picked-before-preview', 'manifest-r7', libraryPath]);
  assert.equal(restored, true);
  assert.equal(coordinator.getState().receipt?.message, '整个笔记库已恢复。');
});

test('a failed saved-editor gate never starts the host restore', async () => {
  let restoreCalls = 0;
  let getCalls = 0;
  const coordinator = createLibraryRestoreCoordinator({
    api: {
      previewLibraryRestore: async backupId => preview(backupId),
      restoreLibraryFromBackup: async () => {
        restoreCalls += 1;
        return receipt();
      },
      resumeLibraryRestore: async () => receipt(),
      get: async () => {
        getCalls += 1;
        return workspace();
      },
      cancelBackup: async () => undefined,
    },
    clock: clock(),
  });
  coordinator.synchronize({ libraryPath, backupId: 'backup-1', pending: null, allowed: true });
  await coordinator.previewBackup('backup-1');

  const restored = await coordinator.startRestore(async () => {
    throw new Error('当前编辑保存失败');
  });

  assert.equal(restored, false);
  assert.equal(restoreCalls, 0);
  assert.equal(getCalls, 0);
  assert.match(coordinator.getState().error ?? '', /当前编辑保存失败/);
  assert.equal(coordinator.getState().operation, null);
});

test('a second confirmation cannot start a duplicate restore while the editor gate is running', async () => {
  const hostResult = deferred<LibraryRestoreReceipt>();
  const gateEntered = deferred<void>();
  const releaseGate = deferred<void>();
  let restoreCalls = 0;
  const coordinator = createLibraryRestoreCoordinator({
    api: {
      previewLibraryRestore: async backupId => preview(backupId),
      restoreLibraryFromBackup: () => {
        restoreCalls += 1;
        return hostResult.promise;
      },
      resumeLibraryRestore: async () => receipt(),
      get: async () => workspace(),
      cancelBackup: async () => undefined,
    },
    clock: clock(),
  });
  coordinator.synchronize({ libraryPath, backupId: 'backup-1', pending: null, allowed: true });
  await coordinator.previewBackup('backup-1');
  const first = coordinator.startRestore(async run => {
    gateEntered.resolve();
    await releaseGate.promise;
    return run();
  });
  await gateEntered.promise;

  assert.equal(await coordinator.startRestore(async run => run()), false);
  assert.equal(restoreCalls, 0);
  releaseGate.resolve();
  await settlePromises();
  assert.equal(restoreCalls, 1);
  hostResult.resolve(receipt());
  assert.equal(await first, true);
});

test('continuing a rolling-back restore keeps the host action pointed at rollback', async () => {
  const actions: string[] = [];
  const pending = {
    restoreId: 'restore-1',
    libraryPath,
    sourceBackupId: 'backup-1',
    preRestoreBackup: backupSummary('pre-restore'),
    phase: 'rollingBack' as const,
    completedFiles: 2,
    totalFiles: 5,
    canContinue: true,
    canRollBack: false,
    message: '正在回退到恢复前的笔记库。',
  };
  const coordinator = createLibraryRestoreCoordinator({
    api: {
      previewLibraryRestore: async backupId => preview(backupId),
      restoreLibraryFromBackup: async () => receipt(),
      resumeLibraryRestore: async action => {
        actions.push(action);
        return receipt();
      },
      get: async () => workspace(),
      cancelBackup: async () => undefined,
    },
    clock: clock(),
  });
  coordinator.synchronize({ libraryPath, backupId: null, pending, allowed: true });

  const resumed = await coordinator.resumePending('continue', async (run, isResume) => {
    assert.equal(isResume, true);
    return run();
  });

  assert.equal(resumed, true);
  assert.deepEqual(actions, ['continue']);
  assert.equal(await coordinator.resumePending('rollBack', async run => run()), false);
});

test('a failed host refresh keeps the original restore error visible', async () => {
  const coordinator = createLibraryRestoreCoordinator({
    api: {
      previewLibraryRestore: async backupId => preview(backupId),
      restoreLibraryFromBackup: async () => { throw new Error('目标文件版本发生变化'); },
      resumeLibraryRestore: async () => receipt(),
      get: async () => { throw new Error('状态刷新连接失败'); },
      cancelBackup: async () => undefined,
    },
    clock: clock(),
  });
  coordinator.synchronize({ libraryPath, backupId: 'backup-1', pending: null, allowed: true });
  await coordinator.previewBackup('backup-1');

  assert.equal(await coordinator.startRestore(async run => run()), false);
  assert.match(coordinator.getState().error ?? '', /目标文件版本发生变化/);
  assert.match(coordinator.getState().error ?? '', /状态刷新连接失败/);
});

for (const hasPending of [true, false]) {
test(`a host error reloads actual recovery state (pending=${hasPending})`, async () => {
  const pending = hasPending ? pendingStatus() : null;
  const refreshed = workspace(pending);
  let applied: WorkspaceView | null = null;
  const coordinator = createLibraryRestoreCoordinator({
    api: {
      previewLibraryRestore: async backupId => preview(backupId),
      restoreLibraryFromBackup: async () => { throw new Error('恢复在安全边界暂停'); },
      resumeLibraryRestore: async () => receipt(),
      get: async () => refreshed,
      cancelBackup: async () => undefined,
    },
    onWorkspace: view => { applied = view; },
    clock: clock(),
  });
  coordinator.synchronize({ libraryPath, backupId: 'backup-1', pending: null, allowed: true });
  await coordinator.previewBackup('backup-1');

  assert.equal(await coordinator.startRestore(async run => run()), false);
  assert.equal(coordinator.getState().pending?.restoreId ?? null, pending?.restoreId ?? null);
  assert.match(coordinator.getState().notice ?? '', hasPending ? /继续或回退/ : /没有待处理/);
  assert.equal(applied, refreshed);
});
}

test('a receipt remains successful when the editor wrapper fails after host completion', async () => {
  let applied: WorkspaceView | null = null;
  const coordinator = createLibraryRestoreCoordinator({
    api: {
      previewLibraryRestore: async backupId => preview(backupId),
      restoreLibraryFromBackup: async () => receipt(),
      resumeLibraryRestore: async () => receipt(),
      get: async () => workspace(),
      cancelBackup: async () => undefined,
    },
    onWorkspace: view => { applied = view; throw new Error('render update failed'); },
    clock: clock(),
  });
  coordinator.synchronize({ libraryPath, backupId: 'backup-1', pending: null, allowed: true });
  await coordinator.previewBackup('backup-1');

  const restored = await coordinator.startRestore(async run => {
    await run();
    throw new Error('wrapper failed after receipt');
  });

  assert.equal(restored, true);
  assert.equal(coordinator.getState().receipt?.preRestoreBackup.backupId, 'pre-restore');
  assert.equal(coordinator.getState().error, null);
  assert.match(coordinator.getState().notice ?? '', /完成回执/);
  assert.ok(applied);
});

test('progress is accepted only for the active request and library path', async () => {
  const hostResult = deferred<LibraryRestoreReceipt>();
  const coordinator = createLibraryRestoreCoordinator({
    api: {
      previewLibraryRestore: async backupId => preview(backupId),
      restoreLibraryFromBackup: () => hostResult.promise,
      resumeLibraryRestore: async () => receipt(),
      get: async () => workspace(),
      cancelBackup: async () => undefined,
    },
    clock: clock(),
    createRequestId: () => 'active-restore-request',
  });
  coordinator.synchronize({ libraryPath, backupId: 'backup-1', pending: null, allowed: true });
  await coordinator.previewBackup('backup-1');
  const restore = coordinator.startRestore(async run => run());
  await settlePromises();

  coordinator.handleProgress({
    requestId: 'other-request', libraryPath, phase: 'restoring', checked: 2, total: 5,
  });
  assert.equal(coordinator.getState().progress, null);
  coordinator.handleProgress({
    requestId: 'active-restore-request', libraryPath: 'D:\\CueNote\\Other Library',
    phase: 'restoring', checked: 2, total: 5,
  });
  assert.equal(coordinator.getState().progress, null);
  coordinator.handleProgress({
    requestId: 'active-restore-request', libraryPath, phase: 'restoring', checked: 2, total: 5,
  });
  assert.equal(coordinator.getState().progress?.checked, 2);

  hostResult.resolve(receipt());
  assert.equal(await restore, true);
});

test('a new library waits for both the restore and a late cancel acknowledgement', async () => {
  for (const settlementOrder of ['host-first', 'cancel-first'] as const) {
    const hostResult = deferred<LibraryRestoreReceipt>();
    const cancelAcknowledgement = deferred<void>();
    const previewedLibraries: string[] = [];
    const coordinator = createLibraryRestoreCoordinator({
      api: {
        previewLibraryRestore: async backupId => {
          previewedLibraries.push(backupId);
          return preview(backupId);
        },
        restoreLibraryFromBackup: () => hostResult.promise,
        resumeLibraryRestore: async () => receipt(),
        get: async () => workspace(),
        cancelBackup: () => cancelAcknowledgement.promise,
      },
      clock: clock(),
    });
    coordinator.synchronize({ libraryPath, backupId: 'backup-old', pending: null, allowed: true });
    await coordinator.previewBackup('backup-old');
    const oldRestore = coordinator.startRestore(async run => run());
    await settlePromises();
    assert.equal(coordinator.cancel(), true);

    coordinator.synchronize({ libraryPath: 'D:\\CueNote\\Other Library', backupId: 'backup-new', pending: null, allowed: true });
    if (settlementOrder === 'host-first') hostResult.resolve(receipt());
    else cancelAcknowledgement.resolve();
    await settlePromises();
    assert.equal(await coordinator.previewBackup('backup-new'), false);
    assert.deepEqual(previewedLibraries, ['backup-old']);

    if (settlementOrder === 'host-first') cancelAcknowledgement.resolve();
    else hostResult.resolve(receipt());
    await oldRestore;
    assert.equal(await coordinator.previewBackup('backup-new'), true);
    assert.deepEqual(previewedLibraries, ['backup-old', 'backup-new']);
  }
});
