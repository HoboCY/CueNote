//! Deterministic Q51 corpus model: fixed scales, stable identities, timestamps and
//! the known search-query -> note-id expectations.
//!
//! Everything here is a pure function of the recorded scale, so `generate` and
//! `verify` derive the same identities without trusting the corpus on disk.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const TEXT_CHARS: usize = 2_000;
pub const ORDINARY_CHARS: usize = 5_000;
pub const STRESS_CHARS: usize = 50_000;

pub const ORDINARY_IMAGES: usize = 5;
pub const ORDINARY_IMAGE_SIDE: u32 = 1_024;
pub const ORDINARY_ASSET_BYTES: u64 = 5 * 1024 * 1024;

pub const STRESS_IMAGES: usize = 10;
pub const STRESS_IMAGE_SIDE: u32 = 2_048;
pub const STRESS_ASSET_BYTES: u64 = 50 * 1024 * 1024;

pub const ORDINARY_MAP_NODES: usize = 50;
pub const STRESS_MAP_NODES: usize = 500;

/// Balanced maps stay far below this bound; a 500-child root would violate it.
pub const MAX_ROOT_CHILDREN: usize = 32;

pub const MARKER_PATH: &str = ".cuenote-performance";
pub const MARKER_BYTES: &[u8] = b"CueNote isolated performance root v1\n";
pub const LIBRARY_DIR: &str = "library";
pub const SAMPLES_DIR: &str = "samples";
pub const MANIFEST_PATH: &str = "manifest.json";

/// Standalone sample paths in `sample_notes()` order (ordinary, stress).
pub const SAMPLE_PATHS: [&str; 2] = ["samples/ordinary.cuenote", "samples/stress.cuenote"];

pub const ASTRAL_QUERY: &str = "🜁AST-EMOJI🜁";

/// Fixed epoch used for every package timestamp (`2026-01-05T08:00:00.000Z`).
pub const TIMESTAMP_BASE_SECONDS: i64 = 1_767_600_000;
pub const TIMESTAMP_STEP_SECONDS: i64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    pub text_notes: usize,
    pub ordinary_notes: usize,
    pub stress_notes: usize,
    pub notebooks: usize,
    pub name: &'static str,
}

pub const FULL: Scale = Scale {
    text_notes: 900,
    ordinary_notes: 90,
    stress_notes: 10,
    notebooks: 10,
    name: "full",
};

pub const SMALL: Scale = Scale {
    text_notes: 7,
    ordinary_notes: 2,
    stress_notes: 1,
    notebooks: 10,
    name: "small",
};

impl Scale {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "full" => Some(FULL),
            "small" => Some(SMALL),
            _ => None,
        }
    }

    pub fn library_notes(&self) -> usize {
        self.text_notes + self.ordinary_notes + self.stress_notes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Text,
    Ordinary,
    Stress,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Text => "text",
            Profile::Ordinary => "ordinary",
            Profile::Stress => "stress",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Profile::Text),
            "ordinary" => Some(Profile::Ordinary),
            "stress" => Some(Profile::Stress),
            _ => None,
        }
    }

    pub fn expected_chars(self) -> usize {
        match self {
            Profile::Text => TEXT_CHARS,
            Profile::Ordinary => ORDINARY_CHARS,
            Profile::Stress => STRESS_CHARS,
        }
    }

    pub fn image_count(self) -> usize {
        match self {
            Profile::Text => 0,
            Profile::Ordinary => ORDINARY_IMAGES,
            Profile::Stress => STRESS_IMAGES,
        }
    }

    pub fn image_side(self) -> u32 {
        match self {
            Profile::Text => 0,
            Profile::Ordinary => ORDINARY_IMAGE_SIDE,
            Profile::Stress => STRESS_IMAGE_SIDE,
        }
    }

    pub fn asset_target_bytes(self) -> u64 {
        match self {
            Profile::Text => 0,
            Profile::Ordinary => ORDINARY_ASSET_BYTES,
            Profile::Stress => STRESS_ASSET_BYTES,
        }
    }

    pub fn map_nodes(self) -> usize {
        match self {
            Profile::Text => 0,
            Profile::Ordinary => ORDINARY_MAP_NODES,
            Profile::Stress => STRESS_MAP_NODES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Collection {
    Library,
    Sample,
}

impl Collection {
    pub fn as_str(self) -> &'static str {
        match self {
            Collection::Library => "library",
            Collection::Sample => "sample",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteRef {
    pub profile: Profile,
    pub index: usize,
    pub collection: Collection,
    pub note_id: String,
    pub unique_token: String,
}

fn note_domain(profile: Profile, collection: Collection) -> &'static str {
    match (collection, profile) {
        (Collection::Library, Profile::Text) => "q51-library-text-note",
        (Collection::Library, Profile::Ordinary) => "q51-library-ordinary-note",
        (Collection::Library, Profile::Stress) => "q51-library-stress-note",
        (Collection::Sample, Profile::Ordinary) => "q51-sample-ordinary-note",
        (Collection::Sample, Profile::Stress) => "q51-sample-stress-note",
        (Collection::Sample, Profile::Text) => "q51-sample-text-note",
    }
}

fn unique_token(profile: Profile, collection: Collection, index: usize) -> String {
    match collection {
        Collection::Library => match profile {
            Profile::Text => format!("UNIQ-PLAIN-{index:04}"),
            Profile::Ordinary => format!("UNIQ-ORD-{index:03}"),
            Profile::Stress => format!("UNIQ-STR-{index:02}"),
        },
        Collection::Sample => match profile {
            Profile::Ordinary => "UNIQ-SAMPLE-ORD".to_owned(),
            Profile::Stress => "UNIQ-SAMPLE-STR".to_owned(),
            Profile::Text => format!("UNIQ-SAMPLE-TXT-{index:04}"),
        },
    }
}

pub fn note_ref(profile: Profile, index: usize, collection: Collection) -> NoteRef {
    NoteRef {
        profile,
        index,
        collection,
        note_id: stable_id(note_domain(profile, collection), index as u64),
        unique_token: unique_token(profile, collection, index),
    }
}

/// Library notes in generation order: text, ordinary, stress.
pub fn library_notes(scale: &Scale) -> Vec<NoteRef> {
    let mut notes = Vec::with_capacity(scale.library_notes());
    for index in 0..scale.text_notes {
        notes.push(note_ref(Profile::Text, index, Collection::Library));
    }
    for index in 0..scale.ordinary_notes {
        notes.push(note_ref(Profile::Ordinary, index, Collection::Library));
    }
    for index in 0..scale.stress_notes {
        notes.push(note_ref(Profile::Stress, index, Collection::Library));
    }
    notes
}

/// The two standalone samples are independent identities of their own.
pub fn sample_notes() -> Vec<NoteRef> {
    vec![
        note_ref(Profile::Ordinary, 0, Collection::Sample),
        note_ref(Profile::Stress, 0, Collection::Sample),
    ]
}

/// Serial used for timestamps; library notes keep their generation position and
/// samples follow the full library.
pub fn note_serial(scale: &Scale, note: &NoteRef) -> usize {
    match note.collection {
        Collection::Library => library_notes(scale)
            .iter()
            .position(|candidate| candidate.note_id == note.note_id)
            .unwrap_or_else(|| note.index),
        Collection::Sample => {
            scale.library_notes()
                + match note.profile {
                    Profile::Ordinary => 0,
                    _ => 1,
                }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotebookRef {
    pub index: usize,
    pub notebook_id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn notebooks(scale: &Scale) -> Vec<NotebookRef> {
    (0..scale.notebooks)
        .map(|index| NotebookRef {
            index,
            notebook_id: stable_id("q51-notebook", index as u64),
            name: if index == 0 {
                "默认笔记本".to_owned()
            } else {
                format!("性能样本 {index:02}")
            },
            is_default: index == 0,
        })
        .collect()
}

/// Notebook assigned to a library note: round-robin over generation order.
pub fn notebook_index(scale: &Scale, serial: usize) -> usize {
    serial % scale.notebooks
}

/// `sha256(domain || 0x00 || index)` with UUIDv4 version/variant bits forced.
pub fn stable_id(domain: &str, index: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0u8]);
    hasher.update(index.to_be_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format_uuid(&bytes)
}

pub fn derived_id(domain: &str, parent: &str, index: u64) -> String {
    stable_id(&format!("{domain}:{parent}"), index)
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    let mut value = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            value.push('-');
        }
        value.push_str(&format!("{byte:02x}"));
    }
    value
}

pub fn timestamp_for(step_seconds: i64) -> String {
    let seconds = TIMESTAMP_BASE_SECONDS + step_seconds;
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
        .expect("fixed corpus timestamp is in range")
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// Returns (createdAt, contentModifiedAt, organizationModifiedAt).
pub fn note_timestamps(serial: usize) -> (String, String, String) {
    let step = serial as i64 * TIMESTAMP_STEP_SECONDS;
    (
        timestamp_for(step),
        timestamp_for(step + 30),
        timestamp_for(step + 60),
    )
}

pub fn notebook_timestamps(index: usize) -> (String, String) {
    let step = index as i64 * TIMESTAMP_STEP_SECONDS;
    (timestamp_for(-3_600 + step), timestamp_for(-1_800 + step))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedQuery {
    pub query: String,
    pub note_ids: Vec<String>,
}

/// Known search queries and their exact library-note hit sets. Samples have their
/// own unique tokens and are not part of the shared-token sets.
pub fn expected_queries(scale: &Scale) -> Vec<ExpectedQuery> {
    let notes = library_notes(scale);
    let ids = |profile: Profile| -> Vec<String> {
        notes
            .iter()
            .filter(|note| note.profile == profile)
            .map(|note| note.note_id.clone())
            .collect()
    };

    let mut queries: Vec<ExpectedQuery> = notes
        .iter()
        .map(|note| ExpectedQuery {
            query: note.unique_token.clone(),
            note_ids: vec![note.note_id.clone()],
        })
        .collect();

    queries.push(ExpectedQuery {
        query: "SHARED-Q51-PLAIN".to_owned(),
        note_ids: ids(Profile::Text),
    });
    queries.push(ExpectedQuery {
        query: "SHARED-Q51-ORDINARY".to_owned(),
        note_ids: ids(Profile::Ordinary),
    });
    queries.push(ExpectedQuery {
        query: "SHARED-Q51-STRESS".to_owned(),
        note_ids: ids(Profile::Stress),
    });
    // Lower-case variant documents the ASCII case-insensitive matching contract.
    queries.push(ExpectedQuery {
        query: "shared-q51-ordinary".to_owned(),
        note_ids: ids(Profile::Ordinary),
    });

    let astral: Vec<String> = notes
        .iter()
        .filter(|note| note.index % 10 == 0)
        .map(|note| note.note_id.clone())
        .collect();
    queries.push(ExpectedQuery {
        query: ASTRAL_QUERY.to_owned(),
        note_ids: astral,
    });

    for note in sample_notes() {
        queries.push(ExpectedQuery {
            query: note.unique_token.clone(),
            note_ids: vec![note.note_id.clone()],
        });
    }

    for query in &mut queries {
        query.note_ids.sort();
        query.note_ids.dedup();
    }
    queries.sort_by(|left, right| left.query.cmp(&right.query));
    queries
}

/// `astral` token appears exactly in notes whose index is divisible by ten.
pub fn uses_astral_token(note: &NoteRef) -> bool {
    note.collection == Collection::Library && note.index % 10 == 0
}

pub fn shared_token(profile: Profile) -> &'static str {
    match profile {
        Profile::Text => "SHARED-Q51-PLAIN",
        Profile::Ordinary => "SHARED-Q51-ORDINARY",
        Profile::Stress => "SHARED-Q51-STRESS",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapShape {
    pub root_children: usize,
    pub base_leaves: usize,
    pub extra_leaves: usize,
}

impl MapShape {
    pub fn node_count(&self) -> usize {
        1 + self.root_children + self.root_children * self.base_leaves + self.extra_leaves
    }
}

/// Balanced few-level shape: root -> k branches -> leaves. The root never owns
/// every node, unlike a 500-root-child map.
pub fn map_shape(node_count: usize) -> MapShape {
    let mut k = ((node_count as f64).sqrt().round() as usize).max(2);
    while 1 + k + k * (k - 1) > node_count && k > 2 {
        k -= 1;
    }
    let base_total = 1 + k + k * (k - 1);
    let extra = node_count.saturating_sub(base_total);
    MapShape {
        root_children: k,
        base_leaves: k - 1,
        extra_leaves: extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_ids_are_uuid_v4_and_unique() {
        let first = stable_id("q51-library-text-note", 0);
        let second = stable_id("q51-library-text-note", 1);
        assert_ne!(first, second);
        let parsed = uuid::Uuid::parse_str(&first).expect("parse stable id");
        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(parsed.to_string(), first);
    }

    #[test]
    fn map_shapes_match_the_fixed_profiles() {
        assert_eq!(
            map_shape(50),
            MapShape {
                root_children: 7,
                base_leaves: 6,
                extra_leaves: 0
            }
        );
        assert_eq!(map_shape(50).node_count(), 50);
        assert_eq!(
            map_shape(500),
            MapShape {
                root_children: 22,
                base_leaves: 21,
                extra_leaves: 15
            }
        );
        assert_eq!(map_shape(500).node_count(), 500);
        assert!(map_shape(500).extra_leaves <= map_shape(500).root_children);
        assert!(map_shape(500).root_children <= MAX_ROOT_CHILDREN);
    }

    #[test]
    fn query_expectations_are_sorted_and_self_consistent() {
        let queries = expected_queries(&SMALL);
        assert!(queries.windows(2).all(|pair| pair[0].query < pair[1].query));
        let unique = queries
            .iter()
            .find(|query| query.query == "UNIQ-ORD-000")
            .expect("unique token query");
        assert_eq!(unique.note_ids.len(), 1);
        let plain = queries
            .iter()
            .find(|query| query.query == "SHARED-Q51-PLAIN")
            .expect("plain shared query");
        assert_eq!(plain.note_ids.len(), SMALL.text_notes);
    }
}
