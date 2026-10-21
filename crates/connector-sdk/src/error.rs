//! Errors at the external request boundary.

use thiserror::Error;

/// Failure to validate or convert a serialized read request.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum RequestError {
    /// The input is not a valid Substrait request or is internally inconsistent.
    #[error("invalid request: {reason}")]
    InvalidRequest { reason: String },

    /// The input is valid Substrait but exceeds the connector contract subset.
    #[error("request is outside the declared contract: {reason}")]
    Undeclared { reason: String },
}

impl RequestError {
    pub(crate) fn invalid(reason: impl Into<String>) -> Self {
        Self::InvalidRequest {
            reason: reason.into(),
        }
    }

    pub(crate) fn undeclared(reason: impl Into<String>) -> Self {
        Self::Undeclared {
            reason: reason.into(),
        }
    }
}

/// Stable error categories exposed by source-independent connector processing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCategory {
    InvalidRequest,
    Unsupported,
    Authentication,
    Authorization,
    NotFound,
    Unavailable,
    Timeout,
    Cancelled,
    ResourceExhausted,
    SchemaMismatch,
    Source,
    Internal,
}

/// An error safe to return across the connector boundary.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{category:?}: {message}")]
pub struct ConnectorError {
    category: ErrorCategory,
    message: String,
}

impl ConnectorError {
    pub fn new(category: ErrorCategory, redacted_message: impl Into<String>) -> Self {
        Self {
            category,
            message: redacted_message.into(),
        }
    }

    pub fn category(&self) -> ErrorCategory {
        self.category
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}
