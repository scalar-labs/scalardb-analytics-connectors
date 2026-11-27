//! Unity Catalog metadata read with SQL from `system.information_schema`.
//!
//! The statements are text built from the request, and the conversions turn
//! their result rows into lists and source metadata without reaching the
//! warehouse, so each is checked on its own. `system.information_schema`
//! spans every catalog of the metastore, so one statement answers each
//! request whatever namespaces it names. It shows only the objects the
//! principal holds a privilege on and stores every name but column names in
//! lower case.

use std::{collections::BTreeMap, time::SystemTime};

use arrow_array::RecordBatch;
use connector_sdk::{
    ConnectorError, Listed, NameFilter, NamespacePath, TableEntry, TableRecord, TableReference,
};
use connector_sql::catalog::{int32, required, text, unexpected_catalog_result};

use super::source_metadata::{
    DatabricksColumn, DatabricksForeignKey, DatabricksPrimaryKey, DatabricksReferencedTable,
    DatabricksTable, table_kind,
};
use crate::dialect::string_literal;

/// Catalogs the connector never lists or reads: Databricks' own, and the
/// legacy Hive metastore, which `information_schema` does not describe.
const EXCLUDED_CATALOGS: [&str; 2] = ["system", "hive_metastore"];

/// The schema of each catalog that describes the catalog itself.
const INFORMATION_SCHEMA: &str = "information_schema";

/// The table type of metric views, which a plain `SELECT` cannot read.
const METRIC_VIEW: &str = "METRIC_VIEW";

/// A table's catalog, schema, and name.
type TableKey = (String, String, String);

/// Whether the connector lists and reads the objects of a schema.
fn is_read_schema(catalog: &str, schema: &str) -> bool {
    !EXCLUDED_CATALOGS.contains(&catalog) && schema != INFORMATION_SCHEMA
}

/// Whether the connector lists and reads a table.
fn is_read_table(catalog: &str, schema: &str, table_type: &str) -> bool {
    is_read_schema(catalog, schema) && table_type != METRIC_VIEW
}

/// A condition narrowing `column` to one value, starting with ` AND `; empty
/// without one.
fn equals_condition(column: &str, value: Option<&str>) -> String {
    value.map_or_else(String::new, |value| {
        format!(" AND {column} = {}", string_literal(value))
    })
}

/// A condition narrowing `column` to the filter's names, starting with
/// ` AND `; empty without a filter, and `FALSE` for an empty one.
fn name_condition(column: &str, filter: &NameFilter) -> String {
    match filter {
        None => String::new(),
        Some(names) if names.is_empty() => " AND FALSE".to_owned(),
        Some(names) => format!(
            " AND {column} IN ({})",
            names
                .iter()
                .map(|name| string_literal(name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// The namespace list.

/// Selects the schemas, in the one catalog `scope` names when it names one;
/// `filter` applies to the schema name.
pub(crate) fn namespace_list_sql(scope: Option<&str>, filter: &NameFilter) -> String {
    format!(
        "SELECT catalog_name, schema_name FROM system.information_schema.schemata \
         WHERE TRUE{}{}",
        equals_condition("catalog_name", scope),
        name_condition("schema_name", filter)
    )
}

/// The namespaces of [`namespace_list_sql`]'s rows, `[catalog, schema]`,
/// without the excluded ones, in name order.
pub(crate) fn namespaces(
    batches: &[RecordBatch],
) -> Result<Vec<Listed<NamespacePath>>, ConnectorError> {
    let mut namespaces = Vec::new();
    for batch in batches {
        let (catalogs, schemas) = (text(batch, 0)?, text(batch, 1)?);
        for row in 0..batch.num_rows() {
            let (catalog, schema) = (required(catalogs[row])?, required(schemas[row])?);
            if is_read_schema(catalog, schema) {
                namespaces.push(vec![catalog.to_owned(), schema.to_owned()]);
            }
        }
    }
    namespaces.sort();
    Ok(namespaces
        .into_iter()
        .map(|names| Listed::Item(NamespacePath { names }))
        .collect())
}

// The table list.

/// Selects the tables, in the one catalog `scope` names when it names one;
/// `namespaces` applies to the schema name and `tables` to the table name.
pub(crate) fn table_list_sql(
    scope: Option<&str>,
    namespaces: &NameFilter,
    tables: &NameFilter,
) -> String {
    format!(
        "SELECT table_catalog, table_schema, table_name, table_type \
         FROM system.information_schema.tables WHERE TRUE{}{}{}",
        equals_condition("table_catalog", scope),
        name_condition("table_schema", namespaces),
        name_condition("table_name", tables)
    )
}

/// The tables of [`table_list_sql`]'s rows, without the excluded ones, in
/// name order.
pub(crate) fn table_entries(
    batches: &[RecordBatch],
) -> Result<Vec<Listed<TableEntry>>, ConnectorError> {
    let mut entries = Vec::new();
    for batch in batches {
        let (catalogs, schemas, names, types) = (
            text(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            text(batch, 3)?,
        );
        for row in 0..batch.num_rows() {
            let (catalog, schema, name, table_type) = (
                required(catalogs[row])?,
                required(schemas[row])?,
                required(names[row])?,
                required(types[row])?,
            );
            if !is_read_table(catalog, schema, table_type) {
                continue;
            }
            let table =
                TableReference::new(vec![catalog.to_owned(), schema.to_owned(), name.to_owned()])
                    .map_err(|_| unexpected_catalog_result())?;
            entries.push(TableEntry {
                table,
                kind: table_kind(table_type),
            });
        }
    }
    entries.sort_by(|left, right| left.table.cmp(&right.table));
    Ok(entries.into_iter().map(Listed::Item).collect())
}

// The table metadata request.

/// The requested tables that can name a Unity Catalog table: those of three
/// names.
pub(crate) fn requested_tables(tables: &[TableReference]) -> Vec<[&str; 3]> {
    tables
        .iter()
        .filter_map(|table| match table.names() {
            [catalog, schema, name] => Some([catalog.as_str(), schema.as_str(), name.as_str()]),
            _ => None,
        })
        .collect()
}

/// A condition selecting the requested tables' rows of a view with
/// `table_catalog`, `table_schema`, and `table_name` columns, qualified with
/// `qualifier`.
///
/// `requested` must not be empty.
fn requested_condition(qualifier: &str, requested: &[[&str; 3]]) -> String {
    let tables: Vec<String> = requested
        .iter()
        .map(|[catalog, schema, name]| {
            format!(
                "({qualifier}table_catalog = {} AND {qualifier}table_schema = {} \
                 AND {qualifier}table_name = {})",
                string_literal(catalog),
                string_literal(schema),
                string_literal(name)
            )
        })
        .collect();
    format!("({})", tables.join(" OR "))
}

/// Selects the requested tables. `requested` must not be empty.
pub(crate) fn tables_sql(requested: &[[&str; 3]]) -> String {
    format!(
        "SELECT table_catalog, table_schema, table_name, table_type, data_source_format, comment \
         FROM system.information_schema.tables WHERE {}",
        requested_condition("", requested)
    )
}

/// Selects the requested tables' columns. `requested` must not be empty.
pub(crate) fn columns_sql(requested: &[[&str; 3]]) -> String {
    format!(
        "SELECT table_catalog, table_schema, table_name, column_name, ordinal_position, \
                is_nullable, full_data_type, comment \
         FROM system.information_schema.columns WHERE {}",
        requested_condition("", requested)
    )
}

/// Selects one row per column of the requested tables' primary and foreign
/// keys, with the column a foreign key column references when the principal
/// can see it. `requested` must not be empty.
pub(crate) fn keys_sql(requested: &[[&str; 3]]) -> String {
    format!(
        "SELECT tc.table_catalog, tc.table_schema, tc.table_name, tc.constraint_name, \
                tc.constraint_type, kcu.column_name, kcu.ordinal_position, \
                ref.table_catalog, ref.table_schema, ref.table_name, ref.column_name \
         FROM system.information_schema.table_constraints tc \
         JOIN system.information_schema.key_column_usage kcu \
           ON kcu.constraint_catalog = tc.constraint_catalog \
          AND kcu.constraint_schema = tc.constraint_schema \
          AND kcu.constraint_name = tc.constraint_name \
         LEFT JOIN system.information_schema.referential_constraints rc \
           ON rc.constraint_catalog = tc.constraint_catalog \
          AND rc.constraint_schema = tc.constraint_schema \
          AND rc.constraint_name = tc.constraint_name \
         LEFT JOIN system.information_schema.key_column_usage ref \
           ON ref.constraint_catalog = rc.unique_constraint_catalog \
          AND ref.constraint_schema = rc.unique_constraint_schema \
          AND ref.constraint_name = rc.unique_constraint_name \
          AND ref.ordinal_position = kcu.position_in_unique_constraint \
         WHERE tc.constraint_type IN ('PRIMARY KEY', 'FOREIGN KEY') AND {}",
        requested_condition("tc.", requested)
    )
}

/// One key of a table, as its key column rows give it.
struct Key {
    constraint_type: String,
    /// Each key column and the column it references, by key position.
    columns: BTreeMap<i32, (String, Option<(TableKey, String)>)>,
}

/// Assembles the source metadata of each table in [`tables_sql`]'s rows from
/// those rows, [`columns_sql`]'s, and [`keys_sql`]'s, without the excluded
/// tables, keyed by catalog, schema, and name.
pub(crate) fn source_metadata(
    tables: &[RecordBatch],
    columns: &[RecordBatch],
    keys: &[RecordBatch],
) -> Result<BTreeMap<TableKey, DatabricksTable>, ConnectorError> {
    let mut read = read_tables(tables)?;
    for (key, column) in read_columns(columns)? {
        if let Some(table) = read.get_mut(&key) {
            table.columns.push(column);
        }
    }
    for ((key, name), constraint) in read_keys(keys)? {
        if let Some(table) = read.get_mut(&key) {
            add_key(table, name, constraint)?;
        }
    }
    for table in read.values_mut() {
        table.columns.sort_by_key(|column| column.ordinal_position);
        table.foreign_keys.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(read)
}

/// The table of each row, from three columns starting at `first` that hold a
/// table's catalog, schema, and name; `None` where the catalog is NULL.
fn table_keys(batch: &RecordBatch, first: usize) -> Result<Vec<Option<TableKey>>, ConnectorError> {
    let (catalogs, schemas, names) = (
        text(batch, first)?,
        text(batch, first + 1)?,
        text(batch, first + 2)?,
    );
    (0..batch.num_rows())
        .map(|row| {
            let Some(catalog) = catalogs[row] else {
                return Ok(None);
            };
            Ok(Some((
                catalog.to_owned(),
                required(schemas[row])?.to_owned(),
                required(names[row])?.to_owned(),
            )))
        })
        .collect()
}

/// The table of each row whose first three columns name it, which are never
/// NULL.
fn row_tables(batch: &RecordBatch) -> Result<Vec<TableKey>, ConnectorError> {
    table_keys(batch, 0)?
        .into_iter()
        .map(|key| key.ok_or_else(unexpected_catalog_result))
        .collect()
}

/// The requested tables that are read, with no columns or keys yet.
fn read_tables(
    batches: &[RecordBatch],
) -> Result<BTreeMap<TableKey, DatabricksTable>, ConnectorError> {
    let mut tables = BTreeMap::new();
    for batch in batches {
        let (types, formats, comments) = (text(batch, 3)?, text(batch, 4)?, text(batch, 5)?);
        for (row, (catalog, schema, name)) in row_tables(batch)?.into_iter().enumerate() {
            let table_type = required(types[row])?;
            if !is_read_table(&catalog, &schema, table_type) {
                continue;
            }
            tables.insert(
                (catalog.clone(), schema.clone(), name.clone()),
                DatabricksTable {
                    catalog,
                    schema,
                    name,
                    table_type: table_type.to_owned(),
                    data_source_format: required(formats[row])?.to_owned(),
                    comment: comments[row].map(str::to_owned),
                    columns: Vec::new(),
                    primary_key: None,
                    foreign_keys: Vec::new(),
                },
            );
        }
    }
    Ok(tables)
}

fn read_columns(
    batches: &[RecordBatch],
) -> Result<Vec<(TableKey, DatabricksColumn)>, ConnectorError> {
    let mut columns = Vec::new();
    for batch in batches {
        let (names, positions, nullable, types, comments) = (
            text(batch, 3)?,
            int32(batch, 4)?,
            text(batch, 5)?,
            text(batch, 6)?,
            text(batch, 7)?,
        );
        for (row, table) in row_tables(batch)?.into_iter().enumerate() {
            let nullable = match required(nullable[row])? {
                "YES" => true,
                "NO" => false,
                _ => return Err(unexpected_catalog_result()),
            };
            columns.push((
                table,
                DatabricksColumn {
                    name: required(names[row])?.to_owned(),
                    ordinal_position: u32::try_from(positions.value(row))
                        .map_err(|_| unexpected_catalog_result())?,
                    nullable,
                    full_data_type: required(types[row])?.to_owned(),
                    comment: comments[row].map(str::to_owned),
                },
            ));
        }
    }
    Ok(columns)
}

/// Groups the key column rows by table and constraint name.
fn read_keys(batches: &[RecordBatch]) -> Result<BTreeMap<(TableKey, String), Key>, ConnectorError> {
    let mut keys: BTreeMap<(TableKey, String), Key> = BTreeMap::new();
    for batch in batches {
        let (names, types, columns, positions, referenced_columns) = (
            text(batch, 3)?,
            text(batch, 4)?,
            text(batch, 5)?,
            int32(batch, 6)?,
            text(batch, 10)?,
        );
        let referenced_tables = table_keys(batch, 7)?;
        for (row, (table, referenced_table)) in row_tables(batch)?
            .into_iter()
            .zip(referenced_tables)
            .enumerate()
        {
            let constraint_type = required(types[row])?;
            let referenced = match (referenced_table, referenced_columns[row]) {
                (Some(referenced_table), Some(column)) => {
                    Some((referenced_table, column.to_owned()))
                }
                _ => None,
            };
            let key = keys
                .entry((table, required(names[row])?.to_owned()))
                .or_insert_with(|| Key {
                    constraint_type: constraint_type.to_owned(),
                    columns: BTreeMap::new(),
                });
            if key.constraint_type != constraint_type {
                return Err(unexpected_catalog_result());
            }
            key.columns.insert(
                positions.value(row),
                (required(columns[row])?.to_owned(), referenced),
            );
        }
    }
    Ok(keys)
}

/// Adds one key to its table.
///
/// A foreign key references one table; when the principal cannot see some
/// referenced column, the reference is unknown.
fn add_key(table: &mut DatabricksTable, name: String, key: Key) -> Result<(), ConnectorError> {
    let (columns, referenced): (Vec<String>, Vec<Option<(TableKey, String)>>) =
        key.columns.into_values().unzip();
    match key.constraint_type.as_str() {
        "PRIMARY KEY" => table.primary_key = Some(DatabricksPrimaryKey { name, columns }),
        "FOREIGN KEY" => {
            let referenced = match referenced.into_iter().collect::<Option<Vec<_>>>() {
                None => None,
                Some(referenced) => {
                    let (tables, columns): (Vec<TableKey>, Vec<String>) =
                        referenced.into_iter().unzip();
                    let Some(first) = tables.first() else {
                        return Err(unexpected_catalog_result());
                    };
                    if tables.iter().any(|other| other != first) {
                        return Err(unexpected_catalog_result());
                    }
                    let (catalog, schema, referenced_table) = first.clone();
                    Some(DatabricksReferencedTable {
                        catalog,
                        schema,
                        table: referenced_table,
                        columns,
                    })
                }
            };
            table.foreign_keys.push(DatabricksForeignKey {
                name,
                columns,
                referenced,
            });
        }
        _ => return Err(unexpected_catalog_result()),
    }
    Ok(())
}

/// One record per requested table, from the tables read.
///
/// A reference that is not a catalog, a schema, and a table name does not
/// exist. A table `information_schema` does not show is not found:
/// `information_schema` hides what the principal holds no privilege on, and
/// the connector does not read the excluded tables.
pub(crate) fn records(
    tables: &[TableReference],
    read: &BTreeMap<TableKey, DatabricksTable>,
    read_at: SystemTime,
) -> Vec<TableRecord<DatabricksTable>> {
    tables
        .iter()
        .map(|table| {
            let [catalog, schema, name] = table.names() else {
                return TableRecord::DoesNotExist {
                    table: table.clone(),
                };
            };
            match read.get(&(catalog.clone(), schema.clone(), name.clone())) {
                Some(metadata) => TableRecord::Read {
                    metadata: metadata.clone(),
                    read_at,
                },
                None => TableRecord::NotFound {
                    table: table.clone(),
                    reason: "Unity Catalog shows only the tables the principal holds a \
                             privilege on, and the connector does not read system \
                             catalogs, information_schema, or metric views"
                        .to_owned(),
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, sync::Arc};

    use arrow_array::{ArrayRef, Int32Array, StringArray};
    use connector_sdk::{ErrorKind, TableKind};

    use super::*;

    /// A text column of a result batch.
    fn text_column(values: &[Option<&str>]) -> ArrayRef {
        Arc::new(StringArray::from(values.to_vec()))
    }

    fn int_column(values: &[i32]) -> ArrayRef {
        Arc::new(Int32Array::from(values.to_vec()))
    }

    /// A batch of text rows, every value present.
    fn text_rows(rows: &[&[&str]]) -> RecordBatch {
        let width = rows.first().map_or(0, |row| row.len());
        RecordBatch::try_from_iter((0..width).map(|index| {
            (
                format!("c{index}"),
                text_column(&rows.iter().map(|row| Some(row[index])).collect::<Vec<_>>()),
            )
        }))
        .unwrap()
    }

    fn filter(names: &[&str]) -> NameFilter {
        Some(
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<BTreeSet<_>>(),
        )
    }

    fn reference(names: [&str; 3]) -> TableReference {
        TableReference::new(names.map(str::to_owned).to_vec()).unwrap()
    }

    #[test]
    fn the_namespace_list_narrows_by_catalog_scope_and_schema_name() {
        assert_eq!(
            namespace_list_sql(None, &None),
            "SELECT catalog_name, schema_name FROM system.information_schema.schemata WHERE TRUE"
        );
        assert_eq!(
            namespace_list_sql(Some("main"), &filter(&["sales", "it's"])),
            "SELECT catalog_name, schema_name FROM system.information_schema.schemata \
             WHERE TRUE AND catalog_name = 'main' AND schema_name IN ('it\\'s', 'sales')"
        );
        assert!(namespace_list_sql(None, &filter(&[])).ends_with(" AND FALSE"));
    }

    #[test]
    fn namespaces_are_catalog_and_schema_without_the_excluded_ones() {
        let batch = text_rows(&[
            &["main", "sales"],
            &["main", "information_schema"],
            &["system", "billing"],
            &["hive_metastore", "default"],
            &["samples", "tpch"],
            &["main", "default"],
        ]);
        assert_eq!(
            namespaces(&[batch]).unwrap(),
            [["main", "default"], ["main", "sales"], ["samples", "tpch"]]
                .map(|names| Listed::Item(NamespacePath {
                    names: names.map(str::to_owned).to_vec()
                }))
                .to_vec()
        );
    }

    #[test]
    fn the_table_list_narrows_by_scope_schema_and_table_name() {
        assert_eq!(
            table_list_sql(Some("main"), &filter(&["sales"]), &filter(&["orders"])),
            "SELECT table_catalog, table_schema, table_name, table_type \
             FROM system.information_schema.tables WHERE TRUE AND table_catalog = 'main' \
             AND table_schema IN ('sales') AND table_name IN ('orders')"
        );
        assert_eq!(
            table_list_sql(None, &None, &None),
            "SELECT table_catalog, table_schema, table_name, table_type \
             FROM system.information_schema.tables WHERE TRUE"
        );
    }

    #[test]
    fn table_entries_have_their_kinds_without_the_excluded_ones() {
        let batch = text_rows(&[
            &["main", "sales", "orders", "MANAGED"],
            &["main", "sales", "files", "EXTERNAL"],
            &["main", "sales", "recent", "VIEW"],
            &["main", "sales", "daily", "MATERIALIZED_VIEW"],
            &["main", "sales", "events", "STREAMING_TABLE"],
            &["main", "sales", "remote", "FOREIGN"],
            &["main", "sales", "kpis", "METRIC_VIEW"],
            &["main", "information_schema", "tables", "VIEW"],
            &["system", "access", "audit", "MANAGED"],
        ]);
        let entries = table_entries(&[batch]).unwrap();
        let listed: Vec<(String, TableKind)> = entries
            .into_iter()
            .map(|entry| match entry {
                Listed::Item(entry) => (entry.table.names().join("."), entry.kind),
                Listed::Unreadable { .. } => panic!("every row is an item"),
            })
            .collect();
        assert_eq!(
            listed,
            [
                (
                    "main.sales.daily",
                    TableKind::Other("MATERIALIZED_VIEW".to_owned())
                ),
                (
                    "main.sales.events",
                    TableKind::Other("STREAMING_TABLE".to_owned())
                ),
                ("main.sales.files", TableKind::External),
                ("main.sales.orders", TableKind::Table),
                ("main.sales.recent", TableKind::View),
                ("main.sales.remote", TableKind::Other("FOREIGN".to_owned())),
            ]
            .map(|(name, kind)| (name.to_owned(), kind))
            .to_vec()
        );
    }

    #[test]
    fn metadata_statements_select_the_requested_tables() {
        let requested = [["main", "sales", "orders"], ["main", "crm", "it's"]];
        let condition = "((table_catalog = 'main' AND table_schema = 'sales' \
                         AND table_name = 'orders') OR (table_catalog = 'main' \
                         AND table_schema = 'crm' AND table_name = 'it\\'s'))";
        assert_eq!(
            tables_sql(&requested),
            format!(
                "SELECT table_catalog, table_schema, table_name, table_type, \
                 data_source_format, comment FROM system.information_schema.tables \
                 WHERE {condition}"
            )
        );
        assert!(columns_sql(&requested).ends_with(&format!("WHERE {condition}")));
        assert!(columns_sql(&requested).contains("full_data_type"));
        let keys = keys_sql(&requested[..1]);
        assert!(keys.contains("WHERE tc.constraint_type IN ('PRIMARY KEY', 'FOREIGN KEY')"));
        assert!(keys.ends_with(
            "((tc.table_catalog = 'main' AND tc.table_schema = 'sales' \
             AND tc.table_name = 'orders'))"
        ));
        assert!(keys.contains("ref.ordinal_position = kcu.position_in_unique_constraint"));
    }

    #[test]
    fn only_three_part_references_are_requested() {
        let tables = [
            reference(["main", "sales", "orders"]),
            TableReference::new(vec!["sales".to_owned(), "orders".to_owned()]).unwrap(),
        ];
        assert_eq!(requested_tables(&tables), [["main", "sales", "orders"]]);
    }

    fn tables_batch() -> RecordBatch {
        RecordBatch::try_from_iter([
            (
                "table_catalog",
                text_column(&[Some("main"), Some("main"), Some("main")]),
            ),
            (
                "table_schema",
                text_column(&[Some("sales"), Some("sales"), Some("sales")]),
            ),
            (
                "table_name",
                text_column(&[Some("orders"), Some("lines"), Some("kpis")]),
            ),
            (
                "table_type",
                text_column(&[Some("MANAGED"), Some("EXTERNAL"), Some("METRIC_VIEW")]),
            ),
            (
                "data_source_format",
                text_column(&[
                    Some("DELTA"),
                    Some("PARQUET"),
                    Some("UNKNOWN_DATA_SOURCE_FORMAT"),
                ]),
            ),
            ("comment", text_column(&[Some("All orders"), None, None])),
        ])
        .unwrap()
    }

    /// One column row: table, column, position, nullability, type, and
    /// comment.
    type ColumnRow<'a> = (&'a str, &'a str, i32, &'a str, &'a str, Option<&'a str>);

    fn columns_batch() -> RecordBatch {
        let rows: [ColumnRow<'_>; 5] = [
            ("orders", "customer_id", 1, "YES", "bigint", None),
            ("orders", "id", 0, "NO", "bigint", Some("Order number")),
            (
                "orders",
                "Note",
                2,
                "YES",
                "string collate UTF8_LCASE",
                None,
            ),
            ("lines", "order_id", 0, "NO", "bigint", None),
            ("lines", "line", 1, "NO", "int", None),
        ];
        RecordBatch::try_from_iter([
            ("table_catalog", text_column(&[Some("main"); 5])),
            ("table_schema", text_column(&[Some("sales"); 5])),
            ("table_name", text_column(&rows.map(|row| Some(row.0)))),
            ("column_name", text_column(&rows.map(|row| Some(row.1)))),
            ("ordinal_position", int_column(&rows.map(|row| row.2))),
            ("is_nullable", text_column(&rows.map(|row| Some(row.3)))),
            ("full_data_type", text_column(&rows.map(|row| Some(row.4)))),
            ("comment", text_column(&rows.map(|row| row.5))),
        ])
        .unwrap()
    }

    /// One key column row: table, constraint, type, column, position, and the
    /// referenced catalog, schema, table, and column.
    type KeyRow<'a> = (
        &'a str,
        &'a str,
        &'a str,
        &'a str,
        i32,
        Option<[&'a str; 4]>,
    );

    fn keys_batch(rows: &[KeyRow<'_>]) -> RecordBatch {
        let texts = |pick: &dyn Fn(&KeyRow<'_>) -> Option<String>| {
            Arc::new(StringArray::from(rows.iter().map(pick).collect::<Vec<_>>())) as ArrayRef
        };
        let referenced = |index: usize| texts(&|row| row.5.map(|names| names[index].to_owned()));
        RecordBatch::try_from_iter([
            ("table_catalog", texts(&|_| Some("main".to_owned()))),
            ("table_schema", texts(&|_| Some("sales".to_owned()))),
            ("table_name", texts(&|row| Some(row.0.to_owned()))),
            ("constraint_name", texts(&|row| Some(row.1.to_owned()))),
            ("constraint_type", texts(&|row| Some(row.2.to_owned()))),
            ("column_name", texts(&|row| Some(row.3.to_owned()))),
            (
                "ordinal_position",
                int_column(&rows.iter().map(|row| row.4).collect::<Vec<_>>()),
            ),
            ("ref_catalog", referenced(0)),
            ("ref_schema", referenced(1)),
            ("ref_table", referenced(2)),
            ("ref_column", referenced(3)),
        ])
        .unwrap()
    }

    #[test]
    fn source_metadata_assembles_columns_and_keys_of_each_read_table() {
        let keys = keys_batch(&[
            ("orders", "orders_pk", "PRIMARY KEY", "id", 1, None),
            (
                "lines",
                "lines_order_fk",
                "FOREIGN KEY",
                "order_id",
                1,
                Some(["main", "sales", "orders", "id"]),
            ),
            ("lines", "lines_pk", "PRIMARY KEY", "line", 2, None),
            ("lines", "lines_pk", "PRIMARY KEY", "order_id", 1, None),
        ]);
        let read = source_metadata(&[tables_batch()], &[columns_batch()], &[keys]).unwrap();
        assert_eq!(
            read.keys()
                .map(|(_, _, name)| name.as_str())
                .collect::<Vec<_>>(),
            ["lines", "orders"],
            "the metric view is not read"
        );
        let orders = &read[&("main".to_owned(), "sales".to_owned(), "orders".to_owned())];
        assert_eq!(
            orders,
            &DatabricksTable {
                catalog: "main".to_owned(),
                schema: "sales".to_owned(),
                name: "orders".to_owned(),
                table_type: "MANAGED".to_owned(),
                data_source_format: "DELTA".to_owned(),
                comment: Some("All orders".to_owned()),
                columns: vec![
                    DatabricksColumn {
                        name: "id".to_owned(),
                        ordinal_position: 0,
                        nullable: false,
                        full_data_type: "bigint".to_owned(),
                        comment: Some("Order number".to_owned()),
                    },
                    DatabricksColumn {
                        name: "customer_id".to_owned(),
                        ordinal_position: 1,
                        nullable: true,
                        full_data_type: "bigint".to_owned(),
                        comment: None,
                    },
                    DatabricksColumn {
                        name: "Note".to_owned(),
                        ordinal_position: 2,
                        nullable: true,
                        full_data_type: "string collate UTF8_LCASE".to_owned(),
                        comment: None,
                    },
                ],
                primary_key: Some(DatabricksPrimaryKey {
                    name: "orders_pk".to_owned(),
                    columns: vec!["id".to_owned()],
                }),
                foreign_keys: Vec::new(),
            }
        );
        let lines = &read[&("main".to_owned(), "sales".to_owned(), "lines".to_owned())];
        assert_eq!(
            lines.primary_key,
            Some(DatabricksPrimaryKey {
                name: "lines_pk".to_owned(),
                columns: vec!["order_id".to_owned(), "line".to_owned()],
            })
        );
        assert_eq!(
            lines.foreign_keys,
            [DatabricksForeignKey {
                name: "lines_order_fk".to_owned(),
                columns: vec!["order_id".to_owned()],
                referenced: Some(DatabricksReferencedTable {
                    catalog: "main".to_owned(),
                    schema: "sales".to_owned(),
                    table: "orders".to_owned(),
                    columns: vec!["id".to_owned()],
                }),
            }]
        );
    }

    #[test]
    fn a_foreign_key_to_a_hidden_key_has_no_known_reference() {
        let keys = keys_batch(&[
            (
                "lines",
                "lines_fk",
                "FOREIGN KEY",
                "order_id",
                1,
                Some(["main", "sales", "orders", "id"]),
            ),
            ("lines", "lines_fk", "FOREIGN KEY", "line", 2, None),
        ]);
        let read = source_metadata(&[tables_batch()], &[columns_batch()], &[keys]).unwrap();
        let lines = &read[&("main".to_owned(), "sales".to_owned(), "lines".to_owned())];
        assert_eq!(lines.foreign_keys[0].columns, ["order_id", "line"]);
        assert_eq!(lines.foreign_keys[0].referenced, None);
    }

    #[test]
    fn inconsistent_rows_are_an_unexpected_catalog_result() {
        let two_tables = keys_batch(&[
            (
                "lines",
                "fk",
                "FOREIGN KEY",
                "a",
                1,
                Some(["main", "sales", "orders", "id"]),
            ),
            (
                "lines",
                "fk",
                "FOREIGN KEY",
                "b",
                2,
                Some(["main", "sales", "other", "id"]),
            ),
        ]);
        let two_types = keys_batch(&[
            ("lines", "k", "PRIMARY KEY", "a", 1, None),
            ("lines", "k", "FOREIGN KEY", "b", 2, None),
        ]);
        let unique = keys_batch(&[("lines", "k", "UNIQUE", "a", 1, None)]);
        for keys in [two_tables, two_types, unique] {
            let error = source_metadata(&[tables_batch()], &[], &[keys]).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Internal);
        }
        let mut columns = columns_batch().columns().to_vec();
        columns[5] = text_column(&[Some("MAYBE"); 5]);
        let columns = RecordBatch::try_from_iter(
            columns_batch()
                .schema()
                .fields()
                .iter()
                .map(|field| field.name().clone())
                .zip(columns),
        )
        .unwrap();
        assert!(source_metadata(&[tables_batch()], &[columns], &[]).is_err());
        let wrong_shape = text_rows(&[&["main", "sales"]]);
        assert!(source_metadata(&[wrong_shape], &[], &[]).is_err());
    }

    #[test]
    fn a_table_not_shown_is_not_found_and_a_malformed_reference_does_not_exist() {
        let read = source_metadata(&[tables_batch()], &[columns_batch()], &[]).unwrap();
        let read_at = SystemTime::UNIX_EPOCH;
        let tables = [
            reference(["main", "sales", "orders"]),
            reference(["main", "sales", "hidden"]),
            reference(["main", "sales", "kpis"]),
            reference(["main", "sales", "ORDERS"]),
            TableReference::new(vec!["sales".to_owned(), "orders".to_owned()]).unwrap(),
            reference(["main", "sales", "orders"]),
        ];
        let records = records(&tables, &read, read_at);
        assert!(
            matches!(&records[0], TableRecord::Read { metadata, .. } if metadata.name == "orders")
        );
        for record in &records[1..4] {
            assert!(matches!(record, TableRecord::NotFound { .. }), "{record:?}");
        }
        assert!(matches!(records[4], TableRecord::DoesNotExist { .. }));
        assert_eq!(records[5], records[0]);
    }
}
