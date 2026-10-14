import type { Block } from './generated/note-format.ts';

export type MindMapBlock = Extract<Block, { type: 'mindMap' }>;
export const MAX_MIND_MAP_NODES = 5_000;

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('导图结构必须是对象。');
  return value as Record<string, unknown>;
}

function exact(value: Record<string, unknown>, keys: string[]) {
  if (Object.keys(value).some(key => !keys.includes(key))) throw new Error('导图包含不支持的字段。');
}

function uuid(value: unknown): string {
  if (typeof value !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value)) {
    throw new Error('导图和节点标识必须是小写 UUIDv4。');
  }
  return value;
}

/** Copy only the version-1 canonical structure; never persist editor-private data. */
export function readMindMap(input: unknown): MindMapBlock {
  const value = record(input);
  exact(value, ['type', 'mapId', 'rootId', 'nodes']);
  if (value.type !== 'mindMap') throw new Error('导图类型无效。');
  const mapId = uuid(value.mapId);
  const rootId = uuid(value.rootId);
  if (!Array.isArray(value.nodes) || !value.nodes.length) throw new Error('导图必须包含根节点。');
  if (value.nodes.length > MAX_MIND_MAP_NODES) throw new Error('单篇笔记的导图合计不能超过 5,000 个节点。');
  const nodes = value.nodes.map(inputNode => {
    const node = record(inputNode);
    exact(node, ['id', 'text', 'children']);
    if (typeof node.text !== 'string' || !Array.isArray(node.children)) throw new Error('导图节点文字或子节点列表无效。');
    return { id: uuid(node.id), text: node.text, children: node.children.map(uuid) };
  });
  const byId = new Map(nodes.map(node => [node.id, node]));
  if (byId.size !== nodes.length || !byId.has(rootId)) throw new Error('导图含重复节点，或根节点不存在。');
  const parents = new Set<string>();
  for (const node of nodes) {
    for (const childId of node.children) {
      if (!byId.has(childId) || parents.has(childId) || childId === rootId) {
        throw new Error('导图子节点不存在、重复或具有多个父节点。');
      }
      parents.add(childId);
    }
  }
  // Flat native trees can legitimately be deep. Avoid recursive validation.
  const visited = new Set<string>();
  const stack = [rootId];
  while (stack.length) {
    const id = stack.pop()!;
    if (visited.has(id)) throw new Error('导图不能包含循环。');
    visited.add(id);
    stack.push(...byId.get(id)!.children);
  }
  if (visited.size !== nodes.length) throw new Error('导图包含未连接的节点或循环。');
  return { type: 'mindMap', mapId, rootId, nodes };
}

/** The note budget and identity space span all top-level maps, not each map. */
export function assertMindMapCollection(blocks: Block[]): void {
  let count = 0;
  const maps = new Set<string>();
  const nodes = new Set<string>();
  for (const block of blocks) {
    if (block.type !== 'mindMap') continue;
    count += block.nodes.length;
    if (count > MAX_MIND_MAP_NODES) throw new Error('单篇笔记的导图合计不能超过 5,000 个节点。');
    if (maps.has(block.mapId)) throw new Error('笔记中存在重复的导图标识。');
    maps.add(block.mapId);
    for (const node of block.nodes) {
      if (nodes.has(node.id)) throw new Error('笔记中存在重复的导图节点标识。');
      nodes.add(node.id);
    }
  }
}
