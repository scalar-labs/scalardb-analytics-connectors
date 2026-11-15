//! The namespace list and table list requests.
//!
//! `information_schema` lists only the databases and tables the account holds
//! some privilege on, so a list holds what the account can see.

use connector_sdk::{
    ConnectorError, Listed, NameFilter, NamespacePath, TableEntry, TableKind, TableReference,
};
use connector_sql::catalog::{required, text, unexpected_catalog_result};

use super::{Catalog, EXCLUDED_SCHEMAS, matches, name_condition};

/// Lists databases, without the server's own.
pub(crate) fn namespace_list(
    catalog: Catalog<'_, '_>,
    filter: &NameFilter,
) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
    let sql = format!(
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA \
         WHERE SCHEMA_NAME NOT IN {EXCLUDED_SCHEMAS}{}",
        name_condition("SCHEMA_NAME", filter)
    );
    let mut names = Vec::new();
    for batch in &catalog.query(sql)? {
        for name in text(batch, 0)? {
            let name = required(name)?;
            if matches(filter, name) {
                names.push(name.to_owned());
            }
        }
    }
    names.sort();
    Ok(names
        .into_iter()
        .map(|name| Listed::Item(NamespacePath { names: vec![name] }))
        .collect())
}

/// Lists base tables and views.
pub(crate) fn table_list(
    catalog: Catalog<'_, '_>,
    namespaces: &NameFilter,
    tables: &NameFilter,
) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
    let sql = format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE FROM information_schema.TABLES \
         WHERE TABLE_SCHEMA NOT IN {EXCLUDED_SCHEMAS} \
           AND TABLE_TYPE IN ('BASE TABLE', 'VIEW'){}{}",
        name_condition("TABLE_SCHEMA", namespaces),
        name_condition("TABLE_NAME", tables)
    );
    let mut entries = Vec::new();
    for batch in &catalog.query(sql)? {
        let (schemas, names, kinds) = (text(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
        for row in 0..batch.num_rows() {
            let (schema, name) = (required(schemas[row])?, required(names[row])?);
            if !matches(namespaces, schema) || !matches(tables, name) {
                continue;
            }
            let kind = match required(kinds[row])? {
                "VIEW" => TableKind::View,
                _ => TableKind::Table,
            };
            let table = TableReference::new(vec![schema.to_owned(), name.to_owned()])
                .map_err(|_| unexpected_catalog_result())?;
            entries.push((table, kind));
        }
    }
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(entries
        .into_iter()
        .map(|(table, kind)| Listed::Item(TableEntry { table, kind }))
        .collect())
}
