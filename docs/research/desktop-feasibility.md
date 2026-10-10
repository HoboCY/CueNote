# CueNote 桌面宿主可行性：本地文件、剪贴板与分发

- 研究问题：[跨平台桌面方案对 CueNote 的本地文件、剪贴板和打包有哪些约束？（Issue #5）](https://github.com/HoboCY/CueNote/issues/5)
- 父地图：[CueNote 首版产品规格与技术决策地图（Issue #1）](https://github.com/HoboCY/CueNote/issues/1)
- 核验日期：2026-10-10（以该日可见的官方文档、源码和发布页为准）
- 证据边界：文档与源码研究，未在真实 Windows/macOS 机器运行；未安装依赖、建应用骨架或执行生产构建。

## 结论

Electron 与 Tauri 2 都有实现离线笔记、图片粘贴、文件导入导出的宿主能力；它们都不会替 CueNote 定义笔记/附件格式、崩溃恢复、原子保存或迁移完整性。

Electron 自带 Chromium 与 Node.js，网页编辑器可运行在同一 Chromium 家族的渲染环境；代价是应用随框架打包这些运行组件。Tauri 2 使用 Windows WebView2、macOS 原生 WebKit WebView，权限通过 capabilities、插件和路径作用域配置；这使宿主权限更可约束，也让 WebView 行为与系统版本相关。[Electron 架构](https://www.electronjs.org/docs/latest/tutorial/tutorial-prerequisites)；[Tauri 安全模型](https://v2.tauri.app/security/)；[Wry 平台说明](https://github.com/tauri-apps/wry)

候选优先级可按团队约束决定：若先求网页编辑器行为较一致、主要使用 JS/TS，先验证 Electron；若团队可维护 Rust、优先采用系统 WebView 并细化 WebView 文件权限，先验证 Tauri 2。现有证据不足以替用户冻结选型或判断总成本。

## 核验版本与许可范围

本报告的版本核对范围为 Electron v44.5.1（2026-09-30 发布）与 Tauri v2.12.2（2026-10-09 发布）；这只标明核验时参照的发行版，不表示在 CueNote 上实测通过。文中的 `latest` 文档链接是当天读取的滚动文档，实施前需锁定精确版本并复核 API/系统要求。[Electron v44.5.1 release](https://github.com/electron/electron/releases/tag/v44.5.1)；[Tauri v2.12.2 release](https://github.com/tauri-apps/tauri/releases/tag/tauri-v2.12.2)

Electron 仓库采用 MIT；Tauri 2.12.2 工作区声明 `Apache-2.0 OR MIT`。Clipboard Manager 等插件有各自的发布版本和依赖，应对最终锁文件生成组件/许可证清单；CueNote 根目录的 Apache-2.0 不会自动覆盖依赖的条款。本段是仓库元数据事实，不是法律保证。[Electron LICENSE](https://github.com/electron/electron/blob/v44.5.1/LICENSE)；[Tauri v2.12.2 Cargo.toml](https://github.com/tauri-apps/tauri/blob/tauri-v2.12.2/Cargo.toml)；[插件 Cargo.toml](https://github.com/tauri-apps/plugins-workspace/blob/v2/plugins/clipboard-manager/Cargo.toml)

## CueNote 产品边界映射

- 本地私有目录适合作为自动保存数据库/附件仓库，但不能替代用户可选择的整篇导出位置；两种位置应有清楚的恢复和备份说明。
- “导出一篇笔记”要形成可迁移的文档包，不能把绝对路径或机器私有缓存路径当作附件引用。
- 自动同步在首版之后；本报告不把框架自带更新器、WebView 数据库或云存储误作笔记同步能力。
- 内容区编辑器是另一个调查边界；本报告只说明其宿主权限、输入法、拖放与剪贴板接口。

## 能力对照

| 项目 | Electron | Tauri 2 |
|---|---|---|
| 前端宿主 | Electron 内置 Chromium 与 Node.js 运行时，Node.js 运行主进程；开发机仍需 Node.js/npm 工具链，终端用户无需另装 Node.js。 | Rust 宿主调用系统 WebView；前端可使用 JS/TS，也可由 Rust 命令提供能力。 |
| 文件读写 | 主进程可用 Node `fs`；`app.getPath('userData')` 可定位应用私有数据目录。 | `plugin-fs` 提供文件 API；必须在 capabilities 配命令权限和路径 scope，也可把 I/O 收到 Rust 命令。 |
| 原生文件选择 | Electron `dialog` 可选文件/目录并返回路径；主进程再执行校验与读写。 | `plugin-dialog` 提供打开/保存/目录选择；选中的路径加入文件与 asset-protocol 运行时 scope。 |
| 图片剪贴板 | 主进程 `clipboard.read()` 可读取 MIME 数据（包括 `image/png` 等）；Electron 40 起不建议在 renderer 直接用 clipboard API。 | `plugin-clipboard-manager` 有 `readImage()`；Windows/macOS 标为完整支持，需启用 `clipboard-manager:allow-read-image` 权限。 |
| 文件拖入 | 前端 WebView 可处理 HTML 拖放；主进程仍应验证路径、类型和大小。 | 可用 `onDragDropEvent` 取得文件路径；Windows 上若要使用 HTML5 前端拖放，需关闭默认启用的 WebView 原生 `dragDropEnabled`。 |
| WebView 引擎 | Chromium 随框架发布；可减少 Windows/macOS 系统 WebView 版本差异。 | Windows 为 WebView2，macOS 为系统 WebKit/WKWebView；版本和行为由系统运行环境影响。 |
| 构建工具 | Node.js/npm；按目标平台打包并处理签名。 | Rust；Windows 还需 MSVC C++ Build Tools 与 WebView2，macOS 需 Xcode Command Line Tools 或 Xcode。 |


### 本地文件、目录与附件路径

Electron 的 `app.getPath('userData')` 为应用配置/用户数据提供系统路径；文档建议在其中使用应用自己的子目录，并避免把大文件不加区分地塞入该目录。主进程 Node 文件 API 在当前用户及操作系统允许范围内工作。将文件 I/O 留在主进程，renderer 只经窄接口发出“保存笔记/选附件”请求。[Electron app paths](https://www.electronjs.org/docs/latest/api/app)；[Electron dialog](https://www.electronjs.org/docs/latest/api/dialog)

Tauri 的 `BaseDirectory.AppData`、`AppLocalData` 等路径可用于应用自己的笔记库；`plugin-fs` 的 JS API 仍受 capability 权限和 scope 限制。仅开启 `fs:allow-read-*` 之类命令权限，不等于允许任意路径；缺少 scope 会返回 forbidden path。这样可以把编辑器的文件访问限制在 CueNote 数据目录和明确授权的用户选择范围。[Tauri filesystem plugin](https://v2.tauri.app/plugin/file-system/)

Tauri `plugin-dialog` 让用户选择文件/目录，并将所选路径暂时加入 filesystem 与 asset-protocol scope；scope 默认不会跨应用重启保留，若要持续引用用户原位置需设计持久授权流程。正文展示磁盘图片时，Tauri asset protocol 另有独立 allow/deny scope；未启用并授权的路径不能直接作为 WebView 图片源。导入后复制到 CueNote 私有附件目录可减少长期外部路径依赖。[dialog API 源码](https://raw.githubusercontent.com/tauri-apps/plugins-workspace/v2/plugins/dialog/guest-js/index.ts)；[asset protocol scope](https://v2.tauri.app/security/asset-protocol/)

宿主“路径可访问”和用户“文件可迁移”是两件事：沙盒 scope 只控制 WebView 可读写哪些路径，不会自动把附件复制进笔记包，也不会保存路径映射。对长期离线笔记，导入时将附件复制入 CueNote 私有库并保存相对引用是一项可评估的应用设计，而非框架默认语义。

macOS 若选择 Mac App Store，必须采用 App Sandbox，文件访问要用系统对话框/相应 entitlement 授权；直接分发也要按目标 API 和 OS 文件隐私行为实测，不能把 Windows 上的路径假设照搬过去。[Apple App Sandbox 文件访问](https://developer.apple.com/documentation/security/accessing-files-from-the-macos-app-sandbox)

### 剪贴板图片、拖入与网页编辑器

Electron 当前 clipboard API 在主进程，`read()` 返回 ClipboardItem；按 MIME 类型读取图片 Blob，再经 IPC 发送给前端或由前端发起单一的图片导入命令。它同时能处理 `text/plain`、`text/html`、`image/png` 等格式。使用 renderer 的标准 `paste` 事件也可让编辑器消费浏览器暴露的剪贴板项目，但各应用放入系统剪贴板的格式不同，不能只以“有 readImage API”推断所有来源均会得到同一图像字节。[Electron clipboard API](https://www.electronjs.org/docs/latest/api/clipboard)

Tauri Clipboard Manager 提供 `readImage(): Promise<Image>`，可取 RGBA 图像数据；当前官方 API 源码标明 Android/iOS 不支持该图片接口，插件支持表对 Windows/macOS 标为完整。图片粘贴仍需把读取结果变成 CueNote 附件并更新笔记内容；插件不负责资产命名、去重、持久化或导出。[Clipboard Manager JS 源码](https://raw.githubusercontent.com/tauri-apps/plugins-workspace/v2/plugins/clipboard-manager/guest-js/index.ts)；[插件支持表与权限](https://v2.tauri.app/plugin/clipboard/)

Tauri 的文件拖入事件包含 enter/over/drop/leave 和所拖路径。Windows 上默认启用的原生拖放与 HTML5 拖放有冲突边界：官方 API 指出要使用 HTML5 拖放需禁用 `dragDropEnabled`。若富文本编辑器依赖浏览器 `DataTransfer`，必须针对 Windows 实测拖图、拖文件和编辑器内部拖动。[Tauri Webview drag/drop API](https://v2.tauri.app/reference/javascript/api/namespacewebview/)

两者都能承载本地网页式编辑器。嵌入同一可信应用前端的编辑器代码属于该渲染上下文，可能使用该窗口获准的宿主能力；外部网页/不可信插件不应获得文件系统或任意 IPC。Electron 要保留 `contextIsolation`、renderer sandbox、关闭 `nodeIntegration`，只经 `contextBridge` 暴露按操作定义的函数，不暴露原始 `ipcRenderer`/Node。Tauri 默认仅打包代码可调用 API；远端 API capability 是显式配置，不要把主窗口的本地文件能力赋给远端 editor origin。[Electron security](https://www.electronjs.org/docs/latest/tutorial/security)；[context isolation](https://www.electronjs.org/docs/latest/tutorial/context-isolation)；[IPC](https://www.electronjs.org/docs/latest/tutorial/ipc)；[Tauri capabilities](https://v2.tauri.app/security/capabilities/)

### 中文输入与系统 WebView 差异

Electron 发行包固定 Electron/Chromium 版本，因此同一发行版在 Win/macOS 上不会依赖用户机器的 Edge/WebView 更新节奏；操作系统输入法、字体、快捷键和原生窗口仍不同。Tauri 在 Windows 使用 Microsoft Edge WebView2；Tauri 前置条件页注明 Windows 10 1803 起及后续 Windows 通常预装 WebView2。Microsoft 支持 Evergreen 自动更新，也提供 Fixed Version 运行时分发模式，选哪种会影响运行时更新责任。[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)；[Microsoft WebView2 distribution](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/evergreen-vs-fixed-version)

Tauri macOS 端由系统 WebKit WebView 提供渲染；Tauri 的安全文档明确不把 WebView 打进应用。推断：系统更新可能改变 WebKit 的行为版本，故应在支持的 macOS 版本矩阵上回归编辑器。[Tauri WebView security](https://v2.tauri.app/security/)；[Wry macOS/Windows 实现说明](https://github.com/tauri-apps/wry)

当前资料没有给出 CueNote 中文输入法的跨宿主兼容承诺。需验证拼音组合态、候选词确认、Enter/方向键、撤销、光标与选区、标题/三分区切换，以及导图节点文字编辑。富文本组件自己的 `compositionstart/update/end` 处理也属于编辑器能力，不由桌面框架保证。

### 本地保存与迁移语义

两套方案都可写应用私有目录，也都可接入 SQLite/其他本地存储；它们没有内建 CueNote 笔记数据库或迁移格式。Electron 由应用选 Node 文件 API/数据库模块；Tauri 可选 `plugin-fs`、Rust I/O 或额外数据库插件。数据库驱动、原生模块打包、升级迁移与许可证须按所选依赖另行核对。

不能仅凭框架文件 API 声称“原子保存”。Tauri fs 文档示例的写入会创建/覆盖文件，`rename` 只是可调用的操作；Node `fs.writeFile` 也只是写文件 API。CueNote 要自行设计临时文件/替换策略、刷新与恢复、旧版本备份、损坏检测、附件先后顺序与失败回滚，并在 NTFS 与 APFS 上实测崩溃/断电行为。[Tauri fs API](https://v2.tauri.app/plugin/file-system/)；[Node fs API](https://nodejs.org/api/fs.html)

首版整篇笔记导入/导出需要应用定义封装格式：笔记元数据、标题/线索区/内容区/总结区、附件相对路径、版本号和完整性校验。应用需处理不完整包、重复文件名、恶意路径穿越、超大/损坏附件、覆盖冲突和未来格式升级。系统文件对话框只选输入/输出位置，并不实现整篇迁移；后续自动同步也需独立解决冲突和一致性。

## 开发、打包、签名和分发

Electron 开发需要 Node.js/npm；官方教程建议使用当前 LTS。应用打包需将应用资源与 Electron runtime 组装并选择分发工具。终端用户无需安装 Node；Windows 与 macOS 发布包仍须分别验证。[Electron prerequisites](https://www.electronjs.org/docs/latest/tutorial/tutorial-prerequisites)；[distribution overview](https://www.electronjs.org/docs/latest/tutorial/distribution-overview)

Tauri 2 需要 Rust；Windows 开发另需 Microsoft C++ Build Tools 与 WebView2，macOS 桌面开发可使用 Xcode Command Line Tools（iOS 才要求完整 Xcode）。使用 JS/TS 前端时还需 Node 工具链。当前 Tauri v2.12.2 工作区声明 Rust 1.90；clipboard/dialog 插件文档也列出 Rust 1.90 最低版本，精确依赖升级前需再次核对。[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)；[Tauri v2.12.2 Cargo.toml](https://github.com/tauri-apps/tauri/blob/tauri-v2.12.2/Cargo.toml)；[clipboard plugin](https://v2.tauri.app/plugin/clipboard/)

macOS 包可由本地 Mac 或托管 macOS CI 构建；官方前置条件与签名流程要求 macOS/Xcode 工具环境，但这不意味着团队必须购买自有 Mac。无证书开发、ad-hoc 签名测试，与可交付签名是不同阶段：Tauri 文档称免费 Apple 开发者账号仅用于测试/开发，不能公证；下载分发需 Developer ID Application 签名及 Apple 公证，App Store 则需 Apple Distribution、App Sandbox/entitlements 与商店审核。Electron 文档同样列出 macOS 签名/公证所需的 Apple 开发者计划、Xcode 和证书。需在决定发布方式后核验账户资格、费用和 CI 秘密配置，当前没有证据表明项目已有 Mac 环境、Apple 账号或预算。[Tauri macOS signing](https://v2.tauri.app/distribute/sign/macos/)；[Electron code signing](https://www.electronjs.org/docs/latest/tutorial/code-signing)；[Apple command line tools](https://developer.apple.com/documentation/xcode/installing-the-command-line-tools/)

Windows 本地开发在现有 Windows 环境配置即可。签名与是否能运行需区分：Tauri 文档称签名不是运行的技术前提，但浏览器下载未签名程序可能触发 SmartScreen；微软说明新程序即使有有效签名，信誉累积前仍可能提示，EV 不再自动绕过警告。商店分发由商店另行签署。电子签名证书、商店账号和首次安装体验都需按最终渠道评估，不预设预算。[Tauri Windows signing](https://v2.tauri.app/distribute/sign/windows/)；[Microsoft SmartScreen reputation](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)

Tauri 的 Windows 运行时部署还需选 WebView2 Evergreen 安装/依赖或 Fixed Version 随应用分发；后者由应用承担运行时文件与更新决策，前者由 WebView2 自动更新。框架默认构建配置应在锁定后检查实际安装器目标，并在干净 Windows 虚拟机验证“无 Edge 浏览器/无预装运行时”等首次启动路径。[Microsoft runtime distribution](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/distribution)

## 未决事项与真实机器验证清单

- 决定用户支持的 Windows/macOS 最低版本、发行渠道（直接下载或商店）、安装器格式及是否需要自动更新；目前均未冻结。
- 盘点可用 Windows 构建机、托管 macOS CI、开发者账号/证书、商店账号和成本；本研究没有查询私人机器或账户。
- 在支持的 Windows/macOS 版本验证中文输入组合态；测试从浏览器、Office、截图工具粘贴图片及多 MIME 内容；测试文件拖入和编辑器内部拖动。
- 在 Tauri 验证所选 WebView2 版本、WebKit 版本、路径选取后的读写权限、附件 asset URL、重启后授权持久性；在 Electron 验证 renderer sandbox/IPC、用户目录与系统对话框。
- 对大笔记/大量图片、写入中退出、强制结束、磁盘空间不足、文件重名、导入路径穿越、更新后恢复进行故障注入；用已定义的导入/导出样本做往返校验。
- 实际签署并从浏览器下载：测试 Windows SmartScreen 首次安装提示；测试 macOS Developer ID 签名、公证和 Gatekeeper 首次打开；如上架则再走商店构建与审核条件。

## 参考来源

上文各项事实均在相应段落链接一手资料。链接包含官方产品文档、维护者仓库源文件、Microsoft Learn 与 Apple Developer 文档；发布版本按核验日记录。所有运行时/API 与发行规则均可能随新版本和操作系统更新，技术选型前应对实际锁定版本复核。
