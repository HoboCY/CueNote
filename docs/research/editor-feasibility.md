# CueNote 编辑器与可编辑思维导图可行性

- 研究问题：[哪些编辑方案能满足 CueNote 的 Markdown、图片与可编辑思维导图？](https://github.com/HoboCY/CueNote/issues/4)；父地图：[CueNote 首版产品规格与技术决策地图](https://github.com/HoboCY/CueNote/issues/1)。
- 核验日期：2026-10-10。依据官方文档、官方仓库源码入口及许可证页面；**这是文档研究，未安装依赖、未运行代码、未实测桌面或输入法行为**。
- 需求依据：个人使用，目标 Windows/macOS；笔记标题、线索区、内容区、总结区；内容区容纳普通文字、Markdown 所见即所得、粘贴图片和可编辑思维导图；首版离线保存且可完整导入导出，后续再自动同步。
- 尚未选定应用框架、前端栈、编辑器、导图库和持久化格式；本文只比较方案，不冻结选择。
- 术语采用已确认地图中的“笔记”“内容区”“思维导图”。仓库根 [LICENSE](../../LICENSE) 为 Apache License 2.0；下文各依赖分别核对，未作整体法律兼容结论。

## 结论速览

| 方案 | 文档显示的原生能力 | CueNote 仍需实现 |
|---|---|---|
| Tiptap / ProseMirror | 富文本 schema、扩展与节点视图、撤销重做；图片节点；文件粘贴回调；Markdown 双向接口 | 自定义导图块及序列化；本地资产管理；Markdown 自定义语法；两个编辑器的焦点和撤销边界 |
| Lexical | 富文本编辑框架、扩展/自定义节点、JSON/Markdown 序列化、撤销历史、IME 扩展 | 图片节点/资产保存和粘贴流程；导图节点/自定义 Markdown transformer；本地笔记容器 |
| Milkdown | 插件驱动的 WYSIWYG Markdown；CommonMark/GFM；历史、上传插件；图片块组件 | 导图节点和 Markdown 语法；上传器接入本地资产；图片块以外的图片元数据保真 |
| Mind Elixir | 可编辑节点、增删/移动事件、撤销重做、结构化数据导入导出 | 嵌入内容区、在笔记中持久化 JSON、统一键盘撤销策略、附件路径 |
| SimpleMindMap | 可编辑思维导图库；多插件；结构化数据；丰富的库/产品能力 | 同上；核对低维护状态和文档中的署名授权说法 |

## 富文本与 Markdown 编辑器

### Tiptap（建立在 ProseMirror 上）

- **事实：**Tiptap 将自己定位为无头、框架无关的富文本编辑器，底层基于 ProseMirror；扩展可增加 schema 节点、属性、命令和事件。官方仓库标示 MIT。[项目介绍与许可证](https://github.com/ueberdosis/tiptap)；[扩展模型](https://tiptap.dev/docs/editor/core-concepts/extensions)
- **事实：**StarterKit 含常用段落、标题、列表、链接、粗斜体及 Undo/Redo；Image extension 负责显示图片节点，但不负责上传。[StarterKit](https://tiptap.dev/docs/editor/extensions/functionality/starterkit)；[Image extension](https://tiptap.dev/docs/editor/extensions/nodes/image)
- **事实：**FileHandler 提供粘贴/拖放文件回调，拿到 File 后由应用插入节点或保存文件；扩展本身不上传、不保存资产。该扩展于 2025 年从付费层转为 MIT 开源；其他 Pro/Cloud 功能仍可能需账户或订阅。[FileHandler API](https://tiptap.dev/docs/editor/extensions/functionality/filehandler)；[开源范围公告](https://tiptap.dev/blog/release-notes/were-open-sourcing-more-of-tiptap)；[Pro 扩展边界](https://tiptap.dev/docs/guides/pro-extensions)
- **事实：**自定义节点视图可承载交互式内容；节点视图与 HTML 输出有意分离，需定义解析/输出；JSON 会序列化节点属性。[Node views](https://tiptap.dev/docs/editor/extensions/custom-extensions/node-views)；[React node views](https://tiptap.dev/docs/editor/extensions/custom-extensions/node-views/react)
- **事实：**官方 Markdown 页面将扩展标为 Beta，并称其处于 early release，可能变化或存在未支持的 edge cases；目前列出的限制包括 comments 不受支持、Markdown 表格每格仅允许一个 child node。扩展可定义 parser/serializer。此状态按 2026-10-10 核验，定版时应复核。[Markdown 官方介绍](https://tiptap.dev/docs/editor/markdown)；[API 参考](https://tiptap.dev/docs/editor/markdown/api/editor)
- **推断：**适合以富文本交互为主、同时要嵌入导图块的方向；可将导图 JSON 放在自定义块属性中，使用 React node view 挂载独立导图实例。
- **未知：**Markdown 包仍为 Beta；扩展组合、图片复制来源和跨平台 IME 必须在实际 UI 上验证。若导图运行在独立组件中，Tiptap 的撤销记录不会自然包含导图内部编辑。

### Lexical

- **事实：**Lexical 是供 Web 使用的富文本框架；核心不绑定 UI 框架，React 绑定为独立包。官方文档要求应用提供布局、工具栏、菜单、样式和存储。[简介](https://lexical.dev/docs/intro)
- **事实：**可组合扩展和节点；DecoratorNode 可渲染框架组件；节点可定义 JSON 与 HTML 导入/导出。[节点模型](https://lexical.dev/docs/concepts/nodes)；[JSON/HTML 序列化](https://lexical.dev/docs/serialization)
- **事实：**官方 Markdown 包以 transformers 支持 Markdown 转换；Lexical JSON 是内部文档结构，Markdown 必须由已注册的 transformer 表达。[Markdown 包](https://lexical.dev/docs/packages/lexical-markdown)；[与 ProseMirror 的比较（含 history / clipboard）](https://lexical.dev/docs/concepts/compared-with-prosemirror)
- **事实：**官方 API 有 IMEExtension；官方当前历史说明连续输入按默认 300ms 合并，默认历史不限深度（可设上限）。[IMEExtension](https://lexical.dev/docs/api/modules/lexical_extension/IMEExtension)；[history 行为](https://lexical.dev/docs/concepts/compared-with-prosemirror)
- **图片边界：**文档说明可构建含图片的完整编辑器，但本地图片的文件读取、资产保存、插入/恢复约定仍属应用职责；官方版本记录提供 pasted-file 处理配置，可决定文件与浏览器附带 HTML 同时出现时由哪条路径处理。[简介](https://lexical.dev/docs/intro)；[官方版本记录](https://github.com/facebook/lexical/releases)
- **推断：**如果后续 UI 选 React，Lexical 的自定义 DecoratorNode 可以承载导图视图；定制空间大，但图片、Markdown 细节和文件格式会更依赖应用代码。
- **未知：**目标桌面壳中的输入法、图片剪贴板来源和自定义节点 Markdown transformer 组合未实测；JSON 版本迁移策略仍需应用定义。

### Milkdown

- **事实：**Milkdown 定位为插件驱动的 WYSIWYG Markdown 框架，基于 ProseMirror、Remark；项目仓库标示整体 MIT。[官方介绍](https://milkdown.dev/docs)；[官方仓库](https://github.com/Milkdown/milkdown)
- **事实：**CommonMark 与 GFM preset 提供 Markdown 语法和表格等基础；自定义节点 schema、ProseMirror node view、Remark 插件及 Markdown parser/serializer 有公开扩展接口。[GFM preset](https://milkdown.dev/docs/api/preset-gfm)；[组件扩展](https://milkdown.dev/docs/api/utils)；[Markdown transformer](https://milkdown.dev/docs/api/transformer)
- **事实：**上传插件可处理拖放与粘贴；默认把图片转为 base64，也可提供异步 uploader 返回新节点。Image Block 组件提供调整尺寸、caption、链接和上传 UI，但 Markdown 输出中的图片仍按 inline image 渲染。[Upload plugin](https://milkdown.dev/docs/api/plugin-upload)；[Image Block](https://milkdown.dev/docs/api/component-image-block)；[组件配置](https://milkdown.dev/docs/plugin/using-components)
- **事实：**官方 kit 列出 history 和 upload 插件；历史插件建立在 ProseMirror 编辑模型之上。[Kit 插件列表](https://milkdown.dev/docs/guide/using-milkdown-kit)；[项目依赖说明](https://milkdown.dev/docs)
- **推断：**若优先级是“边编辑边看到 Markdown 对应排版”，Milkdown 是三者中最贴近该工作流的候选；自定义思维导图语法可以写成 Remark/节点插件，并由 node view 呈现交互图。
- **未知：**自定义导图节点在普通 Markdown 工具中的呈现方式、导入时保真策略及中文输入法行为需原型决定。默认 base64 便于单文档携带，但可能放大笔记、备份和同步负担。

## 可编辑思维导图组件

### Mind Elixir

- **事实：**官方仓库称其为可嵌入任意前端框架的 JavaScript 导图核心，列出节点编辑、拖放、增删/移动事件、撤销重做；不是只读 Mermaid、图片或通用画布。[官方仓库与 API 示例](https://github.com/ssshooter/mind-elixir-core)
- **事实：**数据是含 topic、id、children 等字段的对象；提供 getData/getDataString，以及 init/refresh 恢复接口；allowUndo 默认开启，支持清除历史基线。[数据、事件和导入导出](https://github.com/ssshooter/mind-elixir-core/blob/master/readme.md)；[类/API](https://docs.mind-elixir.com/docs/api/mind-elixir.mindelixir)
- **事实：**组件支持自定义 topic Markdown parser；这只说明节点文字可自行解析，不等于整张图可无损 Markdown 导入导出。仓库标示 MIT。[Markdown parser 示例与许可证文件](https://github.com/ssshooter/mind-elixir-core/blob/master/readme.md)；[LICENSE](https://github.com/ssshooter/mind-elixir-core/blob/master/LICENSE)
- **推断：**适合作为首个导图原型：最小需求覆盖直接且结构化数据适合嵌入笔记 JSON；将地图对象变更写回笔记，再用 init/refresh 恢复。
- **未知：**短文本编辑、中文输入法组合键、重开后的焦点与撤销行为、较大地图性能均未实测；官方文档未说明如何与宿主富文本编辑器共用撤销栈。

### SimpleMindMap

- **事实：**其开源部分是框架无关的 JS 导图库；仓库 README 明确说该库进入低维护状态，闭源客户端/插件与开源库是不同产品。[仓库说明](https://github.com/wanglin2/mind-map)；[官方介绍与功能边界](https://wanglin2.github.io/mind-map-docs/en/start/introduction.html)
- **事实：**官方介绍涵盖 Web 导图和插件化能力；库实例使用层级 data/children 结构。导图本身是可编辑组件，不是只读输出。[入门文档](https://wanglin2.github.io/mind-map-docs/en/start/introduction.html)；[官方 README/API 示例](https://github.com/wanglin2/mind-map/blob/main/README_MORE_EN.md)
- **许可证需复核：**仓库登记 MIT 并提供 LICENSE，但扩展 README 又写商业使用需保留版权声明和署名，并提及付费移除选项。不要仅根据徽标推断实际署名安排；若考虑该库，先核对确切分发版本和作者许可表述。[仓库 LICENSE](https://github.com/wanglin2/mind-map/blob/main/LICENSE)；[扩展 README 的许可说明](https://github.com/wanglin2/mind-map/blob/main/README_MORE_EN.md)
- **推断：**功能面可作为 Mind Elixir 的对照原型；低维护状态和授权表述需要额外评估，不优先作为无验证的默认依赖。
- **未知：**库的当前维护节奏、指定版本 API、嵌入所需插件、键盘与输入法冲突均需版本级验证。

## 数据、图片与撤销边界

- **事实：**CommonMark 图片语法给出替代文本和目标地址，不定义附件清单或复制资产二进制的包格式；应用也可把 data URL 文本用作目标。[CommonMark 图片语法](https://spec.commonmark.org/0.31.2/#images)。**推断：**普通 Markdown 规范没有 CueNote 可编辑思维导图的语义节点。
- **推断：**Markdown 往返只能保留可表达且有序列化规则的内容。编辑器内部 JSON 可保留节点属性，Markdown 外发不能自动保留导图交互状态、折叠状态、样式、附件字节等全部数据。
- **推断：**优先定义应用原生的完整笔记包（manifest/内容 JSON/相对路径资产或等价格式），将图片以稳定资产 ID 或相对路径引用；另提供 Markdown 与 assets 文件夹的导出。不要把大块 base64 当成默认长期资产存储格式。
- **推断：**若需 Markdown 中可往返导图，可在应用专属 fenced block 中保存结构化 JSON，并给所选编辑器注册解析/渲染器；其他 Markdown 工具只会看到代码块。需保留原始块或未知节点，避免导入再导出静默丢失。
- **事实：**Tiptap/ProseMirror、Lexical、Mind Elixir 各自有历史能力；组件的历史 API 不等于跨组件统一历史。[Tiptap StarterKit](https://tiptap.dev/docs/editor/extensions/functionality/starterkit)；[Lexical history 对比](https://lexical.dev/docs/concepts/compared-with-prosemirror)；[Mind Elixir history API](https://docs.mind-elixir.com/docs/api/mind-elixir.mindelixir)
- **推断：**嵌套导图作为独立组件时，先按焦点把 Ctrl/Cmd+Z 路由到当前组件，并明确一次撤销的边界；“跨文字编辑和导图操作的单一撤销栈”是额外工程，不应视作现成能力。
- **IME：**ProseMirror API 暴露 composition 状态；Lexical 有 IME 扩展。这证明存在输入法处理接口，不能证明 CueNote 的具体桌面壳、React 包装、导图快捷键在中文拼音/微软拼音/macOS 输入法下没有问题。[ProseMirror composition 状态](https://prosemirror.net/docs/ref/#view.EditorView.composing)；[Lexical IME API](https://lexical.dev/docs/api/modules/lexical_extension/IMEExtension)

### 序列化恢复的具体边界

- **事实：**Tiptap 的交互 node view 不自动等同于 HTML 导出；要从 HTML 恢复自定义节点需定义可识别的 parse/render 规则，JSON 则保留节点属性。[Node views](https://tiptap.dev/docs/editor/extensions/custom-extensions/node-views)。
- **推断：**导图对象可作为独立内容块的 JSON 属性保存；保存笔记时应把导图更新写回父笔记，重新加载时先还原节点数据再初始化视图。
- **事实：**Lexical 的自定义节点需定义相应的序列化恢复逻辑；编辑器还需要注册保存内容所用的节点/扩展。[Serialization](https://lexical.dev/docs/serialization)；[Headless 文档中的节点注册约束](https://lexical.dev/docs/concepts/headless)。
- **推断：**若选择 Lexical，schema 版本、缺失节点和未知节点保留规则都应属于 CueNote 文件格式，而非仅依赖当前运行中的编辑器实例。
- **事实：**Milkdown transformer 明确把 Markdown AST 转为 ProseMirror 文档，也把 schema 节点映射回 Markdown；自定义导图块必须有双向 parser/serializer。[Transformer](https://milkdown.dev/docs/api/transformer)。
- **推断：**Milkdown 更适合把 Markdown 当作编辑与交换层，但完整迁移仍可另有原生包；两者需要独立定义并互相转换。
- **图片保真：**FileHandler 与 Milkdown uploader 给应用提供了粘贴文件入口，但路径、复制、删除、离线引用修复和导出打包依然由 CueNote 持有。[Tiptap FileHandler](https://tiptap.dev/docs/editor/extensions/functionality/filehandler)；[Milkdown upload](https://milkdown.dev/docs/api/plugin-upload)。
- **附件未知项：**截图工具、桌面文件管理器、网页和 Office 应用写入剪贴板的 MIME 内容可能不同；是否收到 File、HTML 图片或远程 URL，必须从目标桌面运行时采样。

## 可行组合与优先级

1. **优先验证：Milkdown + Mind Elixir。**最贴近 Markdown 所见即所得；图片上传器能接本地资产保存；导图对象可随自定义 Markdown/节点扩展留存。主要风险是自定义语法与 Markdown 往返兼容，以及独立历史栈。
2. **并列备选：Tiptap + Mind Elixir。**常规富文本扩展、图片文件粘贴回调和自定义交互节点路径明确；Markdown 支持仍标 Beta，因此更适合把 Markdown 视为导入导出格式而非唯一存储源。
3. **条件备选：Lexical + Mind Elixir。**若未来 UI 决定用 React 且愿意实现自定义节点/图片工作流，可纳入原型；当前任务没有框架选择证据支持现在优先锁定它。
4. SimpleMindMap 可替换进相同导图插槽做小型对照；先处理低维护和署名说明，再决定是否值得原型成本。

以上组合是基于文档的**推断**，不代表已经验证 Windows/macOS WebView、Electron/Tauri 或其他宿主兼容性。

## 有限原型验证清单

- 在候选宿主中用真实中文输入法反复输入、选字、撤销、切换“标题/线索/内容/总结”焦点；覆盖导图节点输入和节点快捷键。
- 用自定义导图块新增/删除节点、改文字、调整层级、折叠，再保存、关闭、导入、恢复；比对 JSON 和界面结构。
- 验证 Ctrl/Cmd+Z、Redo 在普通文字、图片粘贴、导图编辑、切换焦点后的步数边界；确认宿主热键未双重触发。
- 从文件复制、截图复制、浏览器 HTML 图片复制分别粘贴；确认保存的是资产文件/稳定引用而非临时 blob URL，并验证离线重开。
- 建立一份含粗体、链接、列表、表格、图片、导图、中文标点的样例，在原生格式和 Markdown 间完整往返；记录损失字段和未知块保留策略。
- 给原生完整导出包换目录后离线导入；校验无网络依赖、图片均可打开、导图仍可编辑。核对实际解析版本、依赖许可证与 NOTICE。
