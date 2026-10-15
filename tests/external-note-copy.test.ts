import assert from 'node:assert/strict';
import test from 'node:test';
import { createSaveCoordinator } from '../src/save-coordinator.ts';
import { publishDraftSnapshot } from '../src/publish-draft-snapshot.ts';

test('copying an external draft pauses pending autosave and publishes the settled content without writing the source', async () => {
  let sourceWrites = 0;
  let input = { title: '未保存草稿' };
  let timer: (() => void) | null = null;
  const coordinator = createSaveCoordinator<{ title: string }, void>({
    save: async () => { sourceWrites += 1; },
    onSaved: () => {}, onState: () => {},
    setTimeout: callback => { timer = callback; return 1; },
    clearTimeout: () => { timer = null; },
  });
  coordinator.reset('external-session', { title: '磁盘原稿' });
  coordinator.edit('external-session', input);
  let published: unknown;
  const receipt = await publishDraftSnapshot({
    pauseSaving: () => coordinator.pauseAutomaticSavingAndWait(),
    settleEditors: async () => { input = { title: '输入法与导图整理后的草稿' }; coordinator.edit('external-session', input); },
    snapshot: () => ({ sessionId: 'external-session', content: { ...input } }),
    publish: async snapshot => { published = snapshot; return { sessionId: 'new-library-session' }; },
  });
  assert.equal(sourceWrites, 0);
  assert.equal(timer, null);
  assert.deepEqual(published, { sessionId: 'external-session', content: { title: '输入法与导图整理后的草稿' } });
  assert.deepEqual(receipt, { sessionId: 'new-library-session' });
  assert.equal(coordinator.getState().isDirty, true);
  coordinator.dispose();
});
