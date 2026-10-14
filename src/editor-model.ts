import type { JSONContent } from '@tiptap/core';
import type { Block, Inline, Mark, TableRow } from './generated/note-format.ts';

const LINK_DEFAULTS = {
  target: '_blank',
  rel: 'noopener noreferrer nofollow',
  class: null,
  title: null,
} as const;

type JsonObject = Record<string, unknown>;
type EditorMarkJSON =
  | { type: 'bold' | 'italic' | 'strike' }
  | { type: 'link'; attrs: { href: string } };

function fail(path: string, message: string): never {
  throw new Error(`Rich text is unavailable at ${path}: ${message}`);
}

function objectAt(value: unknown, path: string): JsonObject {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    return fail(path, 'expected an object');
  }
  return value as JsonObject;
}

function arrayAt(value: unknown, path: string): unknown[] {
  if (!Array.isArray(value)) return fail(path, 'expected an array');
  return value;
}

function exactKeys(value: JsonObject, allowed: readonly string[], path: string): void {
  const unknown = Object.keys(value).filter(key => !allowed.includes(key));
  if (unknown.length > 0) fail(path, `unsupported field${unknown.length === 1 ? '' : 's'}: ${unknown.join(', ')}`);
}

function optionalAttrs(value: JsonObject, allowed: readonly string[], path: string): JsonObject {
  if (!Object.hasOwn(value, 'attrs') || value.attrs === null) return {};
  const attrs = objectAt(value.attrs, `${path}.attrs`);
  exactKeys(attrs, allowed, `${path}.attrs`);
  return attrs;
}

function stringAt(value: unknown, path: string): string {
  if (typeof value !== 'string') return fail(path, 'expected text');
  return value;
}

function positiveIntegerAt(value: unknown, path: string): number {
  if (!Number.isInteger(value) || (value as number) < 1 || (value as number) > 0xffff_ffff) {
    return fail(path, 'expected a positive 32-bit integer');
  }
  return value as number;
}

function validLink(href: string): boolean {
  const separator = href.indexOf(':');
  if (separator < 0) return false;
  const scheme = href.slice(0, separator).toLowerCase();
  const rest = href.slice(separator + 1);
  if ([...href].some(character => /[\s\p{Cc}]/u.test(character))) return false;
  if (scheme === 'http' || scheme === 'https') {
    if (!rest.startsWith('//')) return false;
    return rest.slice(2).split(/[/?#]/, 1)[0].length > 0;
  }
  return scheme === 'mailto' && rest.length > 0;
}

function toMark(mark: unknown, simple: boolean, path: string): EditorMarkJSON {
  const value = objectAt(mark, path);
  exactKeys(value, value.type === 'link' ? ['type', 'href'] : ['type'], path);
  switch (value.type) {
    case 'bold':
      return { type: 'bold' };
    case 'italic':
    case 'strike':
      if (simple) return fail(path, `${value.type} is not supported in this section`);
      return { type: value.type };
    case 'link': {
      if (simple) return fail(path, 'links are not supported in this section');
      const href = stringAt(value.href, `${path}.href`);
      if (!validLink(href)) return fail(`${path}.href`, 'links must use http, https, or mailto');
      return { type: 'link', attrs: { href } };
    }
    case 'code':
      return fail(path, 'code marks are not supported by this editor');
    default:
      return fail(path, `unsupported mark: ${String(value.type)}`);
  }
}

function toInline(inline: Inline, simple: boolean, path: string): JSONContent {
  const value = objectAt(inline, path);
  switch (value.type) {
    case 'text': {
      exactKeys(value, ['type', 'text', 'marks'], path);
      const text = stringAt(value.text, `${path}.text`);
      if (text.length === 0) return fail(`${path}.text`, 'empty text nodes cannot be represented');
      const marks = arrayAt(value.marks, `${path}.marks`).map((mark, index) =>
        toMark(mark, simple, `${path}.marks[${index}]`),
      );
      const markKeys = marks.map(mark => mark.type);
      if (new Set(markKeys).size !== markKeys.length) return fail(`${path}.marks`, 'duplicate marks cannot be represented');
      return marks.length > 0 ? { type: 'text', text, marks } : { type: 'text', text };
    }
    case 'hardBreak':
      exactKeys(value, ['type'], path);
      return { type: 'hardBreak' };
    case 'mathInline':
      return fail(path, 'inline math is not supported by this editor');
    default:
      return fail(path, `unsupported inline node: ${String(value.type)}`);
  }
}

function toParagraphContent(children: unknown, simple: boolean, path: string): JSONContent[] | undefined {
  const content = arrayAt(children, path).map((inline, index) => toInline(inline as Inline, simple, `${path}[${index}]`));
  return content.length > 0 ? content : undefined;
}

function toListItems(items: unknown, simple: boolean, task: boolean, path: string): JSONContent[] {
  return arrayAt(items, path).map((item, index) => {
    const itemPath = `${path}[${index}]`;
    const value = objectAt(item, itemPath);
    exactKeys(value, task ? ['checked', 'blocks'] : ['blocks'], itemPath);
    const content = arrayAt(value.blocks, `${itemPath}.blocks`).map((block, blockIndex) =>
      toBlock(block as Block, simple, `${itemPath}.blocks[${blockIndex}]`),
    );
    if (task && typeof value.checked !== 'boolean') fail(`${itemPath}.checked`, 'expected a boolean');
    const node: JSONContent = task
      ? { type: 'taskItem', attrs: { checked: value.checked }, content }
      : { type: 'listItem', content };
    return node;
  });
}

function validateTableRows(rowsValue: unknown, path: string): TableRow[] {
  const rows = arrayAt(rowsValue, path) as TableRow[];
  if (rows.length === 0) fail(path, 'a table must contain its header row');

  let columnCount: number | null = null;
  rows.forEach((row, rowIndex) => {
    const rowPath = `${path}[${rowIndex}]`;
    const value = objectAt(row, rowPath);
    const isHeader = rowIndex === 0;
    exactKeys(value, ['type', 'cells'], rowPath);
    if (value.type !== (isHeader ? 'header' : 'body')) {
      fail(`${rowPath}.type`, isHeader ? 'the first row must be the header' : 'only the first row may be the header');
    }
    const cells = arrayAt(value.cells, `${rowPath}.cells`);
    if (cells.length === 0) fail(`${rowPath}.cells`, 'a row must contain at least one cell');
    if (columnCount === null) columnCount = cells.length;
    if (cells.length !== columnCount) fail(`${rowPath}.cells`, 'table rows must have the same number of cells');
    cells.forEach((cell, cellIndex) => {
      arrayAt(cell, `${rowPath}.cells[${cellIndex}]`).forEach((inline, inlineIndex) => {
        const inlineValue = objectAt(inline, `${rowPath}.cells[${cellIndex}][${inlineIndex}]`);
        if (inlineValue.type === 'text') {
          toInline(inline as Inline, false, `${rowPath}.cells[${cellIndex}][${inlineIndex}]`);
        } else if (inlineValue.type === 'hardBreak') {
          toInline(inline as Inline, false, `${rowPath}.cells[${cellIndex}][${inlineIndex}]`);
        } else {
          fail(`${rowPath}.cells[${cellIndex}][${inlineIndex}]`, 'table cells support text and hard breaks only');
        }
      });
    });
  });
  return rows;
}

function toBlock(block: Block, simple: boolean, path: string): JSONContent {
  const value = objectAt(block, path);
  switch (value.type) {
    case 'paragraph':
    case 'heading1':
    case 'heading2':
    case 'heading3': {
      exactKeys(value, ['type', 'children'], path);
      const content = toParagraphContent(value.children, simple, `${path}.children`);
      const type = value.type === 'paragraph' ? 'paragraph' : 'heading';
      const level = value.type === 'heading1' ? 1 : value.type === 'heading2' ? 2 : 3;
      return type === 'heading'
        ? { type, attrs: { level }, ...(content ? { content } : {}) }
        : { type, ...(content ? { content } : {}) };
    }
    case 'orderedList': {
      exactKeys(value, ['type', 'start', 'items'], path);
      const start = positiveIntegerAt(value.start, `${path}.start`);
      return { type: 'orderedList', attrs: { start }, content: toListItems(value.items, simple, false, `${path}.items`) };
    }
    case 'bulletList':
      exactKeys(value, ['type', 'items'], path);
      return { type: 'bulletList', content: toListItems(value.items, simple, false, `${path}.items`) };
    case 'taskList':
      exactKeys(value, ['type', 'items'], path);
      return { type: 'taskList', content: toListItems(value.items, simple, true, `${path}.items`) };
    case 'blockquote': {
      exactKeys(value, ['type', 'blocks'], path);
      if (simple) return fail(path, 'block quotes are not supported in this section');
      const content = arrayAt(value.blocks, `${path}.blocks`).map((nested, index) =>
        toBlock(nested as Block, simple, `${path}.blocks[${index}]`),
      );
      if (content.length === 0) return fail(`${path}.blocks`, 'empty block quotes cannot be represented');
      return { type: 'blockquote', content };
    }
    case 'horizontalRule':
      exactKeys(value, ['type'], path);
      if (simple) return fail(path, 'horizontal rules are not supported in this section');
      return { type: 'horizontalRule' };
    case 'table': {
      exactKeys(value, ['type', 'rows'], path);
      if (simple) return fail(path, 'tables are not supported in this section');
      const rows = validateTableRows(value.rows, `${path}.rows`);
      return {
        type: 'table',
        content: rows.map((row, rowIndex) => ({
          type: 'tableRow',
          content: row.cells.map((cell, cellIndex) => {
            const content = toParagraphContent(cell, false, `${path}.rows[${rowIndex}].cells[${cellIndex}]`);
            return {
              type: rowIndex === 0 ? 'tableHeader' : 'tableCell',
              content: [{ type: 'paragraph', ...(content ? { content } : {}) }],
            };
          }),
        })),
      };
    }
    case 'codeBlock':
      return fail(path, 'code blocks are not supported by this editor');
    case 'mathBlock':
      return fail(path, 'math blocks are not supported by this editor');
    case 'image':
      return fail(path, 'images are not supported by this editor');
    case 'mindMap':
      return fail(path, 'mind maps are not supported by this editor');
    default:
      return fail(path, `unsupported block node: ${String(value.type)}`);
  }
}

export function toEditorDocument(blocks: Block[], simple: boolean): JSONContent {
  const content = arrayAt(blocks, 'blocks').map((block, index) => toBlock(block as Block, simple, `blocks[${index}]`));
  // ProseMirror's document schema requires a text block. The UI must suppress the
  // initial editor update so opening an empty canonical section does not save it.
  if (content.length === 0) content.push({ type: 'paragraph' });
  return { type: 'doc', content };
}

function fromMark(mark: unknown, simple: boolean, path: string): Mark {
  const value = objectAt(mark, path);
  exactKeys(value, ['type', 'attrs'], path);
  switch (value.type) {
    case 'bold':
      optionalAttrs(value, [], path);
      return { type: 'bold' };
    case 'italic':
    case 'strike':
      optionalAttrs(value, [], path);
      if (simple) return fail(path, `${value.type} is not supported in this section`);
      return { type: value.type };
    case 'link': {
      if (simple) return fail(path, 'links are not supported in this section');
      const attrs = optionalAttrs(value, ['href', 'target', 'rel', 'class', 'title'], path);
      const href = stringAt(attrs.href, `${path}.attrs.href`);
      if (!validLink(href)) return fail(`${path}.attrs.href`, 'links must use http, https, or mailto');
      for (const key of ['target', 'rel', 'class', 'title'] as const) {
        if (Object.hasOwn(attrs, key) && attrs[key] !== LINK_DEFAULTS[key]) {
          fail(`${path}.attrs.${key}`, 'custom link attributes are not supported');
        }
      }
      return { type: 'link', href };
    }
    default:
      return fail(path, `unsupported mark: ${String(value.type)}`);
  }
}

function fromInline(value: unknown, simple: boolean, path: string): Inline {
  const node = objectAt(value, path);
  switch (node.type) {
    case 'text': {
      exactKeys(node, ['type', 'text', 'marks'], path);
      const text = stringAt(node.text, `${path}.text`);
      if (text.length === 0) return fail(`${path}.text`, 'empty text nodes cannot be represented');
      const marks = Object.hasOwn(node, 'marks')
        ? arrayAt(node.marks, `${path}.marks`).map((mark, index) => fromMark(mark, simple, `${path}.marks[${index}]`))
        : [];
      const markKeys = marks.map(mark => mark.type);
      if (new Set(markKeys).size !== markKeys.length) return fail(`${path}.marks`, 'duplicate marks cannot be represented');
      return { type: 'text', text, marks };
    }
    case 'hardBreak':
      exactKeys(node, ['type'], path);
      return { type: 'hardBreak' };
    default:
      return fail(path, `unsupported inline node: ${String(node.type)}`);
  }
}

function fromInlines(content: unknown, simple: boolean, path: string): Inline[] {
  if (content === undefined) return [];
  return arrayAt(content, path).map((inline, index) => fromInline(inline, simple, `${path}[${index}]`));
}

function fromListItems(content: unknown, simple: boolean, task: boolean, path: string): Array<{ blocks: Block[] } | { checked: boolean; blocks: Block[] }> {
  if (content === undefined) return [];
  return arrayAt(content, path).map((item, index) => {
    const itemPath = `${path}[${index}]`;
    const value = objectAt(item, itemPath);
    if (task) {
      exactKeys(value, ['type', 'attrs', 'content'], itemPath);
      if (value.type !== 'taskItem') fail(`${itemPath}.type`, 'expected a task item');
      const attrs = optionalAttrs(value, ['checked'], itemPath);
      const checked = Object.hasOwn(attrs, 'checked') ? attrs.checked : false;
      if (typeof checked !== 'boolean') fail(`${itemPath}.attrs.checked`, 'expected a boolean');
      const blocks = fromBlocks(value.content, simple, `${itemPath}.content`);
      return { checked, blocks };
    }
    exactKeys(value, ['type', 'content'], itemPath);
    if (value.type !== 'listItem') fail(`${itemPath}.type`, 'expected a list item');
    return { blocks: fromBlocks(value.content, simple, `${itemPath}.content`) };
  });
}

function validateTableCellAttrs(value: JsonObject, path: string): void {
  const attrs = optionalAttrs(value, ['colspan', 'rowspan', 'colwidth', 'align'], path);
  if (Object.hasOwn(attrs, 'colspan') && attrs.colspan !== 1) fail(`${path}.attrs.colspan`, 'merged cells are not supported');
  if (Object.hasOwn(attrs, 'rowspan') && attrs.rowspan !== 1) fail(`${path}.attrs.rowspan`, 'merged cells are not supported');
  if (Object.hasOwn(attrs, 'colwidth') && attrs.colwidth !== null) fail(`${path}.attrs.colwidth`, 'custom column widths are not supported');
  if (Object.hasOwn(attrs, 'align') && attrs.align !== null) fail(`${path}.attrs.align`, 'aligned cells are not supported');
}

function fromTable(value: JsonObject, simple: boolean, path: string): Block {
  if (simple) return fail(path, 'tables are not supported in this section');
  exactKeys(value, ['type', 'content'], path);
  const rows = arrayAt(value.content, `${path}.content`);
  if (rows.length === 0) fail(`${path}.content`, 'a table must contain its header row');

  let columnCount: number | null = null;
  const canonicalRows: TableRow[] = rows.map((row, rowIndex) => {
    const rowPath = `${path}.content[${rowIndex}]`;
    const rowValue = objectAt(row, rowPath);
    exactKeys(rowValue, ['type', 'content'], rowPath);
    if (rowValue.type !== 'tableRow') fail(`${rowPath}.type`, 'unsupported table row node');
    const cells = arrayAt(rowValue.content, `${rowPath}.content`);
    if (cells.length === 0) fail(`${rowPath}.content`, 'a row must contain at least one cell');
    if (columnCount === null) columnCount = cells.length;
    if (cells.length !== columnCount) fail(`${rowPath}.content`, 'table rows must have the same number of cells');
    const expectedCellType = rowIndex === 0 ? 'tableHeader' : 'tableCell';
    return {
      type: rowIndex === 0 ? 'header' : 'body',
      cells: cells.map((cell, cellIndex) => {
        const cellPath = `${rowPath}.content[${cellIndex}]`;
        const cellValue = objectAt(cell, cellPath);
        exactKeys(cellValue, ['type', 'attrs', 'content'], cellPath);
        if (cellValue.type !== expectedCellType) {
          fail(`${cellPath}.type`, rowIndex === 0 ? 'the first row must be the header' : 'only the first row may be the header');
        }
        validateTableCellAttrs(cellValue, cellPath);
        const cellBlocks = arrayAt(cellValue.content, `${cellPath}.content`);
        if (cellBlocks.length !== 1) fail(`${cellPath}.content`, 'each cell must contain exactly one paragraph');
        const paragraphPath = `${cellPath}.content[0]`;
        const paragraph = objectAt(cellBlocks[0], paragraphPath);
        exactKeys(paragraph, ['type', 'content'], paragraphPath);
        if (paragraph.type !== 'paragraph') fail(`${paragraphPath}.type`, 'table cells support one paragraph only');
        return fromInlines(paragraph.content, false, `${paragraphPath}.content`);
      }),
    };
  });
  return { type: 'table', rows: canonicalRows };
}

function fromBlock(value: unknown, simple: boolean, path: string): Block {
  const node = objectAt(value, path);
  switch (node.type) {
    case 'paragraph': {
      exactKeys(node, ['type', 'content'], path);
      return { type: 'paragraph', children: fromInlines(node.content, simple, `${path}.content`) };
    }
    case 'heading': {
      exactKeys(node, ['type', 'attrs', 'content'], path);
      const attrs = optionalAttrs(node, ['level'], path);
      const level = Object.hasOwn(attrs, 'level') ? attrs.level : 1;
      if (level === 1) return { type: 'heading1', children: fromInlines(node.content, simple, `${path}.content`) };
      if (level === 2) return { type: 'heading2', children: fromInlines(node.content, simple, `${path}.content`) };
      if (level === 3) return { type: 'heading3', children: fromInlines(node.content, simple, `${path}.content`) };
      return fail(`${path}.attrs.level`, 'only heading levels 1 through 3 are supported');
    }
    case 'orderedList': {
      exactKeys(node, ['type', 'attrs', 'content'], path);
      const attrs = optionalAttrs(node, ['start', 'type'], path);
      if (Object.hasOwn(attrs, 'type') && attrs.type !== null) fail(`${path}.attrs.type`, 'alternate ordered-list markers are not supported');
      const start = positiveIntegerAt(Object.hasOwn(attrs, 'start') ? attrs.start : 1, `${path}.attrs.start`);
      const items = fromListItems(node.content, simple, false, `${path}.content`);
      return { type: 'orderedList', start, items: items as Array<{ blocks: Block[] }> };
    }
    case 'bulletList': {
      exactKeys(node, ['type', 'content'], path);
      const items = fromListItems(node.content, simple, false, `${path}.content`);
      return { type: 'bulletList', items: items as Array<{ blocks: Block[] }> };
    }
    case 'taskList': {
      exactKeys(node, ['type', 'content'], path);
      const items = fromListItems(node.content, simple, true, `${path}.content`);
      return { type: 'taskList', items: items as Array<{ checked: boolean; blocks: Block[] }> };
    }
    case 'blockquote': {
      exactKeys(node, ['type', 'content'], path);
      if (simple) return fail(path, 'block quotes are not supported in this section');
      const blocks = fromBlocks(node.content, simple, `${path}.content`);
      if (blocks.length === 0) return fail(`${path}.content`, 'empty block quotes cannot be represented');
      return { type: 'blockquote', blocks };
    }
    case 'horizontalRule':
      exactKeys(node, ['type'], path);
      if (simple) return fail(path, 'horizontal rules are not supported in this section');
      return { type: 'horizontalRule' };
    case 'table':
      return fromTable(node, simple, path);
    default:
      return fail(path, `unsupported block node: ${String(node.type)}`);
  }
}

function fromBlocks(value: unknown, simple: boolean, path: string): Block[] {
  if (value === undefined) return [];
  return arrayAt(value, path).map((block, index) => fromBlock(block, simple, `${path}[${index}]`));
}

export function fromEditorDocument(doc: JSONContent, simple: boolean): Block[] {
  const value = objectAt(doc, 'doc');
  exactKeys(value, ['type', 'content'], 'doc');
  if (value.type !== 'doc') fail('doc.type', 'expected a ProseMirror document');
  return fromBlocks(value.content, simple, 'doc.content');
}
