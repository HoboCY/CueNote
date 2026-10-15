//! Child process for isolated storage interruption tests. Not bundled with CueNote.
#[cfg(debug_assertions)]
fn main() {
    use cuenote_lib::note_format::{Block, Inline};
    use cuenote_lib::workspace::Workspace;
    use std::path::PathBuf;

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    assert_eq!(
        args.len() == 2 || args.len() == 5,
        true,
        "storage_fault_probe <isolated-root> <note-id> [<content> <ready-file> <start-gate>]"
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
    let content_text = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "中断前请求保存的新草稿 📝".into());
    let mut content = session.content;
    content.sections.content = vec![Block::Paragraph {
        children: vec![Inline::Text {
            text: content_text,
            marks: Vec::new(),
        }],
    }];
    if args.len() == 5 {
        let ready = PathBuf::from(&args[3]);
        let gate = PathBuf::from(&args[4]);
        std::fs::write(ready, b"opened\n").expect("signal note opened");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !gate.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            gate.is_file(),
            "timed out waiting for cooperative start gate"
        );
    }
    match workspace.save_note_content(&session.session_id, content) {
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
