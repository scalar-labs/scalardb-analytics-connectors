//! Answering the metadata requests from `pg_catalog`.
//!
//! - [`listing`]: the namespace list and table list requests.
//! - [`mod@table_metadata`]: the table metadata request, which reads the requested
//!   tables' catalog rows and assembles their source metadata.
//! - [`source_metadata`]: the source metadata document, [`PostgresTable`], and
//!   the table metadata derived from it.
//! - [`type_mapping`]: the PostgreSQL type mapping.
//!
//! This module reads the session context, the server version, which decides
//! the catalog columns that exist.

mod listing;
mod source_metadata;
mod table_metadata;
mod type_mapping;

pub(crate) use listing::{namespace_list, table_list};
pub use source_metadata::{
    PostgresCheck, PostgresColumn, PostgresDomainCheck, PostgresForeignKey,
    PostgresGeneratedStorage, PostgresIdentity, PostgresKey, PostgresNotNull, PostgresPrimaryKey,
    PostgresReferencedTable, PostgresTable, PostgresTableKind, PostgresType, PostgresTypeForm,
    PostgresUniqueKey, PostgresUniqueKind, PostgresValueSource,
};
pub(crate) use table_metadata::table_metadata;
pub(crate) use type_mapping::BaseType;

use connector_sdk::ConnectorError;
use connector_sql::catalog::{CatalogReads, int32, unexpected_catalog_result};

use crate::PostgresConnector;

type Catalog<'a, 'b> = &'a mut CatalogReads<'b, PostgresConnector>;

/// Relation kinds listed as tables: ordinary, partitioned, view, materialized
/// view, and foreign tables.
const LISTED_RELKINDS: &str = "c.relkind IN ('r', 'p', 'v', 'm', 'f')";

/// What a session reads once when it opens: `server_version_num`, which
/// decides the catalog columns that exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerVersion(i32);

/// Reads the session's server version.
pub(crate) fn session_context(catalog: Catalog<'_, '_>) -> Result<ServerVersion, ConnectorError> {
    let batches = catalog.query("SELECT current_setting('server_version_num')::int4")?;
    let batch = batches.first().ok_or_else(unexpected_catalog_result)?;
    Ok(ServerVersion(int32(batch, 0)?.value(0)))
}
