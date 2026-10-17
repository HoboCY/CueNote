# Mac 实机性能工具

这是 #33 的独立验证工具，不是日用安装包。最低 macOS 14、Apple Silicon；用你的 M1 Pro 运行。无需安装 Rust、Node 或 Xcode。工具采用临时签名，未公证。

1. 将整个工具目录放在本机普通目录，例如 ~/CueNote-performance-kit，不要放进 OneDrive 或其他同步目录。保留 receipt.json、manifest.json、plans 和三个可执行文件。
2. 接通电源，关闭其他构建、备份复制等负载。预留至少 20 GiB 空间。确认正式 CueNote 没有尚未完成的编辑，退出正式应用，避免快捷键和窗口混淆。
3. 在终端进入工具目录，运行 ./cuenote-performance-driver doctor。在“系统设置 → 隐私与安全性 → 辅助功能”允许终端/该驱动。重新打开终端后再次运行 doctor，确认 accessibility 和 nativeInput 均为 true。工具不会绕过系统授权或用 JavaScript 模拟输入。录屏权限仅用于窗口截图，未授权时会记录截图缺失。
4. 运行 /bin/bash ./run-matrix.command。先生成并完整校验固定样本，再复制成三个独立笔记库，才开始计时。整个过程请保持应用在前台，不操作鼠标键盘，不改变显示缩放或窗口位置。焦点丢失会停止并保留失败记录。
5. 结果保存在 ~/CueNote-acceptance-日期-时间。查看 summary.json 和 content-verification.log，保留原始日志、截图及失败现场。不要将目录内 .observer-token 发送给他人；也不要直接公开整个结果目录。需要提交的脱敏证据由后续核对整理。

测量通过原生鼠标、键盘和窗口事件驱动 WKWebView，观察器仅允许固定只读查询。统计使用全部样本的最近秩 p95；正常启动 10 次、初次千篇索引 3 次取最大值。首次打开表示本轮首次访问，不代表清空系统缓存。自动备份保持开启。

启动根目录中的设置、库、草稿和备份与正式应用隔离。WKWebView 的数据使用独立系统存储 UUID，记录在每次运行的 observer.json；macOS 实际保管这些缓存，不位于结果目录的 webview 文件夹。工具不自动清理旧结果或缓存，也不强制杀死应用。

此工具的构建、协议检查可在 CI 验证。M1 Pro 的原生输入、Retina 坐标、实际速度及保存重开必须由实机运行证明。timingGatesPassed 只表示计时门槛，不涵盖中文输入法、真实剪贴板、OneDrive、资源上限、安装升级等日用验收。

失败后保留当前目录。处理报告的具体原因后，用新的结果目录重跑；不要混合不同二进制、不同样本或被修改的操作计划。没有测完的指标保持未通过。
