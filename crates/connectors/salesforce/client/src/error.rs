//! Errors the client reports, structured but not classified.
//!
//! Classifying them into the connector contract's error kinds is the
//! connector's decision. No error carries a secret, a request URL, or SOQL
//! text; messages Salesforce returned are kept as [`SourceMessage`], which
//! neither `Debug` nor `Display` prints, because they can echo user data such
//! as fragments of a query.

use std::fmt;

use arrow_schema::DataType;

/// A message Salesforce returned.
///
/// It can contain user data, so it is read only through [`SourceMessage::as_str`].
pub struct SourceMessage(String);

impl SourceMessage {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SourceMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceMessage(..)")
    }
}

/// One entry of a REST API error response
/// ([Status Codes and Error Responses](https://developer.salesforce.com/docs/platform/api-rest/guide/errorcodes.html)).
#[derive(Debug)]
pub struct ApiError {
    /// Salesforce's error code, such as `INVALID_FIELD`.
    pub error_code: String,
    pub message: SourceMessage,
    /// The fields the error names, when Salesforce names any.
    pub fields: Vec<String>,
}

/// How sending a request or receiving its response failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// No connection to Salesforce could be made.
    Connect,
    /// The request or the connection timed out.
    Timeout,
    /// The connection broke or the response could not be read.
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The OAuth token endpoint refused the request.
    #[error("the Salesforce token endpoint returned HTTP {status} ({error})")]
    Authentication {
        status: u16,
        /// The OAuth error code, such as `invalid_grant`.
        error: String,
        description: SourceMessage,
    },
    /// The REST API returned an error response.
    #[error("the Salesforce API returned HTTP {status} ({})", error_codes(.errors))]
    Api { status: u16, errors: Vec<ApiError> },
    /// The request could not be sent or its response could not be received.
    #[error("the request to Salesforce failed: {message}")]
    Transport {
        kind: TransportKind,
        message: String,
    },
    /// The response is not one the client understands.
    #[error("unexpected response from Salesforce: {0}")]
    InvalidResponse(String),
    /// A value cannot be carried exactly by its requested Arrow type.
    #[error("the value of field {field} cannot be read as {data_type}: {reason}")]
    Value {
        field: String,
        data_type: DataType,
        reason: &'static str,
    },
    /// The configuration or a requested schema cannot be used.
    #[error("invalid configuration: {0}")]
    Config(String),
}

fn error_codes(errors: &[ApiError]) -> String {
    if errors.is_empty() {
        return "no error code".to_owned();
    }
    errors
        .iter()
        .map(|error| error.error_code.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

impl Error {
    /// Converts a reqwest error, dropping its URL, whose query can hold SOQL.
    pub(crate) fn transport(error: reqwest::Error) -> Self {
        let kind = if error.is_connect() {
            TransportKind::Connect
        } else if error.is_timeout() {
            TransportKind::Timeout
        } else {
            TransportKind::Other
        };
        Error::Transport {
            kind,
            message: error.without_url().to_string(),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
