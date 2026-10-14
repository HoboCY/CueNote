//! Child process for isolated storage interruption tests. Not bundled with CueNote.
#[cfg(debug_assertions)]
fn main() {
    use cuenote_lib::workspace::Workspace;
    use std::path::PathBuf;

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(
        args.len(),
        2,
        "storage_fault_probe <isolated-root> <note-id>"
    );
    let root = PathBuf::from(&args[0]);
    assert!(root.is_absolute(), "test root must be absolute");
    let mut workspace = Workspace::new(
        root.join("settings"),
        root.join("recovery"),
        root.join("library"),
    );
    workspace.snapshot().expect("reopen isolated workspace");
    let session = workspace.open_note(&args[1]).expect("open seeded note");
    let mut text = session.text;
    text.content = "中断前请求保存的新草稿 📝".into();
    match workspace.save_note_text(&session.session_id, text) {
        Ok(receipt) => println!("{}", serde_json::to_string(&receipt).unwrap()),
        Err(error) => {
            println!("{}", serde_json::to_string(&error).unwrap());
            std::process::exit(1);
        }
    }
}

#[cfg(not(debug_assertions))]
fn main() {
    eprintln!("The storage fault probe is available in debug builds only.");
    std::process::exit(2);
}
