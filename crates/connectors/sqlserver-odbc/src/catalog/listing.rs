//! The namespace list and table list requests.
//!
//! Without `VIEW DEFINITION` on the database, the lists hold only the
//! schemas and tables the credential has some permission on.

use connector_sdk::{
    ConnectorError, Listed, NameFilter, NamespacePath, TableEntry, TableKind, TableReference,
};
use connector_sql::catalog::{required, text, unexpected_catalog_result};

use super::{Catalog, EXCLUDED_NAMESPACES, LISTED_OBJECTS, binary, nstring};

/// Lists the database's schemas, without the system catalog's.
pub(crate) fn namespace_list(
    catalog: Catalog<'_, '_>,
    filter: &NameFilter,
) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
    let sql = format!(
        "SELECT s.name FROM sys.schemas s WHERE {EXCLUDED_NAMESPACES}{} ORDER BY {}",
        name_condition("s.name", filter),
        binary("s.name")
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

/// Lists the user tables and views.
pub(crate) fn table_list(
    catalog: Catalog<'_, '_>,
    namespaces: &NameFilter,
    tables: &NameFilter,
) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
    let sql = format!(
        "SELECT s.name, o.name, CAST(o.type AS char(1)) \
         FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
         WHERE {EXCLUDED_NAMESPACES} AND {LISTED_OBJECTS}{}{} \
         ORDER BY {}, {}",
        name_condition("s.name", namespaces),
        name_condition("o.name", tables),
        binary("s.name"),
        binary("o.name")
    );
    let mut entries = Vec::new();
    for batch in &catalog.query(sql)? {
        let (namespaces, names, kinds) = (text(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
        for row in 0..batch.num_rows() {
            let table = TableReference::new(vec![
                required(namespaces[row])?.to_owned(),
                required(names[row])?.to_owned(),
            ])
            .map_err(|_| unexpected_catalog_result())?;
            let kind = match required(kinds[row])? {
                "U" => TableKind::Table,
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
        Some(names) => {
            let names: Vec<String> = names.iter().map(|name| nstring(name)).collect();
            format!(" AND {} IN ({})", binary(column), names.join(", "))
        }
    }
}
