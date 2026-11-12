//! The table metadata request.
//!
//! [`table_metadata`] reads the requested tables' rows from `pg_class`,
//! `pg_attribute`, `pg_type`, `pg_enum`, `pg_constraint`, and `pg_index`, one
//! catalog at a time and each into typed rows, and assembles each table's
//! source metadata from them. Resolving domains to base types, column numbers
//! to names, and constraint-backed indexes happens in the assembly, in Rust.

use std::{
    collections::{HashMap, HashSet},
    time::SystemTime,
};

use connector_sdk::{ConnectorError, TableRecord, TableReference};
use connector_sql::{
    catalog::{
        boolean, int16, int16_lists, int32, int64, required, text, unexpected_catalog_result,
    },
    literal::quote_string,
};

use super::{
    BaseType, Catalog, LISTED_RELKINDS, PostgresColumn, PostgresConstraint, PostgresTable,
    PostgresUniqueIndex, ServerVersion,
};

/// The first server version with `pg_constraint.conenforced`.
const CONENFORCED_SINCE: i32 = 180_000;

/// Reads one record per requested table; a reference that is not a
/// namespace and a table name does not exist.
pub(crate) fn table_metadata(
    catalog: Catalog<'_, '_>,
    tables: &[TableReference],
    server_version: &ServerVersion,
) -> Result<Vec<TableRecord<PostgresTable>>, ConnectorError> {
    let requested_names: Vec<(&str, &str)> = tables
        .iter()
        .filter_map(|table| match table.names() {
            [namespace, name] => Some((namespace.as_str(), name.as_str())),
            _ => None,
        })
        .collect();
    let read = if requested_names.is_empty() {
        HashMap::new()
    } else {
        source_metadata(catalog, &requested_names, server_version.0)?
    };
    let read_at = SystemTime::now();
    Ok(tables
        .iter()
        .map(|table| {
            let found = match table.names() {
                [namespace, name] => read.get(&(namespace.clone(), name.clone())).cloned(),
                _ => None,
            };
            match found {
                Some(metadata) => TableRecord::Read { metadata, read_at },
                None => TableRecord::NotFound {
                    table: table.clone(),
                },
            }
        })
        .collect())
}

/// Reads the requested tables' catalog rows and assembles the source metadata
/// of each table found, keyed by namespace and name.
///
/// `requested` must not be empty.
fn source_metadata(
    catalog: Catalog<'_, '_>,
    requested: &[(&str, &str)],
    server_version: i32,
) -> Result<HashMap<(String, String), PostgresTable>, ConnectorError> {
    let relids = requested_relids(requested);
    let mut tables = read_pg_class(catalog, &relids)?;
    let columns = read_pg_attribute(catalog, &relids)?;
    let types = read_pg_type(catalog, &relids)?;
    let enums = read_pg_enum(catalog)?;
    let constraints = read_pg_constraint(catalog, &relids, server_version)?;
    let indexes = read_pg_index(catalog, &relids)?;

    let column_names = column_names(&columns);
    add_columns(&mut tables, &columns, &types, &enums)?;
    let constraint_indexes = add_constraints(&mut tables, constraints, &column_names)?;
    add_unique_indexes(&mut tables, indexes, &constraint_indexes, &column_names)?;

    Ok(tables
        .into_values()
        .map(|table| ((table.namespace.clone(), table.name.clone()), table))
        .collect())
}

/// A subquery selecting the OIDs of the requested tables of the listed kinds.
fn requested_relids(requested: &[(&str, &str)]) -> String {
    let pairs: Vec<String> = requested
        .iter()
        .map(|(namespace, name)| format!("({}, {})", quote_string(namespace), quote_string(name)))
        .collect();
    format!(
        "SELECT c.oid FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE (n.nspname::text, c.relname::text) IN (VALUES {}) AND {LISTED_RELKINDS}",
        pairs.join(", ")
    )
}

// Reading the catalogs: one statement each, read into typed rows.

/// The requested tables, keyed by OID, with no columns, constraints, or
/// indexes yet.
fn read_pg_class(
    catalog: Catalog<'_, '_>,
    relids: &str,
) -> Result<HashMap<i64, PostgresTable>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT c.oid::int8, n.nspname::text, c.relname::text, c.relkind::text \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE c.oid IN ({relids})"
    ))?;
    let mut tables = HashMap::new();
    for batch in &batches {
        let (oids, namespaces, names, kinds) = (
            int64(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            text(batch, 3)?,
        );
        for row in 0..batch.num_rows() {
            tables.insert(
                oids.value(row),
                PostgresTable {
                    namespace: required(namespaces[row])?.to_owned(),
                    name: required(names[row])?.to_owned(),
                    oid: oids.value(row).to_string(),
                    relkind: required(kinds[row])?.to_owned(),
                    columns: Vec::new(),
                    constraints: Vec::new(),
                    unique_indexes: Vec::new(),
                },
            );
        }
    }
    Ok(tables)
}

/// One `pg_attribute` row.
struct PgAttributeRow {
    relid: i64,
    attnum: i16,
    name: String,
    not_null: bool,
    data_source_type: String,
    type_oid: i64,
    typmod: i32,
}

/// The columns of the requested tables and of the tables their foreign keys
/// reference, whose names the foreign keys need.
fn read_pg_attribute(
    catalog: Catalog<'_, '_>,
    relids: &str,
) -> Result<Vec<PgAttributeRow>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT a.attrelid::int8, a.attnum, a.attname::text, a.attnotnull, \
                pg_catalog.format_type(a.atttypid, a.atttypmod), a.atttypid::int8, \
                a.atttypmod \
         FROM pg_catalog.pg_attribute a \
         WHERE a.attnum > 0 AND NOT a.attisdropped AND (a.attrelid IN ({relids}) \
           OR a.attrelid IN (SELECT confrelid FROM pg_catalog.pg_constraint \
                             WHERE contype = 'f' AND conrelid IN ({relids}))) \
         ORDER BY a.attrelid, a.attnum"
    ))?;
    let mut columns = Vec::new();
    for batch in &batches {
        let (relids, attnums, names, not_null) = (
            int64(batch, 0)?,
            int16(batch, 1)?,
            text(batch, 2)?,
            boolean(batch, 3)?,
        );
        let (data_source_types, type_oids, typmods) =
            (text(batch, 4)?, int64(batch, 5)?, int32(batch, 6)?);
        for row in 0..batch.num_rows() {
            columns.push(PgAttributeRow {
                relid: relids.value(row),
                attnum: attnums.value(row),
                name: required(names[row])?.to_owned(),
                not_null: not_null.value(row),
                data_source_type: required(data_source_types[row])?.to_owned(),
                type_oid: type_oids.value(row),
                typmod: typmods.value(row),
            });
        }
    }
    Ok(columns)
}

/// One `pg_type` row.
struct PgTypeRow {
    schema: String,
    name: String,
    kind: String,
    category: String,
    base: i64,
    typmod: i32,
    /// `pg_type.typnotnull`: a domain that rejects NULL.
    not_null: bool,
}

/// The types of the requested tables' columns, every domain, and every
/// domain's base type, keyed by OID.
fn read_pg_type(
    catalog: Catalog<'_, '_>,
    relids: &str,
) -> Result<HashMap<i64, PgTypeRow>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT t.oid::int8, n.nspname::text, t.typname::text, t.typtype::text, \
                t.typcategory::text, t.typbasetype::int8, t.typtypmod, t.typnotnull \
         FROM pg_catalog.pg_type t \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
         WHERE t.typtype = 'd' \
           OR t.oid IN (SELECT typbasetype FROM pg_catalog.pg_type WHERE typtype = 'd') \
           OR t.oid IN (SELECT atttypid FROM pg_catalog.pg_attribute \
                        WHERE attrelid IN ({relids}))"
    ))?;
    let mut types = HashMap::new();
    for batch in &batches {
        let (oids, schemas, names, kinds, categories) = (
            int64(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            text(batch, 3)?,
            text(batch, 4)?,
        );
        let (bases, typmods, not_null) = (int64(batch, 5)?, int32(batch, 6)?, boolean(batch, 7)?);
        for row in 0..batch.num_rows() {
            types.insert(
                oids.value(row),
                PgTypeRow {
                    schema: required(schemas[row])?.to_owned(),
                    name: required(names[row])?.to_owned(),
                    kind: required(kinds[row])?.to_owned(),
                    category: required(categories[row])?.to_owned(),
                    base: bases.value(row),
                    typmod: typmods.value(row),
                    not_null: not_null.value(row),
                },
            );
        }
    }
    Ok(types)
}

/// Every enum's labels in sort order, keyed by the enum's OID.
fn read_pg_enum(catalog: Catalog<'_, '_>) -> Result<HashMap<i64, Vec<String>>, ConnectorError> {
    let batches = catalog.query(
        "SELECT enumtypid::int8, enumlabel::text FROM pg_catalog.pg_enum \
         ORDER BY enumtypid, enumsortorder",
    )?;
    let mut enums: HashMap<i64, Vec<String>> = HashMap::new();
    for batch in &batches {
        let (oids, labels) = (int64(batch, 0)?, text(batch, 1)?);
        for (oid, label) in oids.values().iter().zip(labels) {
            enums
                .entry(*oid)
                .or_default()
                .push(required(label)?.to_owned());
        }
    }
    Ok(enums)
}

/// One `pg_constraint` row, with the table a foreign key references.
struct PgConstraintRow {
    relid: i64,
    name: String,
    contype: String,
    enforced: bool,
    validated: bool,
    keys: Option<Vec<i16>>,
    referenced_relid: i64,
    referenced_namespace: Option<String>,
    referenced_table: Option<String>,
    referenced_keys: Option<Vec<i16>>,
    /// The index behind a primary key, unique, or exclusion constraint.
    index: i64,
    expression: Option<String>,
}

/// The requested tables' constraints. Exclusion constraints are read only to
/// recognize the indexes behind them.
fn read_pg_constraint(
    catalog: Catalog<'_, '_>,
    relids: &str,
    server_version: i32,
) -> Result<Vec<PgConstraintRow>, ConnectorError> {
    let enforced = if server_version >= CONENFORCED_SINCE {
        "con.conenforced"
    } else {
        "true"
    };
    let batches = catalog.query(format!(
        "SELECT con.conrelid::int8, con.conname::text, con.contype::text, \
                {enforced}, con.convalidated, con.conkey, con.confrelid::int8, \
                rn.nspname::text, rc.relname::text, con.confkey, con.conindid::int8, \
                pg_catalog.pg_get_expr(con.conbin, con.conrelid) \
         FROM pg_catalog.pg_constraint con \
         LEFT JOIN pg_catalog.pg_class rc ON rc.oid = con.confrelid \
         LEFT JOIN pg_catalog.pg_namespace rn ON rn.oid = rc.relnamespace \
         WHERE con.conrelid IN ({relids}) AND con.contype IN ('p', 'u', 'f', 'c', 'x') \
         ORDER BY con.conrelid, con.conname"
    ))?;
    let mut constraints = Vec::new();
    for batch in &batches {
        let (relids, names, contypes) = (int64(batch, 0)?, text(batch, 1)?, text(batch, 2)?);
        let (enforced, validated) = (boolean(batch, 3)?, boolean(batch, 4)?);
        let (keys, referenced_relids, referenced_keys) = (
            int16_lists(batch, 5)?,
            int64(batch, 6)?,
            int16_lists(batch, 9)?,
        );
        let (referenced_namespaces, referenced_tables) = (text(batch, 7)?, text(batch, 8)?);
        let (indexes, expressions) = (int64(batch, 10)?, text(batch, 11)?);
        for (row, (keys, referenced_keys)) in keys.into_iter().zip(referenced_keys).enumerate() {
            constraints.push(PgConstraintRow {
                relid: relids.value(row),
                name: required(names[row])?.to_owned(),
                contype: required(contypes[row])?.to_owned(),
                enforced: enforced.value(row),
                validated: validated.value(row),
                keys,
                referenced_relid: referenced_relids.value(row),
                referenced_namespace: referenced_namespaces[row].map(str::to_owned),
                referenced_table: referenced_tables[row].map(str::to_owned),
                referenced_keys,
                index: indexes.value(row),
                expression: expressions[row].map(str::to_owned),
            });
        }
    }
    Ok(constraints)
}

/// One valid `pg_index` row of a unique index.
struct PgIndexRow {
    relid: i64,
    oid: i64,
    name: String,
    keys: Vec<i16>,
    /// The number of key columns; included columns follow them in `keys`.
    key_count: i32,
    condition: Option<String>,
}

/// The requested tables' unique indexes that enforce uniqueness. An invalid
/// or unready index, such as one a failed concurrent build left, does not.
fn read_pg_index(
    catalog: Catalog<'_, '_>,
    relids: &str,
) -> Result<Vec<PgIndexRow>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT i.indrelid::int8, i.indexrelid::int8, ic.relname::text, \
                i.indkey::int2[], i.indnkeyatts::int4, \
                pg_catalog.pg_get_expr(i.indpred, i.indrelid) \
         FROM pg_catalog.pg_index i \
         JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
         WHERE i.indisunique AND i.indisvalid AND i.indisready \
           AND i.indrelid IN ({relids}) \
         ORDER BY i.indrelid, ic.relname"
    ))?;
    let mut indexes = Vec::new();
    for batch in &batches {
        let (relids, oids, names) = (int64(batch, 0)?, int64(batch, 1)?, text(batch, 2)?);
        let (keys, key_counts, conditions) =
            (int16_lists(batch, 3)?, int32(batch, 4)?, text(batch, 5)?);
        for (row, keys) in keys.into_iter().enumerate() {
            indexes.push(PgIndexRow {
                relid: relids.value(row),
                oid: oids.value(row),
                name: required(names[row])?.to_owned(),
                keys: keys.unwrap_or_default(),
                key_count: key_counts.value(row),
                condition: conditions[row].map(str::to_owned),
            });
        }
    }
    Ok(indexes)
}

// Assembly: the rows become each table's columns, constraints, and indexes.

/// Column names by table OID and column number, for every column read.
fn column_names(columns: &[PgAttributeRow]) -> HashMap<(i64, i16), &str> {
    columns
        .iter()
        .map(|column| ((column.relid, column.attnum), column.name.as_str()))
        .collect()
}

/// Adds each requested table's columns, with their types resolved through
/// domains.
fn add_columns(
    tables: &mut HashMap<i64, PostgresTable>,
    columns: &[PgAttributeRow],
    types: &HashMap<i64, PgTypeRow>,
    enums: &HashMap<i64, Vec<String>>,
) -> Result<(), ConnectorError> {
    for column in columns {
        let Some(table) = tables.get_mut(&column.relid) else {
            // A column of a referenced table, read only for its name.
            continue;
        };
        let (base_oid, typmod, domain_not_null) =
            resolve_domains(types, column.type_oid, column.typmod)?;
        let base = types.get(&base_oid).ok_or_else(unexpected_catalog_result)?;
        table.columns.push(PostgresColumn {
            name: column.name.clone(),
            attnum: column.attnum,
            not_null: column.not_null || domain_not_null,
            data_source_type: column.data_source_type.clone(),
            base_type: BaseType {
                schema: base.schema.clone(),
                name: base.name.clone(),
                kind: base.kind.clone(),
                category: base.category.clone(),
                typmod,
                enum_labels: enums.get(&base_oid).cloned(),
            },
            domain: base_oid != column.type_oid,
        });
    }
    Ok(())
}

/// Adds each table's constraints, and returns the indexes that back primary
/// key, unique, and exclusion constraints, which are not listed as unique
/// indexes of their own.
fn add_constraints(
    tables: &mut HashMap<i64, PostgresTable>,
    constraints: Vec<PgConstraintRow>,
    column_names: &HashMap<(i64, i16), &str>,
) -> Result<HashSet<i64>, ConnectorError> {
    let mut constraint_indexes = HashSet::new();
    for row in constraints {
        // A foreign key's index belongs to the referenced table.
        if matches!(row.contype.as_str(), "p" | "u" | "x") {
            constraint_indexes.insert(row.index);
        }
        if row.contype == "x" {
            continue;
        }
        let referenced_columns = if row.contype == "f" {
            Some(column_list(
                column_names,
                row.referenced_relid,
                row.referenced_keys.as_deref(),
            )?)
        } else {
            None
        };
        let constraint = PostgresConstraint {
            columns: column_list(column_names, row.relid, row.keys.as_deref())?,
            name: row.name,
            contype: row.contype,
            enforced: row.enforced,
            validated: row.validated,
            referenced_namespace: row.referenced_namespace,
            referenced_table: row.referenced_table,
            referenced_columns,
            expression: row.expression,
        };
        tables
            .get_mut(&row.relid)
            .ok_or_else(unexpected_catalog_result)?
            .constraints
            .push(constraint);
    }
    Ok(constraint_indexes)
}

/// Adds each table's unique indexes that do not back a constraint.
fn add_unique_indexes(
    tables: &mut HashMap<i64, PostgresTable>,
    indexes: Vec<PgIndexRow>,
    constraint_indexes: &HashSet<i64>,
    column_names: &HashMap<(i64, i16), &str>,
) -> Result<(), ConnectorError> {
    for row in indexes {
        if constraint_indexes.contains(&row.oid) {
            continue;
        }
        let key_count = usize::try_from(row.key_count).map_err(|_| unexpected_catalog_result())?;
        let keys = row
            .keys
            .get(..key_count)
            .ok_or_else(unexpected_catalog_result)?;
        // An expression key has column number 0.
        let has_expressions = keys.contains(&0);
        let index = PostgresUniqueIndex {
            name: row.name,
            columns: if has_expressions {
                Vec::new()
            } else {
                column_list(column_names, row.relid, Some(keys))?
            },
            has_expressions,
            condition: row.condition,
        };
        tables
            .get_mut(&row.relid)
            .ok_or_else(unexpected_catalog_result)?
            .unique_indexes
            .push(index);
    }
    Ok(())
}

/// The base type a column's type resolves to through nested domains, the
/// type modifier the column or the innermost domain declares, and whether
/// some domain on the way is `NOT NULL`.
fn resolve_domains(
    types: &HashMap<i64, PgTypeRow>,
    mut oid: i64,
    mut typmod: i32,
) -> Result<(i64, i32, bool), ConnectorError> {
    let mut not_null = false;
    loop {
        let row = types.get(&oid).ok_or_else(unexpected_catalog_result)?;
        if row.kind != "d" {
            return Ok((oid, typmod, not_null));
        }
        if typmod == -1 {
            typmod = row.typmod;
        }
        not_null |= row.not_null;
        oid = row.base;
    }
}

/// The names of a table's columns by column number, in key order.
fn column_list(
    names: &HashMap<(i64, i16), &str>,
    relid: i64,
    keys: Option<&[i16]>,
) -> Result<Vec<String>, ConnectorError> {
    keys.unwrap_or_default()
        .iter()
        .map(|attnum| {
            names
                .get(&(relid, *attnum))
                .map(|name| (*name).to_owned())
                .ok_or_else(unexpected_catalog_result)
        })
        .collect()
}
