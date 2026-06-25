use std::fmt;

#[derive(Debug, Clone)]
pub struct JsonishError(pub String);

impl fmt::Display for JsonishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for JsonishError {}
