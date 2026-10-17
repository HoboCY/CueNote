//! Read-only verification of the exact edits made by the native Q51 matrix.
use std::{collections::BTreeSet, error::Error, fs, io::Cursor, path::Path};

use cuenote_lib::note_format::{read_package, Block, Inline, NotePackage};
use serde_json::{json, Value};

use crate::manifest::{self, CorpusManifest};

const FROZEN: &str = "0e95e7288b26e37248899dedd09fe5c311866febc3d94d72f550bffbc28b0292";

fn compare(
    original: &NotePackage,
    edited: &NotePackage,
    title: &str,
    heading: &str,
) -> Result<(), Box<dyn Error>> {
    let mut expected = original.clone();
    expected.document.title = title.to_owned();
    let Some(Block::Heading1 { children }) = expected.document.sections.content.first_mut() else {
        return Err("Frozen note has no first heading".into());
    };
    *children = vec![Inline::Text {
        text: heading.to_owned(),
        marks: vec![],
    }];
    // Only the content timestamp may advance. Identity, organization, all
    // other text/formatting, formulas, maps and every asset byte stay exact.
    let before =
        chrono::DateTime::parse_from_rfc3339(&original.document.timestamps.content_modified_at)?;
    let after =
        chrono::DateTime::parse_from_rfc3339(&edited.document.timestamps.content_modified_at)?;
    if after <= before {
        return Err("Edited content timestamp did not advance".into());
    }
    expected.document.timestamps.content_modified_at =
        edited.document.timestamps.content_modified_at.clone();
    if &expected != edited {
        return Err(
            "Saved package differs beyond the intended title, heading and content timestamp".into(),
        );
    }
    Ok(())
}

pub fn run(corpus: &Path, edited_root: &Path) -> Result<Value, Box<dyn Error>> {
    if manifest::sha256_file(&corpus.join("manifest.json"))?.0 != FROZEN {
        return Err("Only the frozen full Q51 corpus is accepted".into());
    }
    let manifest: CorpusManifest =
        serde_json::from_slice(&fs::read(corpus.join("manifest.json"))?)?;
    let notes: Vec<_> = manifest
        .notes
        .iter()
        .filter(|note| note.collection == "library")
        .collect();
    let ordinary = notes
        .iter()
        .find(|note| note.profile == "ordinary")
        .ok_or("Missing ordinary note")?;
    let stress = notes
        .iter()
        .find(|note| note.profile == "stress")
        .ok_or("Missing stress note")?;
    let actual = manifest::walk_files(&edited_root.join("library/notes"))?;
    let expected: BTreeSet<_> = notes
        .iter()
        .map(|note| note.path.strip_prefix("library/notes/").unwrap().to_owned())
        .collect();
    if actual
        .iter()
        .map(|f| f.path.clone())
        .collect::<BTreeSet<_>>()
        != expected
    {
        return Err("Saved library note file set differs".into());
    }
    let mut reports = vec![];
    for note in &notes {
        let source = corpus.join(&note.path);
        if manifest::sha256_file(&source)? != (note.sha256.clone(), note.bytes) {
            return Err(format!("Frozen source changed: {}", note.path).into());
        }
        let target = edited_root.join(&note.path);
        let (sha, bytes) = manifest::sha256_file(&target)?;
        let intended = if note.note_id == ordinary.note_id {
            Some(("普通样本 32 · SHARED-Q51-ORDINARY", "内容区 · 普通样本 00"))
        } else if note.note_id == stress.note_id {
            Some(("压力样本 42 · SHARED-Q51-STRESS", "内容区 · 压力样本 00"))
        } else {
            None
        };
        if let Some((title, heading)) = intended {
            let original = read_package(Cursor::new(fs::read(source)?))?;
            let edited = read_package(Cursor::new(fs::read(&target)?))?;
            compare(&original, &edited, title, heading)?;
            reports.push(json!({"noteId":note.note_id,"title":title,"heading":heading,
                "sha256":sha,"bytes":bytes,"assets":edited.assets.iter().map(|(name,data)|
                    json!({"name":name,"sha256":manifest::sha256_bytes(data),"bytes":data.len()})).collect::<Vec<_>>(),
                "allOtherContentAndIdentityExact":true}));
        } else if sha != note.sha256 || bytes != note.bytes {
            return Err(format!("Untouched note changed: {}", note.path).into());
        }
    }
    for notebook in &manifest.notebooks {
        if manifest::sha256_file(&edited_root.join(&notebook.path))?
            != (notebook.sha256.clone(), notebook.bytes)
        {
            return Err("Notebook metadata changed unexpectedly".into());
        }
    }
    Ok(
        json!({"manifestSha256":FROZEN,"notesChecked":notes.len(),"notebooksChecked":manifest.notebooks.len(),
        "editedNotes":reports,"scope":"Exact native package content after all normal exits/restarts"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenote_lib::note_format::PlainText;

    fn pair() -> (NotePackage, NotePackage) {
        let mut original = NotePackage::new_plain(
            "1d2b3753-c04c-43a8-aa75-17fe328c0bdd".into(),
            PlainText {
                title: "before".into(),
                cue: "cue".into(),
                content: "".into(),
                summary: "summary".into(),
            },
        );
        original.document.sections.content = vec![
            Block::Heading1 {
                children: vec![Inline::Text {
                    text: "heading".into(),
                    marks: vec![],
                }],
            },
            Block::CodeBlock {
                language: Some("rust".into()),
                code: "fn main() {}\n".into(),
            },
            Block::MathBlock {
                latex: r"\frac{x}{y}".into(),
            },
        ];
        original.document.timestamps.content_modified_at = "2020-01-01T00:00:00.000Z".into();
        let mut edited = original.clone();
        edited.document.title = "after".into();
        edited.document.sections.content[0] = Block::Heading1 {
            children: vec![Inline::Text {
                text: "new heading".into(),
                marks: vec![],
            }],
        };
        edited.document.timestamps.content_modified_at = "2021-01-01T00:00:00.000Z".into();
        (original, edited)
    }
    #[test]
    fn accepts_only_the_recorded_edits() {
        let (original, edited) = pair();
        compare(&original, &edited, "after", "new heading").unwrap();
        assert!(compare(&original, &edited, "different", "new heading").is_err());
    }
    #[test]
    fn rejects_lost_code_or_formula() {
        for index in [1, 2] {
            let (original, mut edited) = pair();
            edited.document.sections.content.remove(index);
            assert!(compare(&original, &edited, "after", "new heading").is_err());
        }
    }
    #[test]
    fn rejects_asset_or_identity_changes() {
        let (original, mut edited) = pair();
        edited.assets.insert("changed".into(), vec![1, 2, 3]);
        assert!(compare(&original, &edited, "after", "new heading").is_err());
        let (original, mut edited) = pair();
        edited.document.organization.pinned = true;
        assert!(compare(&original, &edited, "after", "new heading").is_err());
    }
}
