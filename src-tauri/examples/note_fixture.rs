//! Exercise an external writer against isolated desktop verification files.
//! This example is not included in the desktop application or its IPC commands.
use std::{env, error::Error, io::Cursor, path::PathBuf};

use cuenote_lib::{note_format, storage};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() < 2 {
        return Err(
            "usage: note_fixture <inspect|replace-text|replace-rich> <note-path> [recovery-dir]"
                .into(),
        );
    }
    let operation = arguments[0].to_str().ok_or("invalid operation")?;
    let path = PathBuf::from(&arguments[1]);
    let mut opened = storage::open_note(&path)?;
    if operation != "inspect" {
        let recovery = PathBuf::from(arguments.get(2).ok_or("recovery-dir is required")?);
        match operation {
            "replace-text" => {
                let mut text = opened.package.plain_text().ok_or("plain note required")?;
                text.content = "来自另一个进程的版本：请保留这段文字。\nExternal version 🧭".into();
                opened.package.replace_plain_text(text)?;
            }
            "replace-rich" => {
                let rich = note_format::read_package(Cursor::new(include_bytes!(
                    "../tests/fixtures/valid-rich-v1.cuenote"
                )))?;
                opened.package.document.sections = rich.document.sections;
                opened.package.document.timestamps.content_modified_at =
                    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                opened.package.manifest = rich.manifest;
                opened.package.assets = rich.assets;
            }
            _ => return Err("unsupported operation".into()),
        }
        storage::save_note(&path, &recovery, &opened.package, Some(&opened.revision))?;
        opened = storage::open_note(&path)?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "noteId": opened.package.document.note_id,
            "organization": opened.package.document.organization,
            "timestamps": opened.package.document.timestamps,
            "revision": opened.revision,
            "text": opened.package.text_projection(),
            "document": opened.package.document,
            "plainEditable": opened.package.plain_text().is_some(),
            "assetCount": opened.package.assets.len(),
        }))?
    );
    Ok(())
}
