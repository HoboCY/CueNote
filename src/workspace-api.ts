import { invoke } from '@tauri-apps/api/core';
import type { PlainText } from './generated/note-format';

export type PlainNoteText = PlainText;

export interface NoteSummary {
  noteId: string;
  title: string;
  modifiedAt: string;
  notebookId: string | null;
  pinned: boolean;
  excerpt: string;
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
  warnings: string[];
}

export interface NoteSession {
  sessionId: string;
  noteId: string;
  text: PlainNoteText;
  revision: string | null;
  readOnlyReason: string | null;
  notebookId: string | null;
  pinned: boolean;
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
  setViewPreferences: (sidebarCollapsed: boolean, selectedNotebookId: string | null) =>
    invoke<void>('set_view_preferences', { sidebarCollapsed, selectedNotebookId }),
  save: (sessionId: string, text: PlainNoteText) =>
    invoke<SaveReceipt>('save_note_text', { sessionId, text }),
  listRecoveryDrafts: () => invoke<RecoveryList>('list_recovery_drafts'),
  restoreRecoveryAsCopy: (recoveryId: string) =>
    invoke<SaveReceipt>('restore_recovery_as_copy', { recoveryId }),
  saveRecoveryAs: (recoveryId: string) =>
    invoke<SavedCopy | null>('save_recovery_as', { recoveryId }),
  saveCurrentAs: (sessionId: string, text: PlainNoteText) =>
    invoke<SavedCopy | null>('save_current_as', { sessionId, text }),
  activateCloseGuard: () => invoke<void>('activate_close_guard'),
  completeClose: () => invoke<void>('complete_close'),
};
