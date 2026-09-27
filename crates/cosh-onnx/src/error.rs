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

impl Error {
    /// The message carried by any variant (the `Display` payload).
    pub fn message(&self) -> &str {
        match self {
            Error::Value(m) | Error::Schema(m) | Error::Runtime(m) | Error::Timeout(m) => m,
        }
    }

    /// Invalid input the caller supplied.
    pub fn is_value(&self) -> bool {
        matches!(self, Error::Value(_))
    }

    /// A JSON schema that cannot be mapped to decision questions.
    pub fn is_schema(&self) -> bool {
        matches!(self, Error::Schema(_))
    }

    /// A load/IO failure (checkpoint, Hub, tokenizer, session).
    pub fn is_runtime(&self) -> bool {
        matches!(self, Error::Runtime(_))
    }

    /// An overrunning `hooks_timeout` hook call.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Timeout(_))
    }

    /// The verbatim hook-timeout text (upstream: `f"hook {name} exceeded
    /// {timeout}s"`, prefixed `cosh-onnx: ` by this port). One constructor so
    /// the wording lives in exactly one place.
    pub(crate) fn hook_timeout(name: &str, timeout: f64) -> Error {
        Error::Timeout(format!(
            "cosh-onnx: hook {} exceeded {}s",
            name,
            crate::pycompat::py_g(timeout)
        ))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
