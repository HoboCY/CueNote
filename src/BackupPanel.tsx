import { useEffect, useRef, useState } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { createBackupCoordinator, type BackupCoordinatorState } from './backup-coordinator';
import { workspaceApi, type BackupProgress, type BackupSummary } from './workspace-api';

export interface BackupPanelProps {
  libraryPath: string | null;
  changeToken: unknown;
  automaticAllowed: boolean;
  beforeManualBackup: () => Promise<boolean>;
}

const desktopRuntime = isTauri();

const initialState: BackupCoordinatorState = {
  libraryPath: null,
  catalog: null,
  catalogLoading: false,
  operation: null,
  progress: null,
  startedAt: null,
  elapsedSeconds: 0,
  error: null,
  notice: null,
};

const phaseLabels: Record<string, string> = {
  preparing: '正在准备备份',
  checkingSource: '正在检查备份来源',
  copying: '正在复制数据',
  verifying: '正在验证备份',
  publishing: '正在发布备份',
  complete: '备份校验完成',
};

function formatCreatedAt(backup: BackupSummary) {
  const date = new Date(backup.createdAt);
  return Number.isNaN(date.getTime()) ? backup.localDate : date.toLocaleString('zh-CN', {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
  });
}

function operationLabel(state: BackupCoordinatorState) {
  if (state.operation === 'preparing') return '正在保存当前编辑…';
  if (state.operation === 'choosing') return '正在选择备份位置…';
  if (state.operation === 'cancelling') {
    const counts = state.progress ? ` · 已检查 ${state.progress.checked}/${state.progress.total}` : '';
    const elapsed = state.elapsedSeconds > 2 ? ` · 已用 ${state.elapsedSeconds} 秒` : '';
    return `正在取消当前任务${counts}${elapsed}…`;
  }
  if (state.operation === 'automatic' || state.operation === 'manual' || state.operation === 'listing') {
    const progress = state.progress;
    const label = progress
      ? phaseLabels[progress.phase] ?? '处理中'
      : state.operation === 'listing' ? '正在校验备份列表' : '正在创建备份';
    const counts = progress ? ` · 已检查 ${progress.checked}/${progress.total}` : '';
    const elapsed = state.elapsedSeconds > 2 ? ` · 已用 ${state.elapsedSeconds} 秒` : '';
    return `${label}${counts}${elapsed}…`;
  }
  return null;
}

export function BackupPanel({ libraryPath, changeToken, automaticAllowed, beforeManualBackup }: BackupPanelProps) {
  const [state, setState] = useState<BackupCoordinatorState>(initialState);
  const [collapsed, setCollapsed] = useState(true);
  const coordinatorRef = useRef<ReturnType<typeof createBackupCoordinator> | null>(null);
  const propsRef = useRef<BackupPanelProps>({ libraryPath, changeToken, automaticAllowed, beforeManualBackup });
  propsRef.current = { libraryPath, changeToken, automaticAllowed, beforeManualBackup };

  if (coordinatorRef.current === null) {
    coordinatorRef.current = createBackupCoordinator({
      api: {
        runBackup: (kind, requestId, expectedLibraryPath) =>
          workspaceApi.runBackup(kind, requestId, expectedLibraryPath),
        listBackups: (requestId, expectedLibraryPath) =>
          workspaceApi.listBackups(requestId, expectedLibraryPath),
        chooseBackupDirectory: (requestId, expectedLibraryPath) =>
          workspaceApi.chooseBackupDirectory(requestId, expectedLibraryPath),
        cancelBackup: () => workspaceApi.cancelBackup(),
      },
      onState: setState,
    });
  }
  const coordinator = coordinatorRef.current;

  useEffect(() => {
    coordinator.activate();
    return () => coordinator.deactivate();
  }, [coordinator]);

  useEffect(() => {
    if (!desktopRuntime) return;
    coordinator.synchronize({ libraryPath, changeToken, automaticAllowed });
  }, [coordinator, libraryPath, changeToken, automaticAllowed]);

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
  const runningBackup = state.operation === 'automatic' || state.operation === 'manual'
    || state.operation === 'listing' || state.operation === 'cancelling';
  const status = operationLabel(state) ?? (state.catalogLoading ? '正在读取备份列表…' : null);
  const catalog = state.catalog;

  const createManualBackup = () => {
    const expectedLibraryPath = libraryPath;
    void coordinator.createManualBackup(async () => {
      if (propsRef.current.libraryPath !== expectedLibraryPath) return false;
      const ready = await propsRef.current.beforeManualBackup();
      return ready && propsRef.current.libraryPath === expectedLibraryPath;
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
          {runningBackup ? (
            <button className="backup-button backup-button-secondary" type="button" disabled={state.operation === 'cancelling'} onClick={() => coordinator.cancel()}>
              {state.operation === 'listing' ? '取消校验' : '取消'}
            </button>
          ) : (
            <button className="backup-button backup-button-primary" type="button" disabled={busy || state.catalogLoading} onClick={createManualBackup}>
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
              disabled={busy || state.catalogLoading}
              onClick={() => { void coordinator.changeDirectory(); }}
            >
              更改位置
            </button>
          </div>

          {state.error && <p className="backup-message is-error" role="alert">{state.error}</p>}
          {!state.error && state.notice && <p className="backup-message" role="status">{state.notice}</p>}
          {catalog?.warnings.map((warning, index) => (
            <p className="backup-message is-warning" key={`${index}-${warning}`}>{warning}</p>
          ))}

          <div className="backup-list-heading">
            <strong>备份记录</strong>
            <button
              className="backup-refresh-button"
              type="button"
              disabled={state.catalogLoading || busy}
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
                  </div>
                </li>
              ))}
            </ul>
          ) : (
            <p className="backup-empty-state">{catalog ? '还没有备份记录。' : '备份列表尚未加载。'}</p>
          )}
          <p className="backup-retention-note">自动备份保留最近 30 份；手动备份会一直保留。</p>
        </div>
      )}
    </section>
  );
}
