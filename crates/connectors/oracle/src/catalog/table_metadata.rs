//! The table metadata request.
//!
//! [`table_metadata`] reads the requested tables' rows from `ALL_TABLES` and
//! `ALL_VIEWS` with `ALL_OBJECTS` and `ALL_TAB_COMMENTS`, `ALL_TAB_COLS` with
//! `ALL_COL_COMMENTS`, `ALL_CONSTRAINTS` with `ALL_CONS_COLUMNS`,
//! `ALL_INDEXES` with `ALL_IND_COLUMNS`, and their long text values, one
//! statement each and each into typed rows, and assembles each table's source
//! metadata from them in Rust.

use std::{collections::HashMap, time::SystemTime};

use arrow_array::RecordBatch;
use connector_sdk::{ConnectorError, TableRecord, TableReference};
use connector_sql::catalog::{required, text, unexpected_catalog_result};

use super::{
    Catalog, LISTED_OBJECTS, LISTED_SCHEMA, OracleCheck, OracleColumn, OracleForeignKey, OracleKey,
    OracleLongText, OracleReferencedTable, OracleTable, OracleTableKind, OracleUniqueKey,
    OracleUniqueKind, OracleValueSource, integer,
    long_text::{LongTexts, TextKind},
    required_integer,
};
use connector_sql::literal::quote_string;

/// Why a requested table is "not found".
const MAY_BE_HIDDEN: &str = "Oracle shows only the tables the credential has a privilege on, \
     so the table may exist";

/// A table's owner and name.
type TableKey = (String, String);

/// Reads one record per requested table.
///
/// A reference that is not an owner and a table name does not exist. A table
/// not found is "not found", because Oracle hides the tables the credential
/// has no privilege on.
pub(crate) fn table_metadata(
    catalog: Catalog<'_, '_>,
    tables: &[TableReference],
) -> Result<Vec<TableRecord<OracleTable>>, ConnectorError> {
    let requested: Vec<(&str, &str)> = tables
        .iter()
        .filter_map(|table| match table.names() {
            [owner, name] => Some((owner.as_str(), name.as_str())),
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
                [owner, name] => read.get(&(owner.clone(), name.clone())).cloned(),
                _ => {
                    return TableRecord::DoesNotExist {
                        table: table.clone(),
                    };
                }
            };
            match found {
                Some(metadata) => TableRecord::Read { metadata, read_at },
                None => TableRecord::NotFound {
                    table: table.clone(),
                    reason: MAY_BE_HIDDEN.to_owned(),
                },
            }
        })
        .collect())
}

/// Reads the requested tables' catalog rows and assembles the source metadata
/// of each table found, keyed by owner and name.
///
/// `requested` must not be empty.
fn source_metadata(
    catalog: Catalog<'_, '_>,
    requested: &[(&str, &str)],
) -> Result<HashMap<TableKey, OracleTable>, ConnectorError> {
    let requested = requested_tables(requested);
    let mut tables = read_tables(catalog, &requested)?;
    let columns = read_columns(catalog, &requested)?;
    let constraints = read_constraints(catalog, &requested)?;
    let indexes = read_unique_indexes(catalog, &requested)?;
    let texts = LongTexts::read(catalog, &requested)?;

    // Rows of a requested table that is not listed, such as one in a schema
    // Oracle maintains, belong to no table here.
    for (table, row) in columns {
        let column = row.into_column(&table, &texts);
        if let Some(table) = tables.get_mut(&table) {
            table.columns.push(column);
        }
    }
    for (table, row) in constraints {
        if let Some(table) = tables.get_mut(&table) {
            row.add_to(table, &texts);
        }
    }
    for (table, key) in indexes {
        if let Some(table) = tables.get_mut(&table) {
            table.unique_keys.push(key);
        }
    }

    Ok(tables
        .into_iter()
        .map(|(key, mut table)| {
            if table.kind == OracleTableKind::View {
                table.definition = Some(texts.get(
                    TextKind::Definition,
                    &table.namespace,
                    &table.name,
                    &table.name,
                ));
            }
            table.unique_keys.sort_by(|a, b| a.name.cmp(&b.name));
            table.foreign_keys.sort_by(|a, b| a.name.cmp(&b.name));
            table.checks.sort_by(|a, b| a.name.cmp(&b.name));
            (key, table)
        })
        .collect())
}

/// A query selecting the requested tables as `owner` and `name` pairs, for
/// an `IN` condition that is not limited to 1000 values.
fn requested_tables(requested: &[(&str, &str)]) -> String {
    requested
        .iter()
        .map(|(owner, name)| {
            format!(
                "SELECT {} AS owner, {} AS name FROM dual",
                quote_string(owner),
                quote_string(name)
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// The rows of a catalog result whose first `columns` columns are text, each
/// as its values in column order, NULL as `None`.
fn text_rows(
    batch: &RecordBatch,
    columns: usize,
) -> Result<Vec<Vec<Option<&str>>>, ConnectorError> {
    let values = (0..columns)
        .map(|index| text(batch, index))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((0..batch.num_rows())
        .map(|row| values.iter().map(|column| column[row]).collect())
        .collect())
}

/// The requested tables and views, with no columns or constraints yet.
fn read_tables(
    catalog: Catalog<'_, '_>,
    requested: &str,
) -> Result<HashMap<TableKey, OracleTable>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT o.owner, o.name, o.kind, TO_CHAR(a.object_id), m.comments \
         FROM ({LISTED_OBJECTS}) o \
         JOIN all_users u ON u.username = o.owner \
         JOIN all_objects a ON a.owner = o.owner AND a.object_name = o.name \
           AND a.subobject_name IS NULL \
           AND a.object_type = CASE o.kind WHEN 'V' THEN 'VIEW' ELSE 'TABLE' END \
         LEFT JOIN all_tab_comments m ON m.owner = o.owner AND m.table_name = o.name \
         WHERE {LISTED_SCHEMA} AND (o.owner, o.name) IN ({requested})"
    ))?;
    let mut tables = HashMap::new();
    for batch in &batches {
        let (owners, names, kinds, object_ids, comments) = (
            text(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            text(batch, 3)?,
            text(batch, 4)?,
        );
        for row in 0..batch.num_rows() {
            let kind = match required(kinds[row])? {
                "T" => OracleTableKind::Table,
                "V" => OracleTableKind::View,
                _ => return Err(unexpected_catalog_result()),
            };
            let (namespace, name) = (
                required(owners[row])?.to_owned(),
                required(names[row])?.to_owned(),
            );
            tables.insert(
                (namespace.clone(), name.clone()),
                OracleTable {
                    namespace,
                    name,
                    object_id: required_integer(object_ids[row])?,
                    kind,
                    comment: comments[row].map(str::to_owned),
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

/// One `ALL_TAB_COLS` row.
struct ColumnRow {
    column: OracleColumn,
    is_virtual: bool,
    identity: bool,
    has_default: bool,
    default_on_null: bool,
}

impl ColumnRow {
    /// The column, with its expression from `texts`.
    fn into_column(self, (owner, name): &TableKey, texts: &LongTexts) -> OracleColumn {
        let expression = || texts.get(TextKind::Default, owner, name, &self.column.name);
        let value_source = if self.identity {
            Some(OracleValueSource::Identity)
        } else if self.is_virtual {
            Some(OracleValueSource::Virtual {
                expression: expression(),
            })
        } else if self.has_default {
            Some(OracleValueSource::Default {
                expression: expression(),
                on_null: self.default_on_null,
            })
        } else {
            None
        };
        OracleColumn {
            value_source,
            ..self.column
        }
    }
}

/// The visible columns of the requested tables, in `COLUMN_ID` order.
///
/// Hidden columns, invisible ones among them, are left out, and so are the
/// constraints and unique indexes that involve one, including a foreign key
/// that references a key on one, as [`read_constraints`] and
/// [`read_unique_indexes`] read them. Invisible columns are mostly used to add
/// a column without breaking `SELECT *` or an `INSERT` without a column list,
/// and their constraints appear once the column is made visible. A key kept on
/// an invisible column, such as a surrogate key, is lost, though: the table
/// looks as if it had no primary key, and the foreign keys that reference it
/// disappear. This is a deliberate choice for now. If such keys are needed,
/// the invisible columns, those with `USER_GENERATED = 'YES'`, can be listed
/// with their constraints, leaving out only the hidden columns Oracle
/// generates.
fn read_columns(
    catalog: Catalog<'_, '_>,
    requested: &str,
) -> Result<Vec<(TableKey, ColumnRow)>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT c.owner, c.table_name, c.column_name, TO_CHAR(c.column_id), c.nullable, \
                c.data_type, c.data_type_owner, TO_CHAR(c.data_length), \
                TO_CHAR(c.data_precision), TO_CHAR(c.data_scale), TO_CHAR(c.char_length), \
                c.char_used, c.virtual_column, c.identity_column, \
                CASE WHEN c.default_length IS NULL THEN 'NO' ELSE 'YES' END, \
                c.default_on_null, m.comments \
         FROM all_tab_cols c \
         LEFT JOIN all_col_comments m ON m.owner = c.owner AND m.table_name = c.table_name \
           AND m.column_name = c.column_name \
         WHERE c.hidden_column = 'NO' AND (c.owner, c.table_name) IN ({requested}) \
         ORDER BY c.owner, c.table_name, c.column_id"
    ))?;
    let yes = |value: Option<&str>| matches!(value, Some("YES" | "Y"));
    let mut columns = Vec::new();
    for batch in &batches {
        for row in text_rows(batch, 17)? {
            let value = |index: usize| row[index];
            let table = (
                required(value(0))?.to_owned(),
                required(value(1))?.to_owned(),
            );
            let column = OracleColumn {
                name: required(value(2))?.to_owned(),
                column_id: required_integer(value(3))?,
                nullable: value(4) == Some("Y"),
                data_type: required(value(5))?.to_owned(),
                data_type_owner: value(6).map(str::to_owned),
                data_length: required_integer(value(7))?,
                data_precision: integer(value(8))?,
                data_scale: integer(value(9))?,
                char_length: required_integer(value(10))?,
                char_used: value(11).map(str::to_owned),
                comment: value(16).map(str::to_owned),
                value_source: None,
            };
            columns.push((
                table,
                ColumnRow {
                    column,
                    is_virtual: yes(value(12)),
                    identity: yes(value(13)),
                    has_default: yes(value(14)),
                    default_on_null: yes(value(15)),
                },
            ));
        }
    }
    Ok(columns)
}

/// One primary key, unique, foreign key, or CHECK constraint of a requested
/// table, with its columns in order.
struct ConstraintRow {
    owner: String,
    name: String,
    /// `CONSTRAINT_TYPE`: `P`, `U`, `R`, or `C`.
    kind: String,
    enforced: bool,
    validated: bool,
    columns: Vec<String>,
    /// For a foreign key, the referenced table when the credential can see
    /// the constraint it references.
    referenced: Option<OracleReferencedTable>,
}

impl ConstraintRow {
    /// Adds the constraint to its table, whose columns are already added.
    ///
    /// A CHECK constraint whose condition is `"<column>" IS NOT NULL` on its
    /// one column, as Oracle stores a NOT NULL constraint, is not added when
    /// the column is not nullable, because the column's nullability states
    /// it. Oracle marks a column not nullable only for an enabled and
    /// validated NOT NULL constraint, so one that is disabled or not
    /// validated is added. The condition cannot tell a NOT NULL constraint
    /// from a CHECK constraint written as `CHECK ("X" IS NOT NULL)`, so such a
    /// CHECK constraint is not added either when its column is not nullable,
    /// where it is redundant, and is added when the column is nullable.
    fn add_to(self, table: &mut OracleTable, texts: &LongTexts) {
        match self.kind.as_str() {
            "P" => {
                table.primary_key = Some(OracleKey {
                    name: self.name,
                    columns: self.columns,
                    enforced: self.enforced,
                    validated: self.validated,
                });
            }
            "U" => table.unique_keys.push(OracleUniqueKey {
                name: self.name,
                kind: OracleUniqueKind::Constraint,
                columns: self.columns,
                enforced: self.enforced,
            }),
            "R" => table.foreign_keys.push(OracleForeignKey {
                name: self.name,
                columns: self.columns,
                referenced: self.referenced,
                enforced: self.enforced,
                validated: self.validated,
            }),
            _ => {
                let expression = texts.get(TextKind::Check, &self.owner, &table.name, &self.name);
                let not_null = match (self.columns.as_slice(), &expression) {
                    ([column], OracleLongText::Text(expression)) => {
                        *expression == format!("\"{column}\" IS NOT NULL")
                            && table
                                .columns
                                .iter()
                                .any(|known| known.name == *column && !known.nullable)
                    }
                    _ => false,
                };
                if !not_null {
                    table.checks.push(OracleCheck {
                        name: self.name,
                        expression,
                        enforced: self.enforced,
                        validated: self.validated,
                    });
                }
            }
        }
    }
}

/// The primary key, unique, foreign key, and CHECK constraints of the
/// requested tables.
///
/// A foreign key whose referenced constraint the credential cannot see has
/// no referenced table.
///
/// A constraint that involves a hidden column, such as an invisible one, is
/// not read, as [`read_unique_indexes`] does not read an index on one: the
/// table's columns do not list the column, so the constraint cannot be stated
/// over them. This covers a key or CHECK constraint with any of its columns
/// hidden, among them the NOT NULL constraint of a hidden column, and a
/// foreign key that references a key on a hidden column of its table.
/// [`read_columns`] says why hidden columns are left out.
fn read_constraints(
    catalog: Catalog<'_, '_>,
    requested: &str,
) -> Result<Vec<(TableKey, ConstraintRow)>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT c.owner, c.table_name, c.constraint_name, c.constraint_type, c.status, \
                c.validated, r.owner, r.table_name \
         FROM all_constraints c \
         LEFT JOIN all_constraints r ON r.owner = c.r_owner \
           AND r.constraint_name = c.r_constraint_name \
         WHERE c.constraint_type IN ('P', 'U', 'R', 'C') \
           AND (c.owner, c.table_name) IN ({requested}) \
           AND NOT EXISTS (SELECT 1 FROM all_cons_columns h \
                           JOIN all_tab_cols t ON t.owner = h.owner \
                             AND t.table_name = h.table_name AND t.column_name = h.column_name \
                           WHERE ((h.owner = c.owner AND h.constraint_name = c.constraint_name) \
                                  OR (h.owner = c.r_owner AND h.constraint_name = c.r_constraint_name)) \
                             AND t.hidden_column = 'YES')"
    ))?;
    let mut constraints: HashMap<(String, String), (TableKey, ConstraintRow)> = HashMap::new();
    for batch in &batches {
        for row in text_rows(batch, 8)? {
            let value = |index: usize| row[index];
            let (owner, name) = (
                required(value(0))?.to_owned(),
                required(value(2))?.to_owned(),
            );
            let referenced = match (value(6), value(7)) {
                (Some(namespace), Some(table)) => Some(OracleReferencedTable {
                    namespace: namespace.to_owned(),
                    table: table.to_owned(),
                    columns: Vec::new(),
                }),
                _ => None,
            };
            let constraint = ConstraintRow {
                owner: owner.clone(),
                name: name.clone(),
                kind: required(value(3))?.to_owned(),
                enforced: value(4) == Some("ENABLED"),
                validated: value(5) == Some("VALIDATED"),
                columns: Vec::new(),
                referenced,
            };
            let table = (owner.clone(), required(value(1))?.to_owned());
            constraints.insert((owner, name), (table, constraint));
        }
    }
    let batches = catalog.query(format!(
        "SELECT k.owner, k.constraint_name, k.column_name FROM all_cons_columns k \
         WHERE (k.owner, k.table_name) IN ({requested}) \
         ORDER BY k.owner, k.constraint_name, k.position"
    ))?;
    for batch in &batches {
        let (owners, names, columns) = (text(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
        for row in 0..batch.num_rows() {
            let key = (
                required(owners[row])?.to_owned(),
                required(names[row])?.to_owned(),
            );
            // Columns of a constraint of another type, such as a view's.
            if let Some((_, constraint)) = constraints.get_mut(&key) {
                constraint.columns.push(required(columns[row])?.to_owned());
            }
        }
    }
    let batches = catalog.query(format!(
        "SELECT c.owner, c.constraint_name, k.column_name FROM all_constraints c \
         JOIN all_cons_columns k ON k.owner = c.r_owner AND k.constraint_name = c.r_constraint_name \
         WHERE c.constraint_type = 'R' AND (c.owner, c.table_name) IN ({requested}) \
         ORDER BY c.owner, c.constraint_name, k.position"
    ))?;
    for batch in &batches {
        let (owners, names, columns) = (text(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
        for row in 0..batch.num_rows() {
            let key = (
                required(owners[row])?.to_owned(),
                required(names[row])?.to_owned(),
            );
            // A foreign key that involves a hidden column is not read.
            if let Some((_, constraint)) = constraints.get_mut(&key)
                && let Some(referenced) = constraint.referenced.as_mut()
            {
                referenced.columns.push(required(columns[row])?.to_owned());
            }
        }
    }
    Ok(constraints.into_values().collect())
}

/// The unique indexes on columns of the requested tables that back no
/// primary key or unique constraint, with their columns in order.
///
/// Every key column is one the table's columns list, a virtual column among
/// them, whose index Oracle calls function-based. An index on expressions,
/// including a descending one, keys on hidden columns Oracle creates for
/// them and is not read, nor is one on an invisible column.
/// [`read_columns`] says why hidden columns are left out.
fn read_unique_indexes(
    catalog: Catalog<'_, '_>,
    requested: &str,
) -> Result<Vec<(TableKey, OracleUniqueKey)>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT i.table_owner, i.table_name, i.owner, i.index_name, k.column_name \
         FROM all_indexes i \
         JOIN all_ind_columns k ON k.index_owner = i.owner AND k.index_name = i.index_name \
         WHERE i.uniqueness = 'UNIQUE' \
           AND i.index_type IN ('NORMAL', 'NORMAL/REV', 'FUNCTION-BASED NORMAL') \
           AND (i.table_owner, i.table_name) IN ({requested}) \
           AND NOT EXISTS (SELECT 1 FROM all_ind_columns h \
                           JOIN all_tab_cols t ON t.owner = h.table_owner \
                             AND t.table_name = h.table_name AND t.column_name = h.column_name \
                           WHERE h.index_owner = i.owner AND h.index_name = i.index_name \
                             AND t.hidden_column = 'YES') \
           AND NOT EXISTS (SELECT 1 FROM all_constraints c \
                           WHERE c.owner = i.table_owner AND c.table_name = i.table_name \
                             AND c.index_owner = i.owner AND c.index_name = i.index_name \
                             AND c.constraint_type IN ('P', 'U')) \
         ORDER BY i.owner, i.index_name, k.column_position"
    ))?;
    let mut indexes: Vec<((String, String), TableKey, OracleUniqueKey)> = Vec::new();
    for batch in &batches {
        for row in text_rows(batch, 5)? {
            let value = |index: usize| row[index];
            let index = (
                required(value(2))?.to_owned(),
                required(value(3))?.to_owned(),
            );
            let column = required(value(4))?.to_owned();
            match indexes.last_mut() {
                Some((last, _, key)) if *last == index => key.columns.push(column),
                _ => indexes.push((
                    index.clone(),
                    (
                        required(value(0))?.to_owned(),
                        required(value(1))?.to_owned(),
                    ),
                    OracleUniqueKey {
                        name: index.1,
                        kind: OracleUniqueKind::Index,
                        columns: vec![column],
                        enforced: true,
                    },
                )),
            }
        }
    }
    Ok(indexes
        .into_iter()
        .map(|(_, table, key)| (table, key))
        .collect())
}
