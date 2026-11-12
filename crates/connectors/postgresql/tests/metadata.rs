//! Metadata retrieval against a PostgreSQL server.

mod support;

use std::collections::BTreeSet;

use analytics_types::{AnalyticsType, DecimalType, EnumType};
use connector_postgresql::{PostgresConnector, PostgresTable};
use connector_sdk::{
    Catalog, ColumnType, Component, Connector, Constraint, ConstraintKind, Listed, NamespacePath,
    Reported, SecretString, SourceTableMetadata, TableEntry, TableKind, TableRecord,
    TableReference, UniqueKind, UnsupportedReason, start_source_read,
};

use support::{PASSWORD, collect_ok, library, open, open_session, options, server, session};

type PostgresCatalog = <PostgresConnector as Connector>::Catalog;

const SCHEMA: &str = "metadata_test";

fn setup() {
    server().reset_schema(
        SCHEMA,
        r#"
CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
CREATE DOMAIN positive AS integer CHECK (VALUE > 0);
CREATE DOMAIN amount AS numeric(12, 2);
CREATE DOMAIN nested_amount AS amount;
CREATE DOMAIN required_integer AS integer NOT NULL;
CREATE DOMAIN nested_required AS required_integer;
CREATE TABLE strict (direct required_integer, nested nested_required, plain integer);
-- An invalid unique index. In production one is left behind when
-- `CREATE UNIQUE INDEX CONCURRENTLY` fails, for example on duplicate rows:
-- the index stays in the catalog with `indisunique` set but `indisvalid`
-- false, and enforces nothing. That statement cannot run here, because this
-- setup runs as one transaction and `CONCURRENTLY` refuses to, and its
-- failure would abort the setup. So the test builds a valid index and then
-- clears `indisvalid` directly, which the superuser test role may do; the
-- catalog then states exactly what a failed concurrent build leaves.
CREATE TABLE pending (code text);
CREATE UNIQUE INDEX pending_code ON pending (code);
UPDATE pg_catalog.pg_index SET indisvalid = false
  WHERE indexrelid = 'pending_code'::regclass;
CREATE TABLE parent (id bigint PRIMARY KEY, code text NOT NULL UNIQUE);
CREATE TABLE "Mixed Case" (
  flag boolean,
  small smallint NOT NULL,
  regular integer,
  big bigint,
  real_value real,
  double_value double precision,
  exact numeric(10, 2),
  widest numeric(38, 0),
  too_wide numeric(39, 0),
  unbounded numeric,
  negative_scale numeric(5, -2),
  free_text text,
  bounded varchar(10),
  padded char(5),
  bytes bytea,
  day date,
  clock time,
  stamp timestamp(3),
  instant timestamptz,
  span interval,
  id uuid,
  document json,
  binary_document jsonb,
  feeling mood,
  zoned timetz,
  count positive,
  money_amount nested_amount,
  numbers integer[],
  money_value money,
  parent_id bigint REFERENCES parent (id),
  CONSTRAINT positive_small CHECK (small > 0)
);
ALTER TABLE "Mixed Case" DROP COLUMN real_value;
CREATE UNIQUE INDEX active_code ON parent (code) WHERE id > 0;
CREATE UNIQUE INDEX lower_code ON parent (lower(code));
CREATE VIEW parent_view AS SELECT id FROM parent;
CREATE MATERIALIZED VIEW parent_snapshot AS SELECT id FROM parent;
"#,
    );
}

fn reference(name: &str) -> TableReference {
    TableReference::new(vec![SCHEMA.to_owned(), name.to_owned()]).unwrap()
}

fn read(names: &[TableReference]) -> Vec<TableRecord<PostgresTable>> {
    let session = session();
    let mut records =
        start_source_read(None, || PostgresCatalog::table_metadata(&session, names)).unwrap();
    collect_ok(&mut records)
}

fn read_one(name: &str) -> PostgresTable {
    match read(&[reference(name)]).remove(0) {
        TableRecord::Read { metadata, .. } => metadata,
        other => panic!("{name} was not read: {other:?}"),
    }
}

#[test]
fn metadata_retrieval_follows_the_postgresql_mapping() {
    setup();
    namespaces_and_tables_are_listed_with_exact_filters();
    columns_map_by_the_type_mapping();
    domain_not_null_makes_columns_not_nullable();
    invalid_unique_indexes_are_not_constraints();
    constraints_and_unique_indexes_are_returned();
    missing_tables_do_not_exist_and_reads_are_deterministic();
    sessions_report_the_driver_from_its_manifest();
    the_configured_search_path_spells_data_source_types();
}

fn namespaces_and_tables_are_listed_with_exact_filters() {
    let session = session();
    let filter = Some(BTreeSet::from([SCHEMA.to_owned(), "pg_catalog".to_owned()]));
    let mut namespaces =
        start_source_read(None, || PostgresCatalog::namespace_list(&session, &filter)).unwrap();
    assert_eq!(
        collect_ok(&mut namespaces),
        [Listed::Item(NamespacePath {
            names: vec![SCHEMA.to_owned()]
        })]
    );

    let namespaces = Some(BTreeSet::from([SCHEMA.to_owned()]));
    let mut tables = start_source_read(None, || {
        PostgresCatalog::table_list(&session, &namespaces, &None)
    })
    .unwrap();
    assert_eq!(
        collect_ok(&mut tables),
        [
            ("Mixed Case", TableKind::Table),
            ("parent", TableKind::Table),
            (
                "parent_snapshot",
                TableKind::Other("materialized view".to_owned())
            ),
            ("parent_view", TableKind::View),
            ("pending", TableKind::Table),
            ("strict", TableKind::Table),
        ]
        .map(|(name, kind)| Listed::Item(TableEntry {
            table: reference(name),
            kind,
        }))
    );

    let names = Some(BTreeSet::from(["parent".to_owned(), "PARENT".to_owned()]));
    let mut tables = start_source_read(None, || {
        PostgresCatalog::table_list(&session, &namespaces, &names)
    })
    .unwrap();
    assert_eq!(collect_ok(&mut tables).len(), 1);
}

fn columns_map_by_the_type_mapping() {
    let metadata = read_one("Mixed Case").to_table_metadata();
    assert!(metadata.is_complete());
    assert_eq!(metadata.kind(), &TableKind::Table);

    let mapped = |data_source_type: &str, analytics_type: AnalyticsType| ColumnType::Mapped {
        data_source_type: data_source_type.to_owned(),
        analytics_type,
    };
    let unsupported = |data_source_type: &str, reason| ColumnType::Unsupported {
        data_source_type: data_source_type.to_owned(),
        reason,
    };
    let decimal =
        |precision, scale| AnalyticsType::Decimal(DecimalType::new(precision, scale).unwrap());
    let expected = [
        ("flag", mapped("boolean", AnalyticsType::Boolean)),
        ("small", mapped("smallint", AnalyticsType::SmallInt)),
        ("regular", mapped("integer", AnalyticsType::Int)),
        ("big", mapped("bigint", AnalyticsType::BigInt)),
        (
            "double_value",
            mapped("double precision", AnalyticsType::Double),
        ),
        ("exact", mapped("numeric(10,2)", decimal(10, 2))),
        ("widest", mapped("numeric(38,0)", decimal(38, 0))),
        (
            "too_wide",
            unsupported("numeric(39,0)", UnsupportedReason::ParameterOutOfRange),
        ),
        (
            "unbounded",
            unsupported("numeric", UnsupportedReason::ParameterOutOfRange),
        ),
        ("negative_scale", mapped("numeric(5,-2)", decimal(5, -2))),
        ("free_text", mapped("text", AnalyticsType::Text)),
        (
            "bounded",
            mapped("character varying(10)", AnalyticsType::Text),
        ),
        ("padded", mapped("character(5)", AnalyticsType::Text)),
        ("bytes", mapped("bytea", AnalyticsType::Blob)),
        ("day", mapped("date", AnalyticsType::Date)),
        (
            "clock",
            mapped("time without time zone", AnalyticsType::Time),
        ),
        (
            "stamp",
            mapped("timestamp(3) without time zone", AnalyticsType::Timestamp),
        ),
        (
            "instant",
            mapped("timestamp with time zone", AnalyticsType::TimestampTz),
        ),
        ("span", mapped("interval", AnalyticsType::Interval)),
        ("id", mapped("uuid", AnalyticsType::Uuid)),
        ("document", mapped("json", AnalyticsType::Json)),
        ("binary_document", mapped("jsonb", AnalyticsType::Json)),
        (
            "feeling",
            mapped(
                "metadata_test.mood",
                AnalyticsType::Enum(EnumType::new(vec![
                    "sad".to_owned(),
                    "ok".to_owned(),
                    "happy".to_owned(),
                ])),
            ),
        ),
        (
            "zoned",
            mapped("time with time zone", AnalyticsType::TimeWithTimeZone),
        ),
        (
            "count",
            mapped("metadata_test.positive", AnalyticsType::Int),
        ),
        (
            "money_amount",
            mapped("metadata_test.nested_amount", decimal(12, 2)),
        ),
        (
            "numbers",
            unsupported("integer[]", UnsupportedReason::NotMapped),
        ),
        (
            "money_value",
            unsupported("money", UnsupportedReason::NotMapped),
        ),
        ("parent_id", mapped("bigint", AnalyticsType::BigInt)),
    ];
    let actual: Vec<_> = metadata
        .columns()
        .iter()
        .map(|column| (column.name.as_str(), column.ty.clone()))
        .collect();
    assert_eq!(actual, expected.to_vec());

    // The dropped column leaves no gap in ordinals, and source identifiers
    // keep PostgreSQL's attribute numbers.
    let ordinals: Vec<u32> = metadata
        .columns()
        .iter()
        .map(|column| column.ordinal)
        .collect();
    assert_eq!(ordinals, (1..=29).collect::<Vec<_>>());
    assert_eq!(
        metadata.columns()[4].source_id,
        Reported::Value("6".to_owned())
    );
    assert!(metadata.columns()[0].nullable);
    assert!(!metadata.columns()[1].nullable);
}

/// An invalid unique index, as a failed concurrent build leaves, does not
/// promise uniqueness, so it is not reported as a unique constraint. The
/// fixture marks `pending_code` invalid by editing the catalog; see `setup`.
fn invalid_unique_indexes_are_not_constraints() {
    let metadata = read_one("pending").to_table_metadata();
    assert_eq!(metadata.constraints(), &Reported::Value(Vec::new()));
}

fn domain_not_null_makes_columns_not_nullable() {
    let metadata = read_one("strict").to_table_metadata();
    let nullable: Vec<(&str, bool)> = metadata
        .columns()
        .iter()
        .map(|column| (column.name.as_str(), column.nullable))
        .collect();
    assert_eq!(
        nullable,
        [("direct", false), ("nested", false), ("plain", true)]
    );
}

fn constraints_and_unique_indexes_are_returned() {
    let child = read_one("Mixed Case").to_table_metadata();
    let Reported::Value(constraints) = child.constraints() else {
        panic!("constraints are read");
    };
    assert_eq!(
        constraints,
        &[
            Constraint {
                name: "Mixed Case_parent_id_fkey".to_owned(),
                enforced: true,
                kind: ConstraintKind::ForeignKey {
                    columns: vec!["parent_id".to_owned()],
                    referenced_table: reference("parent"),
                    referenced_columns: vec!["id".to_owned()],
                },
            },
            Constraint {
                name: "positive_small".to_owned(),
                enforced: true,
                kind: ConstraintKind::Check {
                    expression: "(small > 0)".to_owned(),
                },
            },
        ]
    );

    let parent = read_one("parent").to_table_metadata();
    let Reported::Value(constraints) = parent.constraints() else {
        panic!("constraints are read");
    };
    assert_eq!(
        constraints,
        &[
            Constraint {
                name: "parent_pkey".to_owned(),
                enforced: true,
                kind: ConstraintKind::PrimaryKey {
                    columns: vec!["id".to_owned()],
                },
            },
            Constraint {
                name: "active_code".to_owned(),
                enforced: true,
                kind: ConstraintKind::Unique {
                    columns: vec!["code".to_owned()],
                    kind: UniqueKind::Index,
                    condition: Some("(id > 0)".to_owned()),
                },
            },
            Constraint {
                name: "parent_code_key".to_owned(),
                enforced: true,
                kind: ConstraintKind::Unique {
                    columns: vec!["code".to_owned()],
                    kind: UniqueKind::Constraint,
                    condition: None,
                },
            },
        ]
    );
    let source = read_one("parent");
    assert!(
        source
            .unique_indexes
            .iter()
            .any(|index| index.name == "lower_code" && index.has_expressions)
    );
}

fn missing_tables_do_not_exist_and_reads_are_deterministic() {
    let missing = reference("missing");
    let wrong_depth = TableReference::new(vec!["parent".to_owned()]).unwrap();
    let records = read(&[missing.clone(), reference("parent"), wrong_depth.clone()]);
    assert!(matches!(&records[0], TableRecord::NotFound { table } if *table == missing));
    assert!(matches!(&records[1], TableRecord::Read { metadata, .. } if metadata.name == "parent"));
    assert!(matches!(&records[2], TableRecord::NotFound { table } if *table == wrong_depth));

    let first = read_one("Mixed Case");
    let second = read_one("Mixed Case");
    assert_eq!(first, second);
    let document = serde_json::to_string(&first).unwrap();
    assert_eq!(
        serde_json::from_str::<PostgresTable>(&document).unwrap(),
        first
    );
}

fn sessions_report_the_driver_from_its_manifest() {
    assert_eq!(
        Some(session().access().driver().clone()),
        Some(Component {
            name: "ADBC PostgreSQL Driver".to_owned(),
            version: "1.13.0".to_owned(),
        })
    );
}

/// Without the option the server's path applies, so types outside it are
/// qualified; with it, types on the configured path are not.
fn the_configured_search_path_spells_data_source_types() {
    let feeling = |metadata: &PostgresTable| {
        metadata
            .columns
            .iter()
            .find(|column| column.name == "feeling")
            .map(|column| column.data_source_type.clone())
    };
    assert_eq!(
        feeling(&read_one("Mixed Case")).as_deref(),
        Some("metadata_test.mood")
    );

    let mut configured = options("postgres", PASSWORD);
    configured.push((
        "search_path".to_owned(),
        SecretString::new(format!("{SCHEMA}, public")),
    ));
    let session = open(&library(), configured)
        .and_then(|data_source| open_session(&data_source))
        .unwrap();
    let mut records = start_source_read(None, || {
        PostgresCatalog::table_metadata(&session, &[reference("Mixed Case")])
    })
    .unwrap();
    let TableRecord::Read { metadata, .. } = collect_ok(&mut records).remove(0) else {
        panic!("the table is read");
    };
    assert_eq!(feeling(&metadata).as_deref(), Some("mood"));
}
