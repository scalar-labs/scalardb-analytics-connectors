//! The table metadata request.
//!
//! [`table_metadata`] reads the requested tables' rows from `sys.objects`,
//! `sys.columns` with `sys.types`, `sys.indexes`, `sys.foreign_keys`,
//! `sys.check_constraints`, and their long text values, one statement each
//! and each into typed rows, and assembles each table's source metadata from
//! them in Rust.

use std::{collections::HashMap, time::SystemTime};

use arrow_array::{Array, RecordBatch};
use connector_sdk::{ConnectorError, TableRecord, TableReference};
use connector_sql::catalog::{int32, required, text, unexpected_catalog_result};

use super::{
    BINARY, Catalog, EXCLUDED_NAMESPACES, LISTED_OBJECTS, MetadataVisibility, SqlServerCheck,
    SqlServerColumn, SqlServerForeignKey, SqlServerKey, SqlServerReferencedTable, SqlServerTable,
    SqlServerTableKind, SqlServerType, SqlServerUniqueKey, SqlServerUniqueKind,
    SqlServerValueSource,
    long_text::{LongTexts, TextKind},
    nstring,
};

/// Why a requested table is "not found".
const MAY_BE_HIDDEN: &str = "SQL Server hides tables the credential has no permission on, \
     so the table may exist; VIEW DEFINITION on the database shows every table";

/// Reads one record per requested table.
///
/// A reference that is not a schema and a table name does not exist. A table
/// not found does not exist when the credential sees every object's metadata,
/// and is "not found" otherwise, because it may be hidden.
pub(crate) fn table_metadata(
    catalog: Catalog<'_, '_>,
    tables: &[TableReference],
    visibility: MetadataVisibility,
) -> Result<Vec<TableRecord<SqlServerTable>>, ConnectorError> {
    let requested: Vec<(&str, &str)> = tables
        .iter()
        .filter_map(|table| match table.names() {
            [namespace, name] => Some((namespace.as_str(), name.as_str())),
            _ => None,
        })
        .collect();
    let read = if requested.is_empty() {
        HashMap::new()
    } else {
        source_metadata(catalog, &requested)?
    };
    let read_at = SystemTime::now();
    Ok(tables
        .iter()
        .map(|table| {
            let found = match table.names() {
                [namespace, name] => read.get(&(namespace.clone(), name.clone())).cloned(),
                _ => {
                    return TableRecord::DoesNotExist {
                        table: table.clone(),
                    };
                }
            };
            match found {
                Some(metadata) => TableRecord::Read { metadata, read_at },
                None if visibility.complete => TableRecord::DoesNotExist {
                    table: table.clone(),
                },
                None => TableRecord::NotFound {
                    table: table.clone(),
                    reason: MAY_BE_HIDDEN.to_owned(),
                },
            }
        })
        .collect())
}

/// Reads the requested tables' catalog rows and assembles the source metadata
/// of each table found, keyed by schema and name.
///
/// `requested` must not be empty.
fn source_metadata(
    catalog: Catalog<'_, '_>,
    requested: &[(&str, &str)],
) -> Result<HashMap<(String, String), SqlServerTable>, ConnectorError> {
    let object_ids = requested_object_ids(requested);
    let mut tables = read_objects(catalog, &object_ids)?;
    let columns = read_columns(catalog, &object_ids)?;
    let indexes = read_unique_indexes(catalog, &object_ids)?;
    let foreign_keys = read_foreign_keys(catalog, &object_ids)?;
    let checks = read_checks(catalog, &object_ids)?;
    let texts = LongTexts::read(catalog, &object_ids)?;

    for row in columns {
        let table = tables
            .get_mut(&row.object_id)
            .ok_or_else(unexpected_catalog_result)?;
        let column = row.into_column(&texts);
        table.columns.push(column);
    }
    for index in indexes {
        let table = tables
            .get_mut(&index.object_id)
            .ok_or_else(unexpected_catalog_result)?;
        let enforced = !index.disabled;
        if index.primary_key {
            table.primary_key = Some(SqlServerKey {
                name: index.name,
                columns: index.columns,
                enforced,
            });
        } else {
            table.unique_keys.push(SqlServerUniqueKey {
                name: index.name,
                kind: if index.unique_constraint {
                    SqlServerUniqueKind::Constraint
                } else {
                    SqlServerUniqueKind::Index
                },
                columns: index.columns,
                condition: texts.get(TextKind::Filter, index.object_id, index.index_id),
                filtered: index.filtered,
                enforced,
            });
        }
    }
    for (object_id, foreign_key) in foreign_keys {
        let table = tables
            .get_mut(&object_id)
            .ok_or_else(unexpected_catalog_result)?;
        table.foreign_keys.push(foreign_key);
    }
    for (object_id, check_id, mut check) in checks {
        let table = tables
            .get_mut(&object_id)
            .ok_or_else(unexpected_catalog_result)?;
        check.expression = texts.get(TextKind::Check, object_id, check_id);
        table.checks.push(check);
    }

    Ok(tables
        .into_values()
        .map(|mut table| {
            table.comment = texts.get(TextKind::Comment, table.object_id, 0);
            table.definition = texts.get(TextKind::Definition, table.object_id, 0);
            table.unique_keys.sort_by(|a, b| a.name.cmp(&b.name));
            table.foreign_keys.sort_by(|a, b| a.name.cmp(&b.name));
            table.checks.sort_by(|a, b| a.name.cmp(&b.name));
            ((table.namespace.clone(), table.name.clone()), table)
        })
        .collect())
}

/// A statement selecting the `object_id`s of the requested tables and views.
fn requested_object_ids(requested: &[(&str, &str)]) -> String {
    let pairs: Vec<String> = requested
        .iter()
        .map(|(namespace, name)| format!("({}, {})", nstring(namespace), nstring(name)))
        .collect();
    format!(
        "SELECT o.object_id FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
         WHERE {EXCLUDED_NAMESPACES} AND {LISTED_OBJECTS} AND EXISTS ( \
           SELECT 1 FROM (VALUES {}) r(namespace, name) \
           WHERE s.name COLLATE {BINARY} = r.namespace AND o.name COLLATE {BINARY} = r.name)",
        pairs.join(", ")
    )
}

/// Reads an `int` column that the catalog never leaves NULL.
fn ints(batch: &RecordBatch, index: usize) -> Result<Vec<i32>, ConnectorError> {
    let values = int32(batch, index)?;
    if values.null_count() > 0 {
        return Err(unexpected_catalog_result());
    }
    Ok(values.values().to_vec())
}

/// The requested tables, keyed by `object_id`, with no columns or
/// constraints yet.
fn read_objects(
    catalog: Catalog<'_, '_>,
    object_ids: &str,
) -> Result<HashMap<i32, SqlServerTable>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT o.object_id, s.name, o.name, CAST(o.type AS char(1)), \
                CAST(ISNULL(HAS_PERMS_BY_NAME(QUOTENAME(s.name) + N'.' + QUOTENAME(o.name), \
                                              N'OBJECT', N'VIEW DEFINITION'), 0) AS int) \
         FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id \
         WHERE o.object_id IN ({object_ids})"
    ))?;
    let mut tables = HashMap::new();
    for batch in &batches {
        let (ids, namespaces, names, kinds) = (
            ints(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            text(batch, 3)?,
        );
        let visible = ints(batch, 4)?;
        for row in 0..batch.num_rows() {
            let kind = match required(kinds[row])? {
                "U" => SqlServerTableKind::Table,
                "V" => SqlServerTableKind::View,
                _ => return Err(unexpected_catalog_result()),
            };
            tables.insert(
                ids[row],
                SqlServerTable {
                    namespace: required(namespaces[row])?.to_owned(),
                    name: required(names[row])?.to_owned(),
                    object_id: ids[row],
                    kind,
                    definition_visible: visible[row] == 1,
                    comment: None,
                    definition: None,
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

/// One `sys.columns` row with its type.
struct ColumnRow {
    object_id: i32,
    column_id: i32,
    name: String,
    nullable: bool,
    /// `None` when SQL Server hides the column's alias type.
    ty: Option<SqlServerType>,
    max_length: i16,
    precision: u8,
    scale: u8,
    identity: bool,
    /// `Some(persisted)` for a computed column.
    computed: Option<bool>,
    has_default: bool,
}

impl ColumnRow {
    fn into_column(self, texts: &LongTexts) -> SqlServerColumn {
        let expression = |kind| texts.get(kind, self.object_id, self.column_id);
        let value_source = if self.identity {
            Some(SqlServerValueSource::Identity)
        } else if let Some(persisted) = self.computed {
            Some(SqlServerValueSource::Computed {
                persisted,
                expression: expression(TextKind::Computed),
            })
        } else if self.has_default {
            Some(SqlServerValueSource::Default {
                expression: expression(TextKind::Default),
            })
        } else {
            None
        };
        SqlServerColumn {
            comment: expression(TextKind::Comment),
            name: self.name,
            column_id: self.column_id,
            nullable: self.nullable,
            ty: self.ty,
            max_length: self.max_length,
            precision: self.precision,
            scale: self.scale,
            value_source,
        }
    }
}

/// The columns of the requested tables, in `column_id` order.
fn read_columns(
    catalog: Catalog<'_, '_>,
    object_ids: &str,
) -> Result<Vec<ColumnRow>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT c.object_id, c.column_id, c.name, CAST(c.is_nullable AS int), \
                t.name, SCHEMA_NAME(t.schema_id), CAST(ISNULL(t.is_user_defined, 0) AS int), \
                CASE WHEN t.is_assembly_type = 0 THEN TYPE_NAME(c.system_type_id) END, \
                CAST(c.max_length AS int), CAST(c.precision AS int), CAST(c.scale AS int), \
                CAST(c.is_identity AS int), CAST(c.is_computed AS int), \
                CAST(ISNULL(cc.is_persisted, 0) AS int), \
                CASE WHEN c.default_object_id <> 0 THEN 1 ELSE 0 END \
         FROM sys.columns c \
         LEFT JOIN sys.types t ON t.user_type_id = c.user_type_id \
         LEFT JOIN sys.computed_columns cc \
           ON cc.object_id = c.object_id AND cc.column_id = c.column_id \
         WHERE c.object_id IN ({object_ids}) \
         ORDER BY c.object_id, c.column_id"
    ))?;
    let mut columns = Vec::new();
    for batch in &batches {
        let (object_ids, column_ids, names, nullable) = (
            ints(batch, 0)?,
            ints(batch, 1)?,
            text(batch, 2)?,
            ints(batch, 3)?,
        );
        let (type_names, type_schemas, user_defined, system_types) = (
            text(batch, 4)?,
            text(batch, 5)?,
            ints(batch, 6)?,
            text(batch, 7)?,
        );
        let (max_lengths, precisions, scales) =
            (ints(batch, 8)?, ints(batch, 9)?, ints(batch, 10)?);
        let (identity, computed, persisted, has_default) = (
            ints(batch, 11)?,
            ints(batch, 12)?,
            ints(batch, 13)?,
            ints(batch, 14)?,
        );
        for row in 0..batch.num_rows() {
            let narrow = |value: i32| u8::try_from(value).map_err(|_| unexpected_catalog_result());
            columns.push(ColumnRow {
                object_id: object_ids[row],
                column_id: column_ids[row],
                name: required(names[row])?.to_owned(),
                nullable: nullable[row] == 1,
                ty: match type_names[row] {
                    Some(name) => Some(SqlServerType {
                        name: name.to_owned(),
                        schema: required(type_schemas[row])?.to_owned(),
                        user_defined: user_defined[row] == 1,
                        system_type: system_types[row].map(str::to_owned),
                    }),
                    None => None,
                },
                max_length: i16::try_from(max_lengths[row])
                    .map_err(|_| unexpected_catalog_result())?,
                precision: narrow(precisions[row])?,
                scale: narrow(scales[row])?,
                identity: identity[row] == 1,
                computed: (computed[row] == 1).then_some(persisted[row] == 1),
                has_default: has_default[row] == 1,
            });
        }
    }
    Ok(columns)
}

/// One unique index with its key columns in order.
struct UniqueIndexRow {
    object_id: i32,
    index_id: i32,
    name: String,
    primary_key: bool,
    unique_constraint: bool,
    filtered: bool,
    disabled: bool,
    columns: Vec<String>,
}

/// The unique indexes of the requested tables, including those that back
/// primary keys and unique constraints.
fn read_unique_indexes(
    catalog: Catalog<'_, '_>,
    object_ids: &str,
) -> Result<Vec<UniqueIndexRow>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT i.object_id, i.index_id, i.name, CAST(i.is_primary_key AS int), \
                CAST(i.is_unique_constraint AS int), CAST(i.has_filter AS int), \
                CAST(i.is_disabled AS int) \
         FROM sys.indexes i WHERE i.is_unique = 1 AND i.object_id IN ({object_ids})"
    ))?;
    let mut indexes = HashMap::new();
    for batch in &batches {
        let (object_ids, index_ids, names) = (ints(batch, 0)?, ints(batch, 1)?, text(batch, 2)?);
        let (primary_keys, unique_constraints, filtered, disabled) = (
            ints(batch, 3)?,
            ints(batch, 4)?,
            ints(batch, 5)?,
            ints(batch, 6)?,
        );
        for row in 0..batch.num_rows() {
            let key = (object_ids[row], index_ids[row]);
            indexes.insert(
                key,
                UniqueIndexRow {
                    object_id: object_ids[row],
                    index_id: index_ids[row],
                    name: required(names[row])?.to_owned(),
                    primary_key: primary_keys[row] == 1,
                    unique_constraint: unique_constraints[row] == 1,
                    filtered: filtered[row] == 1,
                    disabled: disabled[row] == 1,
                    columns: Vec::new(),
                },
            );
        }
    }
    let batches = catalog.query(format!(
        "SELECT ic.object_id, ic.index_id, c.name FROM sys.index_columns ic \
         JOIN sys.indexes i ON i.object_id = ic.object_id AND i.index_id = ic.index_id \
         JOIN sys.columns c ON c.object_id = ic.object_id AND c.column_id = ic.column_id \
         WHERE i.is_unique = 1 AND ic.key_ordinal > 0 AND ic.object_id IN ({object_ids}) \
         ORDER BY ic.object_id, ic.index_id, ic.key_ordinal"
    ))?;
    for batch in &batches {
        let (object_ids, index_ids, names) = (ints(batch, 0)?, ints(batch, 1)?, text(batch, 2)?);
        for row in 0..batch.num_rows() {
            let index = indexes
                .get_mut(&(object_ids[row], index_ids[row]))
                .ok_or_else(unexpected_catalog_result)?;
            index.columns.push(required(names[row])?.to_owned());
        }
    }
    Ok(indexes.into_values().collect())
}

/// The foreign keys of the requested tables, keyed by the table's
/// `object_id`, with their columns in order.
///
/// A referenced table the credential cannot see has no names in the catalog;
/// its foreign key then has no referenced table.
fn read_foreign_keys(
    catalog: Catalog<'_, '_>,
    object_ids: &str,
) -> Result<Vec<(i32, SqlServerForeignKey)>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT fk.parent_object_id, fk.object_id, fk.name, \
                OBJECT_SCHEMA_NAME(fk.referenced_object_id), OBJECT_NAME(fk.referenced_object_id), \
                CAST(fk.is_disabled AS int), CAST(fk.is_not_trusted AS int) \
         FROM sys.foreign_keys fk WHERE fk.parent_object_id IN ({object_ids})"
    ))?;
    // Keyed by the constraint's `object_id`, with the table's.
    let mut keys: HashMap<i32, (i32, SqlServerForeignKey)> = HashMap::new();
    for batch in &batches {
        let (tables, ids, names) = (ints(batch, 0)?, ints(batch, 1)?, text(batch, 2)?);
        let (namespaces, referenced, disabled, not_trusted) = (
            text(batch, 3)?,
            text(batch, 4)?,
            ints(batch, 5)?,
            ints(batch, 6)?,
        );
        for row in 0..batch.num_rows() {
            let referenced = match (namespaces[row], referenced[row]) {
                (Some(namespace), Some(table)) => Some(SqlServerReferencedTable {
                    namespace: namespace.to_owned(),
                    table: table.to_owned(),
                    columns: Vec::new(),
                }),
                _ => None,
            };
            let key = SqlServerForeignKey {
                name: required(names[row])?.to_owned(),
                columns: Vec::new(),
                referenced,
                enforced: disabled[row] == 0,
                validated: not_trusted[row] == 0,
            };
            keys.insert(ids[row], (tables[row], key));
        }
    }
    let batches = catalog.query(format!(
        "SELECT fkc.constraint_object_id, COL_NAME(fkc.parent_object_id, fkc.parent_column_id), \
                COL_NAME(fkc.referenced_object_id, fkc.referenced_column_id) \
         FROM sys.foreign_key_columns fkc WHERE fkc.parent_object_id IN ({object_ids}) \
         ORDER BY fkc.constraint_object_id, fkc.constraint_column_id"
    ))?;
    for batch in &batches {
        let (ids, columns, referenced_columns) =
            (ints(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
        for row in 0..batch.num_rows() {
            let (_, key) = keys
                .get_mut(&ids[row])
                .ok_or_else(unexpected_catalog_result)?;
            key.columns.push(required(columns[row])?.to_owned());
            match (key.referenced.as_mut(), referenced_columns[row]) {
                (Some(table), Some(column)) => table.columns.push(column.to_owned()),
                // A referenced column without a name is not visible.
                _ => key.referenced = None,
            }
        }
    }
    Ok(keys.into_values().collect())
}

/// The CHECK constraints of the requested tables, keyed by the table's
/// `object_id` and the constraint's, without their expressions.
fn read_checks(
    catalog: Catalog<'_, '_>,
    object_ids: &str,
) -> Result<Vec<(i32, i32, SqlServerCheck)>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT c.parent_object_id, c.object_id, c.name, CAST(c.is_disabled AS int), \
                CAST(c.is_not_trusted AS int) \
         FROM sys.check_constraints c WHERE c.parent_object_id IN ({object_ids})"
    ))?;
    let mut checks = Vec::new();
    for batch in &batches {
        let (tables, ids, names) = (ints(batch, 0)?, ints(batch, 1)?, text(batch, 2)?);
        let (disabled, not_trusted) = (ints(batch, 3)?, ints(batch, 4)?);
        for row in 0..batch.num_rows() {
            checks.push((
                tables[row],
                ids[row],
                SqlServerCheck {
                    name: required(names[row])?.to_owned(),
                    expression: None,
                    enforced: disabled[row] == 0,
                    validated: not_trusted[row] == 0,
                },
            ));
        }
    }
    Ok(checks)
}
