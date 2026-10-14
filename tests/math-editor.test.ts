import assert from 'node:assert/strict';
import test from 'node:test';
import { Editor } from '@tiptap/core';
import { EditorState } from '@tiptap/pm/state';
import { undo } from '@tiptap/pm/history';
import type { EditorView } from '@tiptap/pm/view';
import { createEditorExtensions } from '../src/editor-extensions.ts';
import { fromEditorDocument } from '../src/editor-model.ts';

function typeText(editor: Editor, text: string): void {
  for (const character of text) {
    const { from, to } = editor.state.selection;
    const view = editor.view as EditorView;
    const handled = editor.extensionManager.plugins.some(plugin =>
      plugin.props.handleTextInput?.(view, from, to, character),
    );
    if (!handled) editor.view.dispatch(editor.state.tr.insertText(character, from, to));
  }
}

function pressEnter(editor: Editor): void {
  const event = {
    key: 'Enter',
    keyCode: 13,
    altKey: false,
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    preventDefault() {},
    stopPropagation() {},
  } as KeyboardEvent;
  const view = editor.view as EditorView;
  for (const plugin of editor.extensionManager.plugins) {
    if (plugin.props.handleKeyDown?.(view, event)) return;
  }
}

test('typing single-dollar delimiters creates an inline math node with exact source', () => {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'paragraph' }] },
  });

  try {
    typeText(editor, 'Euler: $e^{i\\pi}+1=0$!');
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{
      type: 'paragraph',
      children: [
        { type: 'text', text: 'Euler: ', marks: [] },
        { type: 'mathInline', latex: 'e^{i\\pi}+1=0' },
        { type: 'text', text: '!', marks: [] },
      ],
    }]);
  } finally {
    editor.destroy();
  }
});

test('inline parsing leaves escaped dollars, double-dollar delimiters, and typed code spans alone', () => {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'paragraph' }] },
  });

  try {
    typeText(editor, '\\$escaped$ $$block$$ `$code$`');
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{
      type: 'paragraph',
      children: [
        { type: 'text', text: '\\$escaped$ $$block$$ ', marks: [] },
        { type: 'text', text: '$code$', marks: [{ type: 'code' }] },
      ],
    }]);
  } finally {
    editor.destroy();
  }
});

test('standalone dollar fences keep LaTeX lines literal and become a block on closing Enter', () => {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'paragraph' }] },
  });

  try {
    typeText(editor, '$$');
    pressEnter(editor);
    typeText(editor, '\\begin{aligned}');
    pressEnter(editor);
    typeText(editor, '* literal # source');
    pressEnter(editor);
    typeText(editor, 'x&=y\\\\');
    pressEnter(editor);
    typeText(editor, '$$');
    pressEnter(editor);

    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [
      { type: 'mathBlock', latex: '\\begin{aligned}\n* literal # source\nx&=y\\\\' },
      { type: 'paragraph', children: [] },
    ]);
  } finally {
    editor.destroy();
  }
});

test('a new standalone fence converts after a heading and a long plain-text prefix', () => {
  for (const prefixLength of [1, 20]) {
    const editor = new Editor({
      element: null,
      extensions: createEditorExtensions(false),
      content: {
        type: 'doc',
        content: [
          { type: 'heading', attrs: { level: 1 }, content: [{ type: 'text', text: 'Heading' }] },
          ...Array.from({ length: prefixLength }, (_, index) => ({
            type: 'paragraph',
            content: [{ type: 'text', text: `intro ${index}` }],
          })),
        ],
      },
    });

    try {
      editor.commands.setTextSelection(editor.state.doc.content.size - 1);
      pressEnter(editor);
      typeText(editor, '$$');
      pressEnter(editor);
      typeText(editor, '\\begin{aligned}x&=y\\\\');
      pressEnter(editor);
      typeText(editor, '$$');
      pressEnter(editor);

      const blocks = fromEditorDocument(editor.getJSON(), false);
      assert.deepEqual(blocks.at(-2), { type: 'mathBlock', latex: '\\begin{aligned}x&=y\\\\' });
      assert.deepEqual(blocks.at(-1), { type: 'paragraph', children: [] });
      assert.equal(blocks.length, prefixLength + 3);
    } finally {
      editor.destroy();
    }
  }
});

test('a long tracked fence keeps its full source and later inline math parses', () => {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'paragraph' }] },
  });

  try {
    typeText(editor, '$$');
    pressEnter(editor);
    const sourceLines = Array.from({ length: 20 }, (_, index) => `line ${index}:${'x'.repeat(900)}`);
    sourceLines.forEach((line, index) => {
      typeText(editor, line);
      if (index < sourceLines.length - 1) pressEnter(editor);
    });
    pressEnter(editor);
    typeText(editor, '$$');
    pressEnter(editor);
    typeText(editor, '$x$');

    const blocks = fromEditorDocument(editor.getJSON(), false);
    assert.deepEqual(blocks, [
      { type: 'mathBlock', latex: sourceLines.join('\n') },
      { type: 'paragraph', children: [{ type: 'mathInline', latex: 'x' }] },
    ]);
  } finally {
    editor.destroy();
  }
});

test('inline math parses after an existing closed fence and dollar input stays literal in code blocks', () => {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: {
      type: 'doc',
      content: [
        { type: 'paragraph', content: [{ type: 'text', text: '$$' }] },
        { type: 'paragraph', content: [{ type: 'text', text: 'x' }] },
        { type: 'paragraph', content: [{ type: 'text', text: '$$' }] },
        { type: 'paragraph' },
      ],
    },
  });

  try {
    editor.commands.setTextSelection(editor.state.doc.content.size - 1);
    typeText(editor, '$a$');
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [
      { type: 'paragraph', children: [{ type: 'text', text: '$$', marks: [] }] },
      { type: 'paragraph', children: [{ type: 'text', text: 'x', marks: [] }] },
      { type: 'paragraph', children: [{ type: 'text', text: '$$', marks: [] }] },
      { type: 'paragraph', children: [{ type: 'mathInline', latex: 'a' }] },
    ]);
  } finally {
    editor.destroy();
  }

  const codeEditor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'codeBlock', attrs: { language: null }, content: [{ type: 'text', text: '$$x$$' }] }] },
  });
  try {
    codeEditor.commands.setTextSelection(codeEditor.state.doc.content.size - 1);
    typeText(codeEditor, '$$');
    assert.deepEqual(fromEditorDocument(codeEditor.getJSON(), false), [
      { type: 'codeBlock', language: null, code: '$$x$$$$' },
    ]);
  } finally {
    codeEditor.destroy();
  }
});

test('formatting across math nodes applies to text but removes marks from math nodes', () => {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: {
      type: 'doc',
      content: [{ type: 'paragraph', content: [
        { type: 'text', text: 'left ' },
        { type: 'inlineMath', attrs: { latex: 'x^2' } },
        { type: 'text', text: ' right' },
      ] }],
    },
  });

  try {
    const initial = editor.state;
    let state = EditorState.create({
      schema: editor.schema,
      doc: initial.doc,
      plugins: editor.extensionManager.plugins,
    });
    const transaction = state.tr.addMark(1, state.doc.content.size - 1, editor.schema.marks.bold.create());
    const applied = state.applyTransaction(transaction);
    state = applied.state;
    assert.equal(applied.transactions.length, 2);
    assert.deepEqual(fromEditorDocument(state.doc.toJSON(), false), [{
      type: 'paragraph',
      children: [
        { type: 'text', text: 'left ', marks: [{ type: 'bold' }] },
        { type: 'mathInline', latex: 'x^2' },
        { type: 'text', text: ' right', marks: [{ type: 'bold' }] },
      ],
    }]);
  } finally {
    editor.destroy();
  }
});

test('math source updates, including empty source, survive undo as real editor transactions', () => {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: {
      type: 'doc',
      content: [{ type: 'paragraph', content: [{ type: 'inlineMath', attrs: { latex: 'x+1' } }] }],
    },
  });

  try {
    let state = EditorState.create({
      schema: editor.schema,
      doc: editor.state.doc,
      plugins: editor.extensionManager.plugins,
    });
    const formula = state.doc.nodeAt(1)!;
    const update = state.tr.setNodeMarkup(1, formula.type, { ...formula.attrs, latex: '' }, formula.marks);
    state = state.applyTransaction(update).state;
    assert.deepEqual(fromEditorDocument(state.doc.toJSON(), false), [{
      type: 'paragraph', children: [{ type: 'mathInline', latex: '' }],
    }]);
    let undone = false;
    assert.equal(undo(state, transaction => {
      state = state.applyTransaction(transaction).state;
      undone = true;
    }), true);
    assert.equal(undone, true);
    assert.deepEqual(fromEditorDocument(state.doc.toJSON(), false), [{
      type: 'paragraph', children: [{ type: 'mathInline', latex: 'x+1' }],
    }]);
  } finally {
    editor.destroy();
  }
});
