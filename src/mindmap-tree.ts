import type { MindElixirData, NodeObj } from 'mind-elixir';
import type { MindMapNode } from './generated/note-format.ts';
import { readMindMap, type MindMapBlock } from './mindmap-model.ts';

export interface NewMindMapNode {
  id: string;
  text: string;
}

export type MindMapMutation =
  | { type: 'text'; nodeId: string; text: string }
  | { type: 'addChild'; parentId: string; node: NewMindMapNode }
  | { type: 'insertSibling'; targetId: string; side: 'before' | 'after'; node: NewMindMapNode }
  | { type: 'insertParent'; targetId: string; node: NewMindMapNode }
  | { type: 'remove'; nodeIds: string[] }
  | { type: 'move'; nodeIds: string[]; targetId: string; placement: 'in' | 'before' | 'after' };

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('MindElixir 返回了无效的导图数据。');
  return value as Record<string, unknown>;
}

function onlyKeys(value: Record<string, unknown>, allowed: ReadonlySet<string>) {
  const unsupported = Object.keys(value).find(key => !allowed.has(key));
  if (unsupported) throw new Error(`MindElixir 产生了原生格式不支持的节点字段：${unsupported}。`);
}

const mapFields = new Set(['nodeData', 'arrows', 'summaries', 'direction', 'theme', 'compact', 'meta']);
const nodeFields = new Set(['id', 'topic', 'children', 'direction', 'expanded']);

/** Build one MindElixir tree without changing the UUIDs or plain-text topics. */
export function toMindElixirData(value: unknown): MindElixirData {
  const map = readMindMap(value);
  const converted = new Map<string, NodeObj>();
  for (const node of map.nodes) {
    converted.set(node.id, { id: node.id, topic: node.text, children: [] });
  }
  for (const node of map.nodes) {
    const parent = converted.get(node.id)!;
    parent.children = node.children.map(childId => converted.get(childId)!);
  }
  return { nodeData: converted.get(map.rootId)! };
}

/**
 * Read only the text/tree fields MindElixir can represent in native v1.
 * Visual layout fields are discarded; rich node payloads fail closed.
 */
export function fromMindElixirData(value: unknown, previous: MindMapBlock): MindMapBlock {
  const data = record(value);
  onlyKeys(data, mapFields);
  for (const key of ['arrows', 'summaries'] as const) {
    const collection = data[key];
    if (collection !== undefined && (!Array.isArray(collection) || collection.length !== 0)) {
      throw new Error('CueNote 导图只支持节点文字和层级，MindElixir 的连线或摘要无法保存。');
    }
  }

  const root = record(data.nodeData);
  const pending: unknown[] = [root];
  const seen = new Set<string>();
  const flattened: MindMapNode[] = [];
  while (pending.length > 0) {
    const item = record(pending.pop());
    onlyKeys(item, nodeFields);
    if (typeof item.id !== 'string' || typeof item.topic !== 'string') {
      throw new Error('MindElixir 节点必须包含 UUID 和纯文字。');
    }
    if (seen.has(item.id)) throw new Error('MindElixir 导图含有重复节点。');
    seen.add(item.id);
    const children = item.children === undefined ? [] : item.children;
    if (!Array.isArray(children)) throw new Error('MindElixir 子节点列表无效。');
    const childIds = children.map(child => {
      const childRecord = record(child);
      if (typeof childRecord.id !== 'string') throw new Error('MindElixir 子节点标识无效。');
      return childRecord.id;
    });
    flattened.push({ id: item.id, text: item.topic, children: childIds });
    for (let index = children.length - 1; index >= 0; index -= 1) pending.push(children[index]);
  }

  // Keep existing flat record order stable, and append newly created nodes in tree order.
  const byId = new Map(flattened.map(node => [node.id, node]));
  const ordered = previous.nodes.filter(node => byId.has(node.id)).map(node => byId.get(node.id)!);
  const priorIds = new Set(previous.nodes.map(node => node.id));
  for (const node of flattened) if (!priorIds.has(node.id)) ordered.push(node);

  return readMindMap({
    type: 'mindMap',
    mapId: previous.mapId,
    rootId: root.id,
    nodes: ordered,
  });
}

export function createMindMap(text = '新建导图'): MindMapBlock {
  const rootId = createUuidV4();
  return {
    type: 'mindMap',
    mapId: createUuidV4(),
    rootId,
    nodes: [{ id: rootId, text, children: [] }],
  };
}

export function createUuidV4(): string {
  const cryptoApi = globalThis.crypto;
  if (!cryptoApi?.getRandomValues) throw new Error('当前环境无法安全生成导图标识。');
  const bytes = cryptoApi.getRandomValues(new Uint8Array(16));
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function cloneMap(map: MindMapBlock): MindMapBlock {
  return {
    type: 'mindMap',
    mapId: map.mapId,
    rootId: map.rootId,
    nodes: map.nodes.map(node => ({ id: node.id, text: node.text, children: [...node.children] })),
  };
}

function parentOf(map: MindMapBlock, nodeId: string) {
  return map.nodes.find(node => node.children.includes(nodeId));
}

function validateCandidate(map: MindMapBlock) {
  return readMindMap(map);
}

export function applyMindMapMutation(mapValue: unknown, mutation: MindMapMutation): MindMapBlock {
  const map = cloneMap(readMindMap(mapValue));
  const byId = new Map(map.nodes.map(node => [node.id, node]));

  if (mutation.type === 'text') {
    const node = byId.get(mutation.nodeId);
    if (!node) throw new Error('要编辑的导图节点已不存在。');
    node.text = mutation.text;
    return validateCandidate(map);
  }

  if (mutation.type === 'addChild') {
    const parent = byId.get(mutation.parentId);
    if (!parent || byId.has(mutation.node.id)) throw new Error('无法添加重复或不存在的导图节点。');
    parent.children.push(mutation.node.id);
    map.nodes.push({ id: mutation.node.id, text: mutation.node.text, children: [] });
    return validateCandidate(map);
  }

  if (mutation.type === 'insertSibling') {
    const target = byId.get(mutation.targetId);
    const parent = target && parentOf(map, target.id);
    if (!target || !parent || byId.has(mutation.node.id)) throw new Error('根节点不能添加同级节点。');
    const index = parent.children.indexOf(target.id) + (mutation.side === 'after' ? 1 : 0);
    parent.children.splice(index, 0, mutation.node.id);
    map.nodes.push({ id: mutation.node.id, text: mutation.node.text, children: [] });
    return validateCandidate(map);
  }

  if (mutation.type === 'insertParent') {
    const target = byId.get(mutation.targetId);
    if (!target || byId.has(mutation.node.id)) throw new Error('无法插入导图父节点。');
    const parent = parentOf(map, target.id);
    if (parent) {
      const index = parent.children.indexOf(target.id);
      parent.children[index] = mutation.node.id;
    } else {
      map.rootId = mutation.node.id;
    }
    map.nodes.push({ id: mutation.node.id, text: mutation.node.text, children: [target.id] });
    return validateCandidate(map);
  }

  if (mutation.type === 'remove') {
    const topmost = mutation.nodeIds.filter((id, index, ids) => {
      if (id === map.rootId || !byId.has(id) || ids.indexOf(id) !== index) return false;
      let parent = parentOf(map, id);
      while (parent) {
        if (ids.includes(parent.id)) return false;
        parent = parentOf(map, parent.id);
      }
      return true;
    });
    if (topmost.length === 0) throw new Error('导图根节点不能删除。');
    const removed = new Set<string>();
    const stack = [...topmost];
    while (stack.length) {
      const id = stack.pop()!;
      if (removed.has(id)) continue;
      removed.add(id);
      stack.push(...(byId.get(id)?.children ?? []));
    }
    for (const id of topmost) parentOf(map, id)?.children.splice(parentOf(map, id)!.children.indexOf(id), 1);
    map.nodes = map.nodes.filter(node => !removed.has(node.id));
    return validateCandidate(map);
  }

  const selectedIds = [...new Set(mutation.nodeIds)];
  if (selectedIds.includes(map.rootId) || selectedIds.some(id => !byId.has(id)) || !byId.has(mutation.targetId)) {
    throw new Error('无法移动不存在的导图节点或根节点。');
  }
  const selected = selectedIds.filter(id => {
    let parent = parentOf(map, id);
    while (parent) {
      if (selectedIds.includes(parent.id)) return false;
      parent = parentOf(map, parent.id);
    }
    return true;
  });
  const descendants = new Set<string>();
  const stack = [...selected];
  while (stack.length) {
    const id = stack.pop()!;
    if (descendants.has(id)) continue;
    descendants.add(id);
    stack.push(...byId.get(id)!.children);
  }
  if (selected.length === 0 || descendants.has(mutation.targetId)) throw new Error('导图节点不能移动到自身或后代之下。');

  const target = byId.get(mutation.targetId)!;
  const targetParent = mutation.placement === 'in' ? target : parentOf(map, target.id);
  if (!targetParent) throw new Error('根节点没有同级位置。');
  const originalParent = new Map(selected.map(id => [id, parentOf(map, id)!]));
  for (const id of selected) {
    const parent = originalParent.get(id)!;
    parent.children.splice(parent.children.indexOf(id), 1);
  }
  const insertionIndex = mutation.placement === 'in'
    ? targetParent.children.length
    : targetParent.children.indexOf(target.id) + (mutation.placement === 'after' ? 1 : 0);
  targetParent.children.splice(insertionIndex, 0, ...selected);
  return validateCandidate(map);
}

