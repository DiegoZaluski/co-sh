//! The ONNX session seam: what the decode path needs from a runtime session.
//!
//! A trait so the runtime can be driven with fixed logits in tests, mirroring
//! the upstream `_StubSession` technique. `Send + Sync` are supertraits so a
//! boxed session can sit inside an agent that a router shares across
//! threads by reference (`&self` run: the agent's `&mut self` upstream comes
//! from Python's single-threaded model). The forward pass itself is
//! serialised on [`OrtSession`]'s inner mutex — `ort::Session::run` takes
//! `&mut self` because the C API holds run-scoped state — so only that pass
//! is exclusive; tokenization, hooks and decode stay parallel. The mutex is
//! not removable.

use crate::error::{Error, Result};
use crate::runtime::batch::CollatedBatch;
use std::sync::Mutex;

/// One row of model output: `logits[row]` and `act_logits[row]` as delivered
/// by the ONNX graph.
pub struct SessionOutput {
    pub logits: Vec<f32>,
    pub act_logits: Vec<f32>,
}

/// The slice of `ort.InferenceSession.run` the decode path needs. A trait so
/// the runtime can be driven with fixed logits in tests, mirroring the
/// upstream `_StubSession` technique.
/// `Send + Sync` are supertraits so a boxed session can sit inside an agent
/// that the Router shares across threads by reference (upstream's
/// thread-safety guarantees, #95 — `ort::Session::run` takes `&mut self`
/// because the C API holds run-scoped state, so [`OrtSession`] serialises
/// the forward pass itself on an inner mutex; everything around it —
/// tokenization, hooks, decode — stays parallel).
pub trait SessionRunner: Send + Sync {
    fn run(&self, batch: &CollatedBatch) -> Result<Vec<SessionOutput>>;
}

/// The real session: build the five ORT inputs from the collated batch and
/// read `logits` / `act_logits` back. The ORT session is behind an inner
/// mutex because `ort::Session::run` needs `&mut self`; the lock is held
/// only across the forward pass.
pub struct OrtSession {
    session: Mutex<ort::session::Session>,
}

impl OrtSession {
    /// Wrap a built session. Construction stays with the caller (the loader
    /// configures optimization level / threads on the builder).
    pub fn new(session: ort::session::Session) -> Self {
        Self {
            session: Mutex::new(session),
        }
    }
}

impl SessionRunner for OrtSession {
    fn run(&self, batch: &CollatedBatch) -> Result<Vec<SessionOutput>> {
        use ort::value::{Tensor, TensorRef};
        let n = batch.n_rows;
        let width = batch.width;
        let kmax = batch.kmax;

        // The collated buffers are already flat and row-major: only the
        // element cast to ORT's i64 index type is needed (and `marker_mask`,
        // whose element type is bool on both sides).
        let input_ids: Vec<i64> = batch.input_ids.iter().map(|v| *v as i64).collect();
        let attention_mask: Vec<i64> = batch.attention_mask.iter().map(|v| *v as i64).collect();
        let marker_pos: Vec<i64> = batch.marker_pos.iter().map(|v| *v as i64).collect();
        let qtype: Vec<i64> = batch.qtype.iter().map(|q| *q as i64).collect();

        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array(([n, width], input_ids)).map_err(ort_error)?,
            "attention_mask" => Tensor::from_array(([n, width], attention_mask)).map_err(ort_error)?,
            "marker_pos" => Tensor::from_array(([n, kmax], marker_pos)).map_err(ort_error)?,
            "marker_mask" => TensorRef::from_array_view(([n, kmax], &batch.marker_mask[..])).map_err(ort_error)?,
            "qtype" => Tensor::from_array(([n], qtype)).map_err(ort_error)?,
        ];

        let logits = {
            // The outputs borrow the session, so the extraction stays inside
            // the lock scope.
            let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
            let outputs = session.run(inputs).map_err(ort_error)?;
            let logits = extract_matrix(&outputs, "logits")?;
            let act_logits = extract_matrix(&outputs, "act_logits")?;
            if logits.len() != act_logits.len() {
                return Err(Error::Runtime(format!(
                    "ONNX outputs disagree on the batch row count: logits has {}, act_logits has {}",
                    logits.len(),
                    act_logits.len()
                )));
            }
            logits
                .into_iter()
                .zip(act_logits)
                .map(|(logits, act_logits)| SessionOutput { logits, act_logits })
                .collect::<Vec<_>>()
        };
        Ok(logits)
    }
}

pub(crate) fn ort_error<E: std::fmt::Display>(e: E) -> Error {
    Error::Runtime(format!("cosh-onnx: {}", e))
}

/// Extract a `[n, w]` f32 output as row-major vectors.
fn extract_matrix(outputs: &ort::session::SessionOutputs<'_>, name: &str) -> Result<Vec<Vec<f32>>> {
    let value = outputs
        .get(name)
        .ok_or_else(|| Error::Runtime(format!("cosh-onnx: no output named {}", name)))?;
    let (shape, data) = value.try_extract_tensor::<f32>().map_err(ort_error)?;
    let width = *shape.last().unwrap_or(&0) as usize;
    Ok(data.chunks(width.max(1)).map(|row| row.to_vec()).collect())
}
