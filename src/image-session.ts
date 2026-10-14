import type { AssetManifestEntry, Block } from './generated/note-format.ts';

/** Session-bound native operations used by the content editor. */
export interface ImageEditorBridge {
  canStart: () => boolean;
  stage: (file: Blob) => Promise<AssetManifestEntry>;
  read: (assetId: string) => Promise<Blob>;
  /** Check the complete live draft; the returned predicate expires on any draft change. */
  preflight: (contentBlocks: Block[]) => Promise<() => boolean>;
  /** Register immediately, before asynchronous work, so leaving waits for this paste. */
  track: (operation: Promise<void>) => void;
}
