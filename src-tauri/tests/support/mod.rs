//! Optional root for precompiled real-device resource acceptance tests.
//! Normal cargo tests retain their existing target/testdata location.
pub fn tempdir(prefix: &str) -> tempfile::TempDir {
    let base = if let Some(root) = std::env::var_os("CUENOTE_ACCEPTANCE_TEST_ROOT") {
        let root = std::path::PathBuf::from(root);
        assert!(root.is_absolute(), "Acceptance root must be absolute");
        for ancestor in root.ancestors() {
            let metadata = std::fs::symlink_metadata(ancestor).unwrap();
            assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                assert_eq!(metadata.file_attributes() & 0x400, 0, "No reparse points");
            }
        }
        let marker = root.join(".cuenote-resource-tests");
        assert!(std::fs::symlink_metadata(&marker)
            .unwrap()
            .file_type()
            .is_file());
        assert_eq!(
            std::fs::read(marker).unwrap(),
            b"CueNote isolated resource tests v1\n"
        );
        root
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/testdata")
    };
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base)
        .unwrap()
}
