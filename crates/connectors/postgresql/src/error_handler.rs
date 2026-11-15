//! The PostgreSQL error table and the redaction of the ADBC driver's diagnostics.

use adbc_core::error::Status;
use connector_adbc::{AdbcDetail, AdbcDetails, AdbcDiagnostic, AdbcStatus};
use connector_sdk::{ErrorClass, ErrorHandler, ErrorKind};

/// The PostgreSQL connector's handling of the ADBC driver's errors.
pub struct PostgresErrorHandler;

/// Error detail keys whose values hold neither SQL text nor data values; only
/// these stay in the error's diagnostics.
///
/// The others leave: the statement position, internal query, and context can
/// quote the connector's SQL, which carries filter literals, and the primary
/// message, detail, and hint can quote data values. The primary message,
/// detail, and hint are kept only in the Data Source message.
const VALUE_FREE_DETAILS: &[&str] = &[
    "PG_DIAG_SEVERITY",
    "PG_DIAG_SEVERITY_NONLOCALIZED",
    "PG_DIAG_SQLSTATE",
    "PG_DIAG_SCHEMA_NAME",
    "PG_DIAG_TABLE_NAME",
    "PG_DIAG_COLUMN_NAME",
    "PG_DIAG_DATATYPE_NAME",
    "PG_DIAG_CONSTRAINT_NAME",
    "PG_DIAG_SOURCE_FILE",
    "PG_DIAG_SOURCE_LINE",
    "PG_DIAG_SOURCE_FUNCTION",
];

impl ErrorHandler for PostgresErrorHandler {
    type Diagnostic = AdbcDiagnostic;

    /// The PostgreSQL error table, applied before the SQLSTATE classes.
    fn classify(diagnostic: &AdbcDiagnostic) -> Option<ErrorClass> {
        let sqlstate = diagnostic.sqlstate_bytes();
        let class = match sqlstate {
            b"42501" => ErrorClass {
                kind: ErrorKind::PermissionDenied,
                retryable: false,
                safe_message: "the data source denied access to the requested target",
            },
            b"42P01" | b"42703" => ErrorClass {
                kind: ErrorKind::DoesNotExist,
                retryable: false,
                safe_message: "a registered table or column no longer exists at the data source",
            },
            [b'5', b'3', ..] => ErrorClass {
                kind: ErrorKind::Resource,
                retryable: true,
                safe_message: "the data source ran out of resources",
            },
            // libpq does not expose the SQLSTATE of a failed connection, so
            // the driver reports a rejected credential like an unreachable
            // server or a broken connection. Retrying a rejected credential
            // cannot succeed, so none of them is retryable.
            [0, 0, 0, 0, 0] | b"00000" if diagnostic.status() == AdbcStatus::Known(Status::IO) => {
                ErrorClass {
                    kind: ErrorKind::Connection,
                    retryable: false,
                    safe_message: "the data source connection failed, or the data source rejected the credential",
                }
            }
            _ => return None,
        };
        Some(class)
    }

    /// Rebuilds the message from the server's own fields.
    ///
    /// The driver copies the SQL into its messages (`Query was: ...`) and libpq
    /// adds an excerpt of it (`LINE 1: ...`). The server's primary message,
    /// detail, and hint never quote the statement, so they replace the
    /// driver's message. A driver message without those fields is kept only
    /// when it does not come from running a statement.
    fn redact(diagnostic: AdbcDiagnostic) -> AdbcDiagnostic {
        let details = match diagnostic.details() {
            AdbcDetails::Present(details) => AdbcDetails::Present(redact_details(details)),
            AdbcDetails::Malformed { captured, reason } => AdbcDetails::Malformed {
                captured: redact_details(captured),
                reason: reason.clone(),
            },
            AdbcDetails::Missing => AdbcDetails::Missing,
        };
        let message = server_message(diagnostic.details()).unwrap_or_else(|| {
            if mentions_statement(diagnostic.message()) {
                "the data source rejected a statement; its message quoted the statement and was removed"
                    .to_owned()
            } else {
                diagnostic.message().to_owned()
            }
        });
        AdbcDiagnostic::new(
            diagnostic.status(),
            message,
            *diagnostic.sqlstate_bytes(),
            diagnostic.vendor_code(),
            details,
        )
    }
}

fn redact_details(details: &[AdbcDetail]) -> Vec<AdbcDetail> {
    details
        .iter()
        .filter(|detail| VALUE_FREE_DETAILS.contains(&detail.key()))
        .cloned()
        .collect()
}

/// Composes the server's primary message with its detail and hint, if present.
fn server_message(details: &AdbcDetails) -> Option<String> {
    let AdbcDetails::Present(details) = details else {
        return None;
    };
    let field = |key: &str| {
        details
            .iter()
            .find(|detail| detail.key() == key)
            .map(|detail| String::from_utf8_lossy(detail.value()).into_owned())
    };
    let mut message = field("PG_DIAG_MESSAGE_PRIMARY")?;
    if let Some(detail) = field("PG_DIAG_MESSAGE_DETAIL") {
        message.push_str("\nDETAIL: ");
        message.push_str(&detail);
    }
    if let Some(hint) = field("PG_DIAG_MESSAGE_HINT") {
        message.push_str("\nHINT: ");
        message.push_str(&hint);
    }
    Some(message)
}

/// Whether a driver message is one of the forms that quote the statement.
fn mentions_statement(message: &str) -> bool {
    message.contains("Query was:")
        || message.contains("execute query '")
        || message.contains("LINE ")
}

#[cfg(test)]
mod tests {
    use connector_sdk::DiagnosticValue;

    use super::*;
    use connector_sdk::Connector;

    use crate::PostgresConnector;

    fn driver_error(sqlstate: &[u8; 5], message: &str, details: &[(&str, &str)]) -> AdbcDiagnostic {
        AdbcDiagnostic::new(
            AdbcStatus::Known(Status::InvalidData),
            message,
            *sqlstate,
            0,
            AdbcDetails::Present(
                details
                    .iter()
                    .map(|(key, value)| AdbcDetail::new(*key, value.as_bytes()))
                    .collect(),
            ),
        )
    }

    #[test]
    fn the_postgresql_error_table_precedes_the_sqlstate_classes() {
        for (sqlstate, kind) in [
            (b"42501", ErrorKind::PermissionDenied),
            (b"42P01", ErrorKind::DoesNotExist),
            (b"42703", ErrorKind::DoesNotExist),
            (b"53200", ErrorKind::Resource),
            (b"57014", ErrorKind::DataSource),
            (b"42601", ErrorKind::Internal),
        ] {
            let error = PostgresConnector::classify(driver_error(sqlstate, "failed", &[]));
            assert_eq!(error.kind(), kind, "{sqlstate:?}");
        }
    }

    #[test]
    fn statement_text_never_leaves_the_connector() {
        let error = PostgresConnector::classify(driver_error(
            b"42703",
            "Failed to prepare query: ERROR:  column \"x\" does not exist\nLINE 1: select x, 'secret' from t\n\nQuery was: select x, 'secret' from t",
            &[
                ("PG_DIAG_MESSAGE_PRIMARY", "column \"x\" does not exist"),
                ("PG_DIAG_MESSAGE_HINT", "Perhaps you meant \"y\"."),
                ("PG_DIAG_STATEMENT_POSITION", "8"),
                ("PG_DIAG_INTERNAL_QUERY", "select 'secret'"),
                ("PG_DIAG_SQLSTATE", "42703"),
            ],
        ));
        assert_eq!(
            error.data_source_message(),
            Some("column \"x\" does not exist\nHINT: Perhaps you meant \"y\".")
        );
        let keys: Vec<&str> = error
            .diagnostics()
            .iter()
            .map(|entry| entry.key())
            .collect();
        assert!(keys.contains(&"adbc.detail/PG_DIAG_SQLSTATE"), "{keys:?}");
        for entry in error.diagnostics() {
            assert!(!entry.key().contains("POSITION") && !entry.key().contains("QUERY"));
            // The server's message, detail, and hint stay out of diagnostics.
            assert!(!entry.key().contains("MESSAGE"), "{}", entry.key());
            if let DiagnosticValue::Bytes(value) = entry.value() {
                assert!(!String::from_utf8_lossy(value).contains("secret"));
            }
        }

        let without_server_fields = PostgresConnector::classify(driver_error(
            b"HY000",
            "[libpq] Failed to execute query 'select 1 where x = ''secret''': lost",
            &[],
        ));
        assert!(
            !without_server_fields
                .data_source_message()
                .unwrap()
                .contains("secret")
        );
    }

    #[test]
    fn an_io_error_without_sqlstate_is_a_connection_error_that_is_not_retried() {
        let error = PostgresConnector::classify(AdbcDiagnostic::new(
            AdbcStatus::Known(Status::IO),
            "[libpq] Failed to connect: password authentication failed",
            [0; 5],
            0,
            AdbcDetails::Missing,
        ));
        assert_eq!(error.kind(), ErrorKind::Connection);
        assert!(!error.retryable());
    }

    #[test]
    fn messages_that_quote_no_statement_are_kept() {
        let error = PostgresConnector::classify(AdbcDiagnostic::new(
            AdbcStatus::Known(Status::IO),
            "[libpq] Failed to connect: password authentication failed",
            [0; 5],
            0,
            AdbcDetails::Missing,
        ));
        assert_eq!(error.kind(), ErrorKind::Connection);
        assert_eq!(
            error.data_source_message(),
            Some("[libpq] Failed to connect: password authentication failed")
        );
    }
}
