use regex::Regex;

use super::super::format::compute_file_hash;
use super::super::fs::InMemoryFilesystem;
use super::super::input::Patch;
use super::super::messages::HEADTAIL_DRIFT_WARNING;
use super::super::mismatch::MismatchError;
use super::super::patcher::{PatchOp, Patcher};
use super::super::snapshots::{InMemorySnapshotStore, InMemorySnapshotStoreOptions, SnapshotStore};
use super::super::types::SplitOptions;

const PATH: &str = "a.ts";

#[tokio::test]
async fn applies_when_section_tag_is_live_files_content_hash() {
    let fs = InMemoryFilesystem::new([(PATH.to_string(), "before\n".to_string())]);
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag = store.record(PATH, "before\n");
    let mut patcher = Patcher::new(fs, store, None);
    let patch = Patch::parse(
        &format!("¶{PATH}#{tag}\nreplace 1..1:\n+after"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await.unwrap();

    assert_eq!(result.sections[0].op, PatchOp::Update);
    assert_eq!(result.sections[0].file_hash.len(), 4);
    assert_ne!(result.sections[0].file_hash, tag);
    assert_eq!(result.sections[0].after, "after\n");
}

#[tokio::test]
async fn validates_anchor_from_content_hash_even_with_no_recorded_snapshot() {
    // The core fix: the tag fingerprints the WHOLE file. An edit anchored at
    // a line the model never saw recorded applies whenever the live file
    // still hashes to the tag — no stored snapshot is consulted.
    let content = "l1\nl2\nl3\nl4\nl5\n";
    let fs = InMemoryFilesystem::new([(PATH.to_string(), content.to_string())]);
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag = compute_file_hash(content);
    // Store is intentionally empty: byHash(tag) === null.
    assert!(store.by_hash(PATH, &tag).is_none());
    let mut patcher = Patcher::new(fs, store, None);
    let patch = Patch::parse(
        &format!("¶{PATH}#{tag}\nreplace 3..3:\n+L3"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await.unwrap();

    assert_eq!(result.sections[0].op, PatchOp::Update);
    assert_eq!(result.sections[0].after, "l1\nl2\nL3\nl4\nl5\n");
}

#[test]
fn normalizes_lowercase_section_tags_while_parsing() {
    let section = Patch::parse_single(
        &format!("¶{PATH}#1a2b\nreplace 1..1:\n+after"),
        &SplitOptions::default(),
    )
    .unwrap();

    assert_eq!(section.file_hash.unwrap(), "1A2B");
}

#[tokio::test]
async fn refuses_with_mismatch_when_recorded_version_no_longer_matches_live() {
    let fs = InMemoryFilesystem::new([(PATH.to_string(), "drifted\n".to_string())]);
    let check_fs = fs.clone();
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    // Tag was minted from "before\n" but the live file is "drifted\n".
    let tag = store.record(PATH, "before\n");
    let mut patcher = Patcher::new(fs, store, None);
    let patch = Patch::parse(
        &format!("¶{PATH}#{tag}\nreplace 1..1:\n+after"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    let mismatch = err.downcast_ref::<MismatchError>().unwrap();
    let message = mismatch.display_message();
    // Hash WAS observed for this path, so we land on the "file changed" branch.
    assert!(message.contains("file changed between read and edit"));
    assert!(message.contains("Section is bound to #"));
    // Disk untouched — refusal must never leave a partial write.
    assert_eq!(check_fs.get(PATH).unwrap(), "drifted\n");
}

#[tokio::test]
async fn refuses_with_not_from_this_session_when_tag_never_recorded_for_path() {
    let fs = InMemoryFilesystem::new([(PATH.to_string(), "current\n".to_string())]);
    let check_fs = fs.clone();
    let store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let mut patcher = Patcher::new(fs, store, None);
    // A 4-hex tag that is neither the live content hash nor a recorded
    // version — equivalent to the model fabricating it or carrying it over
    // from a prior session.
    let live = compute_file_hash("current\n");
    let bogus = if live == "FFFF" { "0000" } else { "FFFF" };
    let patch = Patch::parse(
        &format!("¶{PATH}#{bogus}\nreplace 1..1:\n+after"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    let mismatch = err.downcast_ref::<MismatchError>().unwrap();
    let message = mismatch.display_message();
    assert!(message.contains(&format!("hash #{} is not from this session", bogus)));
    assert!(message.contains("never invent the tag"));
    // Still surfaces the current hash so the model can pivot to a re-read.
    let hash_re = Regex::new(r"current file hashes to #[0-9A-F]{4}").unwrap();
    assert!(hash_re.is_match(message));
    assert_eq!(check_fs.get(PATH).unwrap(), "current\n");
}

#[tokio::test]
async fn rejects_hashless_head_tail_insert_tag_required_on_every_section() {
    let fs = InMemoryFilesystem::new([(PATH.to_string(), "a\nb\n".to_string())]);
    let check_fs = fs.clone();
    let store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let mut patcher = Patcher::new(fs, store, None);
    let patch = Patch::parse(
        &format!("¶{PATH}\ninsert tail:\n+c"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await;
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("Missing hashline snapshot tag"));
    assert!(msg.contains("use the write tool"));
    assert_eq!(check_fs.get(PATH).unwrap(), "a\nb\n");
}

#[tokio::test]
async fn still_hard_rejects_anchored_edit_that_omits_snapshot_tag() {
    let fs = InMemoryFilesystem::new([(PATH.to_string(), "a\nb\n".to_string())]);
    let store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let mut patcher = Patcher::new(fs, store, None);
    let patch = Patch::parse(
        &format!("¶{PATH}\nreplace 1..1:\n+X"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await;
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("Missing hashline snapshot tag"));
}

#[tokio::test]
async fn rejects_tagged_edit_whose_target_file_does_not_exist() {
    let fs = InMemoryFilesystem::new([]);
    let store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let mut patcher = Patcher::new(fs, store, None);
    let patch = Patch::parse("¶ghost.ts#1A2B\ninsert tail:\n+c", &SplitOptions::default()).unwrap();
    let result = patcher.apply(&patch).await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(err_msg.contains("File not found"));
    assert!(err_msg.contains("Use the write tool"));
}

#[tokio::test]
async fn applies_head_tail_insert_with_stale_tag_and_warns_instead_of_hard_failing() {
    let content = "a\nb\n";
    let fs = InMemoryFilesystem::new([(PATH.to_string(), content.to_string())]);
    let store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let live = compute_file_hash(content);
    let stale = if live == "0000" { "FFFF" } else { "0000" };
    let mut patcher = Patcher::new(fs, store, None);
    let patch = Patch::parse(
        &format!("¶{PATH}#{stale}\ninsert tail:\n+c"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await.unwrap();

    let section = &result.sections[0];
    assert_eq!(section.op, PatchOp::Update);
    assert_eq!(section.after, "a\nb\nc\n");
    assert!(
        section
            .warnings
            .contains(&HEADTAIL_DRIFT_WARNING.to_string())
    );
}

#[tokio::test]
async fn does_not_warn_when_head_tail_insert_carries_live_tag() {
    let content = "a\nb\n";
    let fs = InMemoryFilesystem::new([(PATH.to_string(), content.to_string())]);
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let tag = store.record(PATH, content);
    let mut patcher = Patcher::new(fs, store, None);
    let patch = Patch::parse(
        &format!("¶{PATH}#{tag}\ninsert tail:\n+c"),
        &SplitOptions::default(),
    )
    .unwrap();
    let result = patcher.apply(&patch).await.unwrap();

    let section = &result.sections[0];
    assert_eq!(section.op, PatchOp::Update);
    assert!(
        !section
            .warnings
            .contains(&HEADTAIL_DRIFT_WARNING.to_string())
    );
}
