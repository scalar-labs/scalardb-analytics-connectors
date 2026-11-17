//! Answering the metadata requests from the `ALL_*` data dictionary views.
//!
//! - [`listing`]: the namespace list and table list requests.
//! - [`mod@table_metadata`]: the table metadata request, which reads the
//!   requested tables' catalog rows and assembles their source metadata.
//! - [`long_text`]: reading the `LONG` catalog values: view definitions,
//!   CHECK conditions, and column expressions.
//! - [`source_metadata`]: the source metadata document, [`OracleTable`], and
//!   the table metadata derived from it.
//! - [`type_mapping`]: the Oracle type mapping.
//!
//! A namespace is a user's schema; schemas Oracle maintains are not listed.
//! The `ALL_*` views show only the objects the credential has a privilege
//! on, so a table that is not found may exist. The session compares and
//! orders names in binary, so a name filter matches exactly.

mod listing;
mod long_text;
mod source_metadata;
mod table_metadata;
mod type_mapping;

pub(crate) use listing::{namespace_list, table_list};
pub use source_metadata::{
    OracleCheck, OracleColumn, OracleForeignKey, OracleKey, OracleReferencedTable, OracleTable,
    OracleTableKind, OracleUniqueKey, OracleUniqueKind, OracleValueSource,
};
pub(crate) use table_metadata::table_metadata;

use connector_sdk::ConnectorError;
use connector_sql::{
    catalog::{CatalogReads, unexpected_catalog_result},
    literal::quote_string,
};

use crate::OracleConnector;

type Catalog<'a, 'b> = &'a mut CatalogReads<'b, OracleConnector>;

/// The listed tables and views of every schema, as `owner`, `name`, and
/// `kind` (`T` or `V`).
///
/// Tables exclude those Oracle creates for other objects: nested table
/// storage, domain index data, the overflow segments of index-organized
/// tables, and dropped tables in the recycle bin.
const LISTED_OBJECTS: &str = "SELECT t.owner, t.table_name AS name, 'T' AS kind FROM all_tables t \
     WHERE t.nested = 'NO' AND t.secondary = 'N' AND t.dropped = 'NO' \
       AND (t.iot_type IS NULL OR t.iot_type = 'IOT') \
     UNION ALL SELECT v.owner, v.view_name, 'V' FROM all_views v";

/// The condition on `ALL_USERS` row `u` that selects the schemas the
/// connector lists: those of users Oracle does not maintain.
const LISTED_SCHEMA: &str = "u.oracle_maintained = 'N'";

/// A query returning each of `values` as one row of one column, for an `IN`
/// condition that is not limited to 1000 values.
fn values_query(values: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    values
        .into_iter()
        .map(|value| format!("SELECT {} FROM dual", quote_string(value.as_ref())))
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// Parses a catalog number written with `TO_CHAR`, NULL as `None`.
fn integer(value: Option<&str>) -> Result<Option<i64>, ConnectorError> {
    value
        .map(|value| value.parse().map_err(|_| unexpected_catalog_result()))
        .transpose()
}

/// Parses a catalog number written with `TO_CHAR` that is never NULL.
fn required_integer(value: Option<&str>) -> Result<i64, ConnectorError> {
    integer(value)?.ok_or_else(unexpected_catalog_result)
}
