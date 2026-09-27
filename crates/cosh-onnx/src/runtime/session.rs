//! The ONNX session seam: what the decode path needs from a runtime session.
//!
//! A trait so the runtime can be driven with fixed logits in tests, mirroring
//! the upstream `_StubSession` technique. `Send` is a supertrait so a boxed
//! session can sit inside an agent that a router shares across threads.

use crate::error::{Error, Result};
use crate::runtime::batch::CollatedBatch;

/// One row of model output: `logits[row]` and `act_logits[row]` as delivered
/// by the ONNX graph.
pub struct SessionOutput {
    pub logits: Vec<f32>,
    pub act_logits: Vec<f32>,
}

/// The slice of `ort.InferenceSession.run` the decode path needs. A trait so
/// the runtime can be driven with fixed logits in tests, mirroring the
/// upstream `_StubSession` technique.
/// `Send` is a supertrait so a boxed session can sit inside an agent that
/// the Router shares across threads (upstream's thread-safety guarantees,
/// #95).
pub trait SessionRunner: Send {
    fn run(&mut self, batch: &CollatedBatch) -> Result<Vec<SessionOutput>>;
}

/// The real session: build the five ORT inputs from the collated batch and
/// read `logits` / `act_logits` back.
pub struct OrtSession {
    pub session: ort::session::Session,
}

impl SessionRunner for OrtSession {
    fn run(&mut self, batch: &CollatedBatch) -> Result<Vec<SessionOutput>> {
        use ort::value::Tensor;
        let n = batch.input_ids.len();
        let width = batch.input_ids.first().map(Vec::len).unwrap_or(0);
        let kmax = batch.marker_pos.first().map(Vec::len).unwrap_or(0);

        let flatten_i64 = |rows: &[Vec<u32>]| -> Vec<i64> {
            rows.iter().flatten().map(|v| *v as i64).collect()
        };
        let input_ids = flatten_i64(&batch.input_ids);
        let attention_mask = flatten_i64(&batch.attention_mask);
        let marker_pos = flatten_i64(&batch.marker_pos);
        let marker_mask: Vec<bool> = batch.marker_mask.iter().flatten().copied().collect();
        let qtype: Vec<i64> = batch.qtype.iter().map(|q| *q as i64).collect();

        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array(([n, width], input_ids)).map_err(ort_error)?,
            "attention_mask" => Tensor::from_array(([n, width], attention_mask)).map_err(ort_error)?,
            "marker_pos" => Tensor::from_array(([n, kmax], marker_pos)).map_err(ort_error)?,
            "marker_mask" => Tensor::from_array(([n, kmax], marker_mask)).map_err(ort_error)?,
            "qtype" => Tensor::from_array(([n], qtype)).map_err(ort_error)?,
        ];

        let outputs = self
            .session
            .run(inputs)
            .map_err(ort_error)?;
        let logits = extract_matrix(&outputs, "logits")?;
        let act_logits = extract_matrix(&outputs, "act_logits")?;
        if logits.len() != act_logits.len() {
            return Err(Error::Runtime(format!(
                "ONNX outputs disagree on the batch row count: logits has {}, act_logits has {}",
                logits.len(),
                act_logits.len()
            )));
        }
        Ok(logits
            .into_iter()
            .zip(act_logits)
            .map(|(logits, act_logits)| SessionOutput { logits, act_logits })
            .collect())
    }
}

pub(crate) fn ort_error<E: std::fmt::Display>(e: E) -> Error {
    Error::Runtime(format!("cosh-onnx: {}", e))
}

/// Extract a `[n, w]` f32 output as row-major vectors.
fn extract_matrix(
    outputs: &ort::session::SessionOutputs<'_>,
    name: &str,
) -> Result<Vec<Vec<f32>>> {
    let value = outputs
        .get(name)
        .ok_or_else(|| Error::Runtime(format!("cosh-onnx: no output named {}", name)))?;
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .map_err(ort_error)?;
    let width = *shape.last().unwrap_or(&0) as usize;
    Ok(data.chunks(width.max(1)).map(|row| row.to_vec()).collect())
}

