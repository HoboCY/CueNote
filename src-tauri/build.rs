fn main() {
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "get_app_version",
            "get_workspace",
            "use_default_library",
            "choose_library",
            "open_library_folder",
            "new_note",
            "open_note",
            "save_note_text",
        ]));

    tauri_build::try_build(attributes).expect("failed to build Tauri application metadata");
}
