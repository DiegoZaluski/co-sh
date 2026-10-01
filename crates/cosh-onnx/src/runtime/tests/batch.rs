//! Ported tests: the collate_items section of `tests/test_training.py` from
//! the laya repository. The training-only `target`/`label` cases are omitted:
//! model training is out of scope and those fields do not exist on the
//! inference-path item.

use crate::runtime::batch::{CollateItem, collate_items};

#[test]
fn collate_pads_a_batch() {
    let items = vec![
        CollateItem {
            ids: vec![1, 2, 3],
            markers: vec![1, 2],
            qtype: 0,
        },
        CollateItem {
            ids: vec![4, 5],
            markers: vec![1],
            qtype: 2,
        },
    ];
    let b = collate_items(&[items], 0).unwrap();
    assert_eq!(b.n_rows, 2, "collate/batch size");
    assert_eq!(b.width, 3, "collate/batch size (width)");
    assert_eq!(b.kmax, 2, "collate/marker columns = longest marker list");
    assert_eq!(
        &b.input_ids[3..],
        &[4, 5, 0],
        "collate/padding filled with pad_id"
    );
    assert_eq!(
        b.attention_row(1),
        &[1, 1, 0],
        "collate/attention mask marks real tokens"
    );
    assert_eq!(
        b.marker_row(1),
        &[true, false],
        "collate/marker mask marks real markers"
    );
    assert_eq!(b.qtype, vec![0, 2], "collate/qtype carried");
}

#[test]
fn collate_empty_batch_returns_none() {
    // collate/empty batch returns None
    assert!(collate_items(&[], 0).is_none());
}
