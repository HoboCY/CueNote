//! Process-local successes only. A fresh digest of immutable input bytes is the
//! key; no caller-supplied manifest, filesystem path or persistent cache can add
//! a success. Entries hold dimensions, never compressed bytes or pixel buffers.
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use super::MediaType;

type Key = ([u8; 32], u8);
const CAPACITY: usize = 1_024;

#[derive(Default)]
struct Successes {
    dimensions: HashMap<Key, (u32, u32)>,
    insertion_order: VecDeque<Key>,
}

fn cache() -> &'static Mutex<Successes> {
    static CACHE: OnceLock<Mutex<Successes>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Successes::default()))
}

fn key(digest: [u8; 32], media_type: MediaType) -> Key {
    (
        digest,
        match media_type {
            MediaType::Png => 0,
            MediaType::Jpeg => 1,
        },
    )
}

pub(super) fn lookup(digest: [u8; 32], media_type: MediaType) -> Option<(u32, u32)> {
    cache()
        .lock()
        .ok()?
        .dimensions
        .get(&key(digest, media_type))
        .copied()
}

/// Only call after the complete bounded decoder succeeded. Decoding takes place
/// outside the lock; a poisoned/contended cache never changes format validation.
pub(super) fn remember(digest: [u8; 32], media_type: MediaType, dimensions: (u32, u32)) {
    let Ok(mut cache) = cache().lock() else {
        return;
    };
    let key = key(digest, media_type);
    if cache.dimensions.contains_key(&key) {
        return;
    }
    if cache.dimensions.len() == CAPACITY {
        if let Some(oldest) = cache.insertion_order.pop_front() {
            cache.dimensions.remove(&oldest);
        }
    }
    cache.dimensions.insert(key, dimensions);
    cache.insertion_order.push_back(key);
}
