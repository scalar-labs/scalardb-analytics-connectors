//! The metadata the contract expects of the fixture's objects.

use std::collections::BTreeSet;

use analytics_types::{AnalyticsType, DecimalType};
use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Listed, NameFilter, NamespacePath, Reported,
    TableEntry, TableKind, TableMetadata, UniqueKind, UnsupportedReason,
    conformance::{ExpectedRecord, MetadataCase, MetadataRequest},
};

use super::{OTHER_SCHEMA, OUTSIDER, READER, SCHEMA, options, reference, scalar};

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
            "namespaces are matched exactly, without Oracle's own",
            MetadataRequest::Namespaces {
                filter: names(&[SCHEMA, OTHER_SCHEMA, "conformance", "SYS", "SYSTEM"]),
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
                    "MIXED CASE",
                    "PARENT",
                    "PARENT_VIEW",
                    "Ünï😀",
                ]),
                expected: vec![
                    entry(SCHEMA, "Mixed Case", TableKind::Table),
                    entry(SCHEMA, "PARENT", TableKind::Table),
                    entry(SCHEMA, "PARENT_VIEW", TableKind::View),
                    entry(SCHEMA, "mixed case", TableKind::Table),
                    entry(SCHEMA, "Ünï😀", TableKind::Table),
                ],
            },
        ),
        reader(
            "table names are matched exactly in every namespace",
            MetadataRequest::Tables {
                namespaces: names(&[SCHEMA, OTHER_SCHEMA]),
                tables: names(&["Mixed Case", "parent", "PARENT"]),
                expected: vec![
                    entry(SCHEMA, "Mixed Case", TableKind::Table),
                    entry(SCHEMA, "PARENT", TableKind::Table),
                    entry(OTHER_SCHEMA, "PARENT", TableKind::Table),
                ],
            },
        ),
        reader(
            "table metadata follows the type mapping",
            MetadataRequest::TableMetadata {
                tables: vec![
                    reference(SCHEMA, "VALUE_TYPES"),
                    reference(SCHEMA, "Mixed Case"),
                    reference(SCHEMA, "mixed case"),
                    reference(SCHEMA, "Ünï😀"),
                    reference(SCHEMA, "PARENT"),
                    reference(SCHEMA, "CHILD"),
                    reference(SCHEMA, "PARENT_VIEW"),
                    reference(SCHEMA, "CONDITIONS"),
                    reference(OTHER_SCHEMA, "PARENT"),
                    reference(SCHEMA, "MISSING"),
                    reference("SYS", "OBJ$"),
                    reference("PARENT", "PARENT"),
                ],
                expected: vec![
                    ExpectedRecord::Read(value_types()),
                    ExpectedRecord::Read(single_column(SCHEMA, "Mixed Case", "ID", None)),
                    ExpectedRecord::Read(single_column(SCHEMA, "mixed case", "ID", None)),
                    ExpectedRecord::Read(single_column(SCHEMA, "Ünï😀", "çol", None)),
                    ExpectedRecord::Read(parent()),
                    ExpectedRecord::Read(child()),
                    ExpectedRecord::Read(parent_view()),
                    ExpectedRecord::Read(conditions()),
                    ExpectedRecord::Read(single_column(
                        OTHER_SCHEMA,
                        "PARENT",
                        "ID",
                        Some("OTHER_PARENT_PKEY"),
                    )),
                    ExpectedRecord::NotFound,
                    ExpectedRecord::NotFound,
                    ExpectedRecord::NotFound,
                ],
            },
        ),
        outsider(
            "a privilege other than SELECT shows a table's metadata",
            MetadataRequest::TableMetadata {
                tables: vec![reference(SCHEMA, "HIDDEN")],
                expected: vec![ExpectedRecord::Read(hidden())],
            },
        ),
        outsider(
            "a foreign key to a table the credential cannot see leaves constraints missing",
            MetadataRequest::TableMetadata {
                tables: vec![reference(SCHEMA, "CHILD")],
                expected: vec![ExpectedRecord::Read(TableMetadata::new(
                    reference(SCHEMA, "CHILD"),
                    TableKind::Table,
                    object_id(SCHEMA, "CHILD"),
                    child_columns(),
                    false,
                    Reported::Missing {
                        reason: "Oracle hides the table a foreign key references when the \
                                 credential has no privilege on it"
                            .to_owned(),
                    },
                ))],
            },
        ),
        outsider(
            "a table the credential cannot see may exist",
            MetadataRequest::TableMetadata {
                tables: vec![reference(SCHEMA, "INVISIBLE"), reference(SCHEMA, "MISSING")],
                expected: vec![ExpectedRecord::NotFound, ExpectedRecord::NotFound],
            },
        ),
        outsider(
            "the lists hold what the credential can see",
            MetadataRequest::Tables {
                namespaces: names(&[SCHEMA]),
                tables: names(&["HIDDEN", "CHILD", "INVISIBLE", "PARENT"]),
                expected: vec![
                    entry(SCHEMA, "CHILD", TableKind::Table),
                    entry(SCHEMA, "HIDDEN", TableKind::Table),
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

fn entry(namespace: &str, name: &str, kind: TableKind) -> Listed<TableEntry> {
    Listed::Item(TableEntry {
        table: reference(namespace, name),
        kind,
    })
}

/// The table's `OBJECT_ID`, its source identifier.
fn object_id(namespace: &str, name: &str) -> Reported<String> {
    Reported::Value(scalar(&format!(
        "SELECT object_id FROM all_objects WHERE owner = '{namespace}' \
         AND object_name = '{name}' AND object_type IN ('TABLE', 'VIEW');"
    )))
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

/// `NUMBER(10,0)`, the type of every fixture key.
fn key() -> ColumnType {
    mapped("NUMBER(10,0)", decimal(10, 0))
}

/// Columns in order, each with its position as ordinal and no source
/// identifier, which Oracle does not keep for a column.
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

fn primary_key(name: &str, columns: &[&str]) -> Constraint {
    Constraint {
        name: name.to_owned(),
        enforced: true,
        kind: ConstraintKind::PrimaryKey {
            columns: columns.iter().map(|column| (*column).to_owned()).collect(),
        },
    }
}

fn unique(name: &str, column: &str, kind: UniqueKind) -> Constraint {
    Constraint {
        name: name.to_owned(),
        enforced: true,
        kind: ConstraintKind::Unique {
            columns: vec![column.to_owned()],
            kind,
            condition: None,
        },
    }
}

fn table(
    name: &str,
    kind: TableKind,
    columns: Vec<Column>,
    constraints: Reported<Vec<Constraint>>,
) -> TableMetadata {
    TableMetadata::new(
        reference(SCHEMA, name),
        kind,
        object_id(SCHEMA, name),
        columns,
        false,
        constraints,
    )
}

fn value_types() -> TableMetadata {
    use AnalyticsType::*;
    table(
        "VALUE_TYPES",
        TableKind::Table,
        columns(vec![
            ("ID", false, key()),
            ("WHOLE", true, mapped("NUMBER(38,0)", decimal(38, 0))),
            ("FRACTION", true, mapped("NUMBER(38,38)", decimal(38, 38))),
            ("SMALL", true, mapped("NUMBER(5,2)", decimal(5, 2))),
            ("MIDDLE", true, mapped("NUMBER(18,4)", decimal(18, 4))),
            ("SINGLE", true, mapped("BINARY_FLOAT", Float)),
            ("RATIO", true, mapped("BINARY_DOUBLE", Double)),
            ("FLAG", true, mapped("BOOLEAN", Boolean)),
            ("FIXED", true, mapped("CHAR(5 BYTE)", Text)),
            ("UNICODE_FIXED", true, mapped("NCHAR(5)", Text)),
            ("BOUNDED", true, mapped("VARCHAR2(10 CHAR)", Text)),
            ("WIDE", true, mapped("NVARCHAR2(20)", Text)),
            ("BYTES", true, mapped("RAW(2)", Blob)),
            ("DAY", true, mapped("DATE", Timestamp)),
            ("STAMP", true, mapped("TIMESTAMP(6)", Timestamp)),
            ("STAMP3", true, mapped("TIMESTAMP(3)", Timestamp)),
            ("STAMP0", true, mapped("TIMESTAMP(0)", Timestamp)),
            (
                "INSTANT",
                true,
                mapped("TIMESTAMP(6) WITH TIME ZONE", TimestampTz),
            ),
            (
                "LOCAL_INSTANT",
                true,
                mapped("TIMESTAMP(6) WITH LOCAL TIME ZONE", TimestampTz),
            ),
        ]),
        Reported::Value(vec![primary_key("VALUE_TYPES_PKEY", &["ID"])]),
    )
}

/// A table with one `NUMBER(10)` column, and its primary key, if any.
fn single_column(
    namespace: &str,
    name: &str,
    column: &str,
    primary: Option<&str>,
) -> TableMetadata {
    TableMetadata::new(
        reference(namespace, name),
        TableKind::Table,
        object_id(namespace, name),
        columns(vec![(column, false, key())]),
        false,
        Reported::Value(
            primary
                .map(|primary| primary_key(primary, &[column]))
                .into_iter()
                .collect(),
        ),
    )
}

/// The CHECK condition of `PARENT` as Oracle stores it: 500 comparisons,
/// longer than Oracle's 4000-byte copy of it.
fn long_check() -> String {
    let mut condition = "amount >= 0".to_owned();
    for n in 1..=500 {
        condition.push_str(&format!(" AND amount <> {n}"));
    }
    condition
}

fn parent() -> TableMetadata {
    use AnalyticsType::*;
    use UnsupportedReason::{MappingNotImplemented, NotMapped, ParameterOutOfRange};
    table(
        "PARENT",
        TableKind::Table,
        columns(vec![
            ("ID", false, key()),
            ("CODE", false, mapped("VARCHAR2(10 CHAR)", Text)),
            ("AMOUNT", false, mapped("NUMBER(10,2)", decimal(10, 2))),
            ("DOUBLED", true, mapped("NUMBER(12,2)", decimal(12, 2))),
            ("SLOT", true, key()),
            ("NOTE", true, unsupported("CLOB", MappingNotImplemented)),
            (
                "UNICODE_NOTE",
                true,
                unsupported("NCLOB", MappingNotImplemented),
            ),
            ("PAYLOAD", true, unsupported("BLOB", MappingNotImplemented)),
            ("ANYTHING", true, unsupported("NUMBER", ParameterOutOfRange)),
            (
                "WHOLE",
                true,
                unsupported("NUMBER(*,0)", ParameterOutOfRange),
            ),
            (
                "ROUNDED",
                true,
                unsupported("NUMBER(5,-2)", ParameterOutOfRange),
            ),
            (
                "TINY",
                true,
                unsupported("NUMBER(2,5)", ParameterOutOfRange),
            ),
            ("APPROXIMATE", true, unsupported("FLOAT(10)", NotMapped)),
            (
                "PRECISE",
                true,
                unsupported("TIMESTAMP(9)", ParameterOutOfRange),
            ),
            (
                "PRECISE_INSTANT",
                true,
                unsupported("TIMESTAMP(9) WITH TIME ZONE", ParameterOutOfRange),
            ),
            (
                "SPAN_MONTHS",
                true,
                unsupported("INTERVAL YEAR(2) TO MONTH", NotMapped),
            ),
            (
                "SPAN_SECONDS",
                true,
                unsupported("INTERVAL DAY(2) TO SECOND(6)", NotMapped),
            ),
            ("LEGACY", true, unsupported("LONG", NotMapped)),
            ("ROW_ADDRESS", true, unsupported("ROWID", NotMapped)),
            ("ANY_ROW_ADDRESS", true, unsupported("UROWID", NotMapped)),
            ("EXTERNAL_FILE", true, unsupported("BFILE", NotMapped)),
            ("DOCUMENT", true, unsupported("JSON", NotMapped)),
            ("EMBEDDING", true, unsupported("VECTOR", NotMapped)),
            ("PLACE", true, unsupported("CONFORMANCE.ADDRESS", NotMapped)),
        ]),
        Reported::Value(vec![
            primary_key("PARENT_PKEY", &["ID"]),
            unique("PARENT_CODE_KEY", "CODE", UniqueKind::Constraint),
            unique("PARENT_SLOT_KEY", "SLOT", UniqueKind::Index),
            Constraint {
                name: "PARENT_AMOUNT_CHECK".to_owned(),
                enforced: true,
                kind: ConstraintKind::Check {
                    expression: long_check(),
                },
            },
        ]),
    )
}

fn child_columns() -> Vec<Column> {
    columns(vec![
        ("ID", false, key()),
        ("PARENT_ID", true, key()),
        ("OTHER_ID", true, key()),
    ])
}

fn child() -> TableMetadata {
    let foreign_key = |name: &str, column: &str, namespace: &str, enforced| Constraint {
        name: name.to_owned(),
        enforced,
        kind: ConstraintKind::ForeignKey {
            columns: vec![column.to_owned()],
            referenced_table: reference(namespace, "PARENT"),
            referenced_columns: vec!["ID".to_owned()],
        },
    };
    table(
        "CHILD",
        TableKind::Table,
        child_columns(),
        Reported::Value(vec![
            primary_key("CHILD_PKEY", &["ID"]),
            foreign_key("CHILD_PARENT_FKEY", "PARENT_ID", SCHEMA, true),
            foreign_key("CHILD_OTHER_FKEY", "OTHER_ID", OTHER_SCHEMA, false),
        ]),
    )
}

fn parent_view() -> TableMetadata {
    table(
        "PARENT_VIEW",
        TableKind::View,
        columns(vec![
            ("ID", false, key()),
            (
                "CODE",
                false,
                mapped("VARCHAR2(10 CHAR)", AnalyticsType::Text),
            ),
        ]),
        Reported::Value(Vec::new()),
    )
}

/// NOT NULL conditions that leave their columns nullable are CHECK
/// constraints; of the unique indexes, the one on a virtual column has
/// columns as its key, and the descending one does not.
fn conditions() -> TableMetadata {
    let check = |name: &str, column: &str, enforced| Constraint {
        name: name.to_owned(),
        enforced,
        kind: ConstraintKind::Check {
            expression: format!("\"{column}\" IS NOT NULL"),
        },
    };
    table(
        "CONDITIONS",
        TableKind::Table,
        columns(vec![
            ("ID", false, key()),
            ("CHECKED", true, key()),
            ("UNVALIDATED", true, key()),
            ("DISABLED", true, key()),
            ("DOUBLED", true, mapped("NUMBER(12,0)", decimal(12, 0))),
        ]),
        Reported::Value(vec![
            unique("CONDITIONS_DOUBLED_KEY", "DOUBLED", UniqueKind::Index),
            check("CONDITIONS_CHECKED_CHECK", "CHECKED", true),
            check("CONDITIONS_DISABLED_NN", "DISABLED", false),
            check("CONDITIONS_UNVALIDATED_NN", "UNVALIDATED", true),
        ]),
    )
}

fn hidden() -> TableMetadata {
    table(
        "HIDDEN",
        TableKind::Table,
        columns(vec![
            ("ID", false, key()),
            ("LABEL", true, mapped("NVARCHAR2(20)", AnalyticsType::Text)),
        ]),
        Reported::Value(vec![primary_key("HIDDEN_PKEY", &["ID"])]),
    )
}
