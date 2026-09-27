//! Ported test: the published-repos half of the `resolve_revision` section of
//! `tests/test_revisions.py` — the pins are data of these checkpoints, so the
//! test travels with them.

use crate::hub::resolve_revision;
use crate::laya::checkpoints::PINNED_REVISIONS;

#[test]
fn published_repos_keep_the_hub_default_without_an_explicit_pin() {
    for (repo, _) in PINNED_REVISIONS {
        assert_eq!(resolve_revision(repo, None), None);
    }
}
