//! The namespace list and table list requests.

use connector_sdk::{
    ConnectorError, Listed, NameFilter, NamespacePath, TableEntry, TableKind, TableReference,
};
use connector_sql::{
    catalog::{required, text, unexpected_catalog_result},
    literal::quote_string,
};

use super::{Catalog, LISTED_RELKINDS};

/// Schemas the connector never lists: `information_schema` and every `pg_`
/// schema, which PostgreSQL reserves for `pg_catalog`, `pg_toast`, and
/// temporary schemas.
const EXCLUDED_NAMESPACES: &str = "n.nspname <> 'information_schema' AND n.nspname !~ '^pg_'";

/// Lists namespaces, without the reserved ones.
pub(crate) fn namespace_list(
    catalog: Catalog<'_, '_>,
    filter: &NameFilter,
) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
    let sql = format!(
        "SELECT n.nspname::text FROM pg_catalog.pg_namespace n \
         WHERE {EXCLUDED_NAMESPACES}{} ORDER BY n.nspname",
        name_condition("n.nspname", filter)
    );
    let mut namespaces = Vec::new();
    for batch in &catalog.query(sql)? {
        for name in text(batch, 0)? {
            namespaces.push(Listed::Item(NamespacePath {
                names: vec![required(name)?.to_owned()],
            }));
        }
    }
    Ok(namespaces)
}

/// Lists the relations of the listed kinds.
pub(crate) fn table_list(
    catalog: Catalog<'_, '_>,
    namespaces: &NameFilter,
    tables: &NameFilter,
) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
    let sql = format!(
        "SELECT n.nspname::text, c.relname::text, c.relkind::text \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE {EXCLUDED_NAMESPACES} AND {LISTED_RELKINDS}{}{} \
         ORDER BY n.nspname, c.relname",
        name_condition("n.nspname", namespaces),
        name_condition("c.relname", tables)
    );
    let mut entries = Vec::new();
    for batch in &catalog.query(sql)? {
        let (namespaces, names, kinds) = (text(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
        for row in 0..batch.num_rows() {
            let table = table_reference(required(namespaces[row])?, required(names[row])?)?;
            entries.push(Listed::Item(TableEntry {
                table,
                kind: table_kind(required(kinds[row])?),
            }));
        }
    }
    Ok(entries)
}

fn table_reference(namespace: &str, name: &str) -> Result<TableReference, ConnectorError> {
    TableReference::new(vec![namespace.to_owned(), name.to_owned()])
        .map_err(|_| unexpected_catalog_result())
}

/// The table kind for a listed `relkind`, matching the table metadata's kind.
fn table_kind(relkind: &str) -> TableKind {
    match relkind {
        "r" | "p" => TableKind::Table,
        "v" => TableKind::View,
        "f" => TableKind::External,
        "m" => TableKind::Other("materialized view".to_owned()),
        other => TableKind::Other(other.to_owned()),
    }
}

/// An exact-name condition for an optional filter, starting with ` AND `.
fn name_condition(column: &str, filter: &NameFilter) -> String {
    match filter {
        None => String::new(),
        Some(names) => format!(
            " AND {column} = ANY({})",
            text_array(names.iter().map(String::as_str))
        ),
    }
}

/// A `text[]` literal of the given values.
fn text_array<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let elements: Vec<String> = values.map(quote_string).collect();
    format!("CAST(ARRAY[{}] AS text[])", elements.join(", "))
}
