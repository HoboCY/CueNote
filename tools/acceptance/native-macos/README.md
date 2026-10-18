# CueNote macOS 原生验收工具包

这是复用现有原生故障与恢复测试的预编译工具包，用于在真实 Apple Silicon Mac 上复跑。具体源码提交以 `receipt.json` 为准；它不是正式安装包，也不等同于冻结的设备验收结果。CI 负责构建、检查并加密，实机结果另行记录。

## 环境

- Apple Silicon（M1 或更新），macOS 14 或更新。
- 不需要安装 Rust、Node、Xcode 或 Python。
- 运行器只使用 macOS 自带的 bash、`shasum`、`grep`、`sed`、`awk`、`mktemp`、`cp`、`cat`、`tee`、`head`、`chmod`、`uname`。
- 工具包已经用 ad-hoc 签名，未公证。

## 获取与校验

交付物只有加密包和它的校验文件。先在保管私钥的交付机器上核对加密包 SHA-256 并解密，再把解密后的归档传到 Mac；私钥不需要传到 Mac，也不放在仓库或 CI 中。在 Mac 的新目录中解包：

```bash
mkdir CueNote-native-acceptance
tar -xzf CueNote-macos-native-acceptance.tar.gz -C CueNote-native-acceptance
cd CueNote-native-acceptance
```

不要只运行单个测试可执行文件；运行器会先按 `SHA256SUMS` 校验测试二进制、运行器和 `receipt.json`。

## 运行

```bash
/bin/bash ./run-acceptance.command
```

也可以在终端中运行 `./run-acceptance.command`。请保持整个工具包目录不变，尤其不要拆开：

- `debug/deps/`：9 个预编译测试套件。
- `debug/examples/`：4 个故障探针；测试通过相对位置查找它们。
- `receipt.json`、`suites.tsv`、`SHA256SUMS`：来源、特性、平台、套件测试数和哈希。

运行器会：

1. 校验 `receipt.json` 为干净源码、debug 构建、`storage-fault-injection` 特性和 `aarch64-apple-darwin`。
2. 校验 `SHA256SUMS`，拒绝缺失、修改或不完整的套件。
3. 在 `$HOME` 下创建全新的隔离根目录，写入固定标记 `.cuenote-resource-tests`。
4. 以 `--test-threads=1` 串行运行全部 9 个套件，保留完整 stdout/stderr、退出码和测试计数。
5. 拒绝 0 测试、跳过测试、部分运行、套件缺失和非零失败。

结果目录会打印在结尾，并保留：

- `results/<suite>.log`：每个套件的完整输出。
- `results/summary.txt`：每个套件的退出码、期望数、通过/失败/跳过数。
- `results/console.log`：串行运行的合并输出。

## 套件

| 套件 | 内容 |
| --- | --- |
| `storage_interruptions` | 保存中断、冲突和非协作写入 |
| `backup_failures` | 备份复制中断、磁盘满与保留历史 |
| `backup_restore_failures` | 单笔记恢复前保护与发布中断 |
| `library_restore_failures` | 整库恢复前滚、回滚与部分日志 |
| `save_failures` | 保存失败后的独立副本 |
| `backups` | 备份轮换、来源变化与目录安全 |
| `backup_restore` | 单笔记备份恢复与冲突 |
| `library_restore` | 整库恢复与崩溃恢复方向 |
| `storage` | 基础原生存储与包完整性 |

四个故障套件只在 debug 且启用 `storage-fault-injection` 时编译；构建脚本会拒绝 release、0 测试或缺失的套件。

## 边界

- 工具不启动 GUI、不访问网络、不修改正式应用数据。
- 每个测试的临时目录都位于本次全新隔离根目录下，彼此独立。
- `valid-rich-v1.cuenote` 已嵌入测试二进制，并写入该测试自己的临时目录，因此不依赖构建机路径。
- 本工具包是 CI 可复现的补充证据，不替代冻结验收、人工输入法/剪贴板/同步场景或正式发布检查。
- 失败时保留结果目录和终端输出；不要混用不同工具包、不同源码提交或手工修改过的可执行文件。
