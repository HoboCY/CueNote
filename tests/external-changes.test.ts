import assert from 'node:assert/strict';
import test from 'node:test';
import { createExternalChangesCoordinator } from '../src/external-changes.ts';

type Probe = {
  sessionId: string | null;
  status: 'unchanged' | 'changed' | 'missing' | 'unreadable' | 'deferred';
  revision: string | null;
  message: string | null;
  library: null;
};

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
  return { sessionId, status: 'changed', revision: 'rev-2', message: null, library: null };
}

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
