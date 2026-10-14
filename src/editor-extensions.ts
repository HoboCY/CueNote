import { Extension, textblockTypeInputRule, type Extensions } from '@tiptap/core';
import { Code } from '@tiptap/extension-code';
import { CodeBlockLowlight } from '@tiptap/extension-code-block-lowlight';
import { BulletList, ListItem, ListKeymap, OrderedList, TaskItem, TaskList } from '@tiptap/extension-list';
import { Table, TableCell, TableHeader, TableRow } from '@tiptap/extension-table';
import { StarterKit } from '@tiptap/starter-kit';
import { Plugin } from '@tiptap/pm/state';
import { createLowlight } from 'lowlight';
import { createMathExtensions, type MathEditHandler } from './math-extensions.ts';
import { createImageExtensions, type ImageEditorRuntime } from './image-extensions.ts';
import type { ImageEditorBridge } from './image-session.ts';
import csharp from 'highlight.js/lib/languages/csharp';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import plaintext from 'highlight.js/lib/languages/plaintext';
import python from 'highlight.js/lib/languages/python';
import rust from 'highlight.js/lib/languages/rust';
import sql from 'highlight.js/lib/languages/sql';
import typescript from 'highlight.js/lib/languages/typescript';

const BACKTICK_CODE_FENCE = /^```([^\s`]*)[\s\n]$/;
const TILDE_CODE_FENCE = /^~~~([^\s~]*)[\s\n]$/;

function createCueNoteLowlight() {
  const lowlight = createLowlight();
  lowlight.register({ javascript, typescript, python, rust, csharp, sql, json, plaintext });
  lowlight.registerAlias({
    javascript: 'js',
    typescript: 'ts',
    python: 'py',
    csharp: ['cs', 'c#'],
    plaintext: ['text', 'txt', 'plain'],
  });

  const plainLanguages = new Set(['plaintext', 'text', 'txt', 'plain']);
  const plainTextHast = (value: string) => ({
    type: 'root' as const,
    children: value.length > 0 ? [{ type: 'text' as const, value }] : [],
    data: { language: undefined, relevance: 0 },
  });

  return {
    ...lowlight,
    highlight: (language: string, value: string, options?: Parameters<typeof lowlight.highlight>[2]) => {
      if (plainLanguages.has(language.toLowerCase()) || !lowlight.registered(language)) {
        return plainTextHast(value);
      }
      return lowlight.highlight(language, value, options);
    },
    // Unknown and untagged blocks remain plain text: never guess their language.
    highlightAuto: (value: string) => plainTextHast(value),
  };
}

const cueNoteLowlight = createCueNoteLowlight();

export const TABLE_HEADER_NORMALIZATION_META = 'cuenote:normalize-table-headers';

function isCanonicalLink(href: string | undefined): boolean {
  if (!href || [...href].some(character => /[\s\p{Cc}]/u.test(character))) return false;
  const separator = href.indexOf(':');
  if (separator < 0) return false;
  const scheme = href.slice(0, separator).toLowerCase();
  const rest = href.slice(separator + 1);
  if (scheme === 'mailto') return rest.length > 0;
  return (scheme === 'http' || scheme === 'https')
    && rest.startsWith('//')
    && rest.slice(2).split(/[/?#]/, 1)[0].length > 0;
}

function createTableHeaderNormalization(): Extension {
  return Extension.create({
    name: 'cuenoteTableHeaderNormalization',
    priority: 1000,

    addProseMirrorPlugins() {
      return [new Plugin({
        appendTransaction: (transactions, _oldState, newState) => {
          if (!transactions.some(transaction =>
            transaction.docChanged && transaction.getMeta(TABLE_HEADER_NORMALIZATION_META) === true,
          )) return null;

          const tableType = newState.schema.nodes.table;
          const headerType = newState.schema.nodes.tableHeader;
          const cellType = newState.schema.nodes.tableCell;
          const transaction = newState.tr;
          let changed = false;

          newState.doc.descendants((node, position) => {
            if (node.type !== tableType) return;
            let rowOffset = 0;
            let rowIndex = 0;
            node.forEach(row => {
              let cellOffset = 0;
              row.forEach(cell => {
                const targetType = rowIndex === 0 ? headerType : cellType;
                if (cell.type !== targetType) {
                  const cellPosition = position + 1 + rowOffset + 1 + cellOffset;
                  transaction.setNodeMarkup(cellPosition, targetType, cell.attrs);
                  changed = true;
                }
                cellOffset += cell.nodeSize;
              });
              rowOffset += row.nodeSize;
              rowIndex += 1;
            });
          });

          return changed ? transaction : null;
        },
      })];
    },
  });
}

function createTableCellEnter(): Extension {
  return Extension.create({
    name: 'cuenoteTableCellEnter',
    priority: 1000,

    addKeyboardShortcuts() {
      return {
        Enter: () => {
          const { $from } = this.editor.state.selection;
          for (let depth = $from.depth; depth > 0; depth -= 1) {
            const parentName = $from.node(depth).type.name;
            if (parentName === 'tableCell' || parentName === 'tableHeader') {
              return this.editor.commands.setHardBreak();
            }
          }
          return false;
        },
      };
    },
  });
}

function createCodeBlockPlainTextPaste(): Extension {
  return Extension.create({
    name: 'cuenoteCodeBlockPlainTextPaste',
    priority: 1000,

    addProseMirrorPlugins() {
      const codeBlockType = this.editor.schema.nodes.codeBlock;
      return [new Plugin({
        props: {
          handlePaste: (view, event) => {
            const clipboard = event.clipboardData;
            if (!clipboard) return false;

            const { $from, $to } = view.state.selection;
            if ($from.parent.type !== codeBlockType || $to.parent.type !== codeBlockType) return false;

            const plainText = clipboard.getData('text/plain');
            const text = plainText || clipboard.getData('text/html');
            if (text.length === 0) return true;

            view.dispatch(view.state.tr.replaceSelectionWith(view.state.schema.text(text), false));
            return true;
          },
        },
      })];
    },
  });
}

function createCodeBlockExtension(): Extensions[number] {
  return CodeBlockLowlight.extend({
    addInputRules() {
      const rule = (find: RegExp) => textblockTypeInputRule({
        find,
        type: this.type,
        getAttributes: match => ({ language: match[1] || null }),
      });
      return [rule(BACKTICK_CODE_FENCE), rule(TILDE_CODE_FENCE)];
    },

    addKeyboardShortcuts() {
      return {
        ...this.parent?.(),
        Tab: ({ editor }) => {
          const { $from, $to } = editor.state.selection;
          if ($from.parent.type !== this.type || $to.parent.type !== this.type) return false;
          return editor.commands.command(({ state, tr }) => {
            tr.replaceSelectionWith(state.schema.text('\t'), false);
            return true;
          });
        },
        'Mod-Enter': ({ editor }) => {
          if (editor.state.selection.$from.parent.type !== this.type) return false;
          return editor.commands.exitCode();
        },
      };
    },
  }).configure({
    lowlight: cueNoteLowlight,
    defaultLanguage: null,
    exitOnTripleEnter: false,
    exitOnArrowDown: false,
    exitOnArrowUp: false,
    enableTabIndentation: false,
  });
}

export function createEditorExtensions(
  simple: boolean,
  onEditMath?: MathEditHandler,
  imageBridge?: ImageEditorBridge,
  imageRuntime?: ImageEditorRuntime,
): Extensions {
  return [
    StarterKit.configure({
      blockquote: simple ? false : {},
      bulletList: false,
      code: false,
      codeBlock: false,
      heading: { levels: [1, 2, 3] },
      horizontalRule: simple ? false : {},
      italic: simple ? false : {},
      link: simple ? false : {
        markdownLinks: true,
        openOnClick: false,
        isAllowedUri: href => isCanonicalLink(href),
      },
      listItem: false,
      listKeymap: false,
      orderedList: false,
      strike: simple ? false : {},
      trailingNode: false,
      underline: false,
    }),
    ...(!simple ? [Code.extend({ excludes: '' })] : []),
    ...(!simple ? [createCodeBlockExtension(), createCodeBlockPlainTextPaste()] : []),
    ...(!simple ? createMathExtensions(onEditMath) : []),
    ...(!simple ? createImageExtensions(imageBridge, imageRuntime) : []),
    ListItem.extend({ content: 'block*' }),
    BulletList.extend({ content() { return `${this.options.itemTypeName}*`; } }),
    OrderedList.extend({ content() { return `${this.options.itemTypeName}*`; } }),
    TaskItem.extend({ content: 'block*' }),
    TaskList.extend({ content() { return `${this.options.itemTypeName}*`; } }),
    ListKeymap.configure({
      listTypes: [
        { itemName: 'listItem', wrapperNames: ['bulletList', 'orderedList'] },
        { itemName: 'taskItem', wrapperNames: ['taskList'] },
      ],
    }),
    ...(!simple ? [
      Table.configure({ resizable: false }),
      TableRow,
      TableHeader.extend({ content: 'paragraph' }),
      TableCell.extend({ content: 'paragraph' }),
      createTableCellEnter(),
      createTableHeaderNormalization(),
    ] : []),
  ];
}
