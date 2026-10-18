import assert from 'node:assert/strict';
import test from 'node:test';
import { createExternalChangesCoordinator } from '../src/external-changes.ts';
import type { ExternalChangeProbe, LibraryView } from '../src/workspace-api.ts';

type Probe = ExternalChangeProbe;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

class FakeClock {
  private nextId = 1;
  private readonly timers = new Map<number, () => void>();

  setTimeout = (callback: () => void) => {
    const id = this.nextId++;
    this.timers.set(id, callback);
    return id;
  };

  clearTimeout = (handle: unknown) => {
    if (typeof handle === 'number') this.timers.delete(handle);
  };

  fire() {
    const pending = [...this.timers.values()];
    this.timers.clear();
    pending.forEach((callback) => callback());
  }
}

async function settlePromises() {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
}

test('pending file notifications get a full quiet interval after foreground saving settles', async () => {
  let now = 0, nextId = 0, saving = true, probes = 0;
  const timers = new Map<number, {at:number; callback:()=>void}>();
  const advance = async (milliseconds: number) => {
    now += milliseconds;
    for (const [id, timer] of [...timers]) {
      if (timer.at <= now) { timers.delete(id); timer.callback(); }
    }
    await settlePromises();
  };
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    canProbe: () => !saving,
    probe: async () => { probes += 1; return changedProbe('session-1'); },
    onChanged: () => {},
    setTimeout: (callback,delay) => { const id=++nextId; timers.set(id,{at:now+delay,callback}); return id; },
    clearTimeout: handle => { timers.delete(handle as number); },
  });
  coordinator.schedule();
  await advance(250); // A notification arrived during the save; retry is due at 500.
  await advance(245);
  saving = false;
  coordinator.afterSave();
  await advance(5);
  assert.equal(probes,0,'the old retry must not immediately take the workspace lock');
  await advance(244);
  assert.equal(probes,0);
  await advance(1);
  assert.equal(probes,1,'the retained notification still performs a complete probe');
  coordinator.afterSave();
  await advance(250);
  assert.equal(probes,1,'saving does not invent an extra notification');
  coordinator.dispose();
});

function changedProbe(sessionId: string | null): Probe {
  return { sessionId, status: 'changed', revision: 'rev-2', message: null, library: null, sessionReadOnlyReason: null };
}

function scannedLibrary(scanState: LibraryView['scanState'] = 'complete'): LibraryView {
  return {
    path: '/library', scanState, defaultNotebookName: 'Notes', defaultNotebookId: null,
    notebooks: [], notes: [], trashNotes: [], warnings: [],
  };
}

test('initial library scan can finish during continuous typing without applying content or permissions', async () => {
  const clock = new FakeClock();
  const responses = [deferred<Probe>(), deferred<Probe>(), deferred<Probe>()];
  const libraries: LibraryView[] = [];
  let probes = 0, reconciled = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async () => responses[probes++].promise,
    onLibraryRefreshed: library => { libraries.push(library); },
    onChanged: () => { reconciled += 1; },
    onProbeResult: () => { reconciled += 1; },
    setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout,
  });
  const complete = scannedLibrary();
  void coordinator.checkNow();
  for (let index = 0; index < 2; index += 1) {
    coordinator.noteInputChanged();
    responses[index].resolve({
      ...changedProbe('session-1'), status: index === 0 ? 'changed' : 'unchanged',
      sessionReadOnlyReason: 'metadata changed', library: complete,
    });
    await settlePromises();
    assert.equal(libraries.length, index + 1, 'typing cannot starve the completed list');
    assert.equal(reconciled, 0, 'content and read-only changes still require a current input token');
    clock.fire();
    await settlePromises();
    assert.equal(probes, index + 2, 'a reconciliation retry remains queued');
  }
  responses[2].resolve({ ...changedProbe('session-1'), library: complete });
  await settlePromises();
  assert.equal(reconciled, 1, 'reconcile when the input token is current');
  coordinator.dispose();
});

for (const invalidation of ['session', 'epoch', 'disposed', 'resumed', 'response-session'] as const) {
  test(`library callbacks reject a stale ${invalidation}`, async () => {
    const clock = new FakeClock();
    const response = deferred<Probe>();
    let sessionId = 'session-1', libraries = 0, reconciled = 0;
    const coordinator = createExternalChangesCoordinator({
      getSessionId: () => sessionId,
      probe: async () => response.promise,
      onLibraryRefreshed: () => { libraries += 1; },
      onChanged: () => { reconciled += 1; },
      onProbeResult: () => { reconciled += 1; },
      setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout,
    });
    const operation = coordinator.checkNow();
    if (invalidation === 'session') sessionId = 'session-2';
    if (invalidation === 'epoch') coordinator.sessionChanged();
    if (invalidation === 'disposed' || invalidation === 'resumed') coordinator.dispose();
    if (invalidation === 'resumed') coordinator.resume();
    response.resolve({
      ...changedProbe(invalidation === 'response-session' ? 'session-2' : 'session-1'),
      library: scannedLibrary(),
    });
    await operation;
    assert.equal(libraries, 0);
    assert.equal(reconciled, 0);
    coordinator.dispose();
  });
}

test('a deferred scan cannot publish a list, while an unavailable result can leave pending', async () => {
  const clock = new FakeClock();
  const states: LibraryView['scanState'][] = [];
  let probes = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async () => ({
      ...changedProbe('session-1'), status: probes++ === 0 ? 'deferred' : 'unreadable',
      library: scannedLibrary('unavailable'),
    }),
    onChanged: () => {},
    onLibraryRefreshed: library => { states.push(library.scanState); },
    setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout,
  });
  await coordinator.checkNow();
  assert.deepEqual(states, []);
  clock.fire();
  await settlePromises();
  assert.deepEqual(states, ['unavailable']);
  coordinator.dispose();
});

test('startup notifications wait for the verified session and preserve input during the initial full probe', async () => {
  const clock = new FakeClock();
  let initializing = true;
  let sessionId: string | null = null;
  const first = deferred<Probe>();
  const probed: (string | null)[] = [];
  let applied = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => sessionId,
    canProbe: () => !initializing,
    probe: async id => {
      probed.push(id);
      return probed.length === 1 ? first.promise : { ...changedProbe(id), status: 'unchanged' };
    },
    onChanged: () => { applied += 1; },
    onProbeResult: () => { applied += 1; },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.schedule(); // Focus/restore events may arrive before get_workspace.
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, [], 'do not scan the whole library ahead of startup');
  sessionId = 'verified-startup-session';
  coordinator.sessionChanged();
  initializing = false;
  coordinator.schedule(); // The post-startup probe is mandatory, without a file event.
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, [sessionId]);
  coordinator.noteInputChanged();
  first.resolve(changedProbe(sessionId));
  await settlePromises();
  assert.equal(applied, 0, 'the initial scan must not apply stale content over new input');
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, [sessionId, sessionId], 'stale startup results still get a fresh full probe');
  assert.equal(applied, 1);
  coordinator.dispose();
});

test('a confirmed library save queues a full probe without any file notification', async () => {
  const clock = new FakeClock();
  const probed: (string | null)[] = [];
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async (sessionId) => {
      probed.push(sessionId);
      return { ...changedProbe(sessionId), status: 'unchanged' };
    },
    onChanged: () => {},
    onProbeResult: () => {},
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });

  assert.deepEqual(probed, [], 'nothing requested a probe yet');
  coordinator.librarySaved();
  clock.fire();
  await settlePromises();

  assert.deepEqual(probed, ['session-1'], 'the committed save must request its own fingerprint check');
  coordinator.dispose();
});

test('repeated library saves coalesce into one debounced probe', async () => {
  const clock = new FakeClock();
  const probed: (string | null)[] = [];
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async (sessionId) => {
      probed.push(sessionId);
      return { ...changedProbe(sessionId), status: 'unchanged' };
    },
    onChanged: () => {},
    onProbeResult: () => {},
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });

  coordinator.librarySaved();
  coordinator.librarySaved();
  coordinator.librarySaved();
  clock.fire();
  await settlePromises();

  assert.deepEqual(probed, ['session-1']);
  clock.fire();
  await settlePromises();
  assert.equal(probed.length, 1, 'a burst of saves leaves exactly one trailing probe');
  coordinator.dispose();
});

test('a library save probe waits for an in-flight save to settle before scanning', async () => {
  const clock = new FakeClock();
  let saving = true;
  const probed: (string | null)[] = [];
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    canProbe: () => !saving,
    probe: async (sessionId) => {
      probed.push(sessionId);
      return { ...changedProbe(sessionId), status: 'unchanged' };
    },
    onChanged: () => {},
    onProbeResult: () => {},
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });

  coordinator.librarySaved();
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, [], 'the scan must not start while the write is active');

  saving = false;
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, ['session-1'], 'the queued save probe still runs after the write settles');
  coordinator.dispose();
});

test('a probe deferred by an in-flight save is retried by the next confirmed save', async () => {
  const clock = new FakeClock();
  let saving = false;
  let sessionId = 'session-1';
  const probed: (string | null)[] = [];
  let applied = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => sessionId,
    canProbe: () => !saving,
    probe: async (id) => {
      probed.push(id);
      if (probed.length === 1) {
        saving = true;
        return { ...changedProbe(id), status: 'deferred', revision: null };
      }
      return { ...changedProbe(id), status: 'unchanged' };
    },
    onChanged: () => { applied += 1; },
    onProbeResult: () => { applied += 1; },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });

  await coordinator.checkNow();
  assert.deepEqual(probed, ['session-1']);
  assert.equal(applied, 0, 'a deferred scan cannot install a result');
  sessionId = 'session-2';
  coordinator.sessionChanged();
  saving = false;
  coordinator.librarySaved();
  clock.fire();
  await settlePromises();

  assert.deepEqual(probed, ['session-1', 'session-2'], 'the retry uses the session after the save');
  assert.equal(applied, 1);
  clock.fire();
  await settlePromises();
  assert.equal(probed.length, 2, 'one deferred notification does not keep looping after a save');
  coordinator.dispose();
});

test('a host-deferred probe retries the latest session after saving without a new notification', async () => {
  const clock = new FakeClock();
  let saving = false, sessionId = 'session-1', applied = 0, errors = 0;
  const probed: (string | null)[] = [];
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => sessionId,
    canProbe: () => !saving,
    probe: async id => {
      probed.push(id);
      if (probed.length === 1) {
        saving = true;
        return { ...changedProbe(id), status: 'deferred', revision: null };
      }
      return { ...changedProbe(id), status: 'unchanged' };
    },
    onChanged: () => { errors += 1; },
    onError: () => { errors += 1; },
    onProbeResult: () => { applied += 1; },
    setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout,
  });
  await coordinator.checkNow();
  assert.equal(errors, 0, 'yielding is neither an external conflict nor a read error');
  assert.equal(applied, 0, 'a deferred scan cannot install a result');
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, ['session-1']);
  sessionId = 'session-2';
  coordinator.sessionChanged();
  saving = false;
  coordinator.afterSave();
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, ['session-1', 'session-2']);
  assert.equal(applied, 1);
  assert.equal(errors, 0);
  clock.fire();
  await settlePromises();
  assert.equal(probed.length, 2, 'one retained notification does not loop forever');
  coordinator.dispose();
});

test('disposing a host-deferred probe cancels its retry', async () => {
  const clock = new FakeClock();
  const response = deferred<Probe>();
  let probes = 0, applied = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async () => { probes += 1; return response.promise; },
    onChanged: () => { applied += 1; },
    onProbeResult: () => { applied += 1; },
    setTimeout: clock.setTimeout, clearTimeout: clock.clearTimeout,
  });
  const operation = coordinator.checkNow();
  coordinator.dispose();
  response.resolve({ ...changedProbe('session-1'), status: 'deferred' });
  await operation;
  clock.fire();
  await settlePromises();
  assert.equal(probes, 1);
  assert.equal(applied, 0);
});

test('defers notifications during a save and checks the latest session after it settles', async () => {
  const clock = new FakeClock();
  let saving = true;
  let sessionId = 'session-1';
  const probed: (string | null)[] = [];
  let applied = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => sessionId,
    canProbe: () => !saving,
    probe: async id => { probed.push(id); return changedProbe(id); },
    onChanged: () => { applied += 1; },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });
  coordinator.schedule();
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, [], 'do not queue a full scan behind an in-flight save');
  coordinator.schedule();
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, []);
  sessionId = 'session-2';
  coordinator.sessionChanged();
  saving = false;
  clock.fire();
  await settlePromises();
  assert.deepEqual(probed, ['session-2'], 'the pending notification is retained');
  assert.equal(applied, 1);
  coordinator.dispose();
});

test('does not apply a refresh when the draft changes while external probing is in flight', async () => {
  const clock = new FakeClock();
  const probeResult = deferred<Probe>();
  let applied = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async () => probeResult.promise,
    onChanged: async (_result, _token, isCurrent) => {
      if (isCurrent()) applied += 1;
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });

  coordinator.schedule();
  clock.fire();
  await settlePromises();
  coordinator.noteInputChanged();
  probeResult.resolve(changedProbe('session-1'));
  await settlePromises();

  assert.equal(applied, 0);
  coordinator.dispose();
});

test('does not apply a refresh after the active note session changes during probing', async () => {
  const clock = new FakeClock();
  const probeResult = deferred<Probe>();
  let sessionId: string | null = 'session-1';
  let applied = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => sessionId,
    probe: async () => probeResult.promise,
    onChanged: async (_result, _token, isCurrent) => {
      if (isCurrent()) applied += 1;
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });

  coordinator.schedule();
  clock.fire();
  await settlePromises();
  sessionId = 'session-2';
  coordinator.sessionChanged();
  probeResult.resolve(changedProbe('session-1'));
  await settlePromises();

  assert.equal(applied, 0);
  coordinator.dispose();
});

test('routes missing and unreadable probes through protected reconciliation', async () => {
  const protectedStatuses: Probe['status'][] = [];
  const statuses: Probe['status'][] = ['missing', 'unreadable'];
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async (sessionId) => ({
      ...changedProbe(sessionId),
      status: statuses.shift()!,
    }),
    onChanged: async (result) => { protectedStatuses.push(result.status); },
  });

  await coordinator.checkNow();
  await coordinator.checkNow();

  assert.deepEqual(protectedStatuses, ['missing', 'unreadable']);
  coordinator.dispose();
});

test('awaits current probe-error protection and passes its epoch token', async () => {
  const protection = deferred<void>();
  let observedToken: { sessionId: string | null; epoch: number; inputVersion: number } | null = null;
  let callbackFinished = false;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async () => { throw new Error('temporary read error'); },
    onError: async (_error, token, isCurrent) => {
      assert.equal(isCurrent(), true);
      observedToken = token;
      await protection.promise;
      callbackFinished = true;
    },
  });

  const check = coordinator.checkNow();
  await settlePromises();
  assert.deepEqual(observedToken, { sessionId: 'session-1', epoch: 0, inputVersion: 0 });
  assert.equal(callbackFinished, false);
  protection.resolve();
  await check;

  assert.equal(callbackFinished, true);
  coordinator.dispose();
});

test('ignores a probe error after the draft changes', async () => {
  const clock = new FakeClock();
  const probeResult = deferred<Probe>();
  let handledErrors = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async () => probeResult.promise,
    onChanged: async () => assert.fail('stale probe must not reconcile'),
    onError: async (_error, _token, isCurrent) => {
      assert.equal(isCurrent(), true);
      handledErrors += 1;
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });

  coordinator.schedule();
  clock.fire();
  await settlePromises();
  coordinator.noteInputChanged();
  probeResult.reject(new Error('late read error'));
  await settlePromises();

  assert.equal(handledErrors, 0);
  coordinator.dispose();
});

test('retries after error protection completes when another check was queued', async () => {
  const clock = new FakeClock();
  const protection = deferred<void>();
  let probes = 0;
  let errors = 0;
  let reconciled = 0;
  const coordinator = createExternalChangesCoordinator({
    getSessionId: () => 'session-1',
    probe: async (sessionId) => {
      probes += 1;
      if (probes === 1) throw new Error('temporary read error');
      return changedProbe(sessionId);
    },
    onChanged: async (result, _token, isCurrent) => {
      if (isCurrent() && result.status === 'changed') reconciled += 1;
    },
    onError: async (_error, _token, isCurrent) => {
      if (isCurrent()) errors += 1;
      await protection.promise;
    },
    setTimeout: clock.setTimeout,
    clearTimeout: clock.clearTimeout,
  });

  const firstCheck = coordinator.checkNow();
  await settlePromises();
  const queuedCheck = coordinator.checkNow();
  protection.resolve();
  await Promise.all([firstCheck, queuedCheck]);
  clock.fire();
  await settlePromises();

  assert.equal(probes, 2);
  assert.equal(errors, 1);
  assert.equal(reconciled, 1);
  coordinator.dispose();
});
