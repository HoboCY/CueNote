# CueNote 个人试用安装包

交付范围：Windows x64 与 Apple Silicon Mac，手动更新。未购买签名证书，
未创建公开 Release、商店分发或自动更新。Windows 包未签名，Mac 为 ad-hoc
签名、未公证；系统可能要求用户确认来源。不能把 CI 启动结果当作个人实机或
OneDrive 验收，后者由 [#33](https://github.com/HoboCY/CueNote/issues/33) 完成。

## 安装、更新与卸载

- Windows：运行 `CueNote_*_x64-setup.exe`，默认仅安装给当前用户，程序目录为
  `%LOCALAPPDATA%\Programs\CueNote`。关闭 CueNote 后运行新版本安装器即可升级。
  使用 Windows“已安装的应用”卸载；卸载器不提供清除用户数据的选项。
  缺少系统 WebView2 时安装器会从 Microsoft 获取运行时，需要联网。
- Mac：打开 `CueNote_*_aarch64.dmg`，将 CueNote 拖到“应用程序”；更新时先退出
  CueNote 再替换旧 `.app`。卸载只删除 `.app`。包设定 macOS 14.0 为构建下限；
  CI 使用 macOS 15 ARM64，这不承诺所有 14.x 系统已测试。用户 M1 Pro 的实际
  系统版本、首次打开提示及权限体验需在 #33 记录。
- 应用身份固定为 `com.hobocy.cuenote`，可执行文件为 `cuenote`。
  安装目录内 `licenses`（Mac 为 `.app/Contents/Resources/licenses`）包含许可清单、
  锁文件和许可原文，交付压缩包也有一份。请将笔记库保存在应用目录之外。

| 数据 | Windows | macOS |
| --- | --- | --- |
| 默认笔记库 | 系统“文档”目录下 `CueNote` | `~/Documents/CueNote` |
| 本机配置 | `%APPDATA%\com.hobocy.cuenote` | `~/Library/Application Support/com.hobocy.cuenote` |
| 恢复草稿、历史 | `%LOCALAPPDATA%\com.hobocy.cuenote\recovery` | 上述 Application Support 目录下 `recovery` |
| 默认备份 | `%LOCALAPPDATA%\com.hobocy.cuenote\backups` | 上述 Application Support 目录下 `backups` |

自选笔记库和备份目录保持原位置。同步目录不适合作为本机恢复区；既有检测可能
选择安全的本机备份路径，应以应用显示的实际路径为准。卸载和重装保留这些数据。

## 可重复执行的构建

锁定 Node 24.21.0、Rust 1.99.0、npm/Cargo 锁文件和 Actions commit SHA。
构建机仍依赖系统 SDK 和托管 runner 镜像，回执记录实际环境；不宣称逐字节可复现。
构建需要平台前置环境：[Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)。

```sh
npm ci
npm test
npm run build
cargo test --locked --manifest-path src-tauri/Cargo.toml --all-targets
npm run package:licenses
npm run package:personal
```

普通本机构建在 `package-output/<target>/` 生成明文个人包，不自动安装、不上传。
输出与独立 Cargo `target/packages` 均不提交 Git。安装包不启用故障注入或性能验收功能。
应用没有自动更新器，构建使用 Tauri `--no-binary-patching`，使安装包内程序与
构建回执记录的程序字节一致。
版本发布前同步 `package.json`、npm 锁文件、Cargo manifest/lock、Tauri 配置中的版本。

CI 在标准 `windows-2022`、`macos-15` runner 上检查与构建。
`--upgrade-fixture` 先构建相同源码、版本为 0.0.0 的合成旧包，再恢复所有版本文件并
构建当前版本。测试真实递增版本安装路径，不冒充历史发行版的数据迁移测试。
隔离 runner 使用真实原生笔记、历史和手动备份，逐阶段比较 SHA-256；测试旧版安装、
启动、升级、新版启动、卸载、重装、重启和再次卸载。合成旧包不进入交付物。

## 公开仓库中的私人交付

公开仓库的普通 Actions artifact 不是私人文件。流程只上传 age 加密的 `.age`
文件与密文校验值，不上传明文安装包、构建目录、私钥，不创建 Release。
仓库只保存 `tools/packaging/recipient.txt` 中的收件公钥。私钥在用户本机仓库外
生成并保存；丢失私钥会失去旧密文的解密能力，应由用户自行保管备份。

从 Actions 下载后，使用校验过的 [age](https://github.com/FiloSottile/age) 1.3.2：

```sh
age --decrypt -i /path/to/personal-package-identity.txt -o CueNote.tar.gz CueNote.tar.gz.age
tar -xzf CueNote.tar.gz
```

先验证 `.age.sha256`，解密后再对照 `receipt.json` 的安装包 SHA-256。
回执记录源码 SHA、架构、版本、工具链、锁文件哈希和 CI run；`lifecycle.json`
记录隔离环境的每阶段数据保留结果。未通过检查的包不会进入上传步骤。
更新收件人时，先在仓库外生成新密钥并验证本机往返加密，再只提交新公钥。

## 验证状态

双平台 CI 运行链接、包哈希与本机解密检查记录在仓库的
`docs/package-validation.md`。个人设备日用及真实 OneDrive 的验收状态以 #33 为准。
