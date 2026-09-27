//! Batch collation: pad a batch of encoded questions into the model inputs.
//!
//! The padding shape — `input_ids` padded, `attention_mask` over real tokens,
//! marker positions zero-filled with a `marker_mask` — is the input contract
//! the crate's decision models build on. The training-only `target`/`label`
//! fields of the upstream function are omitted: model training is out of
//! scope for this port.



/// One encoded row handed to [`collate_items`]: token ids, marker positions
/// and the question type code.
#[derive(Debug, Clone)]
pub struct CollateItem {
    pub ids: Vec<u32>,
    pub markers: Vec<usize>,
    pub qtype: u8,
}
/// `collate_items` for the inference path: pad a batch of items into the four
/// model inputs.
///
/// The upstream function also carries `target` / `label` / `meta` fields for
/// the training loop; agent training is out of scope for this port, so those
/// fields are omitted and the marker-count overflow check they motivated
/// (`#311`) does not apply.
///
/// Padding shape: `input_ids` filled with `pad_id`, `attention_mask` 1 over
/// real tokens, `marker_pos` zero-filled to the widest marker count and
/// `marker_mask` true only over a row's own markers — masked marker logits are
/// read only through `marker_mask`.
#[derive(Debug, Clone)]
pub struct CollatedBatch {
    /// `[n, L]`, `pad_id` padded.
    pub input_ids: Vec<Vec<u32>>,
    /// `[n, L]`, 1 over real tokens.
    pub attention_mask: Vec<Vec<u32>>,
    /// `[n, kmax]`, zero-padded marker positions.
    pub marker_pos: Vec<Vec<u32>>,
    /// `[n, kmax]`, true over a row's own markers.
    pub marker_mask: Vec<Vec<bool>>,
    /// `[n]`, question type per row.
    pub qtype: Vec<u8>,
}
/// `collate_items(batch, pad_id)`; `None` on an empty batch, as upstream.
pub fn collate_items(batch: &[Vec<CollateItem>], pad_id: u32) -> Option<CollatedBatch> {
    let items: Vec<&CollateItem> = batch.iter().flatten().collect();
    if items.is_empty() {
        return None;
    }
    let n = items.len();
    let width = items.iter().map(|it| it.ids.len()).max().unwrap();
    let kmax = items.iter().map(|it| it.markers.len()).max().unwrap();
    let mut input_ids = vec![vec![pad_id; width]; n];
    let mut attention_mask = vec![vec![0u32; width]; n];
    let mut marker_pos = vec![vec![0u32; kmax]; n];
    let mut marker_mask = vec![vec![false; kmax]; n];
    for (i, it) in items.iter().enumerate() {
        input_ids[i][..it.ids.len()].copy_from_slice(&it.ids);
        attention_mask[i][..it.ids.len()].fill(1);
        for (k, m) in it.markers.iter().enumerate() {
            marker_pos[i][k] = *m as u32;
            marker_mask[i][k] = true;
        }
    }
    Some(CollatedBatch {
        input_ids,
        attention_mask,
        marker_pos,
        marker_mask,
        qtype: items.iter().map(|it| it.qtype).collect(),
    })
}
