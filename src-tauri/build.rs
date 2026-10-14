fn main() {
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "get_app_version",
            "get_workspace",
            "use_default_library",
            "choose_library",
            "open_library_folder",
            "new_note",
            "create_notebook",
            "rename_notebook",
            "delete_notebook",
            "update_note_organization",
            "trash_note",
            "restore_trashed_note",
            "delete_trashed_note",
            "set_view_preferences",
            "open_note",
            "save_note_text",
            "activate_close_guard",
            "complete_close",
            "list_recovery_drafts",
            "restore_recovery_as_copy",
            "save_current_as",
            "save_recovery_as",
        ]));

    tauri_build::try_build(attributes).expect("failed to build Tauri application metadata");
}
