//! Errors at connector boundaries.

use std::{error::Error, fmt};

use thiserror::Error as ThisError;

/// Failure to validate or convert a read request.
#[derive(Clone, Debug, ThisError, PartialEq, Eq)]
pub enum RequestError {
    /// The expected request shape is malformed or internally inconsistent.
    #[error("invalid request: {reason}")]
    InvalidRequest { reason: String },

    /// The request is outside the accepted shape.
    #[error("request is outside the declared contract: {reason}")]
    Undeclared { reason: String },
}

impl RequestError {
    /// Reports a malformed value inside the declared request profile.
    pub(crate) fn invalid(reason: impl Into<String>) -> Self {
        Self::InvalidRequest {
            reason: reason.into(),
        }
    }

    /// Reports a validly encoded feature outside the declared request profile.
    pub(crate) fn undeclared(reason: impl Into<String>) -> Self {
        Self::Undeclared {
            reason: reason.into(),
        }
    }
}

/// Stable error kinds exposed by Data Source-independent connector processing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidSettings,
    Connection,
    Authentication,
    Permission,
    InvalidRequest,
    InvalidState,
    StaleRegistration,
    Data,
    Cancellation,
    Timeout,
    Resource,
    DataSource,
    Internal,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::InvalidSettings => "invalid settings",
            Self::Connection => "connection",
            Self::Authentication => "authentication",
            Self::Permission => "permission",
            Self::InvalidRequest => "invalid request",
            Self::InvalidState => "invalid state",
            Self::StaleRegistration => "stale registration",
            Self::Data => "data",
            Self::Cancellation => "cancellation",
            Self::Timeout => "timeout",
            Self::Resource => "resource",
            Self::DataSource => "data-source",
            Self::Internal => "internal",
        };
        formatter.write_str(name)
    }
}

/// A typed diagnostic value owned by the connector error.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum DiagnosticValue {
    Bool(bool),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
    Utf8(String),
    Bytes(Vec<u8>),
}

/// One diagnostic entry.
#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticEntry {
    key: String,
    value: DiagnosticValue,
}

impl DiagnosticEntry {
    pub fn new(key: impl Into<String>, value: DiagnosticValue) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn value(&self) -> &DiagnosticValue {
        &self.value
    }
}

/// An owned error that can cross connector and language boundaries.
#[derive(Clone, PartialEq)]
pub struct ConnectorError {
    kind: ErrorKind,
    retryable: bool,
    safe_message: String,
    data_source_code: Option<String>,
    data_source_message: Option<String>,
    diagnostics: Vec<DiagnosticEntry>,
}

impl ConnectorError {
    /// Creates an error containing only information safe for common reporting.
    pub fn new(kind: ErrorKind, retryable: bool, safe_message: impl Into<String>) -> Self {
        Self {
            kind,
            retryable,
            safe_message: safe_message.into(),
            data_source_code: None,
            data_source_message: None,
            diagnostics: Vec::new(),
        }
    }

    /// Attaches the original Data Source code and message for restricted diagnostics.
    pub fn with_data_source_error(
        mut self,
        data_source_code: Option<String>,
        data_source_message: Option<String>,
    ) -> Self {
        self.data_source_code = data_source_code;
        self.data_source_message = data_source_message;
        self
    }

    /// Replaces the typed diagnostic entries attached to this error.
    pub fn with_diagnostics(
        mut self,
        diagnostics: impl IntoIterator<Item = DiagnosticEntry>,
    ) -> Self {
        self.diagnostics = diagnostics.into_iter().collect();
        self
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn retryable(&self) -> bool {
        self.retryable
    }

    pub fn safe_message(&self) -> &str {
        &self.safe_message
    }

    pub fn data_source_code(&self) -> Option<&str> {
        self.data_source_code.as_deref()
    }

    pub fn data_source_message(&self) -> Option<&str> {
        self.data_source_message.as_deref()
    }

    pub fn diagnostics(&self) -> &[DiagnosticEntry] {
        &self.diagnostics
    }
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind, self.safe_message)
    }
}

impl fmt::Debug for ConnectorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectorError")
            .field("kind", &self.kind)
            .field("retryable", &self.retryable)
            .field("safe_message", &self.safe_message)
            .finish_non_exhaustive()
    }
}

impl Error for ConnectorError {}
