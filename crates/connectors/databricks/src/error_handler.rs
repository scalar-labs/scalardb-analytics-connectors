//! The Databricks error table and the redaction of the ADBC driver's
//! diagnostics.
//!
//! Databricks names each error with an error class, which its messages carry
//! in brackets, such as `[TABLE_OR_VIEW_NOT_FOUND]`, and its REST responses as
//! an error code, such as `TEMPORARILY_UNAVAILABLE`. The table reads the
//! names from the message and the SQLSTATE beside them.

use connector_adbc::AdbcDiagnostic;
use connector_sdk::{ErrorClass, ErrorHandler, ErrorKind};

/// The Databricks connector's handling of the ADBC driver's errors.
pub struct DatabricksErrorHandler;

/// The error class of a syntax error, whose message quotes the statement
/// from where parsing stopped; the statement carries filter literals.
const PARSE_SYNTAX_ERROR: &str = "PARSE_SYNTAX_ERROR";

impl ErrorHandler for DatabricksErrorHandler {
    type Diagnostic = AdbcDiagnostic;

    /// The Databricks error table, applied before the SQLSTATE classes.
    fn classify(diagnostic: &AdbcDiagnostic) -> Option<ErrorClass> {
        classify(diagnostic.sqlstate_bytes(), diagnostic.message())
    }

    /// Keeps the first line of a message, since Databricks adds the statement
    /// or its plan on later lines, and removes a syntax error's text, which
    /// quotes the statement.
    fn redact(diagnostic: AdbcDiagnostic) -> AdbcDiagnostic {
        let message = diagnostic.message();
        let first_line = message.lines().next().unwrap_or_default();
        let redacted = if let Some(start) = first_line.find(PARSE_SYNTAX_ERROR) {
            let end = start + PARSE_SYNTAX_ERROR.len();
            let end = end + usize::from(first_line[end..].starts_with(']'));
            format!("{}: the statement's text was removed", &first_line[..end])
        } else if first_line.len() == message.len() {
            return diagnostic;
        } else {
            format!("{first_line} (later lines removed)")
        };
        AdbcDiagnostic::new(
            diagnostic.status(),
            redacted,
            *diagnostic.sqlstate_bytes(),
            diagnostic.vendor_code(),
            diagnostic.details().clone(),
        )
    }
}

/// How the driver begins the message of a failure while it connects, when it
/// checks the new session with a first request. It reports such failures as
/// `internal`, whatever their cause, and passes only their text.
const CONNECT_FAILURE: &str = "failed to ping database:";

/// How databricks-sql-go names a Thrift call whose HTTP request to the
/// workspace failed, whether the connection broke or the response was not
/// `200 OK`; it names a call that the workspace answered with an error
/// otherwise. The driver reports such failures as `internal`.
const FAILED_REQUESTS: [&str; 8] = [
    "open session request error",
    "close session request error",
    "execute statement request error",
    "get operation status request error",
    "get result set metadata request error",
    "fetch results request error",
    "cancel operation request error",
    "close operation request error",
];

/// Classifies a Databricks error by the error classes or codes its message
/// names, by the HTTP status or OAuth error a failed connection reports, and
/// by its SQLSTATE; `None` leaves it to the SQLSTATE classes and the ADBC
/// status.
///
/// A missing table, schema, or catalog is `not found`, not `does not exist`,
/// because Unity Catalog reports an object the principal holds no privilege
/// on the same way. Any other failure while connecting, and any HTTP request
/// to the workspace that failed, is a `connection` error, which makes the
/// session unusable: the driver has lost track of the statement it ran.
pub(crate) fn classify(sqlstate: &[u8; 5], message: &str) -> Option<ErrorClass> {
    let by_name = error_names(message).find_map(|name| {
        Some(match name {
            "UNAUTHENTICATED" => rejected_credentials(),
            "INSUFFICIENT_PERMISSIONS" | "PERMISSION_DENIED" => permission_denied(),
            "TABLE_OR_VIEW_NOT_FOUND" | "SCHEMA_NOT_FOUND" | "CATALOG_NOT_FOUND" => not_found(),
            "TEMPORARILY_UNAVAILABLE" | "RESOURCE_EXHAUSTED" | "REQUEST_LIMIT_EXCEEDED" => {
                ErrorClass::new(
                    ErrorKind::Resource,
                    true,
                    "the workspace or SQL warehouse is busy or temporarily unavailable",
                )
            }
            "INTERNAL_ERROR" => ErrorClass::new(
                ErrorKind::DataSource,
                true,
                "the workspace failed with an internal error",
            ),
            _ => return None,
        })
    });
    by_name
        .or_else(|| http_failure(message))
        .or_else(|| match sqlstate {
            b"42501" => Some(permission_denied()),
            b"42P01" => Some(not_found()),
            _ => None,
        })
        .or_else(|| {
            let failed = message.starts_with(CONNECT_FAILURE)
                || FAILED_REQUESTS.iter().any(|call| message.contains(call));
            failed.then(|| {
                ErrorClass::new(
                    ErrorKind::Connection,
                    true,
                    "the connection to the workspace failed",
                )
            })
        })
}

/// Classifies the HTTP status that databricks-sql-go reports for a rejected
/// request, `unexpected HTTP status 401 Unauthorized`, and the OAuth token
/// endpoint's rejection of the client credentials, as golang.org/x/oauth2
/// reports it.
fn http_failure(message: &str) -> Option<ErrorClass> {
    let status = message
        .split("unexpected HTTP status ")
        .nth(1)
        .and_then(|rest| rest.get(..3));
    let rejected_client = [
        "oauth2: \"invalid_client\"",
        "oauth2: cannot fetch token: 401",
    ]
    .iter()
    .any(|text| message.contains(text));
    match status {
        Some("401") => Some(rejected_credentials()),
        Some("403") => Some(permission_denied()),
        _ => rejected_client.then(rejected_credentials),
    }
}

fn rejected_credentials() -> ErrorClass {
    ErrorClass::new(
        ErrorKind::Authentication,
        false,
        "the workspace rejected the credentials",
    )
}

fn permission_denied() -> ErrorClass {
    ErrorClass::new(
        ErrorKind::PermissionDenied,
        false,
        "the principal lacks a privilege the request needs",
    )
}

fn not_found() -> ErrorClass {
    ErrorClass::new(
        ErrorKind::NotFound,
        false,
        "a registered table is not found; the principal may not see it",
    )
}

/// The names of `message` that can name its error class or code.
///
/// A Databricks SQL error states its class first, in brackets, and its text
/// after the class can quote identifiers and values, such as a column named
/// `INTERNAL_ERROR` or a malformed value `'TEMPORARILY_UNAVAILABLE'`, so a
/// message with a bracketed class names that class only. Any other message,
/// such as a REST error code, names each of its [`name_words`].
fn error_names(message: &str) -> impl Iterator<Item = &str> {
    let class = message.split('[').skip(1).find_map(|rest| {
        let (inside, _) = rest.split_once(']')?;
        let name = name_words(inside).next()?;
        (inside.split('.').next() == Some(name)).then_some(name)
    });
    let words = class
        .is_none()
        .then(|| name_words(message))
        .into_iter()
        .flatten();
    class.into_iter().chain(words)
}

/// The words of `message` that can name an error class or code: runs of
/// upper-case letters, digits, underscores, and dots that start with a
/// letter and contain an underscore or are a known one-word code, such as
/// `TABLE_OR_VIEW_NOT_FOUND` or `UNRESOLVED_COLUMN.WITH_SUGGESTION`. A class
/// with a subclass after a dot names its main class.
fn name_words(message: &str) -> impl Iterator<Item = &str> {
    message
        .split(|character: char| {
            !(character.is_ascii_uppercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == '.')
        })
        .map(|word| word.split('.').next().unwrap_or_default())
        .filter(|word| {
            word.starts_with(|character: char| character.is_ascii_uppercase())
                && (word.contains('_') || *word == "UNAUTHENTICATED")
        })
}

#[cfg(test)]
mod tests {
    use adbc_core::error::Status;
    use connector_adbc::{AdbcDetails, AdbcStatus};
    use connector_sdk::Connector;

    use super::*;
    use crate::DatabricksConnector;

    fn diagnostic(status: Status, sqlstate: &[u8; 5], message: &str) -> AdbcDiagnostic {
        AdbcDiagnostic::new(
            AdbcStatus::Known(status),
            message,
            *sqlstate,
            0,
            AdbcDetails::Missing,
        )
    }

    fn kind(status: Status, sqlstate: &[u8; 5], message: &str) -> (ErrorKind, bool) {
        let error = DatabricksConnector::classify(diagnostic(status, sqlstate, message));
        (error.kind(), error.retryable())
    }

    #[test]
    fn error_classes_in_messages_decide_the_kind() {
        for (sqlstate, message, expected) in [
            (
                b"42P01",
                "[TABLE_OR_VIEW_NOT_FOUND] The table or view `main`.`s`.`t` cannot be found. \
                 SQLSTATE: 42P01",
                (ErrorKind::NotFound, false),
            ),
            (
                b"42704",
                "[SCHEMA_NOT_FOUND] The schema `main`.`gone` cannot be found. SQLSTATE: 42704",
                (ErrorKind::NotFound, false),
            ),
            (
                b"42704",
                "[CATALOG_NOT_FOUND] The catalog `gone` cannot be found. SQLSTATE: 42704",
                (ErrorKind::NotFound, false),
            ),
            (
                b"42501",
                "[INSUFFICIENT_PERMISSIONS] Insufficient privileges: User does not have \
                 SELECT on Table 'main.s.t'. SQLSTATE: 42501",
                (ErrorKind::PermissionDenied, false),
            ),
            (
                b"\0\0\0\0\0",
                "PERMISSION_DENIED: User is not authorized to use this SQL Endpoint",
                (ErrorKind::PermissionDenied, false),
            ),
            (
                b"\0\0\0\0\0",
                "UNAUTHENTICATED: Invalid access token.",
                (ErrorKind::Authentication, false),
            ),
            (
                b"\0\0\0\0\0",
                "TEMPORARILY_UNAVAILABLE: the SQL warehouse is starting",
                (ErrorKind::Resource, true),
            ),
            (
                b"\0\0\0\0\0",
                "{\"error_code\":\"REQUEST_LIMIT_EXCEEDED\",\"message\":\"too many requests\"}",
                (ErrorKind::Resource, true),
            ),
            (
                b"\0\0\0\0\0",
                "[RESOURCE_EXHAUSTED] Query queue is full",
                (ErrorKind::Resource, true),
            ),
            (
                b"XX000",
                "[INTERNAL_ERROR] Unexpected failure. SQLSTATE: XX000",
                (ErrorKind::DataSource, true),
            ),
        ] {
            assert_eq!(
                kind(Status::Unknown, sqlstate, message),
                expected,
                "{message}"
            );
        }
    }

    #[test]
    fn sqlstates_decide_when_no_class_matches() {
        assert_eq!(
            kind(Status::Unknown, b"42501", "denied"),
            (ErrorKind::PermissionDenied, false)
        );
        assert_eq!(
            kind(Status::Unknown, b"42P01", "missing"),
            (ErrorKind::NotFound, false)
        );
    }

    #[test]
    fn other_errors_fall_back_to_sqlstate_classes_and_adbc_status() {
        assert_eq!(
            kind(
                Status::Unknown,
                b"42703",
                "[UNRESOLVED_COLUMN.WITH_SUGGESTION] A column with name `x` cannot be resolved."
            ),
            (ErrorKind::Internal, false)
        );
        assert_eq!(
            kind(Status::Unauthenticated, b"\0\0\0\0\0", "HTTP 401"),
            (ErrorKind::Authentication, false)
        );
        assert_eq!(
            kind(Status::Unauthorized, b"\0\0\0\0\0", "HTTP 403"),
            (ErrorKind::PermissionDenied, false)
        );
        assert_eq!(
            kind(Status::IO, b"\0\0\0\0\0", "connection reset"),
            (ErrorKind::Connection, true)
        );
        assert_eq!(
            kind(Status::Timeout, b"\0\0\0\0\0", "deadline exceeded"),
            (ErrorKind::Timeout, true)
        );
        assert_eq!(
            kind(Status::Unknown, b"22003", "[ARITHMETIC_OVERFLOW] overflow"),
            (ErrorKind::DataSource, false)
        );
    }

    #[test]
    fn failures_while_connecting_are_classified_by_their_cause() {
        let connecting = |cause: &str| {
            format!(
                "failed to ping database: databricks: request error: error connecting: \
                 host=adb-1.2.azuredatabricks.net port=443, httpPath=/sql/1.0/warehouses/abc: \
                 databricks: request error: open session request error: {cause}"
            )
        };
        for (cause, expected) in [
            (
                "Post \"https://adb-1.2.azuredatabricks.net:443/sql/1.0/warehouses/abc\": \
                 request error after 1 attempt(s): unexpected HTTP status 401 Unauthorized",
                (ErrorKind::Authentication, false),
            ),
            (
                "request error after 1 attempt(s): unexpected HTTP status 403 Forbidden",
                (ErrorKind::PermissionDenied, false),
            ),
            (
                "Post \"https://adb-1.2.azuredatabricks.net:443/sql/1.0/warehouses/abc\": \
                 oauth2: \"invalid_client\" \"Client authentication failed\"",
                (ErrorKind::Authentication, false),
            ),
            (
                "request error after 1 attempt(s): TEMPORARILY_UNAVAILABLE: warehouse starting",
                (ErrorKind::Resource, true),
            ),
            (
                "Post \"https://127.0.0.1:4013/sql/1.0/warehouses/abc\": request error after \
                 1 attempt(s): dial tcp 127.0.0.1:4013: connect: connection refused",
                (ErrorKind::Connection, true),
            ),
            (
                "Post \"https://127.0.0.1:4013/sql/1.0/warehouses/abc\": EOF",
                (ErrorKind::Connection, true),
            ),
        ] {
            assert_eq!(
                kind(Status::Internal, b"\0\0\0\0\0", &connecting(cause)),
                expected,
                "{cause}"
            );
        }
        // The same words without a failed request leave the ADBC status.
        assert_eq!(
            kind(Status::Internal, b"\0\0\0\0\0", "EOF"),
            (ErrorKind::Internal, false)
        );
    }

    /// The driver's message for a statement that failed, as it words it:
    /// its own prefix, then databricks-sql-go's execution error around
    /// `cause`.
    fn failed_statement(cause: &str) -> String {
        format!(
            "[Databricks] failed to execute query: databricks: execution error: \
             failed to execute query: {cause}"
        )
    }

    #[test]
    fn statement_failures_are_classified_as_the_driver_words_them() {
        for (sqlstate, cause, expected) in [
            (
                b"42P01",
                "unexpected operation state ERROR_STATE: [TABLE_OR_VIEW_NOT_FOUND] The table \
                 or view `main`.`s`.`t` cannot be found. SQLSTATE: 42P01; line 1 pos 14",
                (ErrorKind::NotFound, false),
            ),
            (
                b"42501",
                "unexpected operation state ERROR_STATE: [INSUFFICIENT_PERMISSIONS] \
                 Insufficient privileges: User does not have SELECT on Table 'main.s.t'. \
                 SQLSTATE: 42501",
                (ErrorKind::PermissionDenied, false),
            ),
            (
                b"\0\0\0\0\0",
                "databricks: request error: get operation status request error: Post \
                 \"https://adb-1.2.azuredatabricks.net:443/sql/1.0/warehouses/abc\": \
                 request error after 1 attempt(s): EOF",
                (ErrorKind::Connection, true),
            ),
            (
                b"\0\0\0\0\0",
                "databricks: request error: execute statement request error: Invalid \
                 SessionHandle",
                (ErrorKind::Connection, true),
            ),
            (
                b"\0\0\0\0\0",
                "databricks: request error: execute statement request error: Post \
                 \"https://adb-1.2.azuredatabricks.net:443/sql/1.0/warehouses/abc\": \
                 request error after 1 attempt(s): unexpected HTTP status 401 Unauthorized",
                (ErrorKind::Authentication, false),
            ),
        ] {
            let message = failed_statement(cause);
            assert_eq!(
                kind(Status::Internal, sqlstate, &message),
                expected,
                "{message}"
            );
        }
    }

    #[test]
    fn a_failed_request_in_a_result_stream_is_a_broken_connection() {
        let message = "databricks: request error: databricks: Rows instance failed to retrieve \
                       results: databricks: request error: fetch results request error: Post \
                       \"https://adb-1.2.azuredatabricks.net:443/sql/1.0/warehouses/abc\": read \
                       tcp 10.0.0.1:50000->10.0.0.2:443: read: connection reset by peer";
        assert_eq!(
            kind(Status::Unknown, b"\0\0\0\0\0", message),
            (ErrorKind::Connection, true)
        );
    }

    #[test]
    fn error_names_are_read_from_brackets_codes_and_subclasses() {
        assert_eq!(
            error_names("[UNRESOLVED_COLUMN.WITH_SUGGESTION] x SQLSTATE: 42703")
                .collect::<Vec<_>>(),
            ["UNRESOLVED_COLUMN"]
        );
        assert_eq!(
            error_names("Error: UNAUTHENTICATED, see TEMPORARILY_UNAVAILABLE").collect::<Vec<_>>(),
            ["UNAUTHENTICATED", "TEMPORARILY_UNAVAILABLE"]
        );
        assert_eq!(error_names("table_or_view_not_found").count(), 0);
        assert_eq!(
            error_names("unexpected operation state ERROR_STATE: [TABLE_OR_VIEW_NOT_FOUND] x")
                .collect::<Vec<_>>(),
            ["TABLE_OR_VIEW_NOT_FOUND"]
        );
    }

    #[test]
    fn names_quoted_after_a_bracketed_class_do_not_decide_the_kind() {
        assert_eq!(
            kind(
                Status::Unknown,
                b"22018",
                "[CAST_INVALID_INPUT] The value 'TEMPORARILY_UNAVAILABLE' of the type \
                 \"STRING\" cannot be cast to \"INT\" because it is malformed. SQLSTATE: 22018"
            ),
            kind(Status::Unknown, b"22018", "[CAST_INVALID_INPUT] malformed")
        );
        assert_eq!(
            kind(
                Status::Unknown,
                b"42703",
                "[UNRESOLVED_COLUMN.WITH_SUGGESTION] A column with name `INTERNAL_ERROR` \
                 cannot be resolved. SQLSTATE: 42703"
            ),
            (ErrorKind::Internal, false)
        );
    }

    #[test]
    fn statement_text_never_leaves_the_connector() {
        let error = DatabricksConnector::classify(diagnostic(
            Status::InvalidArguments,
            b"42601",
            "[PARSE_SYNTAX_ERROR] Syntax error at or near ''secret''. SQLSTATE: 42601 \
             (line 1, pos 40)\n\n== SQL ==\nSELECT * FROM t WHERE c = 'secret'\n----^^^",
        ));
        let message = error.data_source_message().unwrap();
        assert!(!message.contains("secret"), "{message}");
        assert!(message.contains("PARSE_SYNTAX_ERROR"), "{message}");
        assert_eq!(error.kind(), ErrorKind::Internal);

        let error = DatabricksConnector::classify(diagnostic(
            Status::Unknown,
            b"42703",
            "[UNRESOLVED_COLUMN.WITH_SUGGESTION] A column with name `x` cannot be resolved. \
             SQLSTATE: 42703;\n'Filter ('c = secret)\n+- 'UnresolvedRelation [t]",
        ));
        let message = error.data_source_message().unwrap();
        assert!(!message.contains("secret"), "{message}");
        assert!(message.contains("UNRESOLVED_COLUMN"), "{message}");
    }

    #[test]
    fn a_syntax_error_loses_its_text_after_the_driver_prefix() {
        let error = DatabricksConnector::classify(diagnostic(
            Status::Internal,
            b"42601",
            &failed_statement(
                "unexpected operation state ERROR_STATE: [PARSE_SYNTAX_ERROR] Syntax error at \
                 or near ''secret''. SQLSTATE: 42601 (line 1, pos 40)",
            ),
        ));
        let message = error.data_source_message().unwrap();
        assert!(!message.contains("secret"), "{message}");
        assert!(message.ends_with("[PARSE_SYNTAX_ERROR]: the statement's text was removed"));
    }

    #[test]
    fn one_line_messages_are_kept() {
        let text = "[TABLE_OR_VIEW_NOT_FOUND] The table or view `main`.`s`.`t` cannot be found.";
        let error = DatabricksConnector::classify(diagnostic(Status::NotFound, b"42P01", text));
        assert_eq!(error.data_source_message(), Some(text));
    }
}
