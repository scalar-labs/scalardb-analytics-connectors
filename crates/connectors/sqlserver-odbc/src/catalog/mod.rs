//! Answering the metadata requests from the `sys` catalog views.
//!
//! - [`listing`]: the namespace list and table list requests.
//! - [`mod@table_metadata`]: the table metadata request, which reads the
//!   requested tables' catalog rows and assembles their source metadata.
//! - [`long_text`]: reading `nvarchar(max)` catalog values, such as view
//!   definitions and CHECK expressions, in pieces.
//! - [`source_metadata`]: the source metadata document, [`SqlServerTable`],
//!   and the table metadata derived from it.
//! - [`type_mapping`]: the SQL Server type mapping.
//!
//! This module reads the session context: whether the credential sees the
//! metadata of every object in the database.
//!
//! SQL Server shows a credential only the metadata of objects it has some
//! permission on (metadata visibility), and the definitions and expressions
//! of objects it has `VIEW DEFINITION` on. Catalog names are compared in a
//! binary collation, so that a name filter matches exactly, whatever the
//! database's collation.

mod listing;
mod long_text;
mod source_metadata;
mod table_metadata;
mod type_mapping;

pub(crate) use listing::{namespace_list, table_list};
pub use source_metadata::{
    SqlServerCheck, SqlServerColumn, SqlServerForeignKey, SqlServerKey, SqlServerReferencedTable,
    SqlServerTable, SqlServerTableKind, SqlServerType, SqlServerUniqueKey, SqlServerUniqueKind,
    SqlServerValueSource,
};
pub(crate) use table_metadata::table_metadata;

use connector_sdk::ConnectorError;
use connector_sql::{
    catalog::{CatalogReads, int32, unexpected_catalog_result},
    literal::quote_string,
};

use crate::SqlServerConnector;

type Catalog<'a, 'b> = &'a mut CatalogReads<'b, SqlServerConnector>;

/// The collation catalog names are compared and ordered in: by code point,
/// so that names that differ only in case are distinct.
const BINARY: &str = "Latin1_General_100_BIN2";

/// Schemas the connector never lists: the system catalog's.
const EXCLUDED_NAMESPACES: &str = "s.name NOT IN (N'sys', N'INFORMATION_SCHEMA')";

/// Objects listed as tables: user tables and views that do not come with
/// SQL Server.
const LISTED_OBJECTS: &str = "o.type IN ('U', 'V') AND o.is_ms_shipped = 0";

/// What a session reads once when it opens: whether its credential sees the
/// metadata of every object in the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataVisibility {
    /// The credential has `VIEW DEFINITION` on the database, directly or
    /// through a role or a server permission, and none of its principals is
    /// denied `VIEW DEFINITION` or `CONTROL` on anything in the database.
    /// Otherwise the catalog silently omits some objects, so the absence of a
    /// table does not show that it does not exist.
    pub complete: bool,
}

/// Reads the session's metadata visibility.
///
/// With `VIEW DEFINITION` on the database, every permission of the database
/// is visible, so the denials of the credential's principals
/// (`sys.user_token`) can be ruled out.
pub(crate) fn session_context(
    catalog: Catalog<'_, '_>,
) -> Result<MetadataVisibility, ConnectorError> {
    let batches = catalog.query(
        "SELECT CASE WHEN HAS_PERMS_BY_NAME(DB_NAME(), 'DATABASE', 'VIEW DEFINITION') = 1 \
                 AND NOT EXISTS (SELECT 1 FROM sys.database_permissions p \
                                 JOIN sys.user_token u ON u.principal_id = p.grantee_principal_id \
                                 WHERE p.state = 'D' \
                                   AND p.permission_name IN (N'VIEW DEFINITION', N'CONTROL')) \
                 THEN 1 ELSE 0 END",
    )?;
    let batch = batches.first().ok_or_else(unexpected_catalog_result)?;
    Ok(MetadataVisibility {
        complete: int32(batch, 0)?.value(0) == 1,
    })
}

/// A Unicode string literal of `value`.
pub(crate) fn nstring(value: &str) -> String {
    format!("N{}", quote_string(value))
}

/// `column` compared and ordered as catalog names are.
fn binary(column: &str) -> String {
    format!("{column} COLLATE {BINARY}")
}
