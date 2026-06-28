use std::fmt;

/// Error type for the `term_screen` module.
///
/// This is a well-typed error enum that consumers of the SDK
/// can match on exhaustively.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum TermScreenError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Image(#[from] crate::term_screen::cell::image::ImageCellError),

    #[error(transparent)]
    ImageError(#[from] ::image::ImageError),

    #[error("{0}")]
    Msg(String),

    #[error("{context}: {source}")]
    Context {
        context: String,
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

/// Convenience alias for `Result<T, TermScreenError>`.
pub type Result<T> = std::result::Result<T, TermScreenError>;

impl From<String> for TermScreenError {
    fn from(s: String) -> Self {
        TermScreenError::Msg(s)
    }
}

/// Extension trait that adds `.context()` and `.with_context()`
/// to any `Result<T, E>` where `E: std::error::Error`.
#[allow(clippy::missing_errors_doc)]
pub trait ContextExt<T> {
    /// Wrap the error with a static context message.
    fn context(self, context: &'static str) -> Result<T>;

    /// Wrap the error with a dynamically-computed context message.
    fn with_context<C>(self, context: C) -> Result<T>
    where
        C: fmt::Display + Send + Sync + 'static;
}

impl<T, E> ContextExt<T> for std::result::Result<T, E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn context(self, context: &'static str) -> Result<T> {
        self.map_err(|error| TermScreenError::Context {
            context: context.to_string(),
            source: Box::new(error),
        })
    }

    fn with_context<C>(self, context: C) -> Result<T>
    where
        C: fmt::Display + Send + Sync + 'static,
    {
        self.map_err(|error| TermScreenError::Context {
            context: context.to_string(),
            source: Box::new(error),
        })
    }
}

/// Create a `TermScreenError::Msg` from a format string.
/// Like `anyhow::bail!`, accepts a format string and arguments.
#[macro_export]
macro_rules! ts_bail {
    ($($arg:tt)+) => {
        return Err($crate::term_screen::core::error::TermScreenError::Msg(
            format!($($arg)+),
        ))
    };
}

/// Ensure a condition is true, otherwise bail with an error message.
/// Like `anyhow::ensure!`, accepts a condition followed by a format string and arguments.
#[macro_export]
macro_rules! ts_ensure {
    ($cond:expr $(,)?) => {
        if !$cond {
            return Err($crate::term_screen::core::error::TermScreenError::Msg(
                format!("condition failed: {}", stringify!($cond)),
            ));
        }
    };
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err($crate::term_screen::core::error::TermScreenError::Msg(
                format!($($arg)+),
            ));
        }
    };
}
