import assert from 'node:assert/strict';
import test from 'node:test';
import { Editor } from '@tiptap/core';
import type { EditorView } from '@tiptap/pm/view';
import { createEditorExtensions } from '../src/editor-extensions.ts';
import { fromEditorDocument } from '../src/editor-model.ts';

function typeText(editor: Editor, text: string): void {
  for (const character of text) {
    const { from, to } = editor.state.selection;
    const plugins = editor.state.plugins.length ? editor.state.plugins : editor.extensionManager.plugins;
    const handled = plugins.some(plugin =>
      plugin.props.handleTextInput?.(editor.view as EditorView, from, to, character),
    );
    if (!handled) editor.view.dispatch(editor.state.tr.insertText(character, from, to));
  }
}

function enter(editor: Editor): void {
  const event = { key: 'Enter', keyCode: 13, altKey: false, ctrlKey: false,
    metaKey: false, shiftKey: false, preventDefault() {}, stopPropagation() {} } as KeyboardEvent;
  for (const plugin of editor.extensionManager.plugins) {
    if (plugin.props.handleKeyDown?.(editor.view as EditorView, event)) return;
  }
}

test('typing the advertised task marker creates a task list without an outer bullet', () => {
  for (const simple of [false, true]) {
    const editor = new Editor({ element: null, extensions: createEditorExtensions(simple),
      content: { type: 'doc', content: [{ type: 'paragraph' }] } });
    try {
      typeText(editor, '- [ ] 待办');
      const blocks = fromEditorDocument(editor.getJSON(), simple);
      assert.equal(blocks[0].type, 'taskList');
      assert.deepEqual(blocks, [{ type: 'taskList', items: [{ checked: false,
        blocks: [{ type: 'paragraph', children: [{ type: 'text', text: '待办', marks: [] }] }] }] }]);
    } finally { editor.destroy(); }
  }
});

test('two Enters after a task item return subsequent quote and rule to the root', () => {
  const editor = new Editor({ element: null, extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'paragraph' }] } });
  try {
    typeText(editor, '- [ ] 待办'); enter(editor); enter(editor);
    typeText(editor, '> 引用'); enter(editor); enter(editor);
    typeText(editor, '---');
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false).map(block => block.type),
      ['taskList', 'blockquote', 'horizontalRule', 'paragraph']);
  } finally { editor.destroy(); }
});

test('task markers preserve checked state and existing markers without a bullet prefix', () => {
  for (const [marker, checked] of [['+ [x] ', true], ['* [x] ', true], ['- [(] ', false], ['- [|] ', false], ['[ ] ', false], ['[x] ', true]] as const) {
    const editor = new Editor({ element: null, extensions: createEditorExtensions(false),
      content: { type: 'doc', content: [{ type: 'paragraph' }] } });
    try {
      typeText(editor, marker + '任务');
      const block = fromEditorDocument(editor.getJSON(), false)[0];
      assert.equal(block.type, 'taskList');
      if (block.type !== 'taskList') throw new Error('Expected task list');
      assert.equal(block.items[0].checked, checked);
      assert.equal(editor.state.doc.textContent, '任务');
    } finally { editor.destroy(); }
  }
});

test('converting a marker at the start preserves following text and marks', () => {
  const editor = new Editor({ element: null, extensions: createEditorExtensions(false), content: {
    type: 'doc', content: [{ type: 'bulletList', content: [{ type: 'listItem', content: [
      { type: 'paragraph', content: [{ type: 'text', text: '已有内容', marks: [{ type: 'bold' }] }] },
    ] }] }],
  } });
  try {
    editor.commands.setTextSelection(3);
    typeText(editor, '[ ] ');
    assert.equal(editor.state.doc.firstChild!.type.name, 'taskList');
    assert.equal(editor.state.doc.textContent, '已有内容');
    const paragraph = editor.state.doc.firstChild!.firstChild!.firstChild!;
    assert.equal(paragraph.firstChild!.marks[0].type.name, 'bold');
    typeText(editor, '前缀');
    assert.equal(editor.state.doc.textContent, '前缀已有内容');
    assert.doesNotThrow(() => fromEditorDocument(editor.getJSON(), false));
  } finally { editor.destroy(); }
});

test('converting one bullet keeps preceding and following bullet items intact', () => {
  const paragraph = (text: string) => ({ type: 'paragraph', content: text ? [{ type: 'text', text }] : [] });
  const editor = new Editor({ element: null, extensions: createEditorExtensions(false), content: {
    type: 'doc', content: [{ type: 'bulletList', content: ['之前', '', '之后'].map(text =>
      ({ type: 'listItem', content: [paragraph(text)] })) }],
  } });
  try {
    editor.state.doc.descendants((node, position) => {
      if (node.type.name === 'paragraph' && node.content.size === 0) editor.commands.setTextSelection(position + 1);
    });
    typeText(editor, '[ ] 中间');
    const blocks = fromEditorDocument(editor.getJSON(), false);
    assert.deepEqual(blocks.map(block => block.type), ['bulletList', 'taskList', 'bulletList']);
    assert.equal(editor.state.doc.child(0).textContent, '之前');
    assert.equal(editor.state.doc.child(1).textContent, '中间');
    assert.equal(editor.state.doc.child(2).textContent, '之后');
    enter(editor); typeText(editor, '下一项');
    assert.equal(editor.state.doc.child(1).childCount, 2);
    assert.equal(editor.state.doc.child(2).textContent, '之后');
  } finally { editor.destroy(); }
});

test('a nested task stays inside its containing list item', () => {
  const editor = new Editor({ element: null, extensions: createEditorExtensions(false), content: {
    type: 'doc', content: [{ type: 'bulletList', content: [{ type: 'listItem', content: [
      { type: 'paragraph', content: [{ type: 'text', text: '外层' }] },
      { type: 'bulletList', content: [{ type: 'listItem', content: [{ type: 'paragraph' }] }] },
    ] }] }],
  } });
  try {
    editor.state.doc.descendants((node, position) => {
      if (node.type.name === 'paragraph' && node.content.size === 0) editor.commands.setTextSelection(position + 1);
    });
    typeText(editor, '[ ] 内层');
    const root = editor.state.doc.firstChild!;
    assert.equal(editor.state.doc.childCount, 1);
    assert.equal(root.type.name, 'bulletList');
    assert.equal(root.firstChild!.child(0).textContent, '外层');
    assert.equal(root.firstChild!.child(1).type.name, 'taskList');
    assert.equal(root.firstChild!.child(1).textContent, '内层');
    assert.doesNotThrow(() => fromEditorDocument(editor.getJSON(), false));
  } finally { editor.destroy(); }
});

test('ordinary and ordered lists still exit after two Enters', () => {
  for (const prefix of ['- ', '1. ']) {
    const editor = new Editor({ element: null, extensions: createEditorExtensions(false),
      content: { type: 'doc', content: [{ type: 'paragraph' }] } });
    try {
      typeText(editor, prefix + '列表'); enter(editor); enter(editor); typeText(editor, '正文');
      assert.equal(editor.state.doc.childCount, 2);
      assert.equal(editor.state.doc.child(1).type.name, 'paragraph');
      assert.equal(editor.state.doc.child(1).textContent, '正文');
    } finally { editor.destroy(); }
  }
});

test('the conversion can be undone as an input rule without losing typed markers', () => {
  const editor = new Editor({ element: null, extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'paragraph' }] } });
  try {
    editor.view.updateState(editor.state.reconfigure({ plugins: editor.extensionManager.plugins }));
    typeText(editor, '- [ ] ');
    assert.equal(editor.state.doc.firstChild!.type.name, 'taskList');
    assert.equal(editor.commands.undoInputRule(), true);
    assert.equal(editor.state.doc.firstChild!.type.name, 'bulletList');
    assert.equal(editor.state.doc.textContent, '[ ] ');
    assert.doesNotThrow(() => fromEditorDocument(editor.getJSON(), false));
  } finally { editor.destroy(); }
});

test('the toolbar task-list command remains reversible', () => {
  const editor = new Editor({ element: null, extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: '工具栏' }] }] } });
  try {
    assert.equal(editor.commands.toggleTaskList(), true);
    assert.equal(editor.state.doc.firstChild!.type.name, 'taskList');
    assert.equal(editor.state.doc.textContent, '工具栏');
    assert.equal(editor.commands.toggleTaskList(), true);
    assert.equal(editor.state.doc.firstChild!.type.name, 'paragraph');
    assert.equal(editor.state.doc.textContent, '工具栏');
  } finally { editor.destroy(); }
});
