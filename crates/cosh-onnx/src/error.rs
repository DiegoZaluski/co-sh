//! The crate-wide error type.

/// The error every fallible operation in the crate returns.
///
/// One enum so a caller matches a single type regardless of which layer
/// produced the failure:
///
/// * [`Error::Value`] — invalid input the caller supplied (bad question
///   shape, out-of-range `k`, malformed timeout).
/// * [`Error::Schema`] — a JSON schema that cannot be mapped to decision
///   questions; the message names the offending path.
/// * [`Error::Runtime`] — load/IO failures: missing checkpoint files, Hub
///   download errors, digest mismatches, tokenizer and session errors.
///   Python signals these as exceptions or as `RuntimeWarning`s a caller may
///   want to observe; over a `Result` both arrive as errors.
/// * [`Error::Timeout`] — an overrunning `hooks_timeout` hook call
///   (`cosh-onnx: hook {name} exceeded {timeout}s`).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Value(String),
    #[error("{0}")]
    Schema(String),
    #[error("{0}")]
    Runtime(String),
    #[error("{0}")]
    Timeout(String),
}

pub type Result<T> = std::result::Result<T, Error>;
