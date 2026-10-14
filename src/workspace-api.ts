import { invoke } from '@tauri-apps/api/core';
import type { PlainText } from './generated/note-format';

export type PlainNoteText = PlainText;

export interface NoteSummary {
  noteId: string;
  title: string;
  modifiedAt: string;
}

export interface LibraryView {
  path: string;
  defaultNotebookName: string;
  notes: NoteSummary[];
  warnings: string[];
}

export interface NoteSession {
  sessionId: string;
  noteId: string;
  text: PlainNoteText;
  revision: string | null;
  readOnlyReason: string | null;
}

export interface WorkspaceView {
  appVersion: string;
  defaultLibraryPath: string;
  library: LibraryView | null;
  session: NoteSession | null;
  notice: string | null;
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

export const workspaceApi = {
  get: () => invoke<WorkspaceView>('get_workspace'),
  useDefault: () => invoke<WorkspaceView>('use_default_library'),
  choose: () => invoke<WorkspaceView | null>('choose_library'),
  openFolder: () => invoke<void>('open_library_folder'),
  newNote: () => invoke<NoteSession>('new_note'),
  openNote: (noteId: string) => invoke<NoteSession>('open_note', { noteId }),
  save: (sessionId: string, text: PlainNoteText) =>
    invoke<SaveReceipt>('save_note_text', { sessionId, text }),
};
