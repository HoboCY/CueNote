# #32 个人安装包验证

状态：进行中。#32 尚未关闭；双平台隔离 CI 与最终密文下载、解密、包哈希核对仍待完成。

## 已完成的本机预检查

- Windows x64 0.0.0 合成旧包与 0.1.0 当前包成功构建。合成旧包只用于升级测试。
- 前端测试 136/136、类型检查与生产前端构建通过；打包 JavaScript/PowerShell 语法通过。
- Windows 目标许可清单收集 409 个组件（保守包含构建与测试依赖），补齐未随 crate
  提供的许可原文。KaTeX 字体按实际 TTF 内嵌声明单列 OFL-1.1；NSIS runtime/plugin
  与 Rust 标准库声明也包含在随包材料中。
- age 1.3.2 的安装归档通过锁定 SHA-256；正向加密/解密内容一致，错误密钥与篡改
  密文均被拒绝。正式私钥仅存仓库外本机文件，ACL 限定当前用户；CI 只获得公钥。
- 默认系统工具缓存中 NSIS 解压后的重命名返回 Windows error 17；改用 Tauri 的
  `useLocalToolsDir` 工程内缓存后两个版本成功打包。未移动或清理用户系统缓存。

本机预检查没有运行安装器，没有改动用户已有应用安装或实际笔记数据。当前本机
安装包是开发预检查产物，最终交付以干净提交的 CI 构建及后续下载核对为准。

首轮 CI：[`39041481800`](https://github.com/HoboCY/CueNote/actions/runs/39041481800)，
提交 `0a2898ea53f534b30a4d659d6d9528b9a54d4b15`。Mac 前端测试、203 项原生测试
（35 个测试程序，0 ignored）和 schema 检查通过；随后在许可检查中准确拒绝了
12 个未附带许可证文件的 Objective-C crate。已按各自 `.cargo_vcs_info.json`
指定提交补齐上游许可说明及其引用的 MIT 原文，并保留 Cargo 作者声明。新一轮
仍须完成安装包和生命周期检查。

同轮 Windows 前端与 211 项原生测试（35 个测试程序，0 ignored）通过，但 schema
逐字节检查拒绝了 Windows 新检出时的 CRLF 换行。为生成的两个 JSON Schema 和
TypeScript 契约设置 `eol=lf`，保持生成器与两平台 Git 检出的字节一致；未改变 schema。

第二轮有效 CI：[`39042235350`](https://github.com/HoboCY/CueNote/actions/runs/39042235350)，
提交 `e9ef0e6183c9a63d9b7a7a0dd63dcf32bc0ea9e3`。两端测试、schema、许可与包构建通过。
Mac 全部八阶段通过，19 个受保护文件未变；405 个组件、633 份许可文本的产物已在
本机完成解密与哈希校验。DMG SHA-256 为
`bc4e4175b66f8e52c6568d51bd6b70d146806c8e82f5f0987a8ebbab7561a1ad`。

Windows 升级后严格程序哈希检查失败。对本机安装包解包与构建目录程序逐字节比较，
仅发现 Tauri 临时将 `__TAURI_BUNDLE_TYPE_VAR_UNK` 改为 `..._NSS` 的 3 字节差异；
[锁定版本 bundler 源码](https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.12.1/crates/tauri-bundler/src/bundle.rs)
证实其打包后会恢复原文件。应用没有自动更新器，改用官方 `--no-binary-patching`
选项，继续要求安装后的程序与回执哈希完全一致，不放宽验收条件。

## 代码审查

基线：`68583a1e06a0e089b1b025fd6cdfb8aeccd129c2`（#31 已交付）。
依据 #32 正文、已接受平台范围、仓库 AGENTS 与 `code-review` 技能，主代理分别
检查 Spec 和 Standards。遵循用户最新路由要求，不调用未验证的外部代理，不将
本次检查描述为独立子代理审查。

- Spec：构建输入、稳定身份和路径、免费依赖、私密产物交付及实机验收边界均已落实。
  运行验收仍开放，不能以本机 Windows 构建代替双平台安装生命周期结果。
- Standards：限定修改在打包、许可、CI 和 CI 数据生成例程；未改变笔记业务流程。
  卸载模板只保留明确安装文件和空目录清理；安装测试拒绝在非临时 GitHub runner
  执行；上传步骤仅匹配 `.age` 密文及校验值。暂未发现需要阻止首次 CI 验证的代码问题。

## 尚待收集

1. 两平台完整测试、schema 同步、包架构/身份/签名检查。
2. 八个安装生命周期阶段及原生笔记、历史、备份与配置保留哨兵的 SHA-256。
3. 两个平台成功 CI 链接、最终安装包与密文 SHA-256、本机解密核对。
4. 用户真实 Windows/M1 Pro 安装与实际 OS、输入法、剪贴板、OneDrive 仍在 #33 验收。
