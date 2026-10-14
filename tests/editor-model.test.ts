import assert from 'node:assert/strict';
import test from 'node:test';
import { Editor, getSchema } from '@tiptap/core';
import { EditorState } from '@tiptap/pm/state';
import type { Block } from '../src/generated/note-format.ts';
import { createEditorExtensions, TABLE_HEADER_NORMALIZATION_META } from '../src/editor-extensions.ts';
import { CODE_LANGUAGES } from '../src/code-languages.ts';
import { fromEditorDocument, toEditorDocument } from '../src/editor-model.ts';

test('round-trips top-level image blocks with their asset ids and display widths', () => {
  const blocks: Block[] = [
    { type: 'paragraph', children: [{ type: 'text', text: 'Before', marks: [] }] },
    { type: 'image', assetId: '11111111-1111-4111-8111-111111111111', displayWidthPx: 320 },
    { type: 'image', assetId: '22222222-2222-4222-8222-222222222222', displayWidthPx: 4294967295 },
    { type: 'paragraph', children: [{ type: 'text', text: 'After', marks: [] }] },
  ];
  const editorDocument = {
    type: 'doc',
    content: [
      { type: 'paragraph', content: [{ type: 'text', text: 'Before' }] },
      { type: 'image', attrs: { assetId: '11111111-1111-4111-8111-111111111111', displayWidthPx: 320 } },
      { type: 'image', attrs: { assetId: '22222222-2222-4222-8222-222222222222', displayWidthPx: 4294967295 } },
      { type: 'paragraph', content: [{ type: 'text', text: 'After' }] },
    ],
  };

  assert.deepEqual(toEditorDocument(blocks, false), editorDocument);
  assert.deepEqual(fromEditorDocument(editorDocument, false), blocks);
});

test('rejects malformed UUIDv4 image asset ids in native blocks and editor JSON', () => {
  const invalidAssetIds = [
    'AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA',
    '11111111-1111-1111-8111-111111111111',
  ];

  for (const assetId of invalidAssetIds) {
    assert.throws(() => toEditorDocument([
      { type: 'image', assetId, displayWidthPx: 320 },
    ], false), /expected a lowercase UUIDv4/);
    assert.throws(() => fromEditorDocument({
      type: 'doc',
      content: [{ type: 'image', attrs: { assetId, displayWidthPx: 320 } }],
    }, false), /expected a lowercase UUIDv4/);
  }
});

test('rejects image widths outside the positive 32-bit integer range', () => {
  for (const displayWidthPx of [0, 1.5, 4294967296]) {
    assert.throws(() => toEditorDocument([
      { type: 'image', assetId: '11111111-1111-4111-8111-111111111111', displayWidthPx },
    ], false), /expected a positive 32-bit integer/);
    assert.throws(() => fromEditorDocument({
      type: 'doc',
      content: [{
        type: 'image',
        attrs: { assetId: '11111111-1111-4111-8111-111111111111', displayWidthPx },
      }],
    }, false), /expected a positive 32-bit integer/);
  }
});

test('rejects extra native and editor image fields instead of dropping them', () => {
  const assetId = '11111111-1111-4111-8111-111111111111';
  const forbiddenFields = ['src', 'blob', 'path', 'alt', 'title', 'transientURL', 'marks', 'children'];

  for (const field of forbiddenFields) {
    assert.throws(() => toEditorDocument([{
      type: 'image',
      assetId,
      displayWidthPx: 320,
      [field]: 'unexpected',
    } as unknown as Block], false), new RegExp(`unsupported field: ${field}`));
    assert.throws(() => fromEditorDocument({
      type: 'doc',
      content: [{
        type: 'image',
        attrs: { assetId, displayWidthPx: 320, [field]: 'unexpected' },
      }],
    }, false), new RegExp(`unsupported field: ${field}`));
    assert.throws(() => fromEditorDocument({
      type: 'doc',
      content: [{
        type: 'image',
        attrs: { assetId, displayWidthPx: 320 },
        [field]: 'unexpected',
      }],
    }, false), new RegExp(`unsupported field: ${field}`));
  }
});

test('rejects image blocks in simple sections in both document forms', () => {
  assert.throws(() => toEditorDocument([
    { type: 'image', assetId: '11111111-1111-4111-8111-111111111111', displayWidthPx: 320 },
  ], true), /images are not supported in this section/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'image', attrs: { assetId: '11111111-1111-4111-8111-111111111111', displayWidthPx: 320 } }],
  }, true), /images are not supported in this section/);
});

test('rejects images nested in lists and block quotes in both document forms', () => {
  const imageBlock: Block = {
    type: 'image',
    assetId: '11111111-1111-4111-8111-111111111111',
    displayWidthPx: 320,
  };
  const listBlock: Block = { type: 'bulletList', items: [{ blocks: [imageBlock] }] };
  const quoteBlock: Block = { type: 'blockquote', blocks: [imageBlock] };
  const imageNode = { type: 'image', attrs: { assetId: imageBlock.assetId, displayWidthPx: 320 } };

  assert.throws(() => toEditorDocument([listBlock], false), /images must be top-level blocks/);
  assert.throws(() => toEditorDocument([quoteBlock], false), /images must be top-level blocks/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'bulletList', content: [{ type: 'listItem', content: [imageNode] }] }],
  }, false), /images must be top-level blocks/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'blockquote', content: [imageNode] }],
  }, false), /images must be top-level blocks/);
});

test('round-trips paragraphs, headings, inline marks, links, and hard breaks through the editor schema', () => {
  const blocks: Block[] = [
    {
      type: 'paragraph',
      children: [
        { type: 'text', text: 'Bold', marks: [{ type: 'bold' }] },
        { type: 'text', text: ' and italic strike', marks: [{ type: 'italic' }, { type: 'strike' }] },
        { type: 'hardBreak' },
        { type: 'text', text: 'linked', marks: [{ type: 'link', href: 'https://example.test/path' }] },
      ],
    },
    { type: 'heading2', children: [{ type: 'text', text: 'Heading', marks: [] }] },
  ];

  const schema = getSchema(createEditorExtensions(false));
  const document = schema.nodeFromJSON(toEditorDocument(blocks, false));

  assert.deepEqual(JSON.parse(JSON.stringify(document.toJSON())), {
    type: 'doc',
    content: [
      {
        type: 'paragraph',
        content: [
          { type: 'text', text: 'Bold', marks: [{ type: 'bold' }] },
          { type: 'text', text: ' and italic strike', marks: [{ type: 'italic' }, { type: 'strike' }] },
          { type: 'hardBreak' },
          {
            type: 'text',
            text: 'linked',
            marks: [{
              type: 'link',
              attrs: {
                href: 'https://example.test/path',
                target: '_blank',
                rel: 'noopener noreferrer nofollow',
                class: null,
                title: null,
              },
            }],
          },
        ],
      },
      { type: 'heading', attrs: { level: 2 }, content: [{ type: 'text', text: 'Heading' }] },
    ],
  });
  assert.deepEqual(fromEditorDocument(document.toJSON(), false), blocks);
});

test('round-trips inline code without dropping compatible marks', () => {
  const blocks: Block[] = [{
    type: 'paragraph',
    children: [{ type: 'text', text: 'const x', marks: [{ type: 'bold' }, { type: 'code' }] }],
  }];
  const schema = getSchema(createEditorExtensions(false));
  const document = schema.nodeFromJSON(toEditorDocument(blocks, false));

  assert.deepEqual(JSON.parse(JSON.stringify(document.toJSON())), {
    type: 'doc',
    content: [{
      type: 'paragraph',
      content: [{ type: 'text', text: 'const x', marks: [{ type: 'bold' }, { type: 'code' }] }],
    }],
  });
  assert.deepEqual(fromEditorDocument(document.toJSON(), false), blocks);
});

test('round-trips raw code and custom language labels without normalizing content', () => {
  const sourceCode = 'const label = "中文 $`<tag>";\n\t  keep indent\n\n';
  const blocks: Block[] = [
    { type: 'codeBlock', language: 'custom-lang', code: sourceCode },
    { type: 'codeBlock', language: null, code: '' },
  ];
  const document = {
    type: 'doc',
    content: [
      { type: 'codeBlock', attrs: { language: 'custom-lang' }, content: [{ type: 'text', text: sourceCode }] },
      { type: 'codeBlock', attrs: { language: null } },
    ],
  };

  assert.deepEqual(toEditorDocument(blocks, false), document);
  assert.deepEqual(fromEditorDocument(document, false), blocks);
});

test('round-trips code blocks through the public ProseMirror schema without reformatting text', () => {
  const sourceCode = 'let title = "中文 $`<div>";\r\n\tkeep tabs\r\n\r\n';
  const blocks: Block[] = [{ type: 'codeBlock', language: 'c#', code: sourceCode }];
  const schema = getSchema(createEditorExtensions(false));
  const document = schema.nodeFromJSON(toEditorDocument(blocks, false));

  assert.deepEqual(JSON.parse(JSON.stringify(document.toJSON())), {
    type: 'doc',
    content: [{
      type: 'codeBlock',
      attrs: { language: 'c#' },
      content: [{ type: 'text', text: sourceCode }],
    }],
  });
  assert.deepEqual(fromEditorDocument(document.toJSON(), false), blocks);
});

test('keeps bold when inline code is applied through editor commands', () => {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'const x' }] }] },
  });

  try {
    assert.equal(editor.chain()
      .setTextSelection({ from: 1, to: 8 })
      .toggleBold()
      .toggleCode()
      .run(), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{
      type: 'paragraph',
      children: [{ type: 'text', text: 'const x', marks: [{ type: 'bold' }, { type: 'code' }] }],
    }]);
  } finally {
    editor.destroy();
  }
});

test('exposes the seven code-language choices with stable UI values', () => {
  assert.deepEqual(CODE_LANGUAGES, [
    { value: 'javascript', label: 'JavaScript' },
    { value: 'typescript', label: 'TypeScript' },
    { value: 'python', label: 'Python' },
    { value: 'rust', label: 'Rust' },
    { value: 'csharp', label: 'C#' },
    { value: 'sql', label: 'SQL' },
    { value: 'json', label: 'JSON' },
  ]);
});

test('round-trips nested list blocks, task state, block quotes, rules, and basic tables', () => {
  const blocks: Block[] = [
    {
      type: 'orderedList',
      start: 7,
      items: [
        { blocks: [{ type: 'heading2', children: [{ type: 'text', text: 'Heading-first item', marks: [] }] }] },
        { blocks: [] },
      ],
    },
    { type: 'bulletList', items: [] },
    {
      type: 'taskList',
      items: [
        { checked: true, blocks: [{ type: 'paragraph', children: [{ type: 'text', text: 'Done', marks: [] }] }] },
        { checked: false, blocks: [] },
      ],
    },
    { type: 'blockquote', blocks: [{ type: 'paragraph', children: [{ type: 'text', text: 'Quoted', marks: [] }] }] },
    { type: 'horizontalRule' },
    {
      type: 'table',
      rows: [
        {
          type: 'header',
          cells: [
            [{ type: 'text', text: 'Name', marks: [{ type: 'bold' }] }],
            [{ type: 'text', text: 'Notes', marks: [] }],
          ],
        },
        {
          type: 'body',
          cells: [
            [{ type: 'text', text: 'Item', marks: [] }],
            [{ type: 'text', text: 'First line', marks: [] }, { type: 'hardBreak' }, { type: 'text', text: 'Second line', marks: [] }],
          ],
        },
      ],
    },
  ];

  const schema = getSchema(createEditorExtensions(false));
  const document = schema.nodeFromJSON(toEditorDocument(blocks, false));
  const editorJson = JSON.parse(JSON.stringify(document.toJSON()));

  assert.equal(editorJson.content[0].attrs.start, 7);
  assert.equal(editorJson.content[0].content[0].content[0].type, 'heading');
  assert.equal(editorJson.content[2].content[0].attrs.checked, true);
  assert.equal(editorJson.content[2].content[1].attrs.checked, false);
  assert.equal(editorJson.content[5].content[0].content[0].type, 'tableHeader');
  assert.equal(editorJson.content[5].content[1].content[0].type, 'tableCell');
  assert.deepEqual(fromEditorDocument(editorJson, false), blocks);
});

test('round-trips inline and block math source exactly, including table cells', () => {
  const invalidLatex = '\\notARealCommand{\\sqrt{x';
  const largeLatex = `\\sum_{i=0}^{99999} x_i + ${'x'.repeat(128 * 1024)}`;
  const blocks: Block[] = [
    {
      type: 'paragraph',
      children: [
        { type: 'mathInline', latex: '' },
        { type: 'mathInline', latex: invalidLatex },
        { type: 'mathInline', latex: largeLatex },
      ],
    },
    { type: 'mathBlock', latex: '' },
    { type: 'mathBlock', latex: '\\begin{align} x &= \\notARealCommand{2} \\\\ y &= 3 \\end{align}' },
    {
      type: 'table',
      rows: [
        { type: 'header', cells: [[{ type: 'mathInline', latex: 'a^2+b^2' }]] },
        { type: 'body', cells: [[
          { type: 'text', text: 'result = ', marks: [] },
          { type: 'mathInline', latex: '\\frac{1}{2}' },
        ]] },
      ],
    },
  ];
  const editorDocument = {
    type: 'doc',
    content: [
      {
        type: 'paragraph',
        content: [
          { type: 'inlineMath', attrs: { latex: '' } },
          { type: 'inlineMath', attrs: { latex: invalidLatex } },
          { type: 'inlineMath', attrs: { latex: largeLatex } },
        ],
      },
      { type: 'blockMath', attrs: { latex: '' } },
      { type: 'blockMath', attrs: { latex: '\\begin{align} x &= \\notARealCommand{2} \\\\ y &= 3 \\end{align}' } },
      {
        type: 'table',
        content: [
          { type: 'tableRow', content: [{ type: 'tableHeader', content: [{ type: 'paragraph', content: [{ type: 'inlineMath', attrs: { latex: 'a^2+b^2' } }] }] }] },
          { type: 'tableRow', content: [{ type: 'tableCell', content: [{ type: 'paragraph', content: [
            { type: 'text', text: 'result = ' },
            { type: 'inlineMath', attrs: { latex: '\\frac{1}{2}' } },
          ] }] }] },
        ],
      },
    ],
  };

  assert.deepEqual(toEditorDocument(blocks, false), editorDocument);
  assert.deepEqual(fromEditorDocument(editorDocument, false), blocks);
});

test('normalizes table header cells only when a structural command opts in', () => {
  const source: Block[] = [{
    type: 'table',
    rows: [
      { type: 'header', cells: [[{ type: 'text', text: 'Header', marks: [] }]] },
      { type: 'body', cells: [[{ type: 'text', text: 'Body', marks: [] }]] },
    ],
  }];
  const content = toEditorDocument(source, false);
  const editor = new Editor({ element: null, extensions: createEditorExtensions(false), content });

  try {
    const state = EditorState.create({
      schema: editor.schema,
      doc: editor.state.doc,
      plugins: editor.extensionManager.plugins,
    });
    const table = state.doc.child(0);
    const firstRow = table.child(0);
    const deleteHeader = state.tr.delete(1, 1 + firstRow.nodeSize);
    const untagged = state.applyTransaction(deleteHeader).state;
    assert.throws(() => fromEditorDocument(untagged.doc.toJSON(), false), /first row must be the header/);

    const taggedDelete = state.tr
      .delete(1, 1 + firstRow.nodeSize)
      .setMeta(TABLE_HEADER_NORMALIZATION_META, true);
    const deleted = state.applyTransaction(taggedDelete).state;
    assert.deepEqual(fromEditorDocument(deleted.doc.toJSON(), false), [{
      type: 'table',
      rows: [{ type: 'header', cells: [[{ type: 'text', text: 'Body', marks: [] }]] }],
    }]);

    const blankCell = editor.schema.nodes.tableCell.createAndFill();
    assert.ok(blankCell);
    const insertedRow = editor.schema.nodes.tableRow.create(null, [blankCell]);
    const insertBeforeHeader = state.tr.insert(1, insertedRow).setMeta(TABLE_HEADER_NORMALIZATION_META, true);
    const inserted = state.applyTransaction(insertBeforeHeader).state;
    const insertedTable = fromEditorDocument(inserted.doc.toJSON(), false)[0];
    assert.equal(insertedTable.type, 'table');
    if (insertedTable.type === 'table') {
      assert.deepEqual(insertedTable.rows.map(row => row.type), ['header', 'body', 'body']);
      assert.deepEqual(insertedTable.rows[1].cells, [[{ type: 'text', text: 'Header', marks: [] }]]);
    }
  } finally {
    editor.destroy();
  }
});

test('rejects unsupported v1 content and ProseMirror semantics instead of dropping them', () => {
  const schema = getSchema(createEditorExtensions(false));
  assert.throws(() => toEditorDocument([
    { type: 'codeBlock', language: null, code: 'const value = 1;' },
  ], true), /code blocks are not supported in this section/);
  assert.throws(() => toEditorDocument([
    { type: 'paragraph', children: [{ type: 'text', text: 'inline', marks: [{ type: 'code' }] }] },
  ], true), /code is not supported in this section/);
  assert.throws(() => toEditorDocument([
    { type: 'image', assetId: '11111111-1111-1111-8111-111111111111', displayWidthPx: 320 },
  ], false), /expected a lowercase UUIDv4/);
  assert.throws(() => toEditorDocument([
    { type: 'mindMap', mapId: '22222222-2222-4222-8222-222222222222', rootId: '33333333-3333-4333-8333-333333333333', nodes: [] },
  ], false), /mind maps are not supported by this editor/);
  assert.throws(() => toEditorDocument([
    { type: 'blockquote', blocks: [] },
  ], false), /empty block quotes cannot be represented/);
  assert.throws(() => toEditorDocument([
    { type: 'paragraph', children: [], alignment: 'center' } as unknown as Block,
  ], false), /unsupported field: alignment/);
  assert.throws(() => toEditorDocument([
    { type: 'paragraph', children: [{ type: 'text', text: 'unsafe', marks: [{ type: 'link', href: 'javascript:alert(1)' }] }] },
  ], false), /links must use http, https, or mailto/);

  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'futureBlock', attrs: { payload: true } }],
  }, false), /unsupported block node: futureBlock/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'paragraph', attrs: { alignment: 'center' } }],
  }, false), /unsupported field: attrs/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'text', text: 'x', marks: [{ type: 'underline' }] }] }],
  }, false), /unsupported mark: underline/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'codeBlock', attrs: { language: 'custom-lang', executable: true }, content: [] }],
  }, false), /unsupported field: executable/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'codeBlock', attrs: { language: 'javascript' }, content: [{ type: 'text', text: 'x', marks: [] }] }],
  }, false), /unsupported field: marks/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'codeBlock', attrs: { language: 'javascript' }, content: [{ type: 'hardBreak' }] }],
  }, false), /code blocks contain plain text only/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'inlineMath', attrs: { latex: 'x^2', display: true } }] }],
  }, false), /unsupported field: display/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'inlineMath', attrs: { latex: 'x^2' }, marks: [{ type: 'bold' }] }] }],
  }, false), /unsupported field: marks/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'blockMath', attrs: { latex: 'x^2', macros: {} } }],
  }, false), /unsupported field: macros/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'orderedList', attrs: { start: 1, type: 'a' }, content: [] }],
  }, false), /alternate ordered-list markers are not supported/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{
      type: 'paragraph',
      content: [{
        type: 'text',
        text: 'linked',
        marks: [{ type: 'link', attrs: { href: 'https://example.test', target: '_self' } }],
      }],
    }],
  }, false), /custom link attributes are not supported/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{
      type: 'table',
      content: [{
        type: 'tableRow',
        content: [{ type: 'tableHeader', attrs: { colspan: 2 }, content: [{ type: 'paragraph' }] }],
      }],
    }],
  }, false), /merged cells are not supported/);

  assert.ok(schema.nodes.table);
});

test('keeps the simple-section schema and adapter to text, bold, lists, and basic headings', () => {
  const blocks: Block[] = [
    { type: 'heading1', children: [{ type: 'text', text: 'Cue', marks: [{ type: 'bold' }] }] },
    { type: 'paragraph', children: [{ type: 'text', text: 'Line one', marks: [] }, { type: 'hardBreak' }, { type: 'text', text: 'Line two', marks: [] }] },
    { type: 'taskList', items: [{ checked: true, blocks: [{ type: 'paragraph', children: [{ type: 'text', text: 'Done', marks: [] }] }] }] },
  ];
  const schema = getSchema(createEditorExtensions(true));
  const document = schema.nodeFromJSON(toEditorDocument(blocks, true));

  assert.equal(schema.nodes.table, undefined);
  assert.equal(schema.marks.italic, undefined);
  assert.deepEqual(fromEditorDocument(document.toJSON(), true), blocks);
  assert.throws(() => toEditorDocument([
    { type: 'paragraph', children: [{ type: 'text', text: 'Italic', marks: [{ type: 'italic' }] }] },
  ], true), /italic is not supported in this section/);
  assert.throws(() => toEditorDocument([
    { type: 'paragraph', children: [{ type: 'text', text: 'Code', marks: [{ type: 'code' }] }] },
  ], true), /code is not supported in this section/);
  assert.throws(() => toEditorDocument([
    { type: 'paragraph', children: [{ type: 'mathInline', latex: 'x^2' }] },
  ], true), /inline math is not supported in this section/);
  assert.throws(() => toEditorDocument([{ type: 'mathBlock', latex: 'x^2' }], true), /math blocks are not supported in this section/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'paragraph', content: [{ type: 'inlineMath', attrs: { latex: 'x^2' } }] }],
  }, true), /inline math is not supported in this section/);
  assert.throws(() => fromEditorDocument({
    type: 'doc',
    content: [{ type: 'blockMath', attrs: { latex: 'x^2' } }],
  }, true), /math blocks are not supported in this section/);
  assert.throws(() => toEditorDocument([{ type: 'horizontalRule' }], true), /horizontal rules are not supported in this section/);
});

test('provides a ProseMirror paragraph for an empty canonical section', () => {
  const document = toEditorDocument([], true);

  assert.deepEqual(document, { type: 'doc', content: [{ type: 'paragraph' }] });
  assert.deepEqual(fromEditorDocument(document, true), [{ type: 'paragraph', children: [] }]);
});

test('preserves hard breaks while bold remains active', () => {
  const editor = new Editor({
    element: null,
    extensions: createEditorExtensions(false),
    content: { type: 'doc', content: [{ type: 'paragraph' }] },
  });

  try {
    assert.equal(editor.chain()
      .setTextSelection(1)
      .toggleBold()
      .insertContent({ type: 'text', text: 'before' })
      .setHardBreak()
      .insertContent({ type: 'text', text: 'after' })
      .run(), true);
    assert.deepEqual(fromEditorDocument(editor.getJSON(), false), [{
      type: 'paragraph',
      children: [
        { type: 'text', text: 'before', marks: [{ type: 'bold' }] },
        { type: 'hardBreak' },
        { type: 'text', text: 'after', marks: [{ type: 'bold' }] },
      ],
    }]);
  } finally {
    editor.destroy();
  }
});
