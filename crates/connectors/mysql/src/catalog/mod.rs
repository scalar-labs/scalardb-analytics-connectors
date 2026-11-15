//! Answering the metadata requests from `information_schema`.
//!
//! - [`listing`]: the namespace list and table list requests.
//! - [`mod@table_metadata`]: the table metadata request, which reads the
//!   requested tables' rows and assembles their source metadata.
//! - [`privileges`]: the account's `SELECT` grants, which decide whether
//!   `information_schema` may have hidden a table or some of its columns.
//! - [`source_metadata`]: the source metadata document, [`MysqlTable`], and
//!   the table metadata derived from it.
//! - [`type_mapping`]: the MySQL type mapping.
//!
//! A namespace is a MySQL database. `information_schema` compares names by
//! the server's rules, which may ignore case, so statements narrow by name and
//! the rows are then matched exactly.

mod listing;
mod privileges;
mod source_metadata;
mod table_metadata;
mod type_mapping;

pub(crate) use listing::{namespace_list, table_list};
pub use source_metadata::{
    MysqlCheck, MysqlColumn, MysqlForeignKey, MysqlGeneration, MysqlKey, MysqlPrimaryKey,
    MysqlReferencedTable, MysqlTable, MysqlTableKind, MysqlUniqueKey, MysqlValueSource,
};
pub(crate) use table_metadata::table_metadata;

use connector_sdk::NameFilter;
use connector_sql::{catalog::CatalogReads, literal::quote_string};

use crate::MysqlConnector;

type Catalog<'a, 'b> = &'a mut CatalogReads<'b, MysqlConnector>;

/// Databases the connector never lists: the server's own.
const EXCLUDED_SCHEMAS: &str = "('information_schema', 'mysql', 'performance_schema', 'sys')";

/// Whether `name` exactly equals one of the filter's names; no filter
/// matches every name.
fn matches(filter: &NameFilter, name: &str) -> bool {
    filter.as_ref().is_none_or(|names| names.contains(name))
}

/// A condition narrowing `column` to the filter's names, starting with
/// ` AND `; empty without a filter. MySQL has no empty `IN` list, so an empty
/// filter is `FALSE`.
fn name_condition(column: &str, filter: &NameFilter) -> String {
    match filter {
        None => String::new(),
        Some(names) if names.is_empty() => " AND FALSE".to_owned(),
        Some(names) => format!(
            " AND {column} IN ({})",
            names
                .iter()
                .map(|name| quote_string(name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
