//! The SQL Server error table.
//!
//! SQL Server reports most errors of a statement with SQLSTATE `42000`, so
//! they are told apart by the native error number. Errors it does not list
//! are classified by their SQLSTATE class.

use connector_odbc::OdbcDiagnostic;
use connector_sdk::{ErrorClass, ErrorHandler, ErrorKind};

/// The SQL Server through ODBC connector's handling of SQL Server's errors.
pub struct SqlServerErrorHandler;

impl ErrorHandler for SqlServerErrorHandler {
    type Diagnostic = OdbcDiagnostic;

    /// The SQL Server error table, applied before the SQLSTATE classes.
    ///
    /// The diagnostics are kept as they are. SQL Server quotes statement
    /// fragments in syntax errors and values in conversion and truncation
    /// errors (such as 102, 105, 245, and 8152), but no statement the
    /// connector sends can raise them: names are quoted, literals are typed,
    /// and only comparisons between operands of one type run remotely, with no
    /// text among them. Redaction is needed once one can.
    fn classify(diagnostic: &OdbcDiagnostic) -> Option<ErrorClass> {
        let class = match diagnostic.native_error {
            // The login failed, or it may not open the database.
            18456 | 4060 => ErrorClass::new(
                ErrorKind::Authentication,
                false,
                "the data source rejected authentication",
            ),
            // Permission denied on an object or a column.
            229 | 230 => ErrorClass::new(
                ErrorKind::PermissionDenied,
                false,
                "the data source denied access to the requested target",
            ),
            // Invalid object name and invalid column name. SQL Server resolves
            // names whatever the credential's permissions, and reports a
            // table it may not read as 229, so these do not exist.
            208 | 207 => ErrorClass::new(
                ErrorKind::DoesNotExist,
                false,
                "a registered table or column no longer exists at the data source",
            ),
            // Out of memory, lock resources, or space in a filegroup or the
            // transaction log.
            701 | 1204 | 1105 | 9002 => ErrorClass::new(
                ErrorKind::Resource,
                true,
                "the data source ran out of resources",
            ),
            _ => return None,
        };
        Some(class)
    }
}
