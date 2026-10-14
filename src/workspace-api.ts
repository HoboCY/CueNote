import { invoke } from '@tauri-apps/api/core';
import type { AssetManifestEntry, NoteContent, PlainText } from './generated/note-format';

export type { NoteContent } from './generated/note-format';

export type PlainNoteText = PlainText;

export interface NoteSummary {
  noteId: string;
  title: string;
  modifiedAt: string;
  notebookId: string | null;
  pinned: boolean;
  excerpt: string;
  trashedAt: string | null;
}

export interface NotebookSummary {
  notebookId: string;
  name: string;
  isDefault: boolean;
}

export interface ViewPreferences {
  sidebarCollapsed: boolean;
  selectedNotebookId: string | null;
}

export interface LibraryView {
  path: string;
  defaultNotebookName: string;
  defaultNotebookId: string;
  notebooks: NotebookSummary[];
  notes: NoteSummary[];
  trashNotes: NoteSummary[];
  warnings: string[];
}

export interface NoteSession {
  sessionId: string;
  noteId: string;
  text: PlainNoteText;
  content: NoteContent;
  revision: string | null;
  readOnlyReason: string | null;
  notebookId: string | null;
  pinned: boolean;
  trashedAt: string | null;
  assets: AssetManifestEntry[];
}

export interface WorkspaceView {
  appVersion: string;
  defaultLibraryPath: string;
  library: LibraryView | null;
  session: NoteSession | null;
  notice: string | null;
  preferences: ViewPreferences;
}

export interface SaveReceipt {
  session: NoteSession;
  library: LibraryView;
}

export interface WorkspaceError {
  kind: string;
  message: string;
  recoveryPath?: string | null;
  recoveryError?: string | null;
}

export interface RecoveryDraftSummary {
  recoveryId: string;
  noteId: string;
  title: string;
  savedAt: string;
  revision: string;
}

export interface RecoveryList {
  drafts: RecoveryDraftSummary[];
  warnings: string[];
}

export interface SavedCopy {
  path: string;
  revision: string;
}

export const workspaceApi = {
  get: () => invoke<WorkspaceView>('get_workspace'),
  useDefault: () => invoke<WorkspaceView>('use_default_library'),
  choose: () => invoke<WorkspaceView | null>('choose_library'),
  openFolder: () => invoke<void>('open_library_folder'),
  newNote: (notebookId: string | null = null) => invoke<NoteSession>('new_note', { notebookId }),
  openNote: (noteId: string) => invoke<NoteSession>('open_note', { noteId }),
  createNotebook: (name: string) => invoke<LibraryView>('create_notebook', { name }),
  renameNotebook: (notebookId: string, name: string) =>
    invoke<LibraryView>('rename_notebook', { notebookId, name }),
  deleteNotebook: (notebookId: string) => invoke<LibraryView>('delete_notebook', { notebookId }),
  updateNoteOrganization: (sessionId: string, notebookId: string, pinned: boolean) =>
    invoke<SaveReceipt>('update_note_organization', { sessionId, notebookId, pinned }),
  trashNote: (sessionId: string) => invoke<SaveReceipt>('trash_note', { sessionId }),
  restoreTrashedNote: (sessionId: string, targetNotebookId: string | null = null) =>
    invoke<SaveReceipt>('restore_trashed_note', { sessionId, targetNotebookId }),
  deleteTrashedNote: (sessionId: string, confirmed: boolean) =>
    invoke<WorkspaceView>('delete_trashed_note', { sessionId, confirmed }),
  setViewPreferences: (sidebarCollapsed: boolean, selectedNotebookId: string | null) =>
    invoke<void>('set_view_preferences', { sidebarCollapsed, selectedNotebookId }),
  save: (sessionId: string, content: NoteContent) =>
    invoke<SaveReceipt>('save_note_content', { sessionId, content }),
  stageImage: (sessionId: string, bytes: Uint8Array) =>
    invoke<AssetManifestEntry>('stage_image_raw', bytes, {
      headers: { 'x-cuenote-session-id': sessionId },
    }),
  preflightImageInsert: (sessionId: string, content: NoteContent) =>
    invoke<void>('preflight_image_insert', { sessionId, content }),
  preflightNoteContent: (sessionId: string, content: NoteContent) =>
    invoke<void>('preflight_note_content', { sessionId, content }),
  readImage: async (sessionId: string, assetId: string) => {
    const bytes = await invoke<ArrayBuffer>('read_image_asset', { sessionId, assetId });
    // The host only returns already validated PNG/JPEG assets from this session.
    const type = new Uint8Array(bytes)[0] === 0x89 ? 'image/png' : 'image/jpeg';
    return new Blob([bytes], { type });
  },
  listRecoveryDrafts: () => invoke<RecoveryList>('list_recovery_drafts'),
  restoreRecoveryAsCopy: (recoveryId: string) =>
    invoke<SaveReceipt>('restore_recovery_as_copy', { recoveryId }),
  saveRecoveryAs: (recoveryId: string) =>
    invoke<SavedCopy | null>('save_recovery_as', { recoveryId }),
  saveCurrentAs: (sessionId: string, content: NoteContent) =>
    invoke<SavedCopy | null>('save_current_content_as', { sessionId, content }),
  activateCloseGuard: () => invoke<void>('activate_close_guard'),
  completeClose: () => invoke<void>('complete_close'),
};
