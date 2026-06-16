//! corruption window: when a prior in-session edit rewrote the line a
//! later stale-hash edit re-targets, replaying onto current must refuse
//! (the model is anchored against content that no longer exists), not
//! silently overwrite the new content with the stale-authored payload.
//!
//! Companion positive case: when the prior edit changed lines elsewhere
//! but left the re-targeted line alone, replay must still succeed and
//! surface the standard session-chain banner.
use super::super::messages::RECOVERY_SESSION_REPLAY_WARNING;
use super::super::parser::parse_patch;
use super::super::recovery::{Recovery, RecoveryArgs};
use super::super::snapshots::{InMemorySnapshotStore, InMemorySnapshotStoreOptions, SnapshotStore};

const PATH: &str = "/tmp/__hashline-recovery-session-chain__.ts";

struct SeedTwoSnapshots {
    store: InMemorySnapshotStore,
    #[allow(dead_code)]
    v0_text: String,
    v1_text: String,
    h0: String,
    #[allow(dead_code)]
    h1: String,
}

fn seed_two_snapshots() -> SeedTwoSnapshots {
    let mut store = InMemorySnapshotStore::new(&InMemorySnapshotStoreOptions::default());
    let v0_lines = ["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10"];
    let mut v1_lines = v0_lines;
    v1_lines[4] = "L5-CHANGED";
    let v0_text = format!("{}\n", v0_lines.join("\n"));
    let v1_text = format!("{}\n", v1_lines.join("\n"));
    let h0 = store.record(PATH, &v0_text);
    let h1 = store.record(PATH, &v1_text);
    SeedTwoSnapshots {
        store,
        v0_text,
        v1_text,
        h0,
        h1,
    }
}

#[test]
fn refuses_replay_when_edit_anchor_line_content_diverges() {
    let SeedTwoSnapshots {
        store, v1_text, h0, ..
    } = seed_two_snapshots();
    // Edit anchored at line 5 — the exact line the prior in-session edit
    // rewrote. Replaying onto current would overwrite "L5-CHANGED" with
    // payload the model authored against the stale "L5". That is
    // corruption, not recovery.
    let (edits, _) = parse_patch("replace 5..5:\n|L5-MODEL").unwrap();

    let mut recovery = Recovery::new(store);
    let recovered = recovery.try_recover(&RecoveryArgs {
        path: PATH.to_string(),
        current_text: v1_text,
        file_hash: h0,
        edits,
    });

    assert!(recovered.is_none());
}

#[test]
fn replays_edits_onto_current_when_every_anchor_line_is_unchanged() {
    let SeedTwoSnapshots {
        store, v1_text, h0, ..
    } = seed_two_snapshots();
    // Edit anchored at line 3 — unchanged between v0 and v1. The 3-way
    // merge fails (patch context includes the rewritten line 5), but the
    // replay fallback is safe because the model's anchor still names the
    // same logical content.
    let (edits, _) = parse_patch("replace 3..3:\n|L3-MODEL").unwrap();

    let mut recovery = Recovery::new(store);
    let recovered = recovery.try_recover(&RecoveryArgs {
        path: PATH.to_string(),
        current_text: v1_text.clone(),
        file_hash: h0,
        edits,
    });

    assert!(recovered.is_some());
    let recovered = recovered.unwrap();
    assert!(recovered.text.contains("L3-MODEL"));
    // Prior in-session change must survive — the model's edit lands on
    // top of current, not on top of the stale snapshot.
    assert!(recovered.text.contains("L5-CHANGED"));
    // The replay path is the less-certain recovery mode (a coincidental
    // insert+delete pair earlier in the chain could leave indices
    // pointing at duplicated rows even with both guards satisfied), so
    // the dedicated REPLAY warning surfaces a "verify the diff" hedge.
    assert!(recovered
        .warnings
        .contains(&RECOVERY_SESSION_REPLAY_WARNING.to_string()));
}
