# Issue tracker: GitHub

任务和规格保存在本项目的 GitHub Issues，使用 gh CLI 操作。

## 仓库定位

从 git remote -v 的 origin 确定目标仓库。
若尚未配置 origin，先向用户确定目标仓库。
以下命令中的 <owner>/<repo> 均替换为已确定的仓库。

## 常用操作

- 创建：gh issue create --repo <owner>/<repo> --title "..." --body-file <文件路径>
- 查看：gh issue view <编号> --repo <owner>/<repo> --json number,title,body,labels,comments
- 列表：gh issue list --repo <owner>/<repo> --state open --json number,title,labels
- 评论：gh issue comment <编号> --repo <owner>/<repo> --body-file <文件路径>
- 添加标签：gh issue edit <编号> --repo <owner>/<repo> --add-label "<标签>"
- 移除标签：gh issue edit <编号> --repo <owner>/<repo> --remove-label "<标签>"
- 关闭：gh issue close <编号> --repo <owner>/<repo>

多行正文先写入 UTF-8 文件，再通过 --body-file 传入。
标签名称读取同目录的 triage-labels.md。

## Pull requests as a triage surface

**PRs as a request surface: no.**

## 技能指令映射

- “publish to the issue tracker”：创建 GitHub issue。
- “fetch the relevant ticket”：读取指定 issue 及相关评论。

## Wayfinding

- Map：单独创建带 wayfinder:map 标签的 issue，
  正文包含 Notes、Decisions-so-far 和 Fog。
- 子任务：每个任务一个 issue，使用 wayfinder:<type> 标签；
  type 为 research、prototype、grilling 或 task。
- 优先使用 GitHub 原生子任务关系；不可用时，在父任务中维护任务列表，
  子任务正文写明 Part of #<父编号>。
- 阻塞关系优先使用 GitHub 原生依赖；不可用时，
  在正文顶部记录 Blocked by: #<编号>。
- Frontier：按 Map 中的顺序，选择未关闭、无未完成阻塞项且未指派的任务。
- Claim：开始工作前，将任务指派给当前执行者。
- Resolve：评论记录结果并关闭任务，再向 Map 的 Decisions-so-far
  补充结论及链接。
