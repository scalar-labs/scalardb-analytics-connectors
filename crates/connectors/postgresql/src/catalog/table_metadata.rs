//! The table metadata request.
//!
//! [`table_metadata`] reads the requested tables' rows from `pg_class`,
//! `pg_attribute` with `pg_attrdef`, `pg_type`, `pg_enum`, `pg_constraint`, and
//! `pg_index`, one catalog at a time and each into typed rows, and assembles
//! each table's source metadata from them. Comments and display names come
//! with the rows they describe. Following types to their bases and elements,
//! column numbers to names, and constraint-backed indexes happens in the
//! assembly, in Rust.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
    Catalog, LISTED_RELKINDS, PostgresCheck, PostgresColumn, PostgresDomainCheck,
    PostgresForeignKey, PostgresGeneratedStorage, PostgresIdentity, PostgresKey, PostgresNotNull,
    PostgresPrimaryKey, PostgresReferencedTable, PostgresTable, PostgresTableKind, PostgresType,
    PostgresTypeForm, PostgresUniqueKey, PostgresUniqueKind, PostgresValueSource, ServerVersion,
};

/// The first server version with `pg_constraint.conenforced`.
const CONENFORCED_SINCE: i32 = 180_000;

/// PostgreSQL's `FirstNormalObjectId`: objects below it come with the server.
/// Built-in types carry the server's own descriptions as comments, which say
/// nothing about a table and change with the server version, so only types
/// created later, by users or extensions, have their comments read.
const FIRST_NORMAL_OBJECT_ID: i64 = 16_384;

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
    let domain_checks = read_domain_checks(catalog)?;
    let constraints = read_pg_constraint(catalog, &relids, server_version)?;
    let indexes = read_pg_index(catalog, &relids)?;
    let index_expressions = read_index_expressions(catalog, &relids)?;

    let column_names = column_names(&columns);
    add_columns(&mut tables, &columns, &types, &enums, &domain_checks)?;
    let constraint_indexes = add_constraints(&mut tables, constraints, &column_names)?;
    add_unique_indexes(
        &mut tables,
        indexes,
        &index_expressions,
        &constraint_indexes,
        &column_names,
    )?;

    tables
        .into_values()
        .map(|mut table| {
            table.unique_keys.sort_by(|a, b| a.name.cmp(&b.name));
            // The assembly lists every type it refers to; a failure is a
            // catalog result this assembly does not expect.
            table
                .check_types()
                .map_err(|_| unexpected_catalog_result())?;
            Ok(((table.namespace.clone(), table.name.clone()), table))
        })
        .collect()
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

/// The requested tables, keyed by OID, with no columns, types, or constraints
/// yet.
fn read_pg_class(
    catalog: Catalog<'_, '_>,
    relids: &str,
) -> Result<HashMap<i64, PostgresTable>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT c.oid::int8, n.nspname::text, c.relname::text, c.relkind::text, \
                pg_catalog.obj_description(c.oid, 'pg_class'), \
                CASE WHEN c.relkind IN ('v', 'm') \
                     THEN pg_catalog.pg_get_viewdef(c.oid) END \
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
        let (comments, definitions) = (text(batch, 4)?, text(batch, 5)?);
        for row in 0..batch.num_rows() {
            let kind = match required(kinds[row])? {
                "r" => PostgresTableKind::Table,
                "p" => PostgresTableKind::PartitionedTable,
                "v" => PostgresTableKind::View,
                "m" => PostgresTableKind::MaterializedView,
                "f" => PostgresTableKind::ForeignTable,
                _ => return Err(unexpected_catalog_result()),
            };
            tables.insert(
                oids.value(row),
                PostgresTable {
                    namespace: required(namespaces[row])?.to_owned(),
                    name: required(names[row])?.to_owned(),
                    oid: oids.value(row).to_string(),
                    kind,
                    comment: comments[row].map(str::to_owned),
                    definition: definitions[row].map(str::to_owned),
                    columns: Vec::new(),
                    types: Vec::new(),
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

/// One `pg_attribute` row.
struct PgAttributeRow {
    relid: i64,
    attnum: i16,
    name: String,
    not_null: Option<PostgresNotNull>,
    data_source_type: String,
    type_oid: i64,
    typmod: i32,
    /// `attidentity`; empty for a column that is not an identity column.
    identity: String,
    /// `attgenerated`; empty for a column that is not generated.
    generated: String,
    /// The `pg_attrdef` expression: the default, or a generated column's
    /// generation expression.
    expression: Option<String>,
    comment: Option<String>,
}

impl PgAttributeRow {
    /// Where PostgreSQL takes the column's value from, other than the row.
    fn value_source(&self) -> Result<Option<PostgresValueSource>, ConnectorError> {
        let identity = match self.identity.as_str() {
            "" => None,
            "a" => Some(PostgresIdentity::Always),
            "d" => Some(PostgresIdentity::ByDefault),
            _ => return Err(unexpected_catalog_result()),
        };
        if let Some(generation) = identity {
            return Ok(Some(PostgresValueSource::Identity { generation }));
        }
        let storage = match self.generated.as_str() {
            "" => None,
            "s" => Some(PostgresGeneratedStorage::Stored),
            "v" => Some(PostgresGeneratedStorage::Virtual),
            _ => return Err(unexpected_catalog_result()),
        };
        if let Some(storage) = storage {
            return Ok(Some(PostgresValueSource::Generated {
                storage,
                expression: self
                    .expression
                    .clone()
                    .ok_or_else(unexpected_catalog_result)?,
            }));
        }
        Ok(self
            .expression
            .clone()
            .map(|expression| PostgresValueSource::Default { expression }))
    }
}

/// The columns of the requested tables and of the tables their foreign keys
/// reference, whose names the foreign keys need.
///
/// A column's `NOT NULL` is validated unless PostgreSQL 18's `pg_constraint`
/// row for it says otherwise; earlier servers have no such row and no
/// `NOT VALID` `NOT NULL`.
fn read_pg_attribute(
    catalog: Catalog<'_, '_>,
    relids: &str,
) -> Result<Vec<PgAttributeRow>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT a.attrelid::int8, a.attnum, a.attname::text, a.attnotnull, \
                pg_catalog.format_type(a.atttypid, a.atttypmod), a.atttypid::int8, \
                a.atttypmod, a.attidentity::text, a.attgenerated::text, \
                pg_catalog.pg_get_expr(d.adbin, d.adrelid), \
                pg_catalog.col_description(a.attrelid, a.attnum), \
                coalesce((SELECT nn.convalidated FROM pg_catalog.pg_constraint nn \
                          WHERE nn.conrelid = a.attrelid AND nn.contype = 'n' \
                            AND nn.conkey[1] = a.attnum), true) \
         FROM pg_catalog.pg_attribute a \
         LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
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
        let (identities, generated, expressions, comments) = (
            text(batch, 7)?,
            text(batch, 8)?,
            text(batch, 9)?,
            text(batch, 10)?,
        );
        let not_null_validated = boolean(batch, 11)?;
        for row in 0..batch.num_rows() {
            columns.push(PgAttributeRow {
                relid: relids.value(row),
                attnum: attnums.value(row),
                name: required(names[row])?.to_owned(),
                not_null: not_null.value(row).then(|| PostgresNotNull {
                    validated: not_null_validated.value(row),
                }),
                data_source_type: required(data_source_types[row])?.to_owned(),
                type_oid: type_oids.value(row),
                typmod: typmods.value(row),
                identity: required(identities[row])?.to_owned(),
                generated: required(generated[row])?.to_owned(),
                expression: expressions[row].map(str::to_owned),
                comment: comments[row].map(str::to_owned),
            });
        }
    }
    Ok(columns)
}

/// One `pg_type` row.
struct PgTypeRow {
    /// `format_type(oid, NULL)`.
    display_name: String,
    schema: String,
    typname: String,
    typtype: String,
    category: String,
    /// `typbasetype`: a domain's base type.
    base: i64,
    /// `typelem`: an array's element type.
    element: i64,
    typmod: i32,
    /// `pg_type.typnotnull`: a domain that rejects NULL.
    not_null: bool,
    comment: Option<String>,
}

/// The types of the requested tables' columns and every type they lead to
/// through domains' base types and arrays' element types, keyed by OID.
fn read_pg_type(
    catalog: Catalog<'_, '_>,
    relids: &str,
) -> Result<HashMap<i64, PgTypeRow>, ConnectorError> {
    let batches = catalog.query(format!(
        "WITH RECURSIVE reached(oid) AS ( \
             SELECT a.atttypid FROM pg_catalog.pg_attribute a \
             WHERE a.attrelid IN ({relids}) AND a.attnum > 0 AND NOT a.attisdropped \
             UNION \
             SELECT next.oid FROM reached r \
             JOIN pg_catalog.pg_type t ON t.oid = r.oid \
             CROSS JOIN LATERAL (VALUES (t.typbasetype), (t.typelem)) AS next(oid) \
             WHERE next.oid <> 0) \
         SELECT t.oid::int8, pg_catalog.format_type(t.oid, NULL), n.nspname::text, \
                t.typname::text, t.typtype::text, t.typcategory::text, \
                t.typbasetype::int8, t.typelem::int8, t.typtypmod, t.typnotnull, \
                CASE WHEN t.oid >= {FIRST_NORMAL_OBJECT_ID} \
                     THEN pg_catalog.obj_description(t.oid, 'pg_type') END \
         FROM pg_catalog.pg_type t \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
         WHERE t.oid IN (SELECT oid FROM reached)"
    ))?;
    let mut types = HashMap::new();
    for batch in &batches {
        let (oids, display_names, schemas, typnames) = (
            int64(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            text(batch, 3)?,
        );
        let (typtypes, categories, bases, elements) = (
            text(batch, 4)?,
            text(batch, 5)?,
            int64(batch, 6)?,
            int64(batch, 7)?,
        );
        let (typmods, not_null, comments) =
            (int32(batch, 8)?, boolean(batch, 9)?, text(batch, 10)?);
        for row in 0..batch.num_rows() {
            types.insert(
                oids.value(row),
                PgTypeRow {
                    display_name: required(display_names[row])?.to_owned(),
                    schema: required(schemas[row])?.to_owned(),
                    typname: required(typnames[row])?.to_owned(),
                    typtype: required(typtypes[row])?.to_owned(),
                    category: required(categories[row])?.to_owned(),
                    base: bases.value(row),
                    element: elements.value(row),
                    typmod: typmods.value(row),
                    not_null: not_null.value(row),
                    comment: comments[row].map(str::to_owned),
                },
            );
        }
    }
    Ok(types)
}

/// Every domain's CHECK constraints in name order, keyed by the domain's OID.
fn read_domain_checks(
    catalog: Catalog<'_, '_>,
) -> Result<HashMap<i64, Vec<PostgresDomainCheck>>, ConnectorError> {
    let batches = catalog.query(
        "SELECT con.contypid::int8, con.conname::text, \
                pg_catalog.pg_get_expr(con.conbin, 0), con.convalidated \
         FROM pg_catalog.pg_constraint con \
         WHERE con.contypid <> 0 AND con.contype = 'c' \
         ORDER BY con.contypid, con.conname",
    )?;
    let mut checks: HashMap<i64, Vec<PostgresDomainCheck>> = HashMap::new();
    for batch in &batches {
        let (oids, names, expressions, validated) = (
            int64(batch, 0)?,
            text(batch, 1)?,
            text(batch, 2)?,
            boolean(batch, 3)?,
        );
        for row in 0..batch.num_rows() {
            checks
                .entry(oids.value(row))
                .or_default()
                .push(PostgresDomainCheck {
                    name: required(names[row])?.to_owned(),
                    expression: required(expressions[row])?.to_owned(),
                    validated: validated.value(row),
                });
        }
    }
    Ok(checks)
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

/// The unique indexes that enforce uniqueness, as `read_pg_index` and
/// `read_index_expressions` select them. An invalid or unready index, such as
/// one a failed concurrent build left, does not.
const ENFORCING_UNIQUE_INDEX: &str = "i.indisunique AND i.indisvalid AND i.indisready";

/// The requested tables' unique indexes that enforce uniqueness.
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
         WHERE {ENFORCING_UNIQUE_INDEX} AND i.indrelid IN ({relids}) \
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

/// The expression keys of those unique indexes as `pg_get_indexdef` prints
/// them, keyed by index OID and 1-based key position.
fn read_index_expressions(
    catalog: Catalog<'_, '_>,
    relids: &str,
) -> Result<HashMap<(i64, i32), String>, ConnectorError> {
    let batches = catalog.query(format!(
        "SELECT i.indexrelid::int8, k.position::int4, \
                pg_catalog.pg_get_indexdef(i.indexrelid, k.position, true) \
         FROM pg_catalog.pg_index i \
         CROSS JOIN LATERAL generate_series(1, i.indnkeyatts) AS k(position) \
         WHERE {ENFORCING_UNIQUE_INDEX} AND i.indrelid IN ({relids}) \
           AND i.indkey[k.position - 1] = 0"
    ))?;
    let mut expressions = HashMap::new();
    for batch in &batches {
        let (oids, positions, definitions) = (int64(batch, 0)?, int32(batch, 1)?, text(batch, 2)?);
        for (row, definition) in definitions.into_iter().enumerate() {
            expressions.insert(
                (oids.value(row), positions.value(row)),
                required(definition)?.to_owned(),
            );
        }
    }
    Ok(expressions)
}

// Assembly: the rows become each table's columns, types, and constraints.

/// Column names by table OID and column number, for every column read.
fn column_names(columns: &[PgAttributeRow]) -> HashMap<(i64, i16), &str> {
    columns
        .iter()
        .map(|column| ((column.relid, column.attnum), column.name.as_str()))
        .collect()
}

/// Adds each requested table's columns, and the types they lead to through
/// domains' base types and arrays' element types.
fn add_columns(
    tables: &mut HashMap<i64, PostgresTable>,
    columns: &[PgAttributeRow],
    types: &HashMap<i64, PgTypeRow>,
    enums: &HashMap<i64, Vec<String>>,
    domain_checks: &HashMap<i64, Vec<PostgresDomainCheck>>,
) -> Result<(), ConnectorError> {
    // The OIDs of each table's types, ordered by display name.
    let mut table_types: HashMap<i64, BTreeMap<&str, i64>> = HashMap::new();
    for column in columns {
        let Some(table) = tables.get_mut(&column.relid) else {
            // A column of a referenced table, read only for its name.
            continue;
        };
        let reached = table_types.entry(column.relid).or_default();
        let mut pending = vec![column.type_oid];
        while let Some(oid) = pending.pop() {
            let row = types.get(&oid).ok_or_else(unexpected_catalog_result)?;
            match reached.insert(row.display_name.as_str(), oid) {
                Some(listed) if listed == oid => continue,
                // `format_type` names two different types alike.
                Some(_) => return Err(unexpected_catalog_result()),
                None => {}
            }
            if row.typtype == "d" {
                pending.push(row.base);
            } else if is_array(row) {
                pending.push(row.element);
            }
        }
        table.columns.push(PostgresColumn {
            name: column.name.clone(),
            attnum: column.attnum,
            not_null: column.not_null.clone(),
            data_source_type: column.data_source_type.clone(),
            ty: display_name(types, column.type_oid)?,
            typmod: column.typmod,
            comment: column.comment.clone(),
            value_source: column.value_source()?,
        });
    }
    for (relid, reached) in table_types {
        let table = tables
            .get_mut(&relid)
            .ok_or_else(unexpected_catalog_result)?;
        table.types = reached
            .into_values()
            .map(|oid| postgres_type(types, enums, domain_checks, oid))
            .collect::<Result<_, _>>()?;
    }
    Ok(())
}

/// Whether a type is an array, with an element type; some base types, such as
/// `name`, have an element type without being arrays.
fn is_array(row: &PgTypeRow) -> bool {
    row.typtype == "b" && row.category == "A" && row.element != 0
}

/// The display name of the type with this OID.
fn display_name(types: &HashMap<i64, PgTypeRow>, oid: i64) -> Result<String, ConnectorError> {
    types
        .get(&oid)
        .map(|row| row.display_name.clone())
        .ok_or_else(unexpected_catalog_result)
}

/// The source metadata of the type with this OID.
fn postgres_type(
    types: &HashMap<i64, PgTypeRow>,
    enums: &HashMap<i64, Vec<String>>,
    domain_checks: &HashMap<i64, Vec<PostgresDomainCheck>>,
    oid: i64,
) -> Result<PostgresType, ConnectorError> {
    let row = types.get(&oid).ok_or_else(unexpected_catalog_result)?;
    let form = match row.typtype.as_str() {
        "b" if is_array(row) => PostgresTypeForm::Array {
            element: display_name(types, row.element)?,
        },
        "b" => PostgresTypeForm::Base,
        "d" => PostgresTypeForm::Domain {
            base: display_name(types, row.base)?,
            typmod: row.typmod,
            not_null: row.not_null,
            checks: domain_checks.get(&oid).cloned().unwrap_or_default(),
        },
        "e" => PostgresTypeForm::Enum {
            labels: enums.get(&oid).cloned().unwrap_or_default(),
        },
        "c" => PostgresTypeForm::Composite,
        "r" => PostgresTypeForm::Range,
        "m" => PostgresTypeForm::Multirange,
        "p" => PostgresTypeForm::Pseudo,
        _ => return Err(unexpected_catalog_result()),
    };
    Ok(PostgresType {
        name: row.display_name.clone(),
        schema: row.schema.clone(),
        typname: row.typname.clone(),
        comment: row.comment.clone(),
        form,
    })
}

/// Adds each table's primary key, unique constraints, foreign keys, and
/// CHECK constraints, and returns the indexes that back primary key, unique,
/// and exclusion constraints, which are not listed as unique keys of their
/// own.
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
        let columns = column_list(column_names, row.relid, row.keys.as_deref())?;
        let table = tables
            .get_mut(&row.relid)
            .ok_or_else(unexpected_catalog_result)?;
        match row.contype.as_str() {
            "p" => {
                table.primary_key = Some(PostgresPrimaryKey {
                    name: row.name,
                    columns,
                });
            }
            "u" => table.unique_keys.push(PostgresUniqueKey {
                name: row.name,
                kind: PostgresUniqueKind::Constraint,
                keys: columns.into_iter().map(PostgresKey::Column).collect(),
                condition: None,
            }),
            "f" => table.foreign_keys.push(PostgresForeignKey {
                name: row.name,
                columns,
                referenced: PostgresReferencedTable {
                    namespace: row
                        .referenced_namespace
                        .ok_or_else(unexpected_catalog_result)?,
                    table: row.referenced_table.ok_or_else(unexpected_catalog_result)?,
                    columns: column_list(
                        column_names,
                        row.referenced_relid,
                        row.referenced_keys.as_deref(),
                    )?,
                },
                enforced: row.enforced,
                validated: row.validated,
            }),
            "c" => table.checks.push(PostgresCheck {
                name: row.name,
                expression: row.expression.ok_or_else(unexpected_catalog_result)?,
                enforced: row.enforced,
                validated: row.validated,
            }),
            // An exclusion constraint, read only for its index.
            _ => {}
        }
    }
    Ok(constraint_indexes)
}

/// Adds each table's unique indexes that do not back a constraint as unique
/// keys.
fn add_unique_indexes(
    tables: &mut HashMap<i64, PostgresTable>,
    indexes: Vec<PgIndexRow>,
    expressions: &HashMap<(i64, i32), String>,
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
            .ok_or_else(unexpected_catalog_result)?
            .iter()
            .zip(1..)
            .map(|(attnum, position)| match attnum {
                // An expression key has column number 0.
                0 => expressions
                    .get(&(row.oid, position))
                    .map(|expression| PostgresKey::Expression(expression.clone()))
                    .ok_or_else(unexpected_catalog_result),
                attnum => column_names
                    .get(&(row.relid, *attnum))
                    .map(|name| PostgresKey::Column((*name).to_owned()))
                    .ok_or_else(unexpected_catalog_result),
            })
            .collect::<Result<_, _>>()?;
        tables
            .get_mut(&row.relid)
            .ok_or_else(unexpected_catalog_result)?
            .unique_keys
            .push(PostgresUniqueKey {
                name: row.name,
                kind: PostgresUniqueKind::Index,
                keys,
                condition: row.condition,
            });
    }
    Ok(())
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
