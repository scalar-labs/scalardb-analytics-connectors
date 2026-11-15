//! The MySQL error table and the redaction of the ADBC driver's diagnostics.
//!
//! The driver sets no vendor code. It reports the server's error number in
//! its message, which ends with the server's own message in the form
//! `Error <number> (<SQLSTATE>): <text>`, so the error table reads the number
//! from there.

use connector_adbc::AdbcDiagnostic;
use connector_sdk::{ErrorClass, ErrorHandler, ErrorKind};

/// The MySQL connector's handling of the ADBC driver's errors.
pub struct MysqlErrorHandler;

/// Server errors whose message quotes the statement, which carries filter
/// literals: `ER_PARSE_ERROR` and `ER_SYNTAX_ERROR`, which quote it from
/// where parsing stopped.
const STATEMENT_QUOTING_ERRORS: [u32; 2] = [1064, 1149];

impl ErrorHandler for MysqlErrorHandler {
    type Diagnostic = AdbcDiagnostic;

    /// The MySQL error table, applied before the SQLSTATE classes.
    fn classify(diagnostic: &AdbcDiagnostic) -> Option<ErrorClass> {
        let class = match server_error(diagnostic.message())?.number {
            // ER_TABLEACCESS_DENIED_ERROR, ER_COLUMNACCESS_DENIED_ERROR
            1142 | 1143 => ErrorClass::new(
                ErrorKind::PermissionDenied,
                false,
                "the data source denied access to the requested target",
            ),
            // ER_NO_SUCH_TABLE, ER_BAD_FIELD_ERROR: MySQL checks privileges
            // first, so these confirm that the target does not exist.
            1146 | 1054 => ErrorClass::new(
                ErrorKind::DoesNotExist,
                false,
                "a registered table or column no longer exists at the data source",
            ),
            // ER_QUERY_TIMEOUT: `max_execution_time` was exceeded.
            3024 => ErrorClass::new(
                ErrorKind::Timeout,
                true,
                "the data source stopped the request at its execution time limit",
            ),
            _ => return None,
        };
        Some(class)
    }

    /// Removes the statement from the messages that quote it.
    fn redact(diagnostic: AdbcDiagnostic) -> AdbcDiagnostic {
        let Some(error) = server_error(diagnostic.message())
            .filter(|error| STATEMENT_QUOTING_ERRORS.contains(&error.number))
        else {
            return diagnostic;
        };
        let message = format!(
            "{}: the server rejected the statement's syntax; its message quoted the statement \
             and was removed",
            &diagnostic.message()[..error.text_start - 2]
        );
        AdbcDiagnostic::new(
            diagnostic.status(),
            message,
            *diagnostic.sqlstate_bytes(),
            diagnostic.vendor_code(),
            diagnostic.details().clone(),
        )
    }
}

/// The server's error in a driver message.
struct ServerError {
    number: u32,
    /// Where the server's own text starts in the message, after `): ` or `: `.
    text_start: usize,
}

/// Finds `Error <number> (<SQLSTATE>): ` or `Error <number>: ` in a driver
/// message.
fn server_error(message: &str) -> Option<ServerError> {
    let (start, _) = message.match_indices("Error ").find(|(start, _)| {
        message[start + 6..]
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
    })?;
    let rest = &message[start + 6..];
    let digits = rest.find(|character: char| !character.is_ascii_digit())?;
    let number = rest[..digits].parse().ok()?;
    let after = &rest[digits..];
    let separator = if after.starts_with(" (") {
        after.find("): ")? + 3
    } else if after.starts_with(": ") {
        2
    } else {
        return None;
    };
    Some(ServerError {
        number,
        text_start: start + 6 + digits + separator,
    })
}

#[cfg(test)]
mod tests {
    use adbc_core::error::Status;
    use connector_adbc::{AdbcDetails, AdbcStatus};
    use connector_sdk::Connector;

    use super::*;
    use crate::MysqlConnector;

    fn driver_error(status: Status, sqlstate: &[u8; 5], message: &str) -> AdbcDiagnostic {
        AdbcDiagnostic::new(
            AdbcStatus::Known(status),
            message,
            *sqlstate,
            i32::MIN,
            AdbcDetails::Present(Vec::new()),
        )
    }

    fn statement_error(number: u32, sqlstate: &str, text: &str) -> AdbcDiagnostic {
        driver_error(
            Status::InvalidArguments,
            sqlstate.as_bytes().try_into().unwrap(),
            &format!(
                "[MySQL] failed to create record reader: failed to execute query: Error {number} ({sqlstate}): {text}"
            ),
        )
    }

    #[test]
    fn the_mysql_error_table_precedes_the_sqlstate_classes() {
        for (number, sqlstate, kind) in [
            (1142, "42000", ErrorKind::PermissionDenied),
            (1143, "42000", ErrorKind::PermissionDenied),
            (1146, "42S02", ErrorKind::DoesNotExist),
            (1054, "42S22", ErrorKind::DoesNotExist),
            (3024, "HY000", ErrorKind::Timeout),
            (1064, "42000", ErrorKind::Internal),
            (1045, "28000", ErrorKind::Authentication),
        ] {
            let error = MysqlConnector::classify(statement_error(number, sqlstate, "failed"));
            assert_eq!(error.kind(), kind, "{number}");
        }
    }

    #[test]
    fn statement_text_never_leaves_the_connector() {
        let error = MysqlConnector::classify(statement_error(
            1064,
            "42000",
            "You have an error in your SQL syntax; check the manual that corresponds to your \
             MySQL server version for the right syntax to use near ''secret'' at line 1",
        ));
        let message = error.data_source_message().unwrap();
        assert!(!message.contains("secret"), "{message}");
        assert!(message.contains("Error 1064 (42000)"), "{message}");
    }

    #[test]
    fn messages_that_quote_no_statement_are_kept() {
        let text = "[MySQL] failed to create record reader: failed to execute query: \
                    Error 1146 (42S02): Table 'db.t' doesn't exist";
        let error = MysqlConnector::classify(driver_error(Status::NotFound, b"42S02", text));
        assert_eq!(error.data_source_message(), Some(text));
        assert!(server_error("Error: not a server error").is_none());
        assert_eq!(
            server_error("Error 2013: Lost connection").unwrap().number,
            2013
        );
    }
}
