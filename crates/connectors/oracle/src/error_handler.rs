//! The Oracle error table.
//!
//! The ODBC driver reports Oracle's `ORA-` error number as the native error,
//! and many of them with a SQLSTATE that does not match their class, such as
//! `HY003` for a missing privilege or `3F000` for a rejected password, so they
//! are classified by number. Errors the table does not list are classified by
//! their SQLSTATE class.

use connector_odbc::OdbcDiagnostic;
use connector_sdk::{ErrorClass, ErrorHandler, ErrorKind};

/// The Oracle connector's handling of Oracle's errors.
pub struct OracleErrorHandler;

impl ErrorHandler for OracleErrorHandler {
    type Diagnostic = OdbcDiagnostic;

    /// The Oracle error table, applied before the SQLSTATE classes.
    ///
    /// The diagnostics are kept as they are. Oracle quotes values in
    /// conversion errors (such as ORA-01722 and ORA-01858), but no statement
    /// the connector sends can raise them: names are quoted, literals are
    /// typed, and only comparisons between operands of one type run remotely,
    /// with no text among them. Redaction is needed once one can.
    fn classify(diagnostic: &OdbcDiagnostic) -> Option<ErrorClass> {
        let class = match diagnostic.native_error {
            // Invalid user name or password, a locked account, an expired
            // password, and a user without CREATE SESSION.
            1017 | 28000 | 28001 | 1045 => ErrorClass::new(
                ErrorKind::Authentication,
                false,
                "the data source rejected authentication",
            ),
            // Insufficient privileges, and since Oracle 23ai a missing READ
            // privilege, on an object the credential can see.
            1031 | 41900 => ErrorClass::new(
                ErrorKind::PermissionDenied,
                false,
                "the data source denied access to the requested target",
            ),
            // Table or view does not exist. Oracle reports a table the
            // credential has no privilege on the same way, so the table may
            // exist.
            942 => ErrorClass::new(
                ErrorKind::NotFound,
                false,
                "a registered table is not found at the data source, which hides tables \
                 the credential has no privilege on",
            ),
            // Invalid identifier: a column of a table the credential can see,
            // all of whose columns Oracle shows, so the column does not exist.
            904 => ErrorClass::new(
                ErrorKind::DoesNotExist,
                false,
                "a registered column no longer exists at the data source",
            ),
            // Out of process or shared memory, the PGA limit, or temporary
            // space.
            4030 | 4031 | 4036 | 1652 => ErrorClass::new(
                ErrorKind::Resource,
                true,
                "the data source ran out of resources",
            ),
            _ => return None,
        };
        Some(class)
    }
}
