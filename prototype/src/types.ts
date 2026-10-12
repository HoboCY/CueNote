export type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | { [key: string]: JsonValue };

export interface JsonNode {
  type?: string;
  attrs?: Record<string, unknown>;
  content?: JsonNode[];
  text?: string;
  marks?: Array<{ type: string; attrs?: Record<string, unknown> }>;
}

export interface NoteDocument {
  schemaVersion: 1;
  id: string;
  title: string;
  cue: JsonNode;
  content: JsonNode;
  summary: JsonNode;
}

export interface NoteAsset {
  id: string;
  mime: string;
  base64: string;
}

export interface NoteSnapshot {
  document: NoteDocument;
  assets: NoteAsset[];
}

export interface PrototypeInfo {
  path: string;
  recoveryPath: string;
}

export interface SaveResult {
  revision: string;
  elapsedMs: number;
  bytes: number;
}

export interface MindMapNode {
  id: string;
  topic: string;
  children?: MindMapNode[];
  [key: string]: unknown;
}

export interface MindMapData {
  nodeData: MindMapNode;
  linkData?: Array<Record<string, unknown>>;
  [key: string]: unknown;
}
