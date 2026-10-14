import { Extension, type Extensions } from '@tiptap/core';
import { BulletList, ListItem, ListKeymap, OrderedList, TaskItem, TaskList } from '@tiptap/extension-list';
import { Table, TableCell, TableHeader, TableRow } from '@tiptap/extension-table';
import { StarterKit } from '@tiptap/starter-kit';
import { Plugin } from '@tiptap/pm/state';

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

export function createEditorExtensions(simple: boolean): Extensions {
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
