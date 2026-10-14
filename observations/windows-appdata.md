# 本机 AppData 保存失败记录

日期：2026-10-14。所有探针只使用生成的样机数据。

桌面初次保存调用真实 Rust 文件层后返回 `IO: atomically replace scratch note: ... (os error 17)`。界面保持“尚未保存”，草稿仍在，未生成目标笔记。独立 `storage_probe` 在同一 AppData 样机根目录下复现首次保存失败；同一二进制在工作目录下通过。

缩小到 [Windows API 探针](probe-windows-rename.ps1)，排除了编辑器、ZIP 和 Tauri IPC：

| 操作 | 本机 AppData 样机子目录 | 工作目录样机子目录 |
| --- | --- | --- |
| `.tmp` → `.tmp` 同目录 MoveFileExW | 失败，17 | 成功 |
| `.tmp` → `.cuenote` 同目录 MoveFileExW | 失败，17 | 成功 |
| `.cuenote` → `.cuenote` 同目录 MoveFileExW | 失败，17 | 成功 |
| `.cuenote` → `.tmp` 同目录 MoveFileExW | 失败，17 | 成功 |
| `\\?\` 扩展路径，同目录 MoveFileExW | 失败，17 | 未单独运行 |
| 同目录 CreateHardLinkW | 失败，17 | 未单独运行 |
| 同目录 ReplaceFileW，目标已存在 | 失败，5 | 已由文件层正常第二次保存覆盖验证 |

AppData 目录和新建文件具有 `Encrypted` 属性；工作目录的对应文件没有此属性。已定位到本机文件系统/目录环境差异，但没有通过解密或禁用过滤器来进一步归因，因此不能断言 EFS 本身就是唯一根因，也不能把错误信息理解为已证明文件实际跨盘。

官方 [MoveFileExW 文档](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw) 说明 `MOVEFILE_COPY_ALLOWED` 会允许使用复制与删除模拟移动。样机没有添加该回退，也没有改变系统加密、权限或过滤器设置。

为继续独立的编辑/持久化验证，样机启动器显式使用仓库 `.scratch/PROTOTYPE-wipe-me`。这不是修复默认 AppData 目录兼容性；后续正式保存模块仍需定义“目录不支持所需原子操作”的检测、提示与失败恢复行为。直接运行无路径覆盖的可执行文件在此环境仍可能保存失败。

本记录保留反例，不能将工作目录通过推广为任意目录、OneDrive 或 macOS 通过。
