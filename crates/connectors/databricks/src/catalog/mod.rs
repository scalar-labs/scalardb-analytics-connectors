//! Answering the metadata requests from Unity Catalog.
//!
//! - [`mod@data_type`]: Databricks data types as `full_data_type` spells them.
//! - [`type_mapping`]: the Databricks type mapping.
//! - [`source_metadata`]: the source metadata document, [`DatabricksTable`],
//!   and the table metadata derived from it.
//! - [`information_schema`]: where the metadata is read from, its statements
//!   and the conversion of their rows.
//!
//! A namespace is a catalog and a schema, and a table is named by its
//! catalog, schema, and name. A namespace name filter applies to the schema
//! name.
//!
//! The functions here are the seam between the requests and the metadata
//! source: they run the source's statements and hand the rows to it. A
//! different source, such as the Unity Catalog REST API, would replace
//! [`information_schema`] and these functions and produce the same lists and
//! [`DatabricksTable`]s; the type mapping and the source metadata stay.

mod data_type;
mod information_schema;
mod source_metadata;
mod type_mapping;

pub(crate) use data_type::DataType;
pub use source_metadata::{
    DatabricksColumn, DatabricksForeignKey, DatabricksPrimaryKey, DatabricksReferencedTable,
    DatabricksTable,
};

use std::{collections::BTreeMap, time::SystemTime};

use connector_sdk::{
    ConnectorError, Listed, NameFilter, NamespacePath, TableEntry, TableRecord, TableReference,
};
use connector_sql::catalog::CatalogReads;

use crate::{DatabricksConnector, DatabricksSettings};

type Catalog<'a, 'b> = &'a mut CatalogReads<'b, DatabricksConnector>;

/// Lists the schemas the principal can see, within the settings' catalog.
pub(crate) fn namespace_list(
    catalog: Catalog<'_, '_>,
    settings: &DatabricksSettings,
    filter: &NameFilter,
) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
    let sql = information_schema::namespace_list_sql(settings.catalog.as_deref(), filter);
    information_schema::namespaces(&catalog.query(sql)?)
}

/// Lists the tables the principal can see, within the settings' catalog.
pub(crate) fn table_list(
    catalog: Catalog<'_, '_>,
    settings: &DatabricksSettings,
    namespaces: &NameFilter,
    tables: &NameFilter,
) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
    let sql = information_schema::table_list_sql(settings.catalog.as_deref(), namespaces, tables);
    information_schema::table_entries(&catalog.query(sql)?)
}

/// Reads one record per requested table, in any catalog; the settings'
/// catalog narrows only the lists.
pub(crate) fn table_metadata(
    catalog: Catalog<'_, '_>,
    tables: &[TableReference],
) -> Result<Vec<TableRecord<DatabricksTable>>, ConnectorError> {
    let requested = information_schema::requested_tables(tables);
    let read = if requested.is_empty() {
        BTreeMap::new()
    } else {
        let rows = catalog.query(information_schema::tables_sql(&requested))?;
        let columns = catalog.query(information_schema::columns_sql(&requested))?;
        let keys = catalog.query(information_schema::keys_sql(&requested))?;
        information_schema::source_metadata(&rows, &columns, &keys)?
    };
    Ok(information_schema::records(
        tables,
        &read,
        SystemTime::now(),
    ))
}
