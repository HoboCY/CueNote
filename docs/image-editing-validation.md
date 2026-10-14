# 图片编辑本机验证（#21）

基线：08f9c2fa86db033b29eed4cbbae6804b9456cb61。依据 [#21](https://github.com/HoboCY/CueNote/issues/21) 和[图片实现契约](image-editing-contract.md)。本记录区分自动测试、真实来源复制、程序设置系统剪贴板和合成 DOM 事件；Mac 与真实 OneDrive 双端验收归 #33。

证据根目录：C:/Users/Hobo/.codex/visualizations/2026/10/10/01a12393-3b57-76e3-afff-50bd8ef5f539/implementation-execution/issue-21。

## 自动检查

| 检查 | 结果 |
| --- | --- |
| 前端全套测试 | 61 项通过，包含 21 项图片编辑器测试 |
| Rust 默认测试 | 88 项通过，包含 17 项图片 Workspace 测试 |
| 独立保存中断矩阵 | 3 项通过 |
| Rust fmt、check、schema 同步检查 | 通过 |
| TypeScript、Vite 与 Tauri debug 构建 | 通过，最终候选 bin-final8 |

Rust 全套运行的回收站 junction 样本初次被沙箱禁止创建。随后在允许该操作的执行环境重跑回收站 10 项和剩余 Workspace 15 项，均通过；没有改变应用或跳过样本。另构建 storage_fault_probe，启用 storage-fault-injection 运行保存中断矩阵；生产构建不启用注入。

公开 Workspace 与真实隔离文件验证 PNG/JPEG 原始字节、尺寸、SHA-256、首次引用顺序和去重，保存重开、另存与冲突恢复副本保持完整资产。删除并保存后仍能撤销恢复；重开后删除的图片也会先暂存再发布保存。会话切换隔离图片身份，活动实例暂存不被另一实例清理，显式关闭释放本次暂存并保留上次笔记定位。

图片专项覆盖单图及全篇像素边界、损坏或不支持格式、整包超限拒绝且不改变当前草稿。既有格式测试覆盖 256 MiB 包、512 MiB 实际展开量、文档和清单大小、JSON 深度、资产及 ZIP 条目数的边界，包含伪造展开大小。插入预检使用与实际保存相同的 ZIP 写入逻辑。

前端通过真实 Editor/ProseMirror 事务验证段落后插入并保留选择文字、等待期间打字与重新预检、输入法组合结束、目标段落被删除、会话离开和销毁后的迟到结果、格式拒绝、只读保护、缩放/移动/删除独立撤销。模型转换拒绝无效身份、宽度、额外属性以及侧栏或嵌套图片。files/items 返回不同 File 包装时只处理一次。

## Windows 功能验证

流程只使用 desktop-check/roots/image-root-001 隔离库。界面由 CDP 鼠标/键盘操作，真实复制粘贴使用 OS 输入；宿主只读查询、DOM 观察和 ZIP 检查用于取证，没有直接写宿主或编辑器状态代替界面操作。下面的运行目录均相对 desktop-check/runs/image-root-001。

| 场景 | 结果与证据 |
| --- | --- |
| Paint 真实复制粘贴 | manage-final5/paint-paste-retry-record.json：Paint OS Ctrl+A/C → CueNote OS Ctrl+V，事件 isTrusted=true，收到 PNG 9,029 字节，SHA-256 与包内资产一致。原图 500×300；所选文字保留，图片位于段落后，前后文字完整；Paint 随后正常关闭。 |
| 纯系统位图 | manage-final7/bitmap-paste-record.json：辅助程序只将受控样本的 DeviceIndependentBitmap 放入真实剪贴板，实际格式列表只有该项；OS Ctrl+V 收到 PNG 13,439 字节，长度/哈希与包内资产一致。这是程序设置系统剪贴板，不能冒充其他应用复制。 |
| JPEG 与格式拒绝 | manage-final6/extras-failure.json 前两条完成记录：合成 DOM 剪贴板事件传入 JPEG 25,315 字节，保存长度/哈希及 MIME 完全一致，后续文字保留；WebP 明确拒绝，已保存文件哈希不变。整个驱动随后在回收站预览处失败，因此不把整份运行标为通过。 |
| 宽度、拖动、上下移动、删除与撤销 | controls-final6-retry2/image-controls-result.json：18 条记录通过。实际鼠标聚焦宽度输入改为 320，拖动滑块，Ctrl+Z 恢复；移动及撤销后核对原生文档顺序。删除保存后资产和清单归零，撤销再保存恢复相同 ID/原始 SHA；显示保持比例，原始尺寸不变。 |
| 来源关闭后的重开与本地请求 | manage-final6/reopen-result.json：关闭进程再启动，原图、显示尺寸、资产字节保持一致；新的 Blob URL 来自包内资产。启用网络观察后重载，记录仅含 tauri.localhost、ipc.localhost 和 Blob URL，没有外部请求。不等同物理断网测试。 |
| 清空正文与初始滑块 | manage-final7/paste-prepared.png：全选清空并重写正文后没有图片错误条；manage-final6/reopen-result.json 中数字输入及滑块均为 500。 |
| 回收站只读显示与恢复 | manage-final8/trashfull-result.json：图片可见、编辑区为 false、控件隐藏；恢复后编辑区为 true、控件重新显示，原始资产哈希和全部内容保持不变。 |
| 最终构建及正常关闭重开 | manage-final8-reopen/reopen-result.json：跨进程打开后显示宽度 320、原图 500×300、资产 ID/字节一致；控件可用，没有外部网络请求。两次最终进程均正常关闭，退出码 0。 |

Paint 收到的资产 SHA-256：a75d2f9b6fd0f86ec42919aa9b1a8b10847bc182576f14e91a81291c0e135c91。位图转换后的资产 SHA-256：0a4ab4419d14e30126f1b7309cb620a54126e87803c722a0826285ea6a68064e。JPEG SHA-256：8fc6af7113be3822552876ad036ebfa924d6e4cfc0a05a1af774bbdce81d48f5。比较的是 WebView 实际收到的字节，不能把 Paint 打开前的源文件字节当作剪贴板字节。

最终可执行文件为 desktop-check/bin-final8/cuenote.exe，SHA-256：9831FD8573F6082D5E416542926E8DC8EDD67308819E2DBF09FEA2324BFBEDBD。该目录 source-hashes.json 记录相对基线变化的 23 个源码/配置/测试输入；最终签入前再次核对一致。早期候选的场景仅在相关路径未改变或已定向复验时复用。

## 实测发现与修复

- 实际粘贴曾返回 stage_image_raw not allowed：补齐主窗口三个图片命令权限；复验实际暂存、预检、保存和读取成功。
- 宽度输入与滑块原先取消 mousedown 默认行为：删除该拦截，保留 NodeView 事件隔离；真实输入和拖动通过。
- 初始 500 被 range 默认上限 100 截断：先写 max 再写 value；重开实际 DOM 值为 500。
- 图片节点高 schema 优先级使空文档填入属性为空的 image：移除节点高优先级，保留粘贴插件优先级；空文档、全选删除和空段落回车回归通过。
- 相邻段落变化后移动按钮不刷新：刷新使用位置和文档边界比较，避免每张图片遍历全文；首末边界、失效位置及清理回归通过。
- 回收站统一只读原因触发早期纯文字投影：改为禁止编辑的富文本视图，不可转换内容仍回退只读投影。进一步发现恢复后图片控件没有随编辑权限启用：NodeView 注册权限刷新回调，RichTextEditor 在 setEditable 后刷新并在销毁时注销；无额外正文事务。最终桌面回收站恢复与重开均通过。

控件驱动第一次没有等待异步解码，第二次把空段落的 DOM 换行当成正文；修正后第三次完整通过，旧失败记录保留。位图粘贴和文件校验通过，但清理时 Windows 拒绝恢复旧剪贴板对象，驱动退出码为 1，记录 restored=false，之后停止写剪贴板。Paint 流程也因序号变化未覆盖当前剪贴板，不推断变化来自用户还是系统延迟呈现。

更早的 clipboard-probe/run-03 仅在 #20 基线程序中观察剪贴板类型，并阻止编辑器处理，不计作 #21 功能验收；Snipping Tool 来源没有完成验证。

## 独立评审

Standards：没有确认的文档规范违规或 Fowler 气味。三个图片命令只授权主窗口，没有新增任意路径、通用文件系统、shell 或通配权限。

Spec：字符串 image/* 剪贴板可能被静默忽略的问题已修复；普通文本仍走原路径，不读取或执行 SVG。schema 默认填充、滑块设置顺序、移动按钮刷新及回收站渲染入口均完成定向复核。最后的权限刷新、销毁清理及不改变文档对象也获两轴增量复核，无剩余确认问题。

## 验证边界

没有 Mac、真实 OneDrive、物理断网或压力性能验收结论；容量和峰值资源仍由 #31、双端体验由 #33 验证。本轮真实应用来源覆盖 Paint；JPEG 采用合成事件及公开原生接口，纯位图采用辅助程序设置真实系统剪贴板。

构建仍有既有 ts-rs 对 deny_unknown_fields 的提示、增量缓存硬链接回退及 Vite 主包大小提示；schema 同步检查通过。没有新增 Cargo 或 npm 依赖。
