import { Node, mergeAttributes } from '@tiptap/core';
import Image from '@tiptap/extension-image';
import { NodeViewWrapper, ReactNodeViewRenderer, type NodeViewProps } from '@tiptap/react';
import MindElixir from 'mind-elixir';
import { zh_CN } from 'mind-elixir/i18n';
import { useEffect, useRef, useState } from 'react';
import { makeMindMap } from './sample';
import type { MindMapData, MindMapNode } from './types';

function copyMapData(value: unknown): MindMapData {
  if (
    value &&
    typeof value === 'object' &&
    'nodeData' in value &&
    typeof (value as { nodeData?: { topic?: unknown } }).nodeData?.topic === 'string'
  ) {
    return JSON.parse(JSON.stringify(value)) as MindMapData;
  }
  return makeMindMap();
}

function MapBranch({ node, root = false }: { node: MindMapNode; root?: boolean }) {
  return (
    <li>
      <span className={root ? 'map-preview-node map-preview-root' : 'map-preview-node'}>{node.topic}</span>
      {node.children?.length ? (
        <ul>
          {node.children.map((child) => <MapBranch key={child.id} node={child} />)}
        </ul>
      ) : null}
    </li>
  );
}

function MindMapNodeView({ editor, getPos, node, updateAttributes, selected }: NodeViewProps) {
  const [open, setOpen] = useState(false);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const canvasRef = useRef<HTMLDivElement>(null);
  const mindRef = useRef<InstanceType<typeof MindElixir> | null>(null);
  const data = copyMapData(node.attrs.data);

  useEffect(() => {
    if (!open || !dialogRef.current || !canvasRef.current) return;

    const dialog = dialogRef.current;
    dialog.showModal();
    const mind = new MindElixir({
      el: canvasRef.current,
      direction: MindElixir.RIGHT,
      toolBar: true,
      keypress: true,
      contextMenu: { locale: zh_CN },
    });
    mind.init(copyMapData(node.attrs.data));
    mindRef.current = mind;

    return () => {
      (mind as InstanceType<typeof MindElixir> & { destroy?: () => void }).destroy?.();
      mindRef.current = null;
      if (dialog.open) dialog.close();
    };
  }, [open]);

  function closeAndCommit() {
    const latest = mindRef.current?.getData();
    if (latest) updateAttributes({ data: copyMapData(latest) });
    if (dialogRef.current?.open) dialogRef.current.close();
    setOpen(false);
    requestAnimationFrame(() => {
      try {
        const position = getPos();
        if (typeof position === 'number') {
          editor.chain().focus().setNodeSelection(position).run();
          return;
        }
      } catch {
        // The map may have been removed while its editor dialog was open.
      }
      editor.commands.focus();
    });
  }

  return (
    <NodeViewWrapper className={selected ? 'mind-map-block is-selected' : 'mind-map-block'}>
      <div className="mind-map-card" contentEditable={false}>
        <div className="mind-map-card-heading">
          <span className="content-block-label">可编辑思维导图</span>
          <button
            type="button"
            className="small-button map-open-button"
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => setOpen(true)}
          >
            展开编辑
          </button>
        </div>
        <div className="map-preview" aria-label={`思维导图预览：${data.nodeData.topic}`}>
          <ul className="map-preview-tree">
            <MapBranch node={data.nodeData} root />
          </ul>
        </div>
        <p className="map-hint">双击节点可改文字；使用工具栏或快捷键添加节点、调整层级。关闭时作为一次正文改动保存。</p>
      </div>

      {open ? (
        <dialog
          ref={dialogRef}
          className="map-editor-dialog"
          aria-labelledby="map-editor-title"
          onCancel={(event) => {
            event.preventDefault();
            closeAndCommit();
          }}
        >
          <div className="dialog-heading">
            <div>
              <p className="eyebrow">内容区 · 思维导图</p>
              <h2 id="map-editor-title">编辑思维导图</h2>
            </div>
            <button type="button" className="quiet-button" onClick={closeAndCommit}>完成</button>
          </div>
          <div className="mind-editor-canvas" ref={canvasRef} />
          <p className="dialog-help">双击节点编辑文字。选中节点后可用工具栏、右键菜单或键盘快捷键增删节点并改变层级。</p>
        </dialog>
      ) : null}
    </NodeViewWrapper>
  );
}

function ResizableImageView({ deleteNode, node, updateAttributes, selected }: NodeViewProps) {
  const rawWidth = Number(node.attrs.widthPercent);
  const widthPercent = Number.isFinite(rawWidth) ? Math.min(100, Math.max(25, rawWidth)) : 100;
  const isPending = typeof node.attrs.pendingId === 'string' && node.attrs.pendingId.length > 0;

  return (
    <NodeViewWrapper className={selected ? 'resizable-image is-selected' : 'resizable-image'}>
      {isPending ? (
        <div className="image-pending" contentEditable={false}>
          <span>正在读取图片：{String(node.attrs.alt ?? '本地图片')}</span>
          <button type="button" className="quiet-button" onMouseDown={(event) => event.preventDefault()} onClick={deleteNode}>
            取消
          </button>
        </div>
      ) : (
        <>
          <img
            src={String(node.attrs.src ?? '')}
            alt={String(node.attrs.alt ?? '')}
            title={typeof node.attrs.title === 'string' ? node.attrs.title : undefined}
            style={{ width: `${widthPercent}%` }}
            draggable={false}
          />
          <div className="image-size-control" contentEditable={false}>
            <label>
              图片宽度
              <input
                type="range"
                min="25"
                max="100"
                step="1"
                value={widthPercent}
                aria-label="调整图片宽度，保持长宽比"
                onPointerDown={(event) => event.stopPropagation()}
                onChange={(event) => updateAttributes({ widthPercent: Number(event.currentTarget.value) })}
              />
              <span>{widthPercent}%</span>
            </label>
          </div>
        </>
      )}
    </NodeViewWrapper>
  );
}

export const ResizableImage = Image.extend({
  addAttributes() {
    return {
      ...this.parent?.(),
      widthPercent: {
        default: 100,
        parseHTML: (element) => Number(element.getAttribute('data-width-percent')) || 100,
        renderHTML: (attributes) => ({ 'data-width-percent': attributes.widthPercent }),
      },
      pendingId: {
        default: null,
        renderHTML: () => ({}),
      },
    };
  },
  addNodeView() {
    return ReactNodeViewRenderer(ResizableImageView);
  },
}).configure({ allowBase64: true });

export const MindMapExtension = Node.create({
  name: 'mindMap',
  group: 'block',
  atom: true,
  draggable: true,
  addAttributes() {
    return { data: { default: null } };
  },
  parseHTML() {
    return [{ tag: 'div[data-type="mind-map"]' }];
  },
  renderHTML({ HTMLAttributes }) {
    return ['div', mergeAttributes(HTMLAttributes, { 'data-type': 'mind-map' }), '思维导图'];
  },
  addNodeView() {
    return ReactNodeViewRenderer(MindMapNodeView);
  },
});
