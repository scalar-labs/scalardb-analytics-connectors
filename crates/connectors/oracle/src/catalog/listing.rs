//! The namespace list and table list requests.
//!
//! The namespace list holds every schema of a user Oracle does not maintain;
//! the table list holds only the tables and views the credential has a
//! privilege on.

use connector_sdk::{
    ConnectorError, Listed, NameFilter, NamespacePath, TableEntry, TableKind, TableReference,
};
use connector_sql::catalog::{required, text, unexpected_catalog_result};

use super::{Catalog, LISTED_OBJECTS, LISTED_SCHEMA, values_query};

/// Lists the schemas of the users Oracle does not maintain.
pub(crate) fn namespace_list(
    catalog: Catalog<'_, '_>,
    filter: &NameFilter,
) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
    let sql = format!(
        "SELECT u.username FROM all_users u WHERE {LISTED_SCHEMA}{} ORDER BY u.username",
        name_condition("u.username", filter)
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

/// Lists the tables and views of the listed schemas.
pub(crate) fn table_list(
    catalog: Catalog<'_, '_>,
    namespaces: &NameFilter,
    tables: &NameFilter,
) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
    let sql = format!(
        "SELECT o.owner, o.name, o.kind FROM ({LISTED_OBJECTS}) o \
         JOIN all_users u ON u.username = o.owner \
         WHERE {LISTED_SCHEMA}{}{} ORDER BY o.owner, o.name",
        name_condition("o.owner", namespaces),
        name_condition("o.name", tables),
    );
    let mut entries = Vec::new();
    for batch in &catalog.query(sql)? {
        let (owners, names, kinds) = (text(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
        for row in 0..batch.num_rows() {
            let table = TableReference::new(vec![
                required(owners[row])?.to_owned(),
                required(names[row])?.to_owned(),
            ])
            .map_err(|_| unexpected_catalog_result())?;
            let kind = match required(kinds[row])? {
                "T" => TableKind::Table,
                "V" => TableKind::View,
                _ => return Err(unexpected_catalog_result()),
            };
            entries.push(Listed::Item(TableEntry { table, kind }));
        }
    }
    Ok(entries)
}

/// An exact-name condition for an optional filter, starting with ` AND `.
fn name_condition(column: &str, filter: &NameFilter) -> String {
    match filter {
        None => String::new(),
        // An empty filter matches nothing; `IN ()` is not valid SQL.
        Some(names) if names.is_empty() => " AND 1 = 0".to_owned(),
        Some(names) => format!(" AND {column} IN ({})", values_query(names)),
    }
}
