# 思维导图本机验证（#22）

基线：277ecf30d89042111ea0b48e2529ddfc67c13312。依据 [#22](https://github.com/HoboCY/CueNote/issues/22) 和[思维导图实现契约](mindmap-editing-contract.md)。

证据根目录：C:/Users/Hobo/.codex/visualizations/2026/10/10/01a12393-3b57-76e3-afff-50bd8ef5f539/implementation-execution/issue-22。

## 自动检查

| 检查 | 结果 |
| --- | --- |
| 前端全套测试 | 83 项通过；包含 5 项导图模型与 17 项编辑器/树转换、焦点、初始挂载及组合输入调度测试 |
| Rust 默认测试 | 94 项通过；包含 6 项导图 Workspace 测试 |
| Rust fmt、check、schema 同步检查 | 通过 |
| TypeScript、Vite 与 Tauri debug 构建 | 通过；最终桌面构建为 final10 |

模型检查覆盖原始空白、空字符串、换行、Unicode、有序边和稳定 UUID；非法结构、未知字段、嵌套及侧栏导图明确拒绝。全篇所有导图共享 5,000 节点预算和身份空间，模型样本覆盖 500 节点树及 5,000 节点深树；这些转换结果不表示对应图形界面的性能已经验收。

公开 Workspace 使用真实隔离文件验证混合图片与导图的保存、重开、块顺序、删除后恢复，以及独立副本和冲突恢复草稿。4,999 节点预检、跨两图恰好 5,000 节点实际保存重开通过；5,001 节点拒绝后会话与原文件保持不变。新增内容预检复用实际保存的完整候选包校验，不另设较低的导图文件限制。只读、过期会话及不支持的嵌套内容均被拒绝。

完整 Rust 默认测试在允许创建隔离 junction 样本的执行环境中通过。保存发布逻辑未改变，本项没有重复运行上一任务已通过的独立进程中断矩阵。

## Windows 功能验证

仅使用 desktop-check/roots/mindmap-root-001 下的隔离设置、笔记库和恢复目录；不操作用户笔记或剪贴板。本项 Windows 功能验收通过，以下按实际执行版本记录结果，未受后续修复影响的既有通过结果继续复用。

final5 冷启动实际发现白屏：已有导图的 NodeView 构造期间访问尚未挂载的 `editor.view.dom`。修复改用 NodeView 参数提供的 `view`；新增挂载前回归先复现同一异常，再验证预览构造成功且原始数据未改变。final5 保留为失败证据，不能计作保存重开通过。final6 从同一笔记库成功冷启动并继续编辑已有四节点导图；可执行文件 SHA-256 为 `70D7B0828C5DCA779BC4AEFC7DD9B1054DC7ECF50E314A3CA2FAE3CE9B8ADCFD`。

final6 通过收起后展开按钮焦点、一次正文撤销/重做恢复精确导图 JSON，以及连续三轮展开编辑与正文历史往返。实际文件也验证了上移、下移和块删除后的顺序。删除后首次 Ctrl+Z 暴露按钮移除后焦点落到 BODY 的问题；显式点击正文再撤销可恢复原图，证明历史与内容仍在，但不算直接撤销通过。修复在删除事务完成后将焦点交回 EditorView，新增回归先失败再通过；两轴增量源码复核通过。final7 仅构建，实际复验使用包含同一修复的 final8。

final8 从原隔离笔记库冷启动，四节点导图与此前落盘结构全等，保留 Before / mindMap / After 顺序；真实键鼠把原叶子改为 `Beta shortcut cycle 3 | FINAL8 reopen` 后，仍保存到原笔记、原 mapId 和节点。随后从折叠预览删除导图，未再次点击正文便立即按 Ctrl+Z，焦点已在正文 EditorView，导图及前后文字完整恢复，操作前后原生包 SHA-256 同为 `1319db75b62301171a1db73e7f950465a2ff86617d858f5510cce11f22e444a5`。证据分别位于 desktop-check/runs/mindmap-root-001/run-final8-cold-20261015-r 和 run-final8-delete-undo-20261015-s。删除脚本最初试图点击展开态下禁用的删除按钮，其超时属于测试脚本状态错误；没有计作产品结果。

500 节点夹具由离线工具生成，来源不计作产品创建。真实桌面展开得到 500 个 Mind Elixir 节点；键鼠修改可见根节点后自动保存，原生树、节点标识和块顺序完整保留。实际 document.json 除应更新且递增的 contentModifiedAt 外与预期全等，见 desktop-check/runs/mindmap-root-001/run-final6-pressure-root-click-20261015-f/pressure-root-edit-verification-v2.json。final10 再从原生文件重开，500 个连通 UUIDv4 节点与前次保存全等；真实键鼠续写根节点为 `500-node pressure map continued after FINAL10 reopen` 后，完整导图与预期全等，仍为 500 节点。续写后包 SHA-256 为 `0fe7b4da23701fe628007a1fdbcfbb4384d3ee0bfaa4bfd316293176596c6aea`；跨进程及续写证据位于 desktop-check/runs/mindmap-root-001/run-final10-pressure-reopen-20261015-y。

5,000 节点边界通过真实桌面操作：离线夹具的两图各有 2,500 节点，展开第二图并点击“添加子节点”后明确提示整篇超过 5,000 节点。图中仍有 2,500 个可编辑节点，两图预览共 5,000 节点；前后 document.json 全等且原生包 SHA-256 均为 `89167f57fb4bacf992efdbc77e1825695229ad7cccaf2b0a8f482c547b4e2e98`。证据为 root-runtime/capacity-result.json 及前后完整包解读。桌面没有追加可选的“删除一节点后再添加”场景；4,999 到 5,000 的成功路径由前述 Workspace 测试覆盖。

final6 的尾段场景使用 UI 新建笔记：在正文末尾插入导图后保留可输入空段落，下移导图到末尾后仍保留尾段，真实键鼠在该段输入 `After map moved to end` 后实际文件正确保存。证据位于 desktop-check/runs/mindmap-root-001/run-final6-tail-20261015-m/tail-ui-events.jsonl 及三份原生包解读。

组合输入的 focused autosave 在 final6 通过：输入框保持焦点时，原生根节点已保存 `CDP-IME-autosave-漢字-中文-复测2`，树形与前后正文保留。CDP 记录中 compositionstart/update 和 input 为 isTrusted=true，但 compositionend 为 false；旧被动观察器还使事件记录重复。本结果仅表示该 WebView/CDP 事件序列下的保存行为，不称为全可信组合事件或人工 Windows 输入法验收。证据见 desktop-check/runs/mindmap-root-001/run-final6-ime-resume-20261015-p/ime-events.jsonl。

组合输入期间首次收起另发现真实点击问题：mousedown 命中收起按钮后，焦点转移伴随按钮 y 坐标从 414.93 移至 284.93；原坐标 mouseup 落到画布，click 没有到达按钮，虽然最终文字已正确保存。final8 尝试延后 blur 收尾，实测仍失败；后续只读证据确认文字编辑区和 textarea 都没有隐藏，因而撤回“隐藏文字区导致位置变化”的推断。capture 阶段的 blur 观察器已看到位置变化，未采集的 relatedTarget 不能事后推断。该失败证据保留在 desktop-check/runs/mindmap-root-001/run-final8-ime-s9-20261015-u。

final10 将三个自行完成文字收尾的按钮（收起、导图撤销/重做）的左键 mousedown 默认焦点转移延后到 click；点击成立后显式 focus({ preventScroll: true })，触发原生 blur/组合收尾，再执行既有 settle 或导图历史操作。显式聚焦仅在同步调用期间跳过重复的 blur 收尾，避免同一点击并发结束文字编辑；标记用 finally 清理，普通 Tab 和其他失焦保持原逻辑。前端 83 项、类型检查与桌面构建通过。final9 只构建和源码审查，最终构建 final10 SHA-256 为 `DEF29F2BAC617CBD667FE78B96379C5CB72465EDA6055885BBC6FB5290540540`。

final10 真实首击收起通过：mousedown、mouseup、click 均命中收起按钮，按下与松开时按钮 y 均为 414.927、window.scrollY 均为 678.667，之后才出现焦点转移并成功收起。原生包保存 `CDP-IME-collapse-首击-FINAL10-中`，SHA-256 为 `0749b536258a4ca5189fe073964af197bb8403ae26b357f44992c19721d96e00`。同次运行还验证组合输入期间切换到此前 UI 创建的笔记，返回后原篇保存 `CDP-IME-switch-FINAL10-保留-中文`，原 mapId、树形与前后正文顺序保留。证据位于 desktop-check/runs/mindmap-root-001/run-final10-cold-20261015-w 的 ime-events.jsonl、s9-collapse-interaction-events.json 和实际原生包解读。这里的 compositionend 仍为 isTrusted=false，组合事件存在旧观察器的重复记录；与 S8 一样，只计 CDP 事件序列验证，不替代人工输入法验收。

验证期间曾因沙箱内 Get-Process 空结果误判窗口退出。提升后的只读查询确认原 PID 45328、启动器与 WebView 端口始终存在；误开的测试副本已按路径及 SHA 正常关闭。该情况属于工具查询边界，不是应用退出或崩溃证据。

final10 的普通 Tab 失焦也通过：真实 Tab keydown/up 触发 trusted blur，文字编辑区正常结束，UI 显示已保存，原生包除递增的 contentModifiedAt 外与预期全等。样本文字沿用脚本的 `CDP-IME-tab-blur-FINAL8-中文` 标签，实际运行版本由 launcher 和可执行文件 SHA 确认为 final10；文件 SHA-256 为 `b086f515b218a9398d5f0eb644193238d69fc02948b8c26599319fe90b905ad5`。证据位于 desktop-check/runs/mindmap-root-001/run-final10-tab-blur-20261015-x；组合事件可信度边界同前述 CDP 场景。

## 独立评审

Standards 与 Spec 首轮均确认组合输入在关闭或切换时可能被回退；Spec 另发现末尾插入或移动导图后没有后续输入段落，Standards 另发现 Editor 与面板监听器累积。修复加入组合结束的最终输入屏障、同事务尾段落，以及每个 Editor/NodeView 一次的监听和清理。两轴已定向核对组合保护接入实际 NodeView 与 App 的离开等待路径；组合测试本身使用受控事件调度，不冒充真实输入法实测。

桌面实测另发现收起后的展开按钮焦点会拦截正文 Ctrl+Z，已增加折叠态正文历史路由和回归。连续两轮组合输入现在分别等待其预检完成，等待旧轮次时开始的新输入不会被提前释放；预检结束但未失焦的最终文字经稳定回调发布到整篇草稿。回调同时检查当前 NodeView、输入错误和接受版本，避免过期任务发布。

### Standards

最终源码复核通过，鼠标路径的 P2 已关闭。final9 审查提出同步聚焦和操作入口可能同时结束文字编辑，final10 用同步聚焦保护解决；try/finally 确保没有遗留标记。该保护没有将所有文字收尾入口改成全局串行；快速 Tab 后立即 Enter/Space 的既有多入口尚未专项验证，审查未给出可复现的内容丢失、历史错误或死锁证据。完整记录见 checks/review-final10.md。

### Spec

最终源码复核通过，无剩余确认的规格或 IME 阻断。最终实现移除短暂鼠标意图标记；鼠标按下后拖出取消 click 时，输入框保留正常编辑焦点。保存、切换、关闭和导图历史沿用原有稳定输入等待路径。final8 审查回复提前称“桌面 RED→GREEN”的部分不作为实测证据；本文件的 final10 通过结论独立取自实际日志。

## 验证边界

没有 Mac、真实 OneDrive 或正式压力性能验收结论；本项 500 节点功能场景、#31 的完整样本性能与 #33 的双端验收分别记录。5,000 节点深链只验证模型转换和原生读写，没有验证该拓扑的图形展开；不声称已验收所有 5,000 节点树形，也未为此降低原生文件预算。

构建仍出现既有 ts-rs 属性解析、增量缓存硬链接回退和 Vite 主包大小提示。新增直接依赖 Mind Elixir 5.15.1（MIT），版本锁定且许可原文已保留。
