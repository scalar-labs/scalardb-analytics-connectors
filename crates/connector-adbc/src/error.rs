//! Classification of owned ADBC diagnostics into connector errors.

use std::fmt;

use adbc_core::error::{Error, Status};
use connector_sdk::{
    ConnectorError, DiagnosticEntry, DiagnosticValue, ErrorClass, ErrorHandler, ErrorKind,
};

use crate::AdbcConnector;

const SQLSTATE_LENGTH: usize = 5;
const DETAIL_PREFIX: &str = "adbc.detail/";

/// An ADBC status captured before connector-level classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdbcStatus {
    /// A status defined by the ADBC revision used by this connector.
    Known(Status),
    /// A status byte not defined by that ADBC revision.
    Unknown(u8),
}

impl From<Status> for AdbcStatus {
    fn from(status: Status) -> Self {
        Self::Known(status)
    }
}

/// One owned ADBC error detail.
#[derive(Clone, PartialEq, Eq)]
pub struct AdbcDetail {
    key: String,
    value: Vec<u8>,
}

impl fmt::Debug for AdbcDetail {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("AdbcDetail").finish_non_exhaustive()
    }
}

impl AdbcDetail {
    pub fn new(key: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

/// The result of capturing ADBC 1.1 error details.
#[derive(Clone, PartialEq, Eq)]
pub enum AdbcDetails {
    /// The error did not provide details.
    Missing,
    /// Details were captured in driver-provided order.
    Present(Vec<AdbcDetail>),
    /// Detail capture failed after zero or more entries were copied.
    Malformed {
        captured: Vec<AdbcDetail>,
        reason: String,
    },
}

impl fmt::Debug for AdbcDetails {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => formatter.write_str("Missing"),
            Self::Present(_) => formatter.debug_tuple("Present").finish(),
            Self::Malformed { .. } => formatter.debug_struct("Malformed").finish_non_exhaustive(),
        }
    }
}

/// An owned ADBC diagnostic that can be classified without source access.
#[derive(Clone, PartialEq, Eq)]
pub struct AdbcDiagnostic {
    status: AdbcStatus,
    message: String,
    sqlstate: [u8; SQLSTATE_LENGTH],
    vendor_code: i32,
    details: AdbcDetails,
}

impl fmt::Debug for AdbcDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdbcDiagnostic")
            .finish_non_exhaustive()
    }
}

impl AdbcDiagnostic {
    pub fn new(
        status: AdbcStatus,
        message: impl Into<String>,
        sqlstate: [u8; SQLSTATE_LENGTH],
        vendor_code: i32,
        details: AdbcDetails,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            sqlstate,
            vendor_code,
            details,
        }
    }

    pub fn status(&self) -> AdbcStatus {
        self.status
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn sqlstate_bytes(&self) -> &[u8; SQLSTATE_LENGTH] {
        &self.sqlstate
    }

    pub fn vendor_code(&self) -> i32 {
        self.vendor_code
    }

    pub fn details(&self) -> &AdbcDetails {
        &self.details
    }
}

impl From<Error> for AdbcDiagnostic {
    fn from(error: Error) -> Self {
        let details = match error.details {
            Some(details) => AdbcDetails::Present(
                details
                    .into_iter()
                    .map(|(key, value)| AdbcDetail::new(key, value))
                    .collect(),
            ),
            None => AdbcDetails::Missing,
        };
        Self::new(
            error.status.into(),
            error.message,
            error.sqlstate.map(|byte| byte as u8),
            error.vendor_code,
            details,
        )
    }
}

/// Classifies a mid-stream failure that arrives as text without ADBC status,
/// SQLSTATE, or details.
///
/// The text still passes through the Connector's redaction, because a driver
/// may copy statement text into it; the classification stays `data-source`.
pub(crate) fn stream_fallback_error<C: AdbcConnector>(message: String) -> ConnectorError {
    let redacted = <C::ErrorHandler as ErrorHandler>::redact(AdbcDiagnostic::new(
        AdbcStatus::Known(Status::Unknown),
        message,
        [0; SQLSTATE_LENGTH],
        0,
        AdbcDetails::Missing,
    ));
    ConnectorError::new(
        ErrorKind::DataSource,
        false,
        "the data source result stream failed without structured diagnostics",
    )
    .with_data_source_error(None, Some(redacted.message))
}

enum Sqlstate {
    Missing,
    Valid(String),
    Malformed,
}

/// Builds the connector error for one captured ADBC diagnostic.
///
/// `class` is the Connector's classification, when its error table matched;
/// otherwise the diagnostic is classified by SQL-standard SQLSTATE class and
/// ADBC status. A malformed diagnostic becomes a non-retryable internal
/// connector error whatever the table says, while retaining every source
/// field that was captured successfully.
pub(crate) fn to_connector_error(
    diagnostic: AdbcDiagnostic,
    class: Option<ErrorClass>,
) -> ConnectorError {
    let sqlstate = parse_sqlstate(&diagnostic.sqlstate);
    let structurally_valid = matches!(diagnostic.status, AdbcStatus::Known(status) if status != Status::Ok)
        && !matches!(&sqlstate, Sqlstate::Malformed)
        && !matches!(&diagnostic.details, AdbcDetails::Malformed { .. });

    let classification = if !structurally_valid {
        ErrorClass::new(
            ErrorKind::Internal,
            false,
            "the ADBC driver returned an invalid error diagnostic",
        )
    } else if let Some(class) = class {
        class
    } else if let Sqlstate::Valid(sqlstate) = &sqlstate {
        classify_sqlstate(sqlstate)
    } else {
        classify_status(diagnostic.status)
    };

    build_error(diagnostic, sqlstate, classification)
}

fn parse_sqlstate(bytes: &[u8; SQLSTATE_LENGTH]) -> Sqlstate {
    if bytes == &[0; SQLSTATE_LENGTH] || bytes == b"00000" {
        return Sqlstate::Missing;
    }
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Sqlstate::Valid(String::from_utf8_lossy(bytes).into_owned());
    }
    Sqlstate::Malformed
}

fn classify_sqlstate(sqlstate: &str) -> ErrorClass {
    match &sqlstate[..2] {
        "08" => ErrorClass::new(
            ErrorKind::Connection,
            true,
            "the data source connection failed",
        ),
        "28" => ErrorClass::new(
            ErrorKind::Authentication,
            false,
            "the data source rejected authentication",
        ),
        "22" => ErrorClass::new(
            ErrorKind::DataSource,
            false,
            "the data source could not process a value",
        ),
        "40" => ErrorClass::new(
            ErrorKind::DataSource,
            true,
            "the data source rolled back the request",
        ),
        "42" => ErrorClass::new(
            ErrorKind::Internal,
            false,
            "the connector issued a request the data source could not execute",
        ),
        _ => ErrorClass::new(
            ErrorKind::DataSource,
            false,
            "the data source could not run the request",
        ),
    }
}

fn classify_status(status: AdbcStatus) -> ErrorClass {
    match status {
        AdbcStatus::Known(Status::IO) => ErrorClass::new(
            ErrorKind::Connection,
            true,
            "the data source connection failed",
        ),
        AdbcStatus::Known(Status::Unauthenticated) => ErrorClass::new(
            ErrorKind::Authentication,
            false,
            "the data source rejected authentication",
        ),
        AdbcStatus::Known(Status::Unauthorized) => ErrorClass::new(
            ErrorKind::Permission,
            false,
            "the data source denied access to the requested target",
        ),
        AdbcStatus::Known(Status::Cancelled) => ErrorClass::new(
            ErrorKind::Cancellation,
            false,
            "the data source request was cancelled",
        ),
        AdbcStatus::Known(Status::Timeout) => ErrorClass::new(
            ErrorKind::Timeout,
            true,
            "the data source request timed out",
        ),
        AdbcStatus::Known(
            Status::NotImplemented
            | Status::InvalidArguments
            | Status::InvalidState
            | Status::Internal,
        ) => ErrorClass::new(
            ErrorKind::Internal,
            false,
            "the connector or data source driver failed",
        ),
        AdbcStatus::Known(
            Status::Unknown
            | Status::NotFound
            | Status::AlreadyExists
            | Status::InvalidData
            | Status::Integrity,
        ) => ErrorClass::new(
            ErrorKind::DataSource,
            false,
            "the data source could not run the request",
        ),
        AdbcStatus::Known(Status::Ok) | AdbcStatus::Unknown(_) => ErrorClass::new(
            ErrorKind::Internal,
            false,
            "the ADBC driver returned an invalid error diagnostic",
        ),
    }
}

fn status_code(status: AdbcStatus) -> u8 {
    match status {
        AdbcStatus::Known(status) => status.into(),
        AdbcStatus::Unknown(code) => code,
    }
}

fn build_error(
    diagnostic: AdbcDiagnostic,
    sqlstate: Sqlstate,
    classification: ErrorClass,
) -> ConnectorError {
    let source_code = match &sqlstate {
        Sqlstate::Valid(sqlstate) => Some(sqlstate.clone()),
        Sqlstate::Missing | Sqlstate::Malformed => None,
    };
    let mut entries = vec![
        DiagnosticEntry::new(
            "adbc.status",
            DiagnosticValue::U8(status_code(diagnostic.status)),
        ),
        DiagnosticEntry::new(
            "adbc.sqlstate",
            DiagnosticValue::Bytes(diagnostic.sqlstate.to_vec()),
        ),
        DiagnosticEntry::new(
            "adbc.vendor_code",
            DiagnosticValue::I32(diagnostic.vendor_code),
        ),
    ];

    match diagnostic.details {
        AdbcDetails::Missing => entries.push(DiagnosticEntry::new(
            "adbc.details.state",
            DiagnosticValue::Utf8("missing".to_owned()),
        )),
        AdbcDetails::Present(details) => {
            entries.push(DiagnosticEntry::new(
                "adbc.details.state",
                DiagnosticValue::Utf8("present".to_owned()),
            ));
            entries.extend(details.into_iter().map(|detail| {
                DiagnosticEntry::new(
                    format!("{DETAIL_PREFIX}{}", detail.key),
                    DiagnosticValue::Bytes(detail.value),
                )
            }));
        }
        AdbcDetails::Malformed { captured, reason } => {
            entries.push(DiagnosticEntry::new(
                "adbc.details.state",
                DiagnosticValue::Utf8("malformed".to_owned()),
            ));
            entries.extend(captured.into_iter().map(|detail| {
                DiagnosticEntry::new(
                    format!("{DETAIL_PREFIX}{}", detail.key),
                    DiagnosticValue::Bytes(detail.value),
                )
            }));
            entries.push(DiagnosticEntry::new(
                "adbc.details.error",
                DiagnosticValue::Utf8(reason),
            ));
        }
    }

    ConnectorError::new(
        classification.kind,
        classification.retryable,
        classification.safe_message,
    )
    .with_data_source_error(source_code, Some(diagnostic.message))
    .with_diagnostics(entries)
}
