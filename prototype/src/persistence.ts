import type { JsonNode, NoteAsset, NoteDocument, NoteSnapshot } from './types';

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function walkImages(node: JsonNode, visit: (node: JsonNode) => void): void {
  if (node.type === 'image') visit(node);
  for (const child of node.content ?? []) walkImages(child, visit);
}

export function hydrateSnapshot(snapshot: NoteSnapshot): NoteDocument {
  const document = clone(snapshot.document);
  const assets = new Map(snapshot.assets.map((asset) => [asset.id, asset]));

  walkImages(document.content, (node) => {
    const source = node.attrs?.src;
    if (typeof source !== 'string' || !source.startsWith('asset:')) return;
    const assetId = source.slice('asset:'.length);
    const asset = assets.get(assetId);
    if (!asset) throw new Error(`笔记引用的图片资源不存在：${assetId}`);
    node.attrs = { ...node.attrs, src: `data:${asset.mime};base64,${asset.base64}` };
  });

  return document;
}

async function stableAssetId(mime: string, base64: string): Promise<string> {
  const value = `${mime}\u0000${base64}`;
  if (globalThis.crypto?.subtle) {
    const digest = await globalThis.crypto.subtle.digest('SHA-256', new TextEncoder().encode(value));
    const hex = Array.from(new Uint8Array(digest), (part) => part.toString(16).padStart(2, '0')).join('');
    return `image-${hex.slice(0, 24)}`;
  }

  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `image-${(hash >>> 0).toString(16)}`;
}

export async function makePersistentSnapshot(document: NoteDocument): Promise<NoteSnapshot> {
  const storedDocument = clone(document);
  const assetsById = new Map<string, NoteAsset>();
  const idsByDataUrl = new Map<string, string>();
  const pending: Array<Promise<void>> = [];

  walkImages(storedDocument.content, (node) => {
    const source = node.attrs?.src;
    if (typeof source !== 'string') return;
    if (source.startsWith('asset:')) {
      throw new Error('图片尚未载入为可编辑资源，请重新打开笔记后再保存。');
    }
    if (!source.startsWith('data:')) {
      throw new Error('样机仅支持从本地文件插入的图片。');
    }

    const match = /^data:([^;,]+);base64,([A-Za-z0-9+/=]+)$/i.exec(source);
    if (!match) throw new Error('图片数据格式无法保存。');
    const [, mime, base64] = match;
    const alreadyPending = idsByDataUrl.get(source);
    if (alreadyPending) {
      node.attrs = { ...node.attrs, src: `asset:${alreadyPending}` };
      return;
    }

    pending.push(
      stableAssetId(mime, base64).then((id) => {
        idsByDataUrl.set(source, id);
        assetsById.set(id, { id, mime, base64 });
        node.attrs = { ...node.attrs, src: `asset:${id}` };
      }),
    );
  });

  await Promise.all(pending);
  return {
    document: storedDocument,
    assets: Array.from(assetsById.values()),
  };
}

export function estimateJsonBytes(value: unknown): number {
  return new TextEncoder().encode(JSON.stringify(value)).byteLength;
}
