import { useEffect, useRef, useState } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import {
  createLibraryRestoreCoordinator,
  type LibraryRestoreCoordinatorState,
  type RunWithSavedEditor,
} from './library-restore-coordinator';
import { workspaceApi, type BackupProgress, type LibraryRestoreAction, type LibraryRestoreStatus, type WorkspaceView } from './workspace-api';
import './library-restore.css';

export interface LibraryRestorePanelProps {
  libraryPath: string | null;
  backupId: string | null;
  pending: LibraryRestoreStatus | null;
  allowed: boolean;
  disabledReason?: string | null;
  runWithSavedEditor: RunWithSavedEditor;
  onWorkspace: (view: WorkspaceView) => void;
  onDismiss: () => void;
  onBusyChange: (busy: boolean) => void;
}

const desktopRuntime = isTauri();

const initialState: LibraryRestoreCoordinatorState = {
  libraryPath: null,
  selectedBackupId: null,
  preview: null,
  pending: null,
  allowed: false,
  operation: null,
  requestId: null,
  progress: null,
  startedAt: null,
  elapsedSeconds: 0,
  error: null,
  notice: null,
  receipt: null,
};

function dateLabel(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN', {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit',
  });
}

function phaseLabel(phase: string) {
  switch (phase) {
    case 'preparing': return '正在准备恢复';
    case 'checkingSource': return '正在校验来源备份';
    case 'staging': return '正在准备恢复文件';
    case 'restoring': return '正在替换笔记库文件';
    case 'rollingBack': return '正在回退笔记库文件';
    case 'verifying': return '正在核对文件';
    case 'complete': return '正在完成恢复';
    default: return '正在处理整库恢复';
  }
}

function operationLabel(state: LibraryRestoreCoordinatorState) {
  if (state.operation === 'previewing') return '正在预校验来源备份…';
  if (state.operation === 'preparing') return '正在保存并暂停当前编辑…';
  if (state.operation === 'cancelling') return '正在请求安全暂停…';
  if (state.operation === 'refreshing') return '正在读取宿主恢复状态…';
  if (state.operation === 'restoring' || state.operation === 'resuming') {
    const progress = state.progress;
    const phase = progress ? phaseLabel(progress.phase) : '正在恢复整个笔记库';
    const counts = progress ? ` · 已处理 ${progress.checked}/${progress.total}` : '';
    const elapsed = state.elapsedSeconds > 2 ? ` · 已用 ${state.elapsedSeconds} 秒` : '';
    return `${phase}${counts}${elapsed}…`;
  }
  return null;
}

export function LibraryRestorePanel({
  libraryPath,
  backupId,
  pending,
  allowed,
  disabledReason,
  runWithSavedEditor,
  onWorkspace,
  onDismiss,
  onBusyChange,
}: LibraryRestorePanelProps) {
  const [state, setState] = useState<LibraryRestoreCoordinatorState>(initialState);
  const [receiptDismissed, setReceiptDismissed] = useState(false);
  const propsRef = useRef<LibraryRestorePanelProps>({
    libraryPath, backupId, pending, allowed, disabledReason, runWithSavedEditor, onWorkspace, onDismiss, onBusyChange,
  });
  const previewAttemptRef = useRef<string | null>(null);
  propsRef.current = {
    libraryPath, backupId, pending, allowed, disabledReason, runWithSavedEditor, onWorkspace, onDismiss, onBusyChange,
  };

  const coordinatorRef = useRef<ReturnType<typeof createLibraryRestoreCoordinator> | null>(null);
  if (coordinatorRef.current === null) {
    coordinatorRef.current = createLibraryRestoreCoordinator({
      api: {
        previewLibraryRestore: (selectedBackupId, requestId, expectedLibraryPath) =>
          workspaceApi.previewLibraryRestore(selectedBackupId, requestId, expectedLibraryPath),
        restoreLibraryFromBackup: (selectedBackupId, expectedManifestRevision, requestId, expectedLibraryPath) =>
          workspaceApi.restoreLibraryFromBackup(
            selectedBackupId,
            expectedManifestRevision,
            requestId,
            expectedLibraryPath,
          ),
        resumeLibraryRestore: (action, requestId, expectedLibraryPath) =>
          workspaceApi.resumeLibraryRestore(action, requestId, expectedLibraryPath),
        get: () => workspaceApi.get(),
        cancelBackup: () => workspaceApi.cancelBackup(),
      },
      onState: setState,
      onWorkspace: view => propsRef.current.onWorkspace(view),
    });
  }
  const coordinator = coordinatorRef.current;

  useEffect(() => {
    if (state.receipt) setReceiptDismissed(false);
  }, [state.receipt]);

  useEffect(() => {
    coordinator.activate();
    return () => coordinator.deactivate();
  }, [coordinator]);

  useEffect(() => {
    coordinator.synchronize({ libraryPath, backupId, pending, allowed });
  }, [coordinator, libraryPath, backupId, pending, allowed]);

  useEffect(() => {
    const effectivePending = pending ?? state.pending;
    if (effectivePending || !libraryPath || !backupId) {
      previewAttemptRef.current = null;
      return;
    }
    if (!libraryPath || !backupId || !allowed || state.receipt || state.operation !== null) return;
    if (state.preview?.backup.backupId === backupId) return;
    const attemptKey = `${libraryPath}\u0000${backupId}`;
    if (previewAttemptRef.current === attemptKey) return;
    previewAttemptRef.current = attemptKey;
    void coordinator.previewBackup(backupId);
  }, [coordinator, libraryPath, backupId, pending, allowed, state.pending, state.receipt, state.operation, state.preview]);

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

  useEffect(() => {
    onBusyChange(state.operation !== null);
  }, [state.operation]);

  useEffect(() => () => propsRef.current.onBusyChange(false), []);

  const effectivePath = libraryPath ?? pending?.libraryPath ?? state.pending?.libraryPath
    ?? state.receipt?.workspace.library?.path ?? null;
  const effectivePending = pending ?? state.pending;
  const matchingPending = effectivePending?.libraryPath === effectivePath ? effectivePending : null;
  const selectedPreview = state.preview?.backup.backupId === backupId ? state.preview : null;
  const busy = state.operation !== null;
  const status = operationLabel(state);
  const canConfirm = Boolean(effectivePath && backupId && selectedPreview && allowed
    && !matchingPending && !busy && state.selectedBackupId === backupId);

  if (!effectivePath || (!backupId && !matchingPending && (!state.receipt || receiptDismissed))) return null;

  const startRestore = () => {
    const expectedLibraryPath = effectivePath;
    void coordinator.startRestore(async (run, resume) => {
      if (propsRef.current.libraryPath !== expectedLibraryPath) return null;
      return propsRef.current.runWithSavedEditor(run, resume);
    });
  };

  const resume = (action: LibraryRestoreAction) => {
    const expectedLibraryPath = effectivePath;
    void coordinator.resumePending(action, async (run, shouldSkipSave) => {
      if (propsRef.current.libraryPath !== expectedLibraryPath) return null;
      return propsRef.current.runWithSavedEditor(run, shouldSkipSave);
    });
  };

  const cancel = () => { coordinator.cancel(); };

  return (
    <section className="library-restore-panel" aria-label="整库恢复">
      <div className="library-restore-heading">
        <div>
          <p className="library-restore-eyebrow">整库恢复</p>
          <h2>{matchingPending ? '需要处理未完成的笔记库恢复'
            : state.receipt && !receiptDismissed ? '笔记库操作已完成' : '从备份替换整个笔记库'}</h2>
        </div>
        {matchingPending ? <span className="library-restore-state-tag">恢复状态待处理</span> : null}
      </div>

      <p className="library-restore-location">
        当前笔记库位置：<code>{effectivePath}</code>
      </p>

      {matchingPending ? (
        <div className="library-restore-pending" aria-label="未完成的整库恢复" aria-expanded="true">
          <p className="library-restore-pending-message">{matchingPending.message}</p>
          <p className="library-restore-progress-copy">
            {matchingPending.phase === 'rollingBack' ? '回退进度' : '恢复进度'}：
            {matchingPending.completedFiles}/{matchingPending.totalFiles} 个文件
          </p>
          <p className="library-restore-source-copy">来源备份：{matchingPending.sourceBackupId}</p>
          {matchingPending.preRestoreBackup ? (
            <p className="library-restore-protection-copy">
              恢复前完整手动备份：<code>{matchingPending.preRestoreBackup.path}</code>
            </p>
          ) : (
            <p className="library-restore-protection-copy">宿主尚未报告恢复前手动备份位置。</p>
          )}
          {status && <p className="library-restore-status" role="status" aria-live="polite">{status}</p>}
          {state.error && <p className="library-restore-error" role="alert">{state.error}</p>}
          <div className="library-restore-actions">
            {matchingPending.canContinue && (
              <button className="library-restore-button library-restore-primary" type="button"
                disabled={!allowed || busy} onClick={() => resume('continue')}>
                {matchingPending.phase === 'rollingBack' ? '继续回退' : '继续恢复'}
              </button>
            )}
            {matchingPending.canRollBack && (
              <button className="library-restore-button library-restore-secondary" type="button"
                disabled={!allowed || busy} onClick={() => resume('rollBack')}>
                开始回退到恢复前版本
              </button>
            )}
            {(matchingPending.canContinue || matchingPending.canRollBack) && !allowed && (
              <p className="library-restore-disabled-reason">
                {disabledReason || '当前笔记库保护状态暂不允许继续操作。'}
              </p>
            )}
          </div>
          {!matchingPending.canContinue && !matchingPending.canRollBack && (
            <p className="library-restore-disabled-reason">
              {disabledReason || '宿主当前没有可安全执行的继续或回退动作；请保留此状态并检查上述保护位置。'}
            </p>
          )}
        </div>
      ) : (
        <>
          {state.error && <p className="library-restore-error" role="alert">{state.error}</p>}
          {status && <p className="library-restore-status" role="status" aria-live="polite">{status}</p>}
          {state.notice && !status && <p className="library-restore-status" role="status">{state.notice}</p>}

          {selectedPreview && (
            <div className="library-restore-preview" aria-label="已预校验的来源备份">
              <strong>{dateLabel(selectedPreview.backup.createdAt)} · {selectedPreview.backup.kind === 'manual' ? '手动备份' : '自动备份'}</strong>
              <span>{selectedPreview.backup.noteCount} 篇笔记 · {selectedPreview.backup.notebookCount} 个笔记本</span>
              <span>已核验笔记、笔记本和附件的完整性。</span>
            </div>
          )}

          {backupId && !selectedPreview && !state.error && !state.receipt && !busy && (
            <p className="library-restore-status" role="status">正在读取备份清单；预校验完成前不能开始恢复。</p>
          )}
          {backupId && state.error && !busy && (
            <div className="library-restore-actions">
              <button className="library-restore-button library-restore-secondary" type="button"
                disabled={!allowed} onClick={() => {
                  previewAttemptRef.current = null;
                  void coordinator.previewBackup(backupId);
                }}>
                重新预校验备份
              </button>
              <button className="library-restore-button library-restore-secondary" type="button"
                onClick={() => propsRef.current.onDismiss()}>
                返回备份列表
              </button>
            </div>
          )}

          {!backupId && !state.receipt && (
            <p className="library-restore-disabled-reason">
              没有选中的有效备份记录。请从上方经过校验的备份列表选择来源。
            </p>
          )}
          {!allowed && backupId && (
            <p className="library-restore-disabled-reason">{disabledReason || '当前笔记库保护状态暂不允许整库恢复。'}</p>
          )}

          {selectedPreview && !state.receipt && (
            <form className="library-restore-confirmation" onSubmit={event => {
              event.preventDefault();
              if (canConfirm) startRestore();
            }}>
              <strong>确认替换整个笔记库？</strong>
              <p>
                确认后，当前库的笔记与笔记本文件会替换为该备份中的完整集合。源备份没有的当前原生文件（包括库内重复文件）会从活动笔记库移走；
                操作前会创建并保留当前库的完整手动备份。笔记库中的无关文件和库外本机恢复区不在替换范围内。
              </p>
              <div className="library-restore-actions">
                <button className="library-restore-button library-restore-secondary" type="button" disabled={busy}
                  onClick={() => propsRef.current.onDismiss()}>
                  取消，不写入
                </button>
                <button className="library-restore-button library-restore-danger" type="submit" disabled={!canConfirm}>
                  {busy ? '正在准备恢复…' : '确认替换整个笔记库'}
                </button>
              </div>
            </form>
          )}
        </>
      )}

      {(state.operation === 'restoring' || state.operation === 'resuming' || state.operation === 'cancelling') && (
        <div className="library-restore-active-actions">
          {state.operation === 'cancelling' ? (
            <button className="library-restore-button library-restore-secondary" type="button" disabled>
              正在请求安全暂停…
            </button>
          ) : (
            <button className="library-restore-button library-restore-secondary" type="button" onClick={cancel}>
              请求暂停恢复
            </button>
          )}
          <small>宿主会在安全边界暂停；这一步不会自动回滚已完成的文件。</small>
        </div>
      )}

      {state.receipt && !receiptDismissed && (
        <div className="library-restore-receipt" role="status" aria-live="polite">
          <strong>{state.receipt.message || '整个笔记库已恢复。'}</strong>
          <span>恢复前完整手动备份已保留：<code>{state.receipt.preRestoreBackup.path}</code></span>
          <button className="library-restore-button library-restore-secondary" type="button" onClick={() => {
            setReceiptDismissed(true);
            propsRef.current.onDismiss();
          }}>完成</button>
        </div>
      )}
    </section>
  );
}
