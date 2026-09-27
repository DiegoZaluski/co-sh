//! Batch collation: pad a batch of encoded questions into the model inputs.
//!
//! The padding shape — `input_ids` padded, `attention_mask` over real tokens,
//! marker positions zero-filled with a `marker_mask` — is the input contract
//! the crate's decision models build on. The training-only `target`/`label`
//! fields of the upstream function are omitted: model training is out of
//! scope for this port.
//!
//! The collated buffers are stored flat (one allocation per field, row-major)
//! so the session can hand them to ORT as tensors without re-flattening; the
//! row accessors below keep the per-row view the decode path reads.


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
    /// Number of rows (states × questions).
    pub n_rows: usize,
    /// Padded sequence length `[n, width]`.
    pub width: usize,
    /// Widest marker count `[n, kmax]`.
    pub kmax: usize,
    /// Flat row-major `[n * width]`, `pad_id` padded.
    pub input_ids: Vec<u32>,
    /// Flat row-major `[n * width]`, 1 over real tokens.
    pub attention_mask: Vec<u32>,
    /// Flat row-major `[n * kmax]`, zero-padded marker positions.
    pub marker_pos: Vec<u32>,
    /// Flat row-major `[n * kmax]`, true over a row's own markers.
    pub marker_mask: Vec<bool>,
    /// `[n]`, question type per row.
    pub qtype: Vec<u8>,
}

impl CollatedBatch {
    /// The attention row of one batch row.
    pub fn attention_row(&self, row: usize) -> &[u32] {
        &self.attention_mask[row * self.width..(row + 1) * self.width]
    }
    /// The marker-mask row of one batch row.
    pub fn marker_row(&self, row: usize) -> &[bool] {
        &self.marker_mask[row * self.kmax..(row + 1) * self.kmax]
    }
    /// The number of real tokens in one batch row (the row's `usage` count).
    pub fn row_tokens(&self, row: usize) -> u32 {
        self.attention_row(row).iter().sum()
    }
    /// The number of option markers in one batch row (the logit width `k`).
    pub fn row_markers(&self, row: usize) -> usize {
        self.marker_row(row).iter().filter(|b| **b).count()
    }
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
    let mut input_ids = vec![pad_id; n * width];
    let mut attention_mask = vec![0u32; n * width];
    let mut marker_pos = vec![0u32; n * kmax];
    let mut marker_mask = vec![false; n * kmax];
    for (i, it) in items.iter().enumerate() {
        input_ids[i * width..i * width + it.ids.len()].copy_from_slice(&it.ids);
        attention_mask[i * width..i * width + it.ids.len()].fill(1);
        for (k, m) in it.markers.iter().enumerate() {
            marker_pos[i * kmax + k] = *m as u32;
            marker_mask[i * kmax + k] = true;
        }
    }
    Some(CollatedBatch {
        n_rows: n,
        width,
        kmax,
        input_ids,
        attention_mask,
        marker_pos,
        marker_mask,
        qtype: items.iter().map(|it| it.qtype).collect(),
    })
}
