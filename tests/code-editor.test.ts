import assert from 'node:assert/strict';
import test from 'node:test';
import { Editor } from '@tiptap/core';
import { EditorState } from '@tiptap/pm/state';
import type { EditorView } from '@tiptap/pm/view';
import type { Block } from '../src/generated/note-format.ts';
import { createEditorExtensions } from '../src/editor-extensions.ts';
import { fromEditorDocument } from '../src/editor-model.ts';

function applyTextInput(editor: Editor, text: string): boolean {
  const position = editor.state.selection.from;
  for (const plugin of editor.extensionManager.plugins) {
    if (plugin.props.handleTextInput?.(editor.view as EditorView, position, position, text)) return true;
  }
  return false;
}

function applyPaste(editor: Editor, event: ClipboardEvent): boolean {
  for (const plugin of editor.extensionManager.plugins) {
    if (plugin.props.handlePaste?.(editor.view as EditorView, event, undefined)) return true;
  }
  return false;
}

function applyKeyDown(editor: Editor, event: KeyboardEvent): boolean {
  for (const plugin of editor.extensionManager.plugins) {
    if (plugin.props.handleKeyDown?.(editor.view as EditorView, event)) return true;
  }
  return false;
}

test('typed code fences preserve punctuation and unknown language labels', () => {
  const cases = [
    { fence: '```c#', language: 'c#' },
    { fence: '```custom-lang', language: 'custom-lang' },
    { fence: '```', language: null },
  ] as const;

  for (const { fence, language } of cases) {
    const editor = new Editor({
      element: null,
      extensions: createEditorExtensions(false),
      content: { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: fence }] }] },
    });

    try {
      editor.commands.setTextSelection(fence.length + 1);
      assert.equal(applyTextInput(editor, ' '), true, `expected ${fence} to trigger the code fence rule`);
      assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [
        { type: 'codeBlock', language, code: '' },
      ] satisfies Block[]);
    } finally {
      editor.destroy();
    }
  }
});

test('pasting inside a code block inserts literal text and preserves tabs and newlines', () => {
  const originalCode = 'before';
  const pastedText = '\r\n\t**literal** $x$ <div>html</div>\r\n```custom-lang';
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: {
      type: 'doc',
      content: [{
        type: 'codeBlock',
        attrs: { language: 'custom-lang' },
        content: [{ type: 'text', text: originalCode }],
      }],
    },
  });

  try {
    editor.commands.setTextSelection(originalCode.length + 1);
    const event = {
      clipboardData: {
        getData: (type: string) => type === 'text/plain' ? pastedText : '',
      },
    } as unknown as ClipboardEvent;

    assert.equal(applyPaste(editor, event), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{
      type: 'codeBlock',
      language: 'custom-lang',
      code: originalCode + pastedText,
    }]);
  } finally {
    editor.destroy();
  }
});

test('Tab inside a code block inserts one literal tab character', () => {
  const originalCode = 'const answer = 42;';
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'codeBlock', attrs: { language: 'javascript' }, content: [{ type: 'text', text: originalCode }] }] },
  });

  try {
    editor.commands.setTextSelection(originalCode.length + 1);
    const event = {
      key: 'Tab',
      keyCode: 9,
      shiftKey: false,
      altKey: false,
      ctrlKey: false,
      metaKey: false,
      preventDefault() {},
    } as unknown as KeyboardEvent;

    assert.equal(applyKeyDown(editor, event), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{
      type: 'codeBlock',
      language: 'javascript',
      code: `${originalCode}\t`,
    }]);
  } finally {
    editor.destroy();
  }
});

test('changing a code block language preserves its source and arbitrary labels', () => {
  const source = '  {"tab": "\\t"}\r\n';
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'codeBlock', attrs: { language: 'custom lang ✓' }, content: [{ type: 'text', text: source }] }] },
  });

  try {
    assert.equal(editor.commands.updateAttributes('codeBlock', { language: 'json' }), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{ type: 'codeBlock', language: 'json', code: source }]);

    assert.equal(editor.commands.updateAttributes('codeBlock', { language: 'custom marker ✓' }), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{ type: 'codeBlock', language: 'custom marker ✓', code: source }]);
  } finally {
    editor.destroy();
  }
});

test('HTML-only clipboard data is inserted as source text inside code blocks', () => {
  const source = '<h1>literal markup</h1><script>do not run</script>';
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'codeBlock', attrs: { language: null } }] },
  });

  try {
    editor.commands.setTextSelection(1);
    const event = {
      clipboardData: {
        getData: (type: string) => type === 'text/html' ? source : '',
      },
    } as unknown as ClipboardEvent;

    assert.equal(applyPaste(editor, event), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{ type: 'codeBlock', language: null, code: source }]);
  } finally {
    editor.destroy();
  }
});

test('code block decorations highlight registered languages but never guess unknown languages', () => {
  const cases = [
    { language: 'javascript', expectedHighlighted: true },
    { language: 'custom-lang', expectedHighlighted: false },
    { language: null, expectedHighlighted: false },
    { language: 'plaintext', expectedHighlighted: false },
  ] as const;
  const source = 'const answer = 42;';

  for (const { language, expectedHighlighted } of cases) {
    const editor = new Editor({
      element: null,
      extensions: createEditorExtensions(false),
      content: {
        type: 'doc',
        content: [{ type: 'codeBlock', attrs: { language }, content: [{ type: 'text', text: source }] }],
      },
    });

    try {
      const beforeHighlight = editor.getJSON();
      const state = EditorState.create({
        schema: editor.schema,
        doc: editor.state.doc,
        plugins: editor.extensionManager.plugins,
      });
      const classes = state.plugins
        .flatMap(plugin => plugin.props.decorations?.(state)?.find() ?? [])
        .flatMap(decoration => {
          const attrs = 'attrs' in decoration.type ? decoration.type.attrs : {};
          return typeof attrs.class === 'string' ? attrs.class.split(' ') : [];
        });

      assert.equal(classes.some(className => className.startsWith('hljs-')), expectedHighlighted);
      assert.deepEqual(editor.getJSON(), beforeHighlight);
      assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{ type: 'codeBlock', language, code: source }]);
    } finally {
      editor.destroy();
    }
  }
});
