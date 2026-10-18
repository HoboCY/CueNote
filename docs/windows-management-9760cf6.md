# CueNote Windows 笔记本管理验收整理（源 9760cf6，二进制 a4520e94）

**结论：仅 Windows 单端的部分合成证据。A01、A02 部分通过但未闭环，A04 未测；`dailyUseApproved=false`，不得标记为整例或双端验收通过。**

- 验收口径：Q53 A01 / A02 / A04（`accepted-q50-q53-a01-a26.md` 第 100、101、103 行）。
- 证据范围：仅 `windows-management-*`（计划、截图、accessibility、native-snapshot、inspection、restart-close）。
- 证据根：`C:\Users\Hobo\.codex\visualizations\2026\10\10\01a12393-3b57-76e3-afff-50bd8ef5f539\implementation-execution\issue-33`；本文所有证据路径均相对该根。
- 环境：源版本 `9760cf6eb6f7ea353d94ec5f7ebd4c08aaceb50c`（提供的上下文，本次未重建构建溯源）；普通 Windows 二进制 `D:\Apps\CueNote\cuenote.exe`，SHA256 `a4520e94a0807b7f45a2ed722bd7dbce92016342a312ca6480845002846e0919`（本次重算一致），FileVersion 0.1.0；独立库 `D:\Projects\notebook\package-output\ordinary-win-management-9760cf6\library`。
- 时间口径：快照 `utc` 为 UTC（2026-10-18）；界面时间为本地 UTC+8（如 21:15 = 13:15Z）。
- 本整理全程只读：未操作窗口/进程/远程设备，未调用业务接口，未读密钥/个人资料，未提交/推送。

## 一、结论

| 用例 | 状态 | 已核实 | 缺口 |
| --- | --- | --- | --- |
| A01 | 未测（部分子步骤已验证） | 空库初始化；新建/重命名笔记本；非空本拒删；空本取消/确认删除；全部笔记与全库搜索入口新建笔记均归默认本；两条同名笔记独立保存 | 尚缺“笔记本名非空且不重复”的完整拒绝验证（无空白/重名创建尝试）；默认本已记录无编辑/删除控件，原生拒绝路径不在本次范围 |
| A02 | 未测（部分子步骤已验证） | 标题保存更新内容修改时间；打开/置顶/移动不改该时间（四篇逐一比对）；置顶后居全部笔记首位；移入默认本 | 缺重开帧：无法验证“回到上次编辑笔记、已不存在则回列表”与重开持久化；“全局/笔记本置顶一致”缺独立单本视图帧；取消置顶后的内容时间排序待核验 |
| A04 | 未测 | — | 无回收站/恢复/永久删除的计划或快照，完全未测 |

## 二、已核实事实

### A01（创建/重命名/删除笔记本；入口归属；同名笔记）

1. **空库初始化**：初始帧（13:12:34Z）显示 0 个笔记本、搜索索引暂不可用，并提供“初始化当前空笔记库”；`init` 计划点击后，13:12:50Z 帧为 1 个笔记本、0 篇笔记；create-a 快照中默认本 `288e4300` createdAt=13:12:48.957Z。
2. **新建笔记本**：create-a 计划输入“验收甲”，快照新增 `4cf8e8ba`（createdAt=13:13:45.723Z），默认本不变。
3. **重命名笔记本**：first-note 计划改为“验收甲改”；快照中 `4cf8e8ba` name=验收甲改（modifiedAt=13:14:10.844Z）。
4. **全部笔记入口归默认**：default-note 计划在“全部笔记 2”视图点“在『默认笔记本』中新建笔记”，新增 `53f4911e`（默认入口笔记，createdAt=13:18:49.393Z），notebookId=默认本。
5. **全库搜索入口归默认**：global-search 计划搜索“同名”得到“2 条匹配 · 全部笔记”；search-create 计划在结果页新建 `b9964af6`（全局搜索入口笔记，createdAt=13:19:21.658Z），notebookId=默认本。
6. **同名笔记独立保存**：`a5cffb72` 与 `de63e7b1` 标题均为“同名验收笔记”，noteId 与文件独立；全局搜索帧（2 条匹配）与笔记本列表帧（2 篇）均可见两条同名记录。
7. **非空本拒删**：nonempty-delete 计划对“验收甲改”（2 篇）打开删除对话框并点删除；界面明示“此笔记本仍有笔记，请先将笔记移动到其他笔记本，再删除此笔记本。”，快照中该本与笔记均保留。
8. **空本取消删除**：empty-cancel 计划取消后，取消帧与删除前帧的库文件、截图、accessibility 逐字节一致（两个 JSON 快照仅采集 `utc` 不同）；`fd89d972`（验收空本）仍在。
9. **空本确认删除**：empty-delete 计划确认后，快照中 `fd89d972` 消失，默认本与“验收甲改”保留。
10. **默认本限制的界面证据**：所有帧中默认本仅有“选择笔记本 默认笔记本”，无重命名/删除控件；本次未直接调用原生删除接口。

### A02（排序/移动/置顶/重启）

1. **标题保存更新内容修改时间**：`de63e7b1` 13:14:11.692Z→13:15:32.768Z；`a5cffb72` 13:17:00.126Z→13:17:59.802Z。
2. **打开/置顶/移动不改该时间**：四篇笔记 contentModifiedAt 在 search-created、old-pinned、old-moved-default、before-reopen 各阶段完全一致（`53f4911e`=13:18:50.567Z、`a5cffb72`=13:17:59.802Z、`b9964af6`=13:19:22.834Z、`de63e7b1`=13:15:32.768Z）；期间发生打开、置顶与移动。整理时间另行更新：`de63e7b1` 置顶后 organizationModifiedAt=13:20:46.273Z，移入默认本后=13:20:50.791Z。
3. **置顶后居首**：pin-old 计划打开旧笔记（21:15 帧）并置顶；old-pinned 帧中该笔记居列表首位（◆、“已置顶”、按钮变“取消置顶当前笔记”），native 中 pinned=true。
4. **移入默认本**：move-default 计划确认后，`de63e7b1` notebookId 变为 `288e4300`；默认本 3 篇、验收甲改 1 篇。

### 其他（进程、库、哈希）

1. **before-reopen 为最新 UI/原生帧**：inspection utc=13:20:56.7006989Z，native utc=13:20:56.8467119Z；侧栏已收起，当前笔记为已置顶并已移入默认本的 `de63e7b1`。
2. **关闭退出 0（无重开帧）**：restart-close.json：processEnded=true、requestAccepted=true、exitCode=0、pid=87620、utc=13:21:29.7015435Z。
3. **关闭前后保存的库文件字节不变**：本次只读校验，独立库当前 6 个文件（2 本 + 4 笔记）与 before-reopen 快照 path/bytes/sha256 完全一致。
4. **哈希自洽**：14 个 native-snapshot 所列库文件与证据副本逐一比对，0 处不一致；二进制哈希与全部 20 个 inspection.json 记录一致。

## 三、重开状态

- `windows-management-reopened-plan.json` 仅含 `wait 300ms`，不存在 `windows-management-reopened` 证据目录。
- 重开界面/数据、书本与全局排序一致性、上次笔记回退均**无证据**；阻塞原因为系统焦点限制，等待用户。
- 因此上述 A02 缺项保持阻塞，不做推断。

## 四、非通过帧（不得计入通过）

| 帧 | 说明 | 真实成功帧 |
| --- | --- | --- |
| windows-management-first-note-interrupted-view | 标题“未命名笔记”：自动填充焦点中断 | windows-management-first-note-saved |
| windows-management-second-note | 标题“未命名笔记”：focus 方法失败留下的空笔记 | windows-management-second-note-saved |
| windows-management-pointer-release.json | 仅记录自动填充关闭后释放此前测试鼠标按下的清理动作，非业务证据 | — |

## 五、合成 ID

| 类型 | ID | 名称 | 备注 |
| --- | --- | --- | --- |
| 笔记本 | 288e4300-bb22-425e-bff5-00343c7fda72 | 默认笔记本 | default=true，createdAt 13:12:48.957Z |
| 笔记本 | 4cf8e8ba-34ee-4ff2-ae94-76d52f32ac96 | 验收甲 → 验收甲改 | createdAt 13:13:45.723Z，modifiedAt 13:14:10.844Z |
| 笔记本 | fd89d972-fab8-4768-a9cd-fae87718994f | 验收空本 | createdAt 13:19:43.271Z；取消删除后保留，确认删除后消失 |
| 笔记 | de63e7b1-12b2-4b88-bc80-287b618be1f9 | 同名验收笔记（旧） | 创建于验收甲改；置顶；后移入默认本 |
| 笔记 | a5cffb72-d21b-4aa1-a9b9-3cd01b0f94be | 同名验收笔记（新） | 创建于验收甲改 |
| 笔记 | 53f4911e-b23e-403e-92cc-98e866c34587 | 默认入口笔记 | 全部笔记入口创建，归默认本 |
| 笔记 | b9964af6-0aac-4609-a630-3585f6bedbd5 | 全局搜索入口笔记 | 全库搜索入口创建，归默认本 |

## 六、关键证据相对路径与 SHA256

完整 169 个文件清单（路径+SHA256）见同目录 `windows-management-9760cf6-results.json` 的 `evidenceManifest`；以下为支撑本文结论的关键文件。

| 相对路径 | SHA256 |
| --- | --- |
| accepted-q50-q53-a01-a26.md（验收口径来源） | a4cc222c8c6ab6cdac5470bd722477d96d3f592472bed18b08229575a3d80068 |
| windows-management-init-plan.json | 315c6c363da1a04b270d4985b23973be840970cb03284f0154db62b3f7d30fb1 |
| windows-management-initial-view/accessibility.json | 090230d62ef457a02d959ff05f0f9c1de851440b6dfa74705a1abcc59a06903e |
| windows-management-init-dialog-view/accessibility.json | a37e7d354bec948ee08addc33852e46975f918fe970a89bdc24aa51126e4a946 |
| windows-management-create-a-plan.json | cebb9eb07ca1355abd9dfb3aa3171ca8e99605ce38e7646183441f7cb4ce9f6c |
| windows-management-create-a/native-snapshot.json | 376219a5912230d78f7b5086b376ea852b6908ac1bb3e1cd71abab4cbad80413 |
| windows-management-first-note-plan.json | e158fe9a4815de070a597c9da423fdaad24357d59edb1cc36fecd3a099128d0c |
| windows-management-first-note-saved/native-snapshot.json | b910a007ec9994794b909f738ad42d075ef589a37598b477a4e53a7a80d7f56f |
| windows-management-first-note-interrupted-view/accessibility.json | 774872fcf9adfa2361321f3825ec320f310c082114d06104418890a98c29e40e |
| windows-management-second-note-plan.json | 387b5bc4569baf6893835c98fe401de8d56f3a1e0a8a42ea81646086b15efd08 |
| windows-management-second-note-paired-plan.json | d4f4771dd32cc91be83baaa6e57fb04fbb6a63674500d1333c5c72eedc55af7d |
| windows-management-second-note/native-snapshot.json | f6bd96651b03fcbfdb5c36a7783b3d22864b67a8e0967886739737c67158d578 |
| windows-management-second-note-saved/native-snapshot.json | 4d56a877cc0a327775ada77a8b1f9bc736bc1bbf7297939e7a7c9aef3cadc2ad |
| windows-management-default-note-plan.json | e49acf9403f56a751008b776a226ba777be6dbbbf72978d745e54a754bc1d311 |
| windows-management-default-note/native-snapshot.json | 0fda486bd09db517c9428ae694ef12a6e4e225855600e985071ade64afa37cbc |
| windows-management-global-search-plan.json | 1fe1940452954508dbacff12b12e31ba3f3ea17007cd1342831b9cafec775005 |
| windows-management-global-search/accessibility.json | 6daa3cb7ca85c0374d16ad9288b59b59154af1018c46ba572ce693e4b44274bf |
| windows-management-search-create-plan.json | 9d8cadac624e7f36c339c0893090f5ff91b376d9e0c356e7492c22eb203f0bdc |
| windows-management-search-created/native-snapshot.json | 640199e24d5948c8f2c22491af0374744839d982eeacd429d560495efa0937c6 |
| windows-management-nonempty-delete-plan.json | 6a2cb37719991dc13573cc66bf77309d07436c669e42b6e94afb1652961631dd |
| windows-management-nonempty-delete-refused/accessibility.json | c869c9f230e478a23fe667a5e2f50bcc9bf499f3238fc1f61d97015c37d19f4d |
| windows-management-empty-book-plan.json | 3445390f86023be46c18957c4cf5736d5151b99c60f194948e71748fd2c0359d |
| windows-management-empty-book/native-snapshot.json | 22d1f726b4817cab662e1dda417ffd33d1eb6f8c1eb8ac9fde2c4396cf5633e9 |
| windows-management-empty-cancel-plan.json | 1c16b1316403baf1050d15200488b42bdf653e25156d398d0b4dffd886dafcbd |
| windows-management-empty-delete-cancelled/native-snapshot.json | a6bd0c1db6efbc215109294c1e97364d7b7a5cbc813fd5b35e5cd149a6ba2e19 |
| windows-management-empty-delete-plan.json | 594da34d5aeb929bf3b83bd48e30d50ca850beca3418a6cb89ce8ba79c08c1b9 |
| windows-management-empty-deleted/native-snapshot.json | 2721b28e1bea6dc256a2a9733123a4458d20cd74442025221a2911ca2693dfc8 |
| windows-management-pin-old-plan.json | e67e8f280e0a63da4d19656239debc2c85f9c22064448f0e8de30695e9aecb68 |
| windows-management-old-pinned/native-snapshot.json | 286bb3c6af6f55e6262832ae6b6e55f6b539b18be541f832a5218ccba43f4ce7 |
| windows-management-old-pinned/accessibility.json | 5c4b3a8a70bebb0f31fcd2837e0795ad0f9ae8b3b403157b7dc330ded25933a2 |
| windows-management-move-default-plan.json | 82117f0720d3e5243344a81cf8c0861140980723f564d15cfc8e34488b4b7c65 |
| windows-management-old-moved-default/native-snapshot.json | 9ad8d1753d0bfad7c1475e54c89c2efc54fa7d4e736be4dc560ac671fe528aeb |
| windows-management-before-reopen-plan.json | d67e93cc2da8d0e13bd322690b3038b136e4f3dfbf6cd4c018999625638d42e9 |
| windows-management-before-reopen/native-snapshot.json | 1e9fab9b41170beeaf68047bb6aac7097a677bd47a9629f5750a82a375367b7c |
| windows-management-before-reopen/inspection.json | 873e278494b69433cea0c22d866ffd825ed512d78201753e509610128553a2bc |
| windows-management-restart-close.json | ad1f7daf20b9ad53b1c6025ba2067a490ee1f5801e78e87bd69f36ba16389c17 |
| windows-management-reopened-plan.json | 7b9c07e0565a4c017582173449c8b6a381e1f227a8e6dc0fb245931b52f94a84 |
| windows-management-pointer-release.json | e3757a5ff3e4a76eeb4c9b625e45ac9355fdecf751036b2b90a486b6c31f96aa |

## 七、校验方式与边界

- 用 Node/PowerShell 只读重算：二进制、全量证据文件、14 个快照自洽、库当前状态、取消帧逐字节比对、四篇笔记内容修改时间不变性；未操作应用窗口。
- 不含 macOS、真实双端、其他帧（如 `windows-user-*`、`windows-cf350d8-*`）；源→二进制构建溯源未在本次重建。
- 未完成项与阻塞项如上；`dailyUseApproved=false`。
