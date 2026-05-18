//! Errors produced by [`crate`] helpers.

/// Errors produced when serializing or processing observability events.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// JSON serialization of an event payload failed.
    #[error("failed to serialize observability event: {0}")]
    Serialize(#[from] serde_json::Error),
}

impl Error {
    /// Returns a short, stable identifier for the error variant. Useful for
    /// structured logging without leaking the underlying message.
    pub fn kind(&self) -> &'static str {
        match self {
            Error::Serialize(_) => "serialize",
        }
    }
}
