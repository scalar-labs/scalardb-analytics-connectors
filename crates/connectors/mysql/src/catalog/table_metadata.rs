//! The table metadata request.
//!
//! [`table_metadata`] reads the requested tables' rows from `TABLES`, `VIEWS`,
//! `COLUMNS`, `STATISTICS`, `KEY_COLUMN_USAGE`, and `CHECK_CONSTRAINTS` with
//! `TABLE_CONSTRAINTS`, one view at a time and each into typed rows, together
//! with the account's `SELECT` grants, and assembles each table's source
//! metadata from them.

use std::{
    collections::{BTreeMap, HashMap},
    time::SystemTime,
};

use arrow_array::Array;
use connector_sdk::{ConnectorError, TableRecord, TableReference};
use connector_sql::{
    catalog::{int64, required, text, unexpected_catalog_result},
    literal::quote_string,
};

use super::{
    Catalog, MysqlCheck, MysqlColumn, MysqlForeignKey, MysqlGeneration, MysqlKey, MysqlPrimaryKey,
    MysqlReferencedTable, MysqlTable, MysqlTableKind, MysqlUniqueKey, MysqlValueSource,
    privileges::SelectGrants,
};

/// A table's database and name, as `information_schema` reports them.
type TableKey = (String, String);

/// Reads one record per requested table.
///
/// A reference that is not a database and a table name does not exist. A
/// table `information_schema` does not show does not exist when the account
/// may read every table of its database, and is otherwise not found:
/// `information_schema` also hides what the account holds no privilege on.
pub(crate) fn table_metadata(
    catalog: Catalog<'_, '_>,
    tables: &[TableReference],
) -> Result<Vec<TableRecord<MysqlTable>>, ConnectorError> {
    let requested: Vec<(&str, &str)> = tables
        .iter()
        .filter_map(|table| match table.names() {
            [schema, name] => Some((schema.as_str(), name.as_str())),
            _ => None,
        })
        .collect();
    let (read, grants) = if requested.is_empty() {
        (HashMap::new(), None)
    } else {
        let grants = SelectGrants::read(catalog)?;
        (source_metadata(catalog, &requested, &grants)?, Some(grants))
    };
    let read_at = SystemTime::now();
    Ok(tables
        .iter()
        .map(|table| {
            let [schema, name] = table.names() else {
                return TableRecord::DoesNotExist {
                    table: table.clone(),
                };
            };
            match read.get(&(schema.clone(), name.clone())) {
                Some(metadata) => TableRecord::Read {
                    metadata: metadata.clone(),
                    read_at,
                },
                None if grants
                    .as_ref()
                    .is_some_and(|grants| grants.covers_schema(schema)) =>
                {
                    TableRecord::DoesNotExist {
                        table: table.clone(),
                    }
                }
                None => TableRecord::NotFound {
                    table: table.clone(),
                    reason: "information_schema shows only the tables the account holds \
                             privileges on"
                        .to_owned(),
                },
            }
        })
        .collect())
}

/// Reads the requested tables' rows and assembles the source metadata of each
/// table found, keyed by database and name.
///
/// `requested` must not be empty.
fn source_metadata(
    catalog: Catalog<'_, '_>,
    requested: &[(&str, &str)],
    grants: &SelectGrants,
) -> Result<HashMap<TableKey, MysqlTable>, ConnectorError> {
    let condition = requested_condition("", requested);
    let mut tables = read_tables(catalog, &condition, grants)?;
    let definitions = read_views(catalog, &condition)?;
    for (key, definition) in definitions {
        if let Some(table) = tables.get_mut(&key) {
            table.definition = Some(definition).filter(|definition| !definition.is_empty());
        }
    }
    for (key, column) in read_columns(catalog, &condition)? {
        if let Some(table) = tables.get_mut(&key) {
            table.columns.push(column);
        }
    }
    add_unique_keys(&mut tables, read_unique_key_parts(catalog, &condition)?)?;
    add_foreign_keys(&mut tables, read_foreign_key_columns(catalog, &condition)?)?;
    for (key, check) in read_checks(catalog, &requested_condition("tc.", requested))? {
        if let Some(table) = tables.get_mut(&key) {
            table.checks.push(check);
        }
    }
    for table in tables.values_mut() {
        table.columns.sort_by_key(|column| column.ordinal_position);
        table.checks.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(tables)
}

/// A condition selecting the requested tables' rows of a view with
/// `TABLE_SCHEMA` and `TABLE_NAME` columns, qualified with `qualifier`, which
/// may also select tables whose names differ only in case.
fn requested_condition(qualifier: &str, requested: &[(&str, &str)]) -> String {
    let pairs: Vec<String> = requested
        .iter()
        .map(|(schema, name)| format!("({}, {})", quote_string(schema), quote_string(name)))
        .collect();
    format!(
        "({qualifier}TABLE_SCHEMA, {qualifier}TABLE_NAME) IN ({})",
        pairs.join(", ")
    )
}

/// The key of a row, exactly as the view reports it.
fn key(
    schemas: &[Option<&str>],
    names: &[Option<&str>],
    row: usize,
) -> Result<TableKey, ConnectorError> {
    Ok((
        required(schemas[row])?.to_owned(),
        required(names[row])?.to_owned(),
    ))
}

/// Text that `information_schema` leaves empty for "none".
fn non_empty(value: Option<&str>) -> Option<String> {
    value.filter(|value| !value.is_empty()).map(str::to_owned)
}

// Reading the views: one statement each, read into typed rows.

/// The requested base tables and views, with no columns or constraints yet.
fn read_tables(
    catalog: Catalog<'_, '_>,
    condition: &str,
    grants: &SelectGrants,
) -> Result<HashMap<TableKey, MysqlTable>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE, TABLE_COMMENT \
         FROM information_schema.TABLES \
         WHERE {condition} AND TABLE_TYPE IN ('BASE TABLE', 'VIEW')"
    ))?;
    let mut tables = HashMap::new();
    for batch in &batches {
        let (schemas, names, kinds, comments) = (
            text(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            text(batch, 3)?,
        );
        for row in 0..batch.num_rows() {
            let (schema, name) = key(&schemas, &names, row)?;
            let kind = match required(kinds[row])? {
                "BASE TABLE" => MysqlTableKind::BaseTable,
                "VIEW" => MysqlTableKind::View,
                _ => return Err(unexpected_catalog_result()),
            };
            let comment = match kind {
                // MySQL reports a view's comment as `VIEW`.
                MysqlTableKind::View => None,
                MysqlTableKind::BaseTable => non_empty(comments[row]),
            };
            let columns_may_be_hidden = !grants.covers_table(&schema, &name);
            tables.insert(
                (schema.clone(), name.clone()),
                MysqlTable {
                    schema,
                    name,
                    kind,
                    comment,
                    definition: None,
                    columns_may_be_hidden,
                    columns: Vec::new(),
                    primary_key: None,
                    unique_keys: Vec::new(),
                    foreign_keys: Vec::new(),
                    checks: Vec::new(),
                },
            );
        }
    }
    Ok(tables)
}

/// The requested views' definitions, empty for a view whose definition the
/// account may not see.
fn read_views(
    catalog: Catalog<'_, '_>,
    condition: &str,
) -> Result<Vec<(TableKey, String)>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, VIEW_DEFINITION FROM information_schema.VIEWS \
         WHERE {condition}"
    ))?;
    let mut views = Vec::new();
    for batch in &batches {
        let (schemas, names, definitions) = (text(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
        for (row, definition) in definitions.into_iter().enumerate() {
            views.push((
                key(&schemas, &names, row)?,
                definition.unwrap_or_default().to_owned(),
            ));
        }
    }
    Ok(views)
}

/// The columns of the requested tables that the account may see.
fn read_columns(
    catalog: Catalog<'_, '_>,
    condition: &str,
) -> Result<Vec<(TableKey, MysqlColumn)>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, CAST(ORDINAL_POSITION AS SIGNED), \
                IS_NULLABLE, COLUMN_TYPE, DATA_TYPE, CHARACTER_SET_NAME, COLLATION_NAME, \
                COLUMN_COMMENT, COLUMN_DEFAULT, EXTRA, GENERATION_EXPRESSION \
         FROM information_schema.COLUMNS WHERE {condition}"
    ))?;
    let mut columns = Vec::new();
    for batch in &batches {
        let (schemas, names, column_names, positions) = (
            text(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            int64(batch, 3)?,
        );
        let (nullable, column_types, data_types) =
            (text(batch, 4)?, text(batch, 5)?, text(batch, 6)?);
        let (character_sets, collations, comments) =
            (text(batch, 7)?, text(batch, 8)?, text(batch, 9)?);
        let (defaults, extras, generation) = (text(batch, 10)?, text(batch, 11)?, text(batch, 12)?);
        for row in 0..batch.num_rows() {
            let value_source = value_source(
                defaults[row],
                extras[row].unwrap_or_default(),
                generation[row].unwrap_or_default(),
            );
            columns.push((
                key(&schemas, &names, row)?,
                MysqlColumn {
                    name: required(column_names[row])?.to_owned(),
                    ordinal_position: u32::try_from(positions.value(row))
                        .map_err(|_| unexpected_catalog_result())?,
                    nullable: required(nullable[row])? == "YES",
                    data_source_type: required(column_types[row])?.to_owned(),
                    data_type: required(data_types[row])?.to_owned(),
                    character_set: character_sets[row].map(str::to_owned),
                    collation: collations[row].map(str::to_owned),
                    comment: non_empty(comments[row]),
                    value_source,
                },
            ));
        }
    }
    Ok(columns)
}

/// Where MySQL takes a column's value from, by its `COLUMN_DEFAULT`, `EXTRA`,
/// and `GENERATION_EXPRESSION`.
///
/// `EXTRA` holds space-separated attributes, such as `auto_increment`,
/// `DEFAULT_GENERATED` for a default expression, and `STORED GENERATED` or
/// `VIRTUAL GENERATED`.
fn value_source(default: Option<&str>, extra: &str, generation: &str) -> Option<MysqlValueSource> {
    let generated = |kind: &str| extra.contains(&format!("{kind} GENERATED"));
    if generated("STORED") || generated("VIRTUAL") {
        return Some(MysqlValueSource::Generated {
            generation: if generated("STORED") {
                MysqlGeneration::Stored
            } else {
                MysqlGeneration::Virtual
            },
            expression: generation.to_owned(),
        });
    }
    if extra.contains("auto_increment") {
        return Some(MysqlValueSource::AutoIncrement);
    }
    let default = default?.to_owned();
    Some(if extra.contains("DEFAULT_GENERATED") {
        MysqlValueSource::DefaultExpression {
            expression: default,
        }
    } else {
        MysqlValueSource::Default { value: default }
    })
}

/// One key part of a unique index.
struct KeyPart {
    table: TableKey,
    index: String,
    position: i64,
    key: MysqlKey,
}

/// The key parts of the requested tables' unique indexes, the primary key's
/// among them.
fn read_unique_key_parts(
    catalog: Catalog<'_, '_>,
    condition: &str,
) -> Result<Vec<KeyPart>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, INDEX_NAME, CAST(SEQ_IN_INDEX AS SIGNED), \
                COLUMN_NAME, CAST(SUB_PART AS SIGNED), EXPRESSION \
         FROM information_schema.STATISTICS WHERE NON_UNIQUE = 0 AND {condition}"
    ))?;
    let mut parts = Vec::new();
    for batch in &batches {
        let (schemas, names, indexes, positions) = (
            text(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            int64(batch, 3)?,
        );
        let (columns, prefixes, expressions) = (text(batch, 4)?, int64(batch, 5)?, text(batch, 6)?);
        for row in 0..batch.num_rows() {
            let part = match (columns[row], expressions[row]) {
                (Some(column), _) => MysqlKey::Column {
                    column: column.to_owned(),
                    prefix_length: if prefixes.is_null(row) {
                        None
                    } else {
                        Some(
                            u32::try_from(prefixes.value(row))
                                .map_err(|_| unexpected_catalog_result())?,
                        )
                    },
                },
                (None, Some(expression)) => MysqlKey::Expression {
                    expression: expression.to_owned(),
                },
                (None, None) => return Err(unexpected_catalog_result()),
            };
            parts.push(KeyPart {
                table: key(&schemas, &names, row)?,
                index: required(indexes[row])?.to_owned(),
                position: positions.value(row),
                key: part,
            });
        }
    }
    Ok(parts)
}

/// Adds each table's primary key and other unique indexes, in name order.
fn add_unique_keys(
    tables: &mut HashMap<TableKey, MysqlTable>,
    parts: Vec<KeyPart>,
) -> Result<(), ConnectorError> {
    let mut indexes: BTreeMap<(TableKey, String), BTreeMap<i64, MysqlKey>> = BTreeMap::new();
    for part in parts {
        indexes
            .entry((part.table, part.index))
            .or_default()
            .insert(part.position, part.key);
    }
    for ((table, index), keys) in indexes {
        let Some(table) = tables.get_mut(&table) else {
            continue;
        };
        let keys: Vec<MysqlKey> = keys.into_values().collect();
        if index == "PRIMARY" {
            let columns = keys
                .into_iter()
                .map(|key| match key {
                    MysqlKey::Column { column, .. } => Ok(column),
                    MysqlKey::Expression { .. } => Err(unexpected_catalog_result()),
                })
                .collect::<Result<_, _>>()?;
            table.primary_key = Some(MysqlPrimaryKey { columns });
        } else {
            table.unique_keys.push(MysqlUniqueKey { name: index, keys });
        }
    }
    Ok(())
}

/// One column of a foreign key.
struct ForeignKeyColumn {
    table: TableKey,
    name: String,
    position: i64,
    column: String,
    referenced: TableKey,
    referenced_column: String,
}

/// The columns of the requested tables' foreign keys.
fn read_foreign_key_columns(
    catalog: Catalog<'_, '_>,
    condition: &str,
) -> Result<Vec<ForeignKeyColumn>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, CAST(ORDINAL_POSITION AS SIGNED), \
                COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, \
                REFERENCED_COLUMN_NAME \
         FROM information_schema.KEY_COLUMN_USAGE \
         WHERE REFERENCED_TABLE_NAME IS NOT NULL AND {condition}"
    ))?;
    let mut columns = Vec::new();
    for batch in &batches {
        let (schemas, names, constraints, positions) = (
            text(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            int64(batch, 3)?,
        );
        let (column_names, referenced_schemas, referenced_names, referenced_columns) = (
            text(batch, 4)?,
            text(batch, 5)?,
            text(batch, 6)?,
            text(batch, 7)?,
        );
        for row in 0..batch.num_rows() {
            columns.push(ForeignKeyColumn {
                table: key(&schemas, &names, row)?,
                name: required(constraints[row])?.to_owned(),
                position: positions.value(row),
                column: required(column_names[row])?.to_owned(),
                referenced: key(&referenced_schemas, &referenced_names, row)?,
                referenced_column: required(referenced_columns[row])?.to_owned(),
            });
        }
    }
    Ok(columns)
}

/// Adds each table's foreign keys, in name order.
fn add_foreign_keys(
    tables: &mut HashMap<TableKey, MysqlTable>,
    columns: Vec<ForeignKeyColumn>,
) -> Result<(), ConnectorError> {
    // Each foreign key's referenced table and its column pairs by position.
    type ColumnPairs = BTreeMap<i64, (String, String)>;
    let mut keys: BTreeMap<(TableKey, String), (TableKey, ColumnPairs)> = BTreeMap::new();
    for column in columns {
        let (referenced, parts) = keys
            .entry((column.table, column.name))
            .or_insert_with(|| (column.referenced.clone(), BTreeMap::new()));
        if *referenced != column.referenced {
            return Err(unexpected_catalog_result());
        }
        parts.insert(column.position, (column.column, column.referenced_column));
    }
    for ((table, name), ((schema, referenced_table), parts)) in keys {
        let Some(table) = tables.get_mut(&table) else {
            continue;
        };
        let (columns, referenced_columns) = parts.into_values().unzip();
        table.foreign_keys.push(MysqlForeignKey {
            name,
            columns,
            referenced: MysqlReferencedTable {
                schema,
                table: referenced_table,
                columns: referenced_columns,
            },
        });
    }
    Ok(())
}

/// The requested tables' CHECK constraints.
fn read_checks(
    catalog: Catalog<'_, '_>,
    condition: &str,
) -> Result<Vec<(TableKey, MysqlCheck)>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT tc.TABLE_SCHEMA, tc.TABLE_NAME, tc.CONSTRAINT_NAME, cc.CHECK_CLAUSE, \
                tc.ENFORCED \
         FROM information_schema.TABLE_CONSTRAINTS tc \
         JOIN information_schema.CHECK_CONSTRAINTS cc \
           ON cc.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA \
          AND cc.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
         WHERE tc.CONSTRAINT_TYPE = 'CHECK' AND {condition}"
    ))?;
    let mut checks = Vec::new();
    for batch in &batches {
        let (schemas, names, constraints, clauses, enforced) = (
            text(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            text(batch, 3)?,
            text(batch, 4)?,
        );
        for row in 0..batch.num_rows() {
            checks.push((
                key(&schemas, &names, row)?,
                MysqlCheck {
                    name: required(constraints[row])?.to_owned(),
                    expression: required(clauses[row])?.to_owned(),
                    enforced: required(enforced[row])? == "YES",
                },
            ));
        }
    }
    Ok(checks)
}
