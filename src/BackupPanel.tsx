import { useEffect, useRef, useState } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createBackupCoordinator, type BackupCoordinatorState, type RunRestoreWithSavedEditor } from './backup-coordinator';
import {
  workspaceApi,
  type BackupNoteRestoreSelection,
  type BackupNoteSummary,
  type BackupProgress,
  type BackupSummary,
  type NotebookSummary,
} from './workspace-api';

export interface BackupPanelProps {
  libraryPath: string | null;
  changeToken: unknown;
  automaticAllowed: boolean;
  beforeManualBackup: () => Promise<boolean>;
  notebooks: NotebookSummary[];
  restoreAllowed: boolean;
  runRestoreWithSavedEditor: RunRestoreWithSavedEditor;
  onRestoreLibrary?: (backupId: string) => void;
  onBusyChange?: (busy: boolean) => void;
  suspended?: boolean;
}

const desktopRuntime = isTauri();

const initialState: BackupCoordinatorState = {
  libraryPath: null,
  catalog: null,
  backupNotes: null,
  catalogLoading: false,
  operation: null,
  progress: null,
  startedAt: null,
  elapsedSeconds: 0,
  error: null,
  notice: null,
  restoreReceipt: null,
};

const phaseLabels: Record<string, string> = {
  preparing: '正在准备备份',
  checkingSource: '正在检查备份来源',
  copying: '正在复制数据',
  verifying: '正在验证备份',
  publishing: '正在发布备份',
  restoring: '正在恢复备份笔记',
  complete: '备份校验完成',
};

interface RestoreCandidate {
  backupId: string;
  note: BackupNoteSummary;
  targetNotebookId: string | null;
}

function formatCreatedAt(backup: BackupSummary) {
  const date = new Date(backup.createdAt);
  return Number.isNaN(date.getTime()) ? backup.localDate : date.toLocaleString('zh-CN', {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
  });
}

function formatNoteDate(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
  });
}

function operationLabel(state: BackupCoordinatorState) {
  if (state.operation === 'preparing') return '正在保存当前编辑…';
  if (state.operation === 'preparingRestore') return '正在保存当前编辑…';
  if (state.operation === 'choosing') return '正在选择备份位置…';
  if (state.operation === 'cancelling') {
    const counts = state.progress ? ` · 已检查 ${state.progress.checked}/${state.progress.total}` : '';
    const elapsed = state.elapsedSeconds > 2 ? ` · 已用 ${state.elapsedSeconds} 秒` : '';
    return `正在取消当前任务${counts}${elapsed}…`;
  }
  if (state.operation === 'automatic' || state.operation === 'manual' || state.operation === 'listing'
    || state.operation === 'listingNotes' || state.operation === 'restoring') {
    const progress = state.progress;
    const label = progress
      ? phaseLabels[progress.phase] ?? '处理中'
      : state.operation === 'listing' ? '正在校验备份列表'
        : state.operation === 'listingNotes' ? '正在读取备份中的笔记'
          : state.operation === 'restoring' ? '正在恢复备份中的笔记' : '正在创建备份';
    const counts = progress ? ` · 已检查 ${progress.checked}/${progress.total}` : '';
    const elapsed = state.elapsedSeconds > 2 ? ` · 已用 ${state.elapsedSeconds} 秒` : '';
    return `${label}${counts}${elapsed}…`;
  }
  return null;
}

export function BackupPanel({
  libraryPath,
  changeToken,
  automaticAllowed,
  beforeManualBackup,
  notebooks,
  restoreAllowed,
  runRestoreWithSavedEditor,
  onRestoreLibrary,
  onBusyChange,
  suspended = false,
}: BackupPanelProps) {
  const [state, setState] = useState<BackupCoordinatorState>(initialState);
  const [collapsed, setCollapsed] = useState(true);
  const [viewingBackupId, setViewingBackupId] = useState<string | null>(null);
  const [selectedEntryId, setSelectedEntryId] = useState<string | null>(null);
  const [restoreCandidate, setRestoreCandidate] = useState<RestoreCandidate | null>(null);
  const coordinatorRef = useRef<ReturnType<typeof createBackupCoordinator> | null>(null);
  const propsRef = useRef<BackupPanelProps>({
    libraryPath, changeToken, automaticAllowed, beforeManualBackup, notebooks, restoreAllowed, runRestoreWithSavedEditor,
    onRestoreLibrary, onBusyChange, suspended,
  });
  propsRef.current = {
    libraryPath, changeToken, automaticAllowed, beforeManualBackup, notebooks, restoreAllowed, runRestoreWithSavedEditor,
    onRestoreLibrary, onBusyChange, suspended,
  };

  if (coordinatorRef.current === null) {
    coordinatorRef.current = createBackupCoordinator({
      api: {
        runBackup: (kind, requestId, expectedLibraryPath) =>
          workspaceApi.runBackup(kind, requestId, expectedLibraryPath),
        listBackups: (requestId, expectedLibraryPath) =>
          workspaceApi.listBackups(requestId, expectedLibraryPath),
        listBackupNotes: (backupId, requestId, expectedLibraryPath) =>
          workspaceApi.listBackupNotes(backupId, requestId, expectedLibraryPath),
        restoreBackupNote: (selection, requestId, expectedLibraryPath) =>
          workspaceApi.restoreBackupNote(selection, requestId, expectedLibraryPath),
        chooseBackupDirectory: (requestId, expectedLibraryPath) =>
          workspaceApi.chooseBackupDirectory(requestId, expectedLibraryPath),
        cancelBackup: () => workspaceApi.cancelBackup(),
      },
      onState: nextState => {
        setState(nextState);
        propsRef.current.onBusyChange?.(nextState.operation !== null || nextState.catalogLoading);
      },
    });
  }
  const coordinator = coordinatorRef.current;

  useEffect(() => {
    coordinator.activate();
    return () => coordinator.deactivate();
  }, [coordinator]);

  useEffect(() => {
    if (!desktopRuntime) return;
    if (suspended) {
      const synchronizedPath = coordinator.getState().libraryPath;
      if (synchronizedPath !== null) {
        coordinator.synchronize({ libraryPath: synchronizedPath, changeToken, automaticAllowed: false });
      }
      const operation = coordinator.getState().operation;
      if (operation === 'listing' || operation === 'listingNotes') coordinator.cancel();
      return;
    }
    coordinator.synchronize({
      libraryPath,
      changeToken,
      automaticAllowed,
    });
  }, [coordinator, libraryPath, changeToken, automaticAllowed, suspended]);

  useEffect(() => {
    const current = coordinator.getState();
    propsRef.current.onBusyChange?.(current.operation !== null || current.catalogLoading);
  }, [coordinator, onBusyChange]);

  useEffect(() => () => propsRef.current.onBusyChange?.(false), []);

  useEffect(() => {
    setViewingBackupId(null);
    setSelectedEntryId(null);
    setRestoreCandidate(null);
  }, [libraryPath]);

  useEffect(() => {
    setSelectedEntryId(null);
  }, [state.backupNotes]);

  useEffect(() => {
    if (!desktopRuntime) return;
    let active = true;
    let unlisten: (() => void) | null = null;
    void listen<BackupProgress>('cuenote:backup-progress', event => coordinator.handleProgress(event.payload))
      .then(stopListening => {
        if (active) unlisten = stopListening;
        else stopListening();
      })
      .catch(() => undefined);
    return () => {
      active = false;
      unlisten?.();
    };
  }, [coordinator]);

  if (!desktopRuntime || !libraryPath) return null;

  const busy = state.operation !== null;
  const cancelableOperation = state.operation === 'automatic' || state.operation === 'manual'
    || state.operation === 'listing' || state.operation === 'listingNotes'
    || state.operation === 'preparingRestore' || state.operation === 'restoring' || state.operation === 'cancelling';
  const tooLateToCancelRestore = state.operation === 'restoring'
    && (state.progress?.phase === 'restoring' || state.progress?.phase === 'publishing' || state.progress?.phase === 'complete');
  const status = operationLabel(state) ?? (state.catalogLoading ? '正在读取备份列表…' : null);
  const catalog = state.catalog;
  const availableNotebooks = notebooks.filter(notebook =>
    state.backupNotes?.availableNotebookIds.includes(notebook.notebookId));

  const createManualBackup = () => {
    if (propsRef.current.suspended) return;
    const expectedLibraryPath = libraryPath;
    void coordinator.createManualBackup(async () => {
      if (propsRef.current.libraryPath !== expectedLibraryPath) return false;
      const ready = await propsRef.current.beforeManualBackup();
      return ready && propsRef.current.libraryPath === expectedLibraryPath;
    });
  };

  const openBackupNotes = (backup: BackupSummary) => {
    if (propsRef.current.suspended) return;
    setViewingBackupId(backup.backupId);
    setSelectedEntryId(null);
    setRestoreCandidate(null);
    void coordinator.loadBackupNotes(backup.backupId);
  };

  const selectRestoreCandidate = (backupNotes: NonNullable<BackupCoordinatorState['backupNotes']>, note: BackupNoteSummary) => {
    const currentNotebook = note.notebookId === null
      ? undefined
      : propsRef.current.notebooks.find(notebook => notebook.notebookId === note.notebookId
        && backupNotes.availableNotebookIds.includes(notebook.notebookId));
    setRestoreCandidate({
      backupId: backupNotes.backupId,
      note: { ...note },
      targetNotebookId: currentNotebook?.notebookId ?? null,
    });
  };

  const confirmRestore = () => {
    const candidate = restoreCandidate;
    const expectedLibraryPath = libraryPath;
    if (!candidate || !expectedLibraryPath || !propsRef.current.restoreAllowed || propsRef.current.suspended || busy) return;
    const targetNotebookId = candidate.targetNotebookId;
    if (!targetNotebookId || !availableNotebooks.some(notebook => notebook.notebookId === targetNotebookId)) return;
    const selection: BackupNoteRestoreSelection = {
      backupId: candidate.backupId,
      entryId: candidate.note.entryId,
      expectedRevision: candidate.note.revision,
      targetNotebookId,
    };
    setRestoreCandidate(null);
    void coordinator.restoreBackupNote(selection, run => {
      if (propsRef.current.libraryPath !== expectedLibraryPath) return Promise.resolve(null);
      return propsRef.current.runRestoreWithSavedEditor(async () => {
        if (propsRef.current.libraryPath !== expectedLibraryPath) {
          throw { kind: 'backupCancelled', message: '笔记库已切换，恢复未开始。' };
        }
        return run();
      });
    });
  };

  return (
    <section className={`backup-panel${collapsed ? ' is-collapsed' : ''}`} aria-label="笔记库备份">
      <div className="backup-panel-heading">
        <div className="backup-panel-heading-main">
          <button
            className="backup-panel-toggle"
            type="button"
            aria-expanded={!collapsed}
            onClick={() => setCollapsed(value => !value)}
          >
            <span className="backup-panel-icon" aria-hidden="true">↥</span>
            <span className="backup-panel-title">备份</span>
            <span className="backup-panel-count">{catalog ? `${catalog.backups.length} 个` : '整个笔记库'}</span>
            <span className="backup-panel-chevron" aria-hidden="true">{collapsed ? '⌄' : '⌃'}</span>
          </button>
          {status && <span className="backup-panel-status" role="status" aria-live="polite">{status}</span>}
          {!status && (state.notice || state.error) && (
            <span className={`backup-panel-status${state.error ? ' is-error' : ''}`} role={state.error ? 'alert' : 'status'} aria-live="polite">
              {state.notice ?? state.error}
            </span>
          )}
        </div>
        <div className="backup-panel-actions">
          {cancelableOperation ? (
            <button
              className="backup-button backup-button-secondary"
              type="button"
              disabled={suspended || state.operation === 'cancelling' || tooLateToCancelRestore}
              onClick={() => coordinator.cancel()}
            >
              {state.operation === 'listing' || state.operation === 'listingNotes'
                ? '取消读取'
                : state.operation === 'restoring' || state.operation === 'preparingRestore' ? '取消恢复' : '取消'}
            </button>
          ) : (
            <button className="backup-button backup-button-primary" type="button" disabled={suspended || busy || state.catalogLoading} onClick={createManualBackup}>
              {state.operation === 'preparing' ? '正在准备…' : '立即备份'}
            </button>
          )}
        </div>
      </div>

      {collapsed && catalog?.warnings.length ? (
        <p className="backup-message is-warning" role="status">
          {catalog.warnings[0]}
          {catalog.warnings.length > 1 ? `（共 ${catalog.warnings.length} 条提示，展开查看）` : ''}
        </p>
      ) : null}

      {suspended && (
        <p className="backup-message is-warning" role="status">
          整库恢复正在占用笔记库操作；普通备份、单篇恢复和备份位置变更已暂停。
        </p>
      )}

      {state.restoreReceipt && (
        <div className="backup-message backup-restore-result" role="status" aria-live="polite">
          <strong>{state.restoreReceipt.message || '备份中的笔记已恢复。'}</strong>
          <span>恢复前手动备份已创建：{formatCreatedAt(state.restoreReceipt.preRestoreBackup)}</span>
          {state.restoreReceipt.reassignedNotebook && <span>已按所选笔记本调整归属。</span>}
          {state.restoreReceipt.warnings.map((warning, index) => (
            <span key={`${index}-${warning}`}>{warning}</span>
          ))}
        </div>
      )}

      {!collapsed && (
        <div className="backup-panel-body">
          <div className="backup-location-row">
            <div className="backup-location-copy">
              <span className="backup-label">备份位置</span>
              <code title={catalog?.directory ?? ''}>{catalog?.directory || '正在读取备份位置…'}</code>
            </div>
            <button
              className="backup-button backup-button-secondary"
              type="button"
              disabled={suspended || busy || state.catalogLoading}
              onClick={() => { void coordinator.changeDirectory(); }}
            >
              更改位置
            </button>
          </div>

          {state.error && <p className="backup-message is-error" role="alert">{state.error}</p>}
          {!state.error && state.notice && <p className="backup-message" role="status">{state.notice}</p>}
          {!restoreAllowed && (
            <p className="backup-message is-warning" role="status">
              当前笔记库保护状态不允许恢复操作。受保护位置：<code>{libraryPath}</code>
            </p>
          )}
          {!onRestoreLibrary && catalog?.backups.length ? (
            <p className="backup-message is-warning" role="status">
              整库恢复入口尚未连接；当前笔记库位置：<code>{libraryPath}</code>
            </p>
          ) : null}
          {catalog?.warnings.map((warning, index) => (
            <p className="backup-message is-warning" key={`${index}-${warning}`}>{warning}</p>
          ))}

          <div className="backup-list-heading">
            <strong>备份记录</strong>
            <button
              className="backup-refresh-button"
              type="button"
              disabled={suspended || state.catalogLoading || busy}
              onClick={() => { void coordinator.refresh(); }}
            >
              {state.catalogLoading ? '正在刷新…' : '刷新列表'}
            </button>
          </div>
          {state.catalogLoading && !catalog ? (
            <p className="backup-empty-state">正在读取经过校验的备份列表…</p>
          ) : catalog?.backups.length ? (
            <ul className="backup-list">
              {catalog.backups.map(backup => (
                <li className="backup-list-item" key={backup.backupId}>
                  <div className="backup-list-item-main">
                    <strong>{formatCreatedAt(backup)}</strong>
                    <span>{backup.kind === 'automatic' ? '自动备份' : '手动备份'}</span>
                  </div>
                  <div className="backup-list-item-meta">
                    <span>{backup.noteCount} 篇笔记</span>
                    <span>{backup.notebookCount} 个笔记本</span>
                    <button
                      className="backup-refresh-button"
                      type="button"
                      disabled={suspended || busy}
                      onClick={() => openBackupNotes(backup)}
                    >
                      {state.operation === 'listingNotes' && viewingBackupId === backup.backupId
                        ? '正在读取…' : '查看笔记'}
                    </button>
                    <button
                      className="backup-refresh-button"
                      type="button"
                      disabled={suspended || busy || state.catalogLoading || !restoreAllowed || !onRestoreLibrary}
                      onClick={() => onRestoreLibrary?.(backup.backupId)}
                    >恢复整个笔记库…</button>
                  </div>
                </li>
              ))}
            </ul>
          ) : (
            <p className="backup-empty-state">
              {catalog
                ? `没有可用于整库恢复的备份记录。受保护笔记库位置：${libraryPath}`
                : '备份列表尚未加载。'}
            </p>
          )}

          {viewingBackupId && (
            <section className="backup-note-browser" aria-label="备份中的笔记">
              <div className="backup-note-browser-heading">
                <strong>备份中的笔记</strong>
                <button
                  className="backup-refresh-button"
                  type="button"
                  disabled={suspended || busy}
                  onClick={() => setViewingBackupId(null)}
                >返回备份列表</button>
              </div>
              {state.operation === 'listingNotes' && state.backupNotes?.backupId !== viewingBackupId ? (
                <p className="backup-empty-state">正在读取经过校验的笔记条目…</p>
              ) : state.backupNotes?.libraryPath === libraryPath && state.backupNotes.backupId === viewingBackupId ? (
                state.backupNotes.notes.length ? (
                  <>
                    <ul className="backup-note-list">
                      {state.backupNotes.notes.map(note => (
                        <li className="backup-note-item" key={note.entryId}>
                          <label className="backup-note-choice">
                            <input
                              type="radio"
                              name="backup-note-entry"
                              value={note.entryId}
                              checked={selectedEntryId === note.entryId}
                              disabled={suspended || busy}
                              onChange={() => setSelectedEntryId(note.entryId)}
                            />
                            <span className="backup-note-copy">
                              <strong>{note.title || '未命名笔记'}</strong>
                              <span>{formatNoteDate(note.modifiedAt)} · {note.notebookName || '笔记本归属待选择'}</span>
                              <span>{note.pinned ? '已置顶 · ' : ''}{note.trashedAt ? '来自回收站，恢复后仍为只读' : '普通笔记'}</span>
                              {note.excerpt && <span className="backup-note-excerpt">{note.excerpt}</span>}
                            </span>
                          </label>
                        </li>
                      ))}
                    </ul>
                    <button
                      className="backup-button backup-button-primary"
                      type="button"
                      disabled={!selectedEntryId || suspended || busy}
                      onClick={() => {
                        const note = state.backupNotes?.notes.find(item => item.entryId === selectedEntryId);
                        if (note && state.backupNotes) selectRestoreCandidate(state.backupNotes, note);
                      }}
                    >选择并确认恢复…</button>
                  </>
                ) : (
                  <p className="backup-empty-state">此备份中没有笔记条目。</p>
                )
              ) : state.operation !== 'listingNotes' ? (
                <div className="backup-note-load-error">
                  {state.error && <p className="backup-message is-error" role="alert">{state.error}</p>}
                  <button
                    className="backup-refresh-button"
                    type="button"
                    disabled={suspended || busy}
                    onClick={() => { void coordinator.loadBackupNotes(viewingBackupId); }}
                  >重新读取</button>
                </div>
              ) : null}
            </section>
          )}

          {restoreCandidate && (
            <section className="backup-restore-confirmation" aria-label="确认恢复笔记">
              <strong>确认恢复“{restoreCandidate.note.title || '未命名笔记'}”？</strong>
              <p>将使用备份中的同一笔记 ID；当前库若已有该 ID 会覆盖原笔记，否则创建为新笔记。恢复前会创建手动备份，该备份不会自动清理。</p>
              {restoreCandidate.note.notebookId !== null
                && availableNotebooks.some(notebook => notebook.notebookId === restoreCandidate.note.notebookId) ? (
                <p className="backup-note-assignment">
                  将沿用当前笔记本名称“{notebooks.find(notebook => notebook.notebookId === restoreCandidate.note.notebookId)?.name}”。
                </p>
              ) : (
                <label className="backup-notebook-picker">
                  <span>原笔记本不存在或有冲突，请选择当前有效笔记本</span>
                  <select
                    value={restoreCandidate.targetNotebookId ?? ''}
                    disabled={!restoreAllowed || suspended || busy || availableNotebooks.length === 0}
                    onChange={event => setRestoreCandidate(current => current
                      ? { ...current, targetNotebookId: event.target.value || null }
                      : null)}
                  >
                    <option value="">请选择笔记本…</option>
                    {availableNotebooks.map(notebook => (
                      <option key={notebook.notebookId} value={notebook.notebookId}>{notebook.name}</option>
                    ))}
                  </select>
                  {availableNotebooks.length === 0 && <small>当前没有可用笔记本，暂时无法恢复此条目。</small>}
                </label>
              )}
              {restoreCandidate.note.trashedAt && (
                <p className="backup-note-assignment">此条目来自回收站，恢复后仍留在回收站并保持只读。</p>
              )}
              <div className="backup-confirmation-actions">
                <button
                  className="backup-button backup-button-secondary"
                  type="button"
                  disabled={suspended || busy}
                  onClick={() => setRestoreCandidate(null)}
                >取消</button>
                <button
                  className="backup-button backup-button-primary"
                  type="button"
                  disabled={!restoreAllowed || suspended || busy || !restoreCandidate.targetNotebookId
                    || !availableNotebooks.some(notebook => notebook.notebookId === restoreCandidate.targetNotebookId)}
                  onClick={confirmRestore}
                >确认恢复此笔记</button>
              </div>
            </section>
          )}
          <p className="backup-retention-note">自动备份保留最近 30 份；手动备份会一直保留。</p>
        </div>
      )}
    </section>
  );
}
