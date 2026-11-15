//! The metadata the contract expects of the fixture's objects.

use std::collections::BTreeSet;

use analytics_types::{AnalyticsType, DecimalType, EnumType};
use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Listed, NameFilter, NamespacePath, Reported,
    TableEntry, TableKind, TableMetadata, UniqueKind, UnsupportedReason,
    conformance::{ExpectedRecord, MetadataCase, MetadataRequest},
};

use super::{
    OTHER_SCHEMA, OUTSIDER, READER, SCHEMA, UPPER_SCHEMA, byte_options, options, reference,
};

pub fn cases() -> Vec<MetadataCase> {
    let reader = |name, request| MetadataCase {
        name,
        options: options(READER),
        request,
    };
    let outsider = |name, request| MetadataCase {
        name,
        options: options(OUTSIDER),
        request,
    };
    vec![
        reader(
            "namespaces are matched exactly",
            MetadataRequest::Namespaces {
                filter: names(&[SCHEMA, OTHER_SCHEMA, UPPER_SCHEMA, "CONFORMANCE", "mysql"]),
                expected: vec![
                    namespace(UPPER_SCHEMA),
                    namespace(SCHEMA),
                    namespace(OTHER_SCHEMA),
                ],
            },
        ),
        reader(
            "a namespace's tables are listed with their kinds",
            MetadataRequest::Tables {
                namespaces: names(&[SCHEMA]),
                tables: names(&["Mixed Case", "mixed case", "parent", "parent_view"]),
                expected: vec![
                    entry(SCHEMA, "Mixed Case", TableKind::Table),
                    entry(SCHEMA, "mixed case", TableKind::Table),
                    entry(SCHEMA, "parent", TableKind::Table),
                    entry(SCHEMA, "parent_view", TableKind::View),
                ],
            },
        ),
        reader(
            "table names are matched exactly in every namespace",
            MetadataRequest::Tables {
                namespaces: names(&[SCHEMA, OTHER_SCHEMA, UPPER_SCHEMA]),
                tables: names(&["Mixed Case", "PARENT", "parent"]),
                expected: vec![
                    entry(UPPER_SCHEMA, "parent", TableKind::Table),
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
                    reference(SCHEMA, "parent_view"),
                    reference(OTHER_SCHEMA, "parent"),
                    reference(UPPER_SCHEMA, "parent"),
                    reference(SCHEMA, "missing"),
                    reference("parent", "parent"),
                ],
                expected: vec![
                    ExpectedRecord::Read(mixed_case(AnalyticsType::Boolean)),
                    ExpectedRecord::Read(lower_case()),
                    ExpectedRecord::Read(parent()),
                    ExpectedRecord::Read(parent_view()),
                    ExpectedRecord::Read(single_id(OTHER_SCHEMA)),
                    ExpectedRecord::Read(single_id(UPPER_SCHEMA)),
                    ExpectedRecord::DoesNotExist,
                    ExpectedRecord::NotFound,
                ],
            },
        ),
        MetadataCase {
            name: "tinyint(1) is Byte by default",
            options: byte_options(READER),
            request: MetadataRequest::TableMetadata {
                tables: vec![reference(SCHEMA, "Mixed Case")],
                expected: vec![ExpectedRecord::Read(mixed_case(AnalyticsType::Byte))],
            },
        },
        outsider(
            "information_schema hides what the account holds no privilege on",
            MetadataRequest::TableMetadata {
                tables: vec![
                    reference(SCHEMA, "parent"),
                    reference(SCHEMA, "partial"),
                    reference(SCHEMA, "retyped"),
                    reference(SCHEMA, "missing"),
                ],
                expected: vec![
                    ExpectedRecord::Read(parent()),
                    ExpectedRecord::Read(partial()),
                    ExpectedRecord::NotFound,
                    ExpectedRecord::NotFound,
                ],
            },
        ),
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

fn entry(schema: &str, name: &str, kind: TableKind) -> Listed<TableEntry> {
    Listed::Item(TableEntry {
        table: reference(schema, name),
        kind,
    })
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

/// Columns numbered from 1; MySQL offers no column identifiers.
fn columns(columns: Vec<(&str, bool, ColumnType)>) -> Vec<Column> {
    columns
        .into_iter()
        .zip(1..)
        .map(|((name, nullable, ty), ordinal)| Column {
            name: name.to_owned(),
            ordinal,
            nullable,
            source_id: Reported::NotOffered,
            ty,
        })
        .collect()
}

fn table(
    schema: &str,
    name: &str,
    kind: TableKind,
    columns: Vec<Column>,
    constraints: Vec<Constraint>,
) -> TableMetadata {
    TableMetadata::new(
        reference(schema, name),
        kind,
        Reported::NotOffered,
        columns,
        false,
        Reported::Value(constraints),
    )
}

/// `Mixed Case`, whose `tinyint(1)` column `flag` maps to `flag_type`: `Byte`
/// by default, or `Boolean` with `tinyint1_as_boolean`.
fn mixed_case(flag_type: AnalyticsType) -> TableMetadata {
    use AnalyticsType::*;
    use UnsupportedReason::NotMapped;
    let mood = Enum(EnumType::new(vec![
        "sad".to_owned(),
        "ok".to_owned(),
        "happy".to_owned(),
    ]));
    let columns = columns(vec![
        ("tiny", true, mapped("tinyint", Byte)),
        ("flag", true, mapped("tinyint(1)", flag_type)),
        ("small", false, mapped("smallint", SmallInt)),
        ("medium", true, mapped("mediumint", Int)),
        ("regular", true, mapped("int", Int)),
        ("big", true, mapped("bigint", BigInt)),
        (
            "tiny_unsigned",
            true,
            mapped("tinyint unsigned", UnsignedByte),
        ),
        (
            "small_unsigned",
            true,
            mapped("smallint unsigned", UnsignedSmallInt),
        ),
        (
            "medium_unsigned",
            true,
            mapped("mediumint unsigned", UnsignedInt),
        ),
        (
            "regular_unsigned",
            true,
            mapped("int unsigned", UnsignedInt),
        ),
        (
            "big_unsigned",
            true,
            mapped("bigint unsigned", UnsignedBigInt),
        ),
        ("single", true, mapped("float", Float)),
        ("double_value", true, mapped("double", Double)),
        ("exact", true, mapped("decimal(10,2)", decimal(10, 2))),
        ("widest", true, mapped("decimal(38,0)", decimal(38, 0))),
        ("wide", true, mapped("decimal(65,30)", decimal(65, 30))),
        ("fixed", true, mapped("char(5)", Text)),
        ("bounded", true, mapped("varchar(10)", Text)),
        ("tiny_text", true, mapped("tinytext", Text)),
        ("free_text", true, mapped("text", Text)),
        ("medium_text", true, mapped("mediumtext", Text)),
        ("long_text", true, mapped("longtext", Text)),
        ("fixed_bytes", true, mapped("binary(3)", Blob)),
        ("bytes", true, mapped("varbinary(10)", Blob)),
        ("tiny_blob", true, mapped("tinyblob", Blob)),
        ("regular_blob", true, mapped("blob", Blob)),
        ("medium_blob", true, mapped("mediumblob", Blob)),
        ("long_blob", true, mapped("longblob", Blob)),
        ("day", true, mapped("date", Date)),
        ("stamp", true, mapped("datetime(3)", Timestamp)),
        ("instant", true, mapped("timestamp", TimestampTz)),
        ("year_value", true, mapped("year", Year)),
        ("feeling", true, mapped("enum('sad','ok','happy')", mood)),
        ("document", true, mapped("json", Json)),
        ("clock", true, unsupported("time", NotMapped)),
        ("bits", true, unsupported("bit(3)", NotMapped)),
        ("bit_flag", true, mapped("bit(1)", Boolean)),
        ("tags", true, unsupported("set('a','b')", NotMapped)),
        ("location", true, unsupported("point", NotMapped)),
        ("parent_id", true, mapped("bigint", BigInt)),
    ]);
    table(
        SCHEMA,
        "Mixed Case",
        TableKind::Table,
        columns,
        vec![
            Constraint {
                name: "mixed_parent".to_owned(),
                enforced: true,
                kind: ConstraintKind::ForeignKey {
                    columns: vec!["parent_id".to_owned()],
                    referenced_table: reference(SCHEMA, "parent"),
                    referenced_columns: vec!["id".to_owned()],
                },
            },
            Constraint {
                name: "loose".to_owned(),
                enforced: false,
                kind: ConstraintKind::Check {
                    expression: "(`regular` < 100)".to_owned(),
                },
            },
            Constraint {
                name: "positive_small".to_owned(),
                enforced: true,
                kind: ConstraintKind::Check {
                    expression: "(`small` > 0)".to_owned(),
                },
            },
        ],
    )
}

fn lower_case() -> TableMetadata {
    single_id_in(SCHEMA, "mixed case")
}

fn single_id(schema: &str) -> TableMetadata {
    single_id_in(schema, "parent")
}

fn single_id_in(schema: &str, name: &str) -> TableMetadata {
    table(
        schema,
        name,
        TableKind::Table,
        columns(vec![("id", true, mapped("int", AnalyticsType::Int))]),
        Vec::new(),
    )
}

/// A unique column prefix makes its column unique; a functional key part
/// (`lower_code`) is not a column constraint.
fn parent() -> TableMetadata {
    let unique = |name: &str, column: &str| Constraint {
        name: name.to_owned(),
        enforced: true,
        kind: ConstraintKind::Unique {
            columns: vec![column.to_owned()],
            kind: UniqueKind::Constraint,
            condition: None,
        },
    };
    table(
        SCHEMA,
        "parent",
        TableKind::Table,
        columns(vec![
            ("id", false, mapped("bigint", AnalyticsType::BigInt)),
            ("code", false, mapped("varchar(20)", AnalyticsType::Text)),
            ("label", true, mapped("varchar(40)", AnalyticsType::Text)),
        ]),
        vec![
            Constraint {
                name: "PRIMARY".to_owned(),
                enforced: true,
                kind: ConstraintKind::PrimaryKey {
                    columns: vec!["id".to_owned()],
                },
            },
            unique("label_prefix", "label"),
            unique("parent_code", "code"),
        ],
    )
}

fn parent_view() -> TableMetadata {
    table(
        SCHEMA,
        "parent_view",
        TableKind::View,
        columns(vec![("id", false, mapped("bigint", AnalyticsType::BigInt))]),
        Vec::new(),
    )
}

/// The outsider holds a privilege on `id` only, so `secret` is hidden and the
/// table metadata is incomplete.
fn partial() -> TableMetadata {
    TableMetadata::new(
        reference(SCHEMA, "partial"),
        TableKind::Table,
        Reported::NotOffered,
        columns(vec![("id", false, mapped("int", AnalyticsType::Int))]),
        true,
        Reported::Value(Vec::new()),
    )
}
