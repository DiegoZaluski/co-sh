use std::num::NonZeroUsize;

use regex::Regex;

use super::super::format::compute_file_hash;
use super::super::snapshots::{InMemorySnapshotStore, InMemorySnapshotStoreOptions, SnapshotStore};

const PATH: &str = "/tmp/__hashline-snapshots__.ts";
const OTHER: &str = "/tmp/__hashline-other__.ts";
const TAG_RE: &str = r"^[0-9A-F]{4}$";

#[test]
fn derives_tag_from_whole_file_content_matches_compute_file_hash() {
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let text = "L1\nL2\nL3\n";
    let tag = store.record(PATH, text);
    let tag_re = Regex::new(TAG_RE).unwrap();
    assert!(tag_re.is_match(&tag));
    assert_eq!(tag, compute_file_hash(text));
}

#[test]
fn fuses_repeated_reads_of_identical_content_onto_one_tag() {
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let text = "alpha\nbeta\ngamma\n";
    let first = store.record(PATH, text);
    let second = store.record(PATH, text);
    assert_eq!(second, first);
    // One head, byHash resolves to the same full text.
    assert_eq!(store.head(PATH).unwrap().hash, first);
    assert_eq!(store.by_hash(PATH, &first).unwrap().text, text);
}

#[test]
fn mints_new_tag_when_content_changes_and_retains_prior_version() {
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let v1 = "one\ntwo\n";
    let v2 = "one\ntwo\nthree\n";
    let tag1 = store.record(PATH, v1);
    let tag2 = store.record(PATH, v2);
    assert_ne!(tag2, tag1);
    // Head is the latest; the older version is still resolvable by its tag.
    assert_eq!(store.head(PATH).unwrap().hash, tag2);
    assert_eq!(store.by_hash(PATH, &tag1).unwrap().text, v1);
    assert_eq!(store.by_hash(PATH, &tag2).unwrap().text, v2);
}

#[test]
fn promotes_re_observed_older_version_back_to_head() {
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let v1 = "x\n";
    let v2 = "y\n";
    let tag1 = store.record(PATH, v1);
    store.record(PATH, v2);
    // File reverts to v1 content: recording it again makes v1 the head.
    assert_eq!(store.record(PATH, v1), tag1);
    assert_eq!(store.head(PATH).unwrap().hash, tag1);
}

#[test]
fn bounds_per_path_history_to_max_versions_per_path() {
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions {
        max_versions_per_path: Some(2),
        ..Default::default()
    });
    let tag_a = store.record(PATH, "A\n");
    let tag_b = store.record(PATH, "B\n");
    let tag_c = store.record(PATH, "C\n");
    // Only the two newest versions survive.
    assert_eq!(store.by_hash(PATH, &tag_c).unwrap().text, "C\n");
    assert_eq!(store.by_hash(PATH, &tag_b).unwrap().text, "B\n");
    assert!(store.by_hash(PATH, &tag_a).is_none());
}

#[test]
fn bounds_tracked_paths_to_max_paths() {
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions {
        max_paths: NonZeroUsize::new(1),
        ..Default::default()
    });
    let tag = store.record(PATH, "first\n");
    store.record(OTHER, "second\n");
    // Recording OTHER evicted PATH from the LRU.
    assert!(store.by_hash(PATH, &tag).is_none());
    assert!(store.head(PATH).is_none());
}

#[test]
fn rejects_cross_path_lookups() {
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag = store.record(PATH, "shared\n");
    assert!(store.by_hash(OTHER, &tag).is_none());
}

#[test]
fn invalidate_drops_one_path_clear_drops_everything() {
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag_a = store.record(PATH, "A\n");
    let tag_b = store.record(OTHER, "B\n");
    store.invalidate(PATH);
    assert!(store.by_hash(PATH, &tag_a).is_none());
    assert_eq!(store.by_hash(OTHER, &tag_b).unwrap().text, "B\n");
    store.clear();
    assert!(store.by_hash(OTHER, &tag_b).is_none());
}
