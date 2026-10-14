import assert from 'node:assert/strict';
import test from 'node:test';
import type { Block } from '../src/generated/note-format.ts';
import { fromEditorDocument, toEditorDocument } from '../src/editor-model.ts';

const map: Extract<Block, { type: 'mindMap' }> = {
  type: 'mindMap',
  mapId: '00000000-0000-4000-8000-000000000001',
  rootId: '00000000-0000-4000-8000-000000000002',
  nodes: [
    { id: '00000000-0000-4000-8000-000000000003', text: ' 第一支\n保留换行 ', children: [] },
    { id: '00000000-0000-4000-8000-000000000002', text: '康奈尔 <根> 😀', children: ['00000000-0000-4000-8000-000000000004', '00000000-0000-4000-8000-000000000003'] },
    { id: '00000000-0000-4000-8000-000000000004', text: '', children: [] },
  ],
};

test('editable map round-trips full native structure and order between text and images', () => {
  const blocks: Block[] = [
    { type: 'paragraph', children: [{ type: 'text', text: '前文', marks: [] }] },
    map,
    { type: 'image', assetId: '00000000-0000-4000-8000-000000000005', displayWidthPx: 320 },
    { type: 'paragraph', children: [{ type: 'text', text: '后文', marks: [] }] },
  ];
  const doc = toEditorDocument(blocks, false);
  assert.deepEqual(doc.content?.[1], { type: 'mindMap', attrs: { mapId: map.mapId, rootId: map.rootId, nodes: map.nodes } });
  assert.deepEqual(fromEditorDocument(doc, false), blocks);
});

test('maps are rejected outside top-level body blocks without dropping their structure', () => {
  assert.throws(() => toEditorDocument([map], true), /mind maps are not supported in this section/);
  assert.throws(() => fromEditorDocument(toEditorDocument([map], false), true), /mind maps are not supported in this section/);
  for (const block of [
    { type: 'blockquote', blocks: [map] },
    { type: 'bulletList', items: [{ blocks: [map] }] },
  ] as Block[]) {
    assert.throws(() => toEditorDocument([block], false), /mind maps must be top-level blocks/);
  }
  assert.throws(() => fromEditorDocument({ type: 'doc', content: [{ type: 'blockquote', content: toEditorDocument([map], false).content }] }, false), /mind maps must be top-level blocks/);
});

test('malformed or unknown map content fails explicitly instead of silently changing the note', () => {
  const broken: unknown[] = [
    { ...map, style: 'red' },
    { ...map, mapId: map.mapId.toUpperCase().replace('00000000', 'AAAAAAAA') },
    { ...map, rootId: '00000000-0000-4000-8000-000000000099' },
    { ...map, nodes: [] },
    { ...map, nodes: [...map.nodes, map.nodes[0]] },
    { ...map, nodes: map.nodes.map(node => ({ ...node, image: 'hidden-data' })) },
    { ...map, nodes: map.nodes.map(node => node.id === map.rootId ? { ...node, children: [map.rootId] } : node) },
    { ...map, nodes: map.nodes.map(node => node.id === map.rootId ? { ...node, children: [map.nodes[0].id, map.nodes[0].id] } : node) },
    { ...map, nodes: map.nodes.map(node => ({ ...node, children: [] })) },
  ];
  for (const value of broken) {
    const snapshot = structuredClone(value);
    assert.throws(() => toEditorDocument([value as Block], false));
    assert.deepEqual(value, snapshot);
  }
  const doc = toEditorDocument([map], false);
  doc.content![0].attrs!.preview = 'do-not-discard';
  assert.throws(() => fromEditorDocument(doc, false), /unsupported field: preview/);
});

function id(value: number) {
  return `00000000-0000-4000-8000-${value.toString(16).padStart(12, '0')}`;
}

function chain(size: number, offset: number): typeof map {
  return {
    type: 'mindMap', mapId: id(offset), rootId: id(offset + 1),
    nodes: Array.from({ length: size }, (_, index) => ({
      id: id(offset + index + 1), text: `节点 ${index + 1}`,
      children: index < size - 1 ? [id(offset + index + 2)] : [],
    })),
  };
}

test('all maps share the 5,000-node budget and identity space', () => {
  for (const total of [4_999, 5_000]) {
    const blocks = [chain(2_500, 10), chain(total - 2_500, 10_000)];
    assert.deepEqual(fromEditorDocument(toEditorDocument(blocks, false), false), blocks);
  }
  assert.throws(() => toEditorDocument([chain(2_500, 10), chain(2_501, 10_000)], false), /5,000/);
  const oversizedDoc = toEditorDocument([chain(2_500, 10)], false);
  oversizedDoc.content!.push(...toEditorDocument([chain(2_501, 10_000)], false).content!);
  assert.throws(() => fromEditorDocument(oversizedDoc, false), /5,000/);
  assert.throws(() => toEditorDocument([map, structuredClone(map)], false), /重复的导图标识/);
  assert.throws(() => toEditorDocument([map, { ...map, mapId: id(90) }], false), /重复的导图节点标识/);
});

test('a 500-node stress map and deep valid 5,000-node map keep every text and edge', () => {
  for (const size of [500, 5_000]) {
    const blocks = [chain(size, 10)];
    assert.deepEqual(fromEditorDocument(toEditorDocument(blocks, false), false), blocks);
  }
});
