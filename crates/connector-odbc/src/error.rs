//! ODBC diagnostics and their classification into connector errors.

use std::fmt;

use arrow_odbc::{
    ColumnFailure,
    odbc_api::{self, Connection, handles::Record},
};
use arrow_schema::ArrowError;
use connector_sdk::{ConnectorError, DiagnosticEntry, DiagnosticValue, ErrorClass, ErrorKind};

use crate::OdbcConnector;

/// What ODBC reports about one Data Source error: the fields of its
/// diagnostic record.
///
/// The Connector's error handler sees it before classification and may redact
/// its message. It intentionally does not print its message in `Debug`,
/// because an ODBC driver may copy statement text into it.
#[derive(Clone, PartialEq, Eq)]
pub struct OdbcDiagnostic {
    /// The five-character SQLSTATE, when the error has a diagnostic record.
    pub sqlstate: Option<String>,
    /// The Data Source's native error code.
    pub native_error: i32,
    /// The message the ODBC driver reported.
    pub message: String,
    /// Whether the ODBC driver reports the connection dead after the failure
    /// (`SQL_ATTR_CONNECTION_DEAD`), such as after the server ended the
    /// session. Drivers report many such failures with a generic SQLSTATE.
    pub connection_lost: bool,
}

impl fmt::Debug for OdbcDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OdbcDiagnostic")
            .field("sqlstate", &self.sqlstate)
            .field("native_error", &self.native_error)
            .field("connection_lost", &self.connection_lost)
            .finish_non_exhaustive()
    }
}

impl OdbcDiagnostic {
    /// The diagnostic of an ODBC call's failure on `connection`, or while
    /// connecting when there is none yet.
    ///
    /// A connection whose state the driver cannot report is not taken as
    /// lost.
    pub(crate) fn from_odbc(error: &odbc_api::Error, connection: Option<&Connection<'_>>) -> Self {
        let connection_lost =
            connection.is_some_and(|connection| connection.is_dead().unwrap_or(false));
        match diagnostic_record(error) {
            Some(record) => Self {
                sqlstate: Some(String::from_utf8_lossy(&record.state.0).into_owned()),
                native_error: record.native_error,
                message: error.to_string(),
                connection_lost,
            },
            None => Self {
                sqlstate: None,
                native_error: 0,
                message: error.to_string(),
                connection_lost,
            },
        }
    }
}

/// The diagnostic record an ODBC error carries, if any.
fn diagnostic_record(error: &odbc_api::Error) -> Option<&Record> {
    match error {
        odbc_api::Error::Diagnostics { record, .. }
        | odbc_api::Error::UnsupportedOdbcApiVersion(record)
        | odbc_api::Error::InvalidRowArraySize { record, .. }
        | odbc_api::Error::UnableToRepresentNull(record)
        | odbc_api::Error::OracleOdbcDriverDoesNotSupport64Bit(record) => Some(record),
        _ => None,
    }
}

/// Classifies an ODBC call's failure on `connection`, or while connecting
/// when there is none, with Connector `C`'s error handling.
///
/// A value that would be truncated is reported by ODBC without a diagnostic
/// record; it cannot be carried exactly, so it is a `data` error.
pub(crate) fn odbc_error<C: OdbcConnector>(
    error: &odbc_api::Error,
    connection: Option<&Connection<'_>>,
) -> ConnectorError {
    if matches!(error, odbc_api::Error::TooLargeValueForBuffer { .. }) {
        return value_error();
    }
    C::classify(OdbcDiagnostic::from_odbc(error, connection))
}

/// Classifies a failure to set up the Arrow conversion of a result set.
///
/// A failed ODBC call while describing the columns, such as a lost
/// connection, is classified as the ODBC error it wraps.
pub(crate) fn reader_error<C: OdbcConnector>(
    error: arrow_odbc::Error,
    connection: &Connection<'_>,
) -> ConnectorError {
    match error {
        arrow_odbc::Error::UnableToRetrieveNumCols(error)
        | arrow_odbc::Error::ColumnFailure {
            source:
                ColumnFailure::FailedToDescribeColumn(error)
                | ColumnFailure::UnknownStringLength { source: error, .. },
            ..
        } => odbc_error::<C>(&error, Some(connection)),
        // An unsupported column type or a row too large for the transfer
        // buffer: the result cannot be represented.
        _ => ConnectorError::new(
            ErrorKind::Data,
            false,
            "the data source result cannot be read as Arrow",
        ),
    }
}

/// Classifies a failure to read the next batch of a result set.
///
/// The reader reports a fetch failure as the ODBC error it wraps, and a value
/// that cannot be converted exactly as a conversion error.
pub(crate) fn batch_error<C: OdbcConnector>(
    error: ArrowError,
    connection: &Connection<'_>,
) -> ConnectorError {
    match error {
        ArrowError::ExternalError(source) => match source.downcast::<odbc_api::Error>() {
            Ok(error) => odbc_error::<C>(&error, Some(connection)),
            Err(_) => value_error(),
        },
        _ => value_error(),
    }
}

fn value_error() -> ConnectorError {
    ConnectorError::new(
        ErrorKind::Data,
        false,
        "the data source returned a value that cannot be carried exactly",
    )
}

/// The error of a read stopped by a cancellation request.
pub(crate) fn cancelled_error() -> ConnectorError {
    ConnectorError::new(
        ErrorKind::Cancellation,
        false,
        "the data source request was cancelled",
    )
}

/// Builds the connector error for one diagnostic.
///
/// A lost connection is a `connection` error, whatever the driver reported,
/// so that its session becomes unusable. Otherwise `class` is the
/// Connector's classification, when its error table matched, or else the
/// diagnostic is classified by its SQLSTATE class.
pub(crate) fn to_connector_error(
    diagnostic: OdbcDiagnostic,
    class: Option<ErrorClass>,
) -> ConnectorError {
    let class = if diagnostic.connection_lost {
        ErrorClass::new(
            ErrorKind::Connection,
            true,
            "the data source connection was lost",
        )
    } else {
        class.unwrap_or_else(|| classify_sqlstate(diagnostic.sqlstate.as_deref()))
    };
    let mut entries = vec![DiagnosticEntry::new(
        "odbc.native_error",
        DiagnosticValue::I32(diagnostic.native_error),
    )];
    if let Some(sqlstate) = &diagnostic.sqlstate {
        entries.push(DiagnosticEntry::new(
            "odbc.sqlstate",
            DiagnosticValue::Utf8(sqlstate.clone()),
        ));
    }
    ConnectorError::new(class.kind, class.retryable, class.safe_message)
        .with_data_source_error(diagnostic.sqlstate, Some(diagnostic.message))
        .with_diagnostics(entries)
}

/// The generic class of an ODBC SQLSTATE: the ODBC-defined states for
/// cancellation and timeouts, then the SQL-standard classes.
fn classify_sqlstate(sqlstate: Option<&str>) -> ErrorClass {
    match sqlstate {
        Some("HY008") => ErrorClass::new(
            ErrorKind::Cancellation,
            false,
            "the data source request was cancelled",
        ),
        Some("HYT00" | "HYT01") => ErrorClass::new(
            ErrorKind::Timeout,
            true,
            "the data source request timed out",
        ),
        Some(state) if state.starts_with("08") => ErrorClass::new(
            ErrorKind::Connection,
            true,
            "the data source connection failed",
        ),
        Some(state) if state.starts_with("28") => ErrorClass::new(
            ErrorKind::Authentication,
            false,
            "the data source rejected authentication",
        ),
        Some(state) if state.starts_with("22") => ErrorClass::new(
            ErrorKind::DataSource,
            false,
            "the data source could not process a value",
        ),
        Some(state) if state.starts_with("40") => ErrorClass::new(
            ErrorKind::DataSource,
            true,
            "the data source rolled back the request",
        ),
        Some(state) if state.starts_with("42") => ErrorClass::new(
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
