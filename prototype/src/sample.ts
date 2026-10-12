import type { MindMapData, NoteSnapshot } from './types';

export function makeMindMap(title = '学习方法的核心结构'): MindMapData {
  const id = () => globalThis.crypto?.randomUUID?.() ?? `node-${Math.random().toString(36).slice(2)}`;
  return {
    nodeData: {
      id: id(),
      topic: title,
      children: [
        { id: id(), topic: '先理解问题', children: [{ id: id(), topic: '明确目标' }] },
        { id: id(), topic: '整理证据', children: [{ id: id(), topic: '记录线索' }] },
        { id: id(), topic: '回顾并行动' },
      ],
    },
    linkData: [],
  };
}

export function makeSampleSnapshot(): NoteSnapshot {
  return {
    document: {
      schemaVersion: 1,
      id: 'prototype-cornell-note',
      title: '把阅读变成可复用的知识',
      cue: {
        type: 'doc',
        content: [
          { type: 'paragraph', content: [{ type: 'text', text: '为什么读完就忘？' }] },
          { type: 'paragraph', content: [{ type: 'text', text: '怎样把线索变成行动？' }] },
          { type: 'paragraph', content: [{ type: 'text', text: '复习时先问自己什么？' }] },
        ],
      },
      content: {
        type: 'doc',
        content: [
          { type: 'heading', attrs: { level: 2 }, content: [{ type: 'text', text: '从摘录走向理解' }] },
          {
            type: 'paragraph',
            content: [
              { type: 'text', text: '阅读时，先用自己的话写下段落在回答什么问题。' },
              { type: 'text', text: '这样做会把注意力从复制句子移到理解关系上。', marks: [{ type: 'bold' }] },
            ],
          },
          { type: 'paragraph', content: [{ type: 'text', text: '随后记录一个例子、一条证据，以及仍然不确定的地方。' }] },
          {
            type: 'bulletList',
            content: [
              { type: 'listItem', content: [{ type: 'paragraph', content: [{ type: 'text', text: '先写问题，再补充答案' }] }] },
              { type: 'listItem', content: [{ type: 'paragraph', content: [{ type: 'text', text: '把相近线索合并成主题' }] }] },
              { type: 'listItem', content: [{ type: 'paragraph', content: [{ type: 'text', text: '留一个下一步可以执行的动作' }] }] },
            ],
          },
          {
            type: 'mindMap',
            attrs: { data: makeMindMap() },
          },
          {
            type: 'paragraph',
            content: [{ type: 'text', text: '下次复习时，遮住内容区，只看左侧线索并尝试回忆。' }],
          },
        ],
      },
      summary: {
        type: 'doc',
        content: [
          {
            type: 'paragraph',
            content: [
              { type: 'text', text: '好笔记把阅读整理成问题、证据与行动。' },
              { type: 'text', text: '先主动回忆，再回到正文核对。', marks: [{ type: 'bold' }] },
            ],
          },
        ],
      },
    },
    assets: [],
  };
}
