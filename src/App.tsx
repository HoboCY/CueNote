import { useEffect, useState } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';

const desktopRuntime = isTauri();

const sidebarItems = [
  { icon: '⌂', label: '全部笔记' },
  { icon: '▤', label: '笔记本' },
  { icon: '⌕', label: '搜索' },
  { icon: '↶', label: '回收站' },
];

function App() {
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [ipcError, setIpcError] = useState<string | null>(null);
  const runtimeStatus = !desktopRuntime
    ? '浏览器预览'
    : ipcError
      ? '桌面连接失败'
      : appVersion
        ? '桌面已连接'
        : '桌面连接中';

  useEffect(() => {
    if (!desktopRuntime) return;

    let active = true;

    void invoke<string>('get_app_version')
      .then((version) => {
        if (active) setAppVersion(version);
      })
      .catch((error: unknown) => {
        if (active) setIpcError(error instanceof Error ? error.message : String(error));
      });

    return () => {
      active = false;
    };
  }, []);

  return (
    <div className={`app-shell${sidebarCollapsed ? ' sidebar-collapsed' : ''}`}>
      <aside className="sidebar" aria-label="CueNote 工作区">
        <div className="brand-row">
          <div className="brand-mark" aria-hidden="true">C</div>
          {!sidebarCollapsed && (
            <div className="brand-copy">
              <strong>CueNote</strong>
              <span>把想法写清楚</span>
            </div>
          )}
          <button
            className="sidebar-toggle"
            type="button"
            aria-label={sidebarCollapsed ? '展开侧栏' : '收起侧栏'}
            aria-expanded={!sidebarCollapsed}
            title={sidebarCollapsed ? '展开侧栏' : '收起侧栏'}
            onClick={() => setSidebarCollapsed((collapsed) => !collapsed)}
          >
            <span aria-hidden="true">{sidebarCollapsed ? '›' : '‹'}</span>
          </button>
        </div>

        {!sidebarCollapsed && (
          <>
            <div className="sidebar-kicker">
              <span className="status-dot" aria-hidden="true" />
              <span>首版工程预览</span>
            </div>

            <p className="sidebar-group-title">工作区</p>
            <ul className="sidebar-list">
              {sidebarItems.map((item, index) => (
                <li className={index === 0 ? 'sidebar-item is-current' : 'sidebar-item'} key={item.label}>
                  <span className="sidebar-item-icon" aria-hidden="true">{item.icon}</span>
                  <span>{item.label}</span>
                  {index === 0 && <span className="sidebar-item-mark" aria-hidden="true" />}
                </li>
              ))}
            </ul>

            <div className="sidebar-note">
              <span className="sidebar-note-icon" aria-hidden="true">✳</span>
              <div>
                <strong>这里还没有笔记</strong>
                <span>笔记库能力正在开发中</span>
              </div>
            </div>

            <div className="sidebar-footer">
              <span className="sidebar-footer-rule" />
              <span>本地优先 · 专注记录</span>
            </div>
          </>
        )}
      </aside>

      <main className="workspace">
        <header className="workspace-header">
          <div className="header-leading">
            {sidebarCollapsed && <span className="compact-mark" aria-hidden="true">C</span>}
            <div className="breadcrumb">
              <span>工作台</span>
              <span className="breadcrumb-divider" aria-hidden="true">/</span>
              <strong>开发预览</strong>
            </div>
          </div>
          <div className="header-actions">
            <span className={`runtime-badge${desktopRuntime ? ' is-desktop' : ' is-browser'}`}>
              <span className="status-dot" aria-hidden="true" />
              {runtimeStatus}
            </span>
            {appVersion && <span className="version-badge">v{appVersion}</span>}
          </div>
        </header>

        <section className="preview-heading" aria-labelledby="page-title">
          <div>
            <div className="eyebrow"><span>CueNote</span><span className="eyebrow-line" />开发预览</div>
            <h1 id="page-title">让每个想法，都有清晰的落点。</h1>
            <p>康奈尔笔记工作台的基础框架已就位，笔记编辑与本地保存正在建设。</p>
          </div>
          <div className="preview-seal" aria-label="功能尚未完成">
            <span>FIRST</span>
            <strong>预览</strong>
            <span>BUILD</span>
          </div>
        </section>

        <section className="runtime-panel" aria-live="polite">
          <span className={`runtime-indicator${ipcError ? ' has-error' : appVersion ? ' is-ready' : ''}`} aria-hidden="true" />
          <div>
            <strong>
              {runtimeStatus}
            </strong>
            <p>
              {ipcError
                ? '无法连接桌面程序，请从桌面应用重新打开。'
                : appVersion
                  ? `当前版本 v${appVersion}。笔记编辑与本地保存仍在建设中。`
                  : desktopRuntime
                    ? '正在连接桌面程序。'
                    : '这是界面预览，不会读取或保存笔记。'}
            </p>
          </div>
          <span className="runtime-readonly">开发预览</span>
        </section>

        <article className="note-paper" aria-label="康奈尔笔记页面布局预览">
          <div className="paper-topline">
            <span>笔记页面布局</span>
            <span className="paper-topline-right"><span className="paper-dot" />尚未接入笔记数据</span>
          </div>

          <section className="title-preview" aria-labelledby="note-title-label">
            <div className="title-copy">
              <span className="section-number">00</span>
              <div>
                <h2 id="note-title-label">笔记标题</h2>
              </div>
            </div>
            <span className="coming-soon">标题与保存待接入</span>
          </section>

          <div className="cornell-grid">
            <section className="cornell-panel cue-panel" aria-labelledby="cue-title">
              <div className="section-heading">
                <span className="section-number">01</span>
                <div>
                  <h3 id="cue-title">线索区</h3>
                </div>
              </div>
              <div className="panel-placeholder">
                <span className="placeholder-mark" aria-hidden="true">✳</span>
                <p>问题、关键词与回顾提示<br />将在这里整理</p>
              </div>
              <span className="panel-note">用于回想与复习</span>
            </section>

            <section className="cornell-panel content-panel" aria-labelledby="content-title">
              <div className="section-heading">
                <span className="section-number">02</span>
                <div>
                  <h3 id="content-title">内容区</h3>
                </div>
                <span className="content-hint">主要记录区域</span>
              </div>
              <div className="content-placeholder">
                <div className="content-placeholder-mark" aria-hidden="true">
                  <span />
                  <span />
                  <span />
                </div>
                <strong>文字、公式、代码、图片与可编辑思维导图</strong>
                <p>将作为内容块排列在这里</p>
              </div>
              <div className="content-lines" aria-hidden="true"><span /><span /><span /><span /></div>
            </section>
          </div>

          <section className="summary-panel" aria-labelledby="summary-title">
            <div className="section-heading">
              <span className="section-number">03</span>
              <div>
                <h3 id="summary-title">总结区</h3>
              </div>
              <span className="content-hint">收束本篇要点</span>
            </div>
            <div className="summary-placeholder">
              <span className="summary-dash" aria-hidden="true" />
              <span>复述重点，留下自己的结论</span>
              <span className="summary-state">等待实现</span>
            </div>
          </section>
        </article>

        <footer className="page-footer">
          <span>CueNote · 工程基础版</span>
          <span>本地界面预览 · 不会保存笔记</span>
        </footer>
      </main>
    </div>
  );
}

export default App;
