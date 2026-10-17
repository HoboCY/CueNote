# Windows installer template

`installer.nsi` derives from Tauri CLI **2.12.1**, under the adjacent MIT license:
https://raw.githubusercontent.com/tauri-apps/tauri/tauri-cli-v2.12.1/crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi

Unmodified upstream SHA-256:
`dabed59013b1d78b879a1a85bc7f2eed2993b33a9a90cdabe5946de3d3950597`

Only three changes are intended:

1. Remove the uninstall data-deletion checkbox and its callbacks.
2. Remove the complete conditional app-data deletion branch. Silent and interactive
   uninstalls retain configuration, the WebView profile, backups and recovery drafts.
3. Default current-user installation to `$LOCALAPPDATA\Programs\CueNote`, separate
   from the backup fallback `$LOCALAPPDATA\CueNote\backups`.

The remaining upstream uninstall code removes explicitly packaged files and empty
installation directories only. It does not recursively remove the installation
directory. Existing install-location registry values still guide upgrades.
Rebase these three edits and rerun lifecycle checks when updating Tauri CLI.
