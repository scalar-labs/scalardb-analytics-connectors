//! The metadata the contract expects of the fixture's objects.

use std::collections::BTreeSet;

use analytics_types::{AnalyticsType, DecimalType, EnumType};
use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Listed, NameFilter, NamespacePath, Reported,
    TableEntry, TableKind, TableMetadata, UniqueKind, UnsupportedReason,
    conformance::{ExpectedRecord, MetadataCase, MetadataRequest},
};

use super::{OTHER_SCHEMA, OUTSIDER, READER, SCHEMA, options, reference, server};

pub fn cases() -> Vec<MetadataCase> {
    let reader = |name, request| MetadataCase {
        name,
        options: options(READER),
        request,
    };
    vec![
        reader(
            "namespaces are matched exactly",
            MetadataRequest::Namespaces {
                filter: names(&[SCHEMA, OTHER_SCHEMA, "CONFORMANCE", "pg_catalog"]),
                expected: vec![namespace(SCHEMA), namespace(OTHER_SCHEMA)],
            },
        ),
        reader(
            "a namespace's tables are listed with their kinds",
            MetadataRequest::Tables {
                namespaces: names(&[SCHEMA]),
                tables: names(&[
                    "Mixed Case",
                    "mixed case",
                    "parent",
                    "parent_view",
                    "pending",
                ]),
                expected: vec![
                    entry(SCHEMA, "Mixed Case", TableKind::Table),
                    entry(SCHEMA, "mixed case", TableKind::Table),
                    entry(SCHEMA, "parent", TableKind::Table),
                    entry(SCHEMA, "parent_view", TableKind::View),
                    entry(SCHEMA, "pending", TableKind::Table),
                ],
            },
        ),
        reader(
            "table names are matched exactly in every namespace",
            MetadataRequest::Tables {
                namespaces: names(&[SCHEMA, OTHER_SCHEMA]),
                tables: names(&["Mixed Case", "PARENT", "parent"]),
                expected: vec![
                    entry(SCHEMA, "Mixed Case", TableKind::Table),
                    entry(SCHEMA, "parent", TableKind::Table),
                    entry(OTHER_SCHEMA, "parent", TableKind::Table),
                ],
            },
        ),
        reader(
            "table metadata follows the type mapping",
            MetadataRequest::TableMetadata {
                tables: vec![
                    reference(SCHEMA, "Mixed Case"),
                    reference(SCHEMA, "mixed case"),
                    reference(SCHEMA, "parent"),
                    reference(SCHEMA, "strict"),
                    reference(SCHEMA, "pending"),
                    reference(SCHEMA, "parent_view"),
                    reference(OTHER_SCHEMA, "parent"),
                    reference(SCHEMA, "missing"),
                    reference("parent", "parent"),
                ],
                expected: vec![
                    ExpectedRecord::Read(mixed_case()),
                    ExpectedRecord::Read(lower_case()),
                    ExpectedRecord::Read(parent()),
                    ExpectedRecord::Read(strict()),
                    ExpectedRecord::Read(pending()),
                    ExpectedRecord::Read(parent_view()),
                    ExpectedRecord::Read(other_parent()),
                    ExpectedRecord::DoesNotExist,
                    ExpectedRecord::DoesNotExist,
                ],
            },
        ),
        MetadataCase {
            name: "the catalog shows tables without privilege",
            options: options(OUTSIDER),
            request: MetadataRequest::TableMetadata {
                tables: vec![reference(SCHEMA, "strict")],
                expected: vec![ExpectedRecord::Read(strict())],
            },
        },
    ]
}

fn names(names: &[&str]) -> NameFilter {
    Some(
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<_>>(),
    )
}

fn namespace(name: &str) -> Listed<NamespacePath> {
    Listed::Item(NamespacePath {
        names: vec![name.to_owned()],
    })
}

fn entry(namespace: &str, name: &str, kind: TableKind) -> Listed<TableEntry> {
    Listed::Item(TableEntry {
        table: reference(namespace, name),
        kind,
    })
}

/// The table's object identifier, its source identifier.
fn oid(namespace: &str, name: &str) -> Reported<String> {
    let oid = server().psql(&format!("SELECT '{namespace}.\"{name}\"'::regclass::oid"));
    Reported::Value(oid.trim().to_owned())
}

fn mapped(data_source_type: &str, analytics_type: AnalyticsType) -> ColumnType {
    ColumnType::Mapped {
        data_source_type: data_source_type.to_owned(),
        analytics_type,
    }
}

fn unsupported(data_source_type: &str, reason: UnsupportedReason) -> ColumnType {
    ColumnType::Unsupported {
        data_source_type: data_source_type.to_owned(),
        reason,
    }
}

fn decimal(precision: u8, scale: i8) -> AnalyticsType {
    AnalyticsType::Decimal(DecimalType::new(precision, scale).unwrap())
}

/// Columns numbered from 1, each with its attribute number as source identifier.
fn columns(columns: Vec<(&str, u32, bool, ColumnType)>) -> Vec<Column> {
    columns
        .into_iter()
        .enumerate()
        .map(|(index, (name, attnum, nullable, ty))| Column {
            name: name.to_owned(),
            ordinal: u32::try_from(index).unwrap() + 1,
            nullable,
            source_id: Reported::Value(attnum.to_string()),
            ty,
        })
        .collect()
}

fn table(
    namespace: &str,
    name: &str,
    kind: TableKind,
    columns: Vec<Column>,
    constraints: Vec<Constraint>,
) -> TableMetadata {
    TableMetadata::new(
        reference(namespace, name),
        kind,
        oid(namespace, name),
        columns,
        false,
        Reported::Value(constraints),
    )
}

fn mixed_case() -> TableMetadata {
    use AnalyticsType::*;
    let mood = Enum(EnumType::new(vec![
        "sad".to_owned(),
        "ok".to_owned(),
        "happy".to_owned(),
    ]));
    // `real_value`, attribute 5, was dropped: ordinals have no gap, and source
    // identifiers keep the attribute numbers.
    let columns = columns(vec![
        ("flag", 1, true, mapped("boolean", Boolean)),
        ("small", 2, false, mapped("smallint", SmallInt)),
        ("regular", 3, true, mapped("integer", Int)),
        ("big", 4, true, mapped("bigint", BigInt)),
        ("single", 6, true, mapped("real", Float)),
        ("double_value", 7, true, mapped("double precision", Double)),
        ("exact", 8, true, mapped("numeric(10,2)", decimal(10, 2))),
        ("widest", 9, true, mapped("numeric(38,0)", decimal(38, 0))),
        (
            "too_wide",
            10,
            true,
            unsupported("numeric(77,0)", UnsupportedReason::ParameterOutOfRange),
        ),
        (
            "unbounded",
            11,
            true,
            unsupported("numeric", UnsupportedReason::ParameterOutOfRange),
        ),
        (
            "negative_scale",
            12,
            true,
            mapped("numeric(5,-2)", decimal(5, -2)),
        ),
        ("free_text", 13, true, mapped("text", Text)),
        ("bounded", 14, true, mapped("character varying(10)", Text)),
        ("padded", 15, true, mapped("character(5)", Text)),
        ("bytes", 16, true, mapped("bytea", Blob)),
        ("day", 17, true, mapped("date", Date)),
        ("clock", 18, true, mapped("time without time zone", Time)),
        (
            "stamp",
            19,
            true,
            mapped("timestamp(3) without time zone", Timestamp),
        ),
        (
            "instant",
            20,
            true,
            mapped("timestamp with time zone", TimestampTz),
        ),
        ("span", 21, true, mapped("interval", Interval)),
        ("id", 22, true, mapped("uuid", Uuid)),
        ("document", 23, true, mapped("json", Json)),
        ("binary_document", 24, true, mapped("jsonb", Json)),
        ("feeling", 25, true, mapped("conformance.mood", mood)),
        (
            "zoned",
            26,
            true,
            mapped("time with time zone", TimeWithTimeZone),
        ),
        ("count", 27, true, mapped("conformance.positive", Int)),
        (
            "money_amount",
            28,
            true,
            mapped("conformance.nested_amount", decimal(12, 2)),
        ),
        (
            "numbers",
            29,
            true,
            unsupported("integer[]", UnsupportedReason::NotMapped),
        ),
        (
            "money_value",
            30,
            true,
            unsupported("money", UnsupportedReason::NotMapped),
        ),
        ("parent_id", 31, true, mapped("bigint", BigInt)),
    ]);
    table(
        SCHEMA,
        "Mixed Case",
        TableKind::Table,
        columns,
        vec![
            Constraint {
                name: "Mixed Case_parent_id_fkey".to_owned(),
                enforced: true,
                kind: ConstraintKind::ForeignKey {
                    columns: vec!["parent_id".to_owned()],
                    referenced_table: reference(SCHEMA, "parent"),
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
        ],
    )
}

fn lower_case() -> TableMetadata {
    table(
        SCHEMA,
        "mixed case",
        TableKind::Table,
        columns(vec![("id", 1, true, mapped("integer", AnalyticsType::Int))]),
        Vec::new(),
    )
}

fn parent() -> TableMetadata {
    table(
        SCHEMA,
        "parent",
        TableKind::Table,
        columns(vec![
            ("id", 1, false, mapped("bigint", AnalyticsType::BigInt)),
            ("code", 2, false, mapped("text", AnalyticsType::Text)),
        ]),
        vec![
            Constraint {
                name: "parent_pkey".to_owned(),
                enforced: true,
                kind: ConstraintKind::PrimaryKey {
                    columns: vec!["id".to_owned()],
                },
            },
            // A partial unique index is a constraint with its condition; the
            // expression index `lower_code` is not.
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
        ],
    )
}

/// A domain's NOT NULL, directly or through another domain, makes its column
/// not nullable.
fn strict() -> TableMetadata {
    table(
        SCHEMA,
        "strict",
        TableKind::Table,
        columns(vec![
            (
                "direct",
                1,
                false,
                mapped("conformance.required_integer", AnalyticsType::Int),
            ),
            (
                "nested",
                2,
                false,
                mapped("conformance.nested_required", AnalyticsType::Int),
            ),
            ("plain", 3, true, mapped("integer", AnalyticsType::Int)),
        ]),
        Vec::new(),
    )
}

/// An invalid unique index promises nothing, so it is no constraint.
fn pending() -> TableMetadata {
    table(
        SCHEMA,
        "pending",
        TableKind::Table,
        columns(vec![("code", 1, true, mapped("text", AnalyticsType::Text))]),
        Vec::new(),
    )
}

fn parent_view() -> TableMetadata {
    table(
        SCHEMA,
        "parent_view",
        TableKind::View,
        columns(vec![(
            "id",
            1,
            true,
            mapped("bigint", AnalyticsType::BigInt),
        )]),
        Vec::new(),
    )
}

fn other_parent() -> TableMetadata {
    table(
        OTHER_SCHEMA,
        "parent",
        TableKind::Table,
        columns(vec![("id", 1, true, mapped("integer", AnalyticsType::Int))]),
        Vec::new(),
    )
}
