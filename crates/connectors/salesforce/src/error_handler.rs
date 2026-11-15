//! Classifying the client's errors into the contract's error kinds.
//!
//! The error table matches Salesforce's `errorCode`s; the generic classes
//! follow from the HTTP status and from errors the client detects itself.
//! Salesforce's messages can echo the SOQL text and so its filter literals,
//! so only the messages of codes known not to echo the request are returned
//! as the Data Source message. The token endpoint's `error_description` is
//! returned: a token request carries no SOQL, and the endpoint describes the
//! grant rather than echoing its secrets.

use connector_salesforce_client::{ApiError, Error, TransportKind};
use connector_sdk::{ConnectorError, ErrorClass, ErrorHandler, ErrorKind};

/// The Salesforce connector's error table.
pub struct SalesforceErrorHandler;

/// The error codes the table classifies, and their classes.
const ERROR_TABLE: [(&str, ErrorClass); 10] = [
    (
        "INVALID_SESSION_ID",
        ErrorClass::new(
            ErrorKind::Authentication,
            false,
            "Salesforce rejected the access token",
        ),
    ),
    (
        "INSUFFICIENT_ACCESS",
        ErrorClass::new(
            ErrorKind::PermissionDenied,
            false,
            "the Salesforce user may not read the object",
        ),
    ),
    (
        "INSUFFICIENT_ACCESS_OR_READONLY",
        ErrorClass::new(
            ErrorKind::PermissionDenied,
            false,
            "the Salesforce user may not read the object",
        ),
    ),
    (
        "INVALID_FIELD",
        ErrorClass::new(
            ErrorKind::NotFound,
            false,
            "a field the read references is not visible, and whether it still exists cannot be told",
        ),
    ),
    (
        "INVALID_TYPE",
        ErrorClass::new(
            ErrorKind::NotFound,
            false,
            "the object the read references is not visible, and whether it still exists cannot be told",
        ),
    ),
    (
        "REQUEST_LIMIT_EXCEEDED",
        ErrorClass::new(
            ErrorKind::Resource,
            true,
            "the org exceeded its Salesforce API request limit",
        ),
    ),
    (
        "MALFORMED_QUERY",
        ErrorClass::new(
            ErrorKind::Internal,
            false,
            "Salesforce rejected the SOQL the connector generated",
        ),
    ),
    (
        "INVALID_QUERY_FILTER_OPERATOR",
        ErrorClass::new(
            ErrorKind::Internal,
            false,
            "Salesforce rejected the SOQL the connector generated",
        ),
    ),
    (
        "QUERY_TIMEOUT",
        ErrorClass::new(
            ErrorKind::DataSource,
            true,
            "the query exceeded Salesforce's own time limit",
        ),
    ),
    (
        "INVALID_QUERY_LOCATOR",
        ErrorClass::new(
            ErrorKind::DataSource,
            true,
            "the query's cursor expired at Salesforce",
        ),
    ),
];

/// Codes whose messages never contain the request: they describe the
/// session, privileges, or limits, not the query.
const MESSAGE_KEPT: [&str; 5] = [
    "INVALID_SESSION_ID",
    "INSUFFICIENT_ACCESS",
    "INSUFFICIENT_ACCESS_OR_READONLY",
    "REQUEST_LIMIT_EXCEEDED",
    "QUERY_TIMEOUT",
];

impl ErrorHandler for SalesforceErrorHandler {
    type Diagnostic = Error;

    /// Looks up the first error code of a REST API error.
    fn classify(diagnostic: &Error) -> Option<ErrorClass> {
        let Error::Api { errors, .. } = diagnostic else {
            return None;
        };
        let code = errors.first()?.error_code.as_str();
        ERROR_TABLE
            .iter()
            .find(|(known, _)| *known == code)
            .map(|(_, class)| *class)
    }
}

/// Builds the error for a client error, with `class` from the error table
/// when it matched.
pub(crate) fn error(diagnostic: Error, class: Option<ErrorClass>) -> ConnectorError {
    match diagnostic {
        Error::Api { status, errors } => {
            let class = class.unwrap_or_else(|| api_class(status));
            let first = errors.first();
            ConnectorError::new(class.kind, class.retryable, class.safe_message)
                .with_data_source_error(
                    first.map(|error| error.error_code.clone()),
                    first.and_then(kept_message),
                )
        }
        Error::Authentication {
            status,
            error,
            description,
        } => {
            let class = match status {
                429 => ErrorClass::new(
                    ErrorKind::Resource,
                    true,
                    "the Salesforce token endpoint is rate limiting requests",
                ),
                500.. => ErrorClass::new(
                    ErrorKind::DataSource,
                    true,
                    "the Salesforce token endpoint failed",
                ),
                _ => ErrorClass::new(
                    ErrorKind::Authentication,
                    false,
                    "Salesforce refused to issue an access token",
                ),
            };
            ConnectorError::new(class.kind, class.retryable, class.safe_message)
                .with_data_source_error(Some(error), Some(description.as_str().to_owned()))
        }
        Error::Transport { kind, .. } => ConnectorError::new(
            ErrorKind::Connection,
            true,
            match kind {
                TransportKind::Connect => "no connection to Salesforce could be made",
                TransportKind::Timeout => "the connection to Salesforce timed out",
                TransportKind::Other => "the connection to Salesforce broke",
            },
        ),
        Error::Value {
            field,
            data_type,
            reason,
        } => ConnectorError::new(
            ErrorKind::Data,
            false,
            format!("a value of field {field} cannot be carried exactly as {data_type}: {reason}"),
        ),
        // The client's description of an unreadable response can quote the
        // response, so only the fact is reported.
        Error::InvalidResponse(_) => ConnectorError::new(
            ErrorKind::Internal,
            false,
            "Salesforce returned a response the connector cannot read",
        ),
        Error::Config(message) => ConnectorError::new(ErrorKind::InvalidSettings, false, message),
    }
}

/// The class of a REST API error that the error table does not list.
fn api_class(status: u16) -> ErrorClass {
    match status {
        401 => ErrorClass::new(
            ErrorKind::Authentication,
            false,
            "Salesforce rejected the access token",
        ),
        429 => ErrorClass::new(
            ErrorKind::Resource,
            true,
            "Salesforce is rate limiting requests",
        ),
        500.. => ErrorClass::new(ErrorKind::DataSource, true, "Salesforce failed the request"),
        _ => ErrorClass::new(
            ErrorKind::DataSource,
            false,
            "Salesforce refused the request",
        ),
    }
}

fn kept_message(error: &ApiError) -> Option<String> {
    MESSAGE_KEPT
        .contains(&error.error_code.as_str())
        .then(|| error.message.as_str().to_owned())
}
