import assert from 'node:assert/strict';
import test from 'node:test';
import { continueFromVerifiedCopyOrProtect } from '../src/conflict-transition.ts';

type Content = { title: string; body: string };

const sameContent = (left: Content, right: Content) =>
  left.title === right.title && left.body === right.body;

const finalSnapshot = { sessionId: 'session-2', content: { title: 'Final title', body: 'Settled draft' } };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => { resolve = resolvePromise; });
  return { promise, resolve };
}

test('an exact copy of the final settled session snapshot allows the explicit transition', async () => {
  let protectionAttempts = 0;
  const choice = await continueFromVerifiedCopyOrProtect(
    finalSnapshot,
    { sessionId: finalSnapshot.sessionId, content: { ...finalSnapshot.content } },
    sameContent,
    () => true,
    async () => { protectionAttempts += 1; return 'protected'; },
  );

  assert.deepEqual(choice, { kind: 'continue' });
  assert.equal(protectionAttempts, 0);
});

test('input changes, a different session, or an expired copy still require protection', async () => {
  const staleCopies = [
    { sessionId: finalSnapshot.sessionId, content: { ...finalSnapshot.content, body: 'Edited after copy' } },
    { sessionId: 'session-1', content: { ...finalSnapshot.content } },
    null,
  ];

  for (const copy of staleCopies) {
    let protectedSnapshot: typeof finalSnapshot | null = null;
    const choice = await continueFromVerifiedCopyOrProtect(
      finalSnapshot,
      copy,
      sameContent,
      () => true,
      async (snapshot) => { protectedSnapshot = snapshot; return 'protected'; },
    );

    assert.deepEqual(choice, { kind: 'protect', result: 'protected' });
    assert.deepEqual(protectedSnapshot, finalSnapshot);
  }
});

test('cancellation during protection waits for it but suppresses the pending transition', async () => {
  const protection = deferred<{ status: string }>();
  let transitionIsCurrent = true;
  let protectionCompleted = false;
  let transitionRuns = 0;
  const choicePromise = continueFromVerifiedCopyOrProtect(
    finalSnapshot,
    null,
    sameContent,
    () => transitionIsCurrent,
    async () => {
      const result = await protection.promise;
      protectionCompleted = true;
      return result;
    },
  );

  await Promise.resolve();
  transitionIsCurrent = false;
  protection.resolve({ status: 'refreshed' });
  const choice = await choicePromise;
  if (choice.kind === 'continue' || choice.kind === 'protect') transitionRuns += 1;

  assert.deepEqual(choice, { kind: 'cancelled', result: { status: 'refreshed' } });
  assert.equal(protectionCompleted, true);
  assert.equal(transitionRuns, 0);
});
