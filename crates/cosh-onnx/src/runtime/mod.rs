//! ONNX serving primitives: the session seam, the tokenizer surface and
//! batch collation — generic over the decision model behind them.

pub mod batch;
pub mod session;
pub mod tokenizer;

#[cfg(test)]
mod tests;
