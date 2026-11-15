//! The metadata the contract expects of the fixture's objects.

use std::collections::BTreeSet;

use analytics_types::{AnalyticsType, DecimalType};
use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Listed, NameFilter, NamespacePath, Reported,
    TableEntry, TableKind, TableMetadata, UniqueKind, UnsupportedReason,
    conformance::{ExpectedRecord, MetadataCase, MetadataRequest},
};

use super::{DENIED, OTHER_SCHEMA, OUTSIDER, READER, SCHEMA, options, reference, scalar};

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
                filter: names(&[SCHEMA, OTHER_SCHEMA, "CONFORMANCE", "sys"]),
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
                    "PARENT",
                ]),
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
                    reference(SCHEMA, "value_types"),
                    reference(SCHEMA, "Mixed Case"),
                    reference(SCHEMA, "mixed case"),
                    reference(SCHEMA, "parent"),
                    reference(SCHEMA, "child"),
                    reference(SCHEMA, "parent_view"),
                    reference(SCHEMA, "secret_view"),
                    reference(OTHER_SCHEMA, "parent"),
                    reference(SCHEMA, "missing"),
                    reference("parent", "parent"),
                ],
                expected: vec![
                    ExpectedRecord::Read(value_types()),
                    ExpectedRecord::Read(single_column(SCHEMA, "Mixed Case", None)),
                    ExpectedRecord::Read(single_column(SCHEMA, "mixed case", None)),
                    ExpectedRecord::Read(parent()),
                    ExpectedRecord::Read(child()),
                    ExpectedRecord::Read(parent_view()),
                    ExpectedRecord::Read(secret_view()),
                    ExpectedRecord::Read(single_column(
                        OTHER_SCHEMA,
                        "parent",
                        Some("other_parent_pkey"),
                    )),
                    ExpectedRecord::DoesNotExist,
                    ExpectedRecord::DoesNotExist,
                ],
            },
        ),
        outsider(
            "VIEW DEFINITION on a table shows its metadata without SELECT",
            MetadataRequest::TableMetadata {
                tables: vec![reference(SCHEMA, "hidden")],
                expected: vec![ExpectedRecord::Read(hidden())],
            },
        ),
        outsider(
            "without VIEW DEFINITION a table's constraints are hidden",
            MetadataRequest::TableMetadata {
                tables: vec![reference(SCHEMA, "partial")],
                expected: vec![ExpectedRecord::Read(partial())],
            },
        ),
        outsider(
            "a table the credential cannot see may exist",
            MetadataRequest::TableMetadata {
                tables: vec![reference(SCHEMA, "invisible"), reference(SCHEMA, "missing")],
                expected: vec![ExpectedRecord::NotFound, ExpectedRecord::NotFound],
            },
        ),
        outsider(
            "a column whose alias type is hidden has no Data Source type",
            MetadataRequest::TableMetadata {
                tables: vec![reference(SCHEMA, "aliased")],
                expected: vec![ExpectedRecord::Read(aliased())],
            },
        ),
        MetadataCase {
            name: "a denial of VIEW DEFINITION leaves absence unconfirmed",
            options: options(DENIED),
            request: MetadataRequest::TableMetadata {
                tables: vec![
                    reference(OTHER_SCHEMA, "parent"),
                    reference(SCHEMA, "missing"),
                ],
                expected: vec![ExpectedRecord::NotFound, ExpectedRecord::NotFound],
            },
        },
        outsider(
            "the lists hold what the credential can see",
            MetadataRequest::Tables {
                namespaces: names(&[SCHEMA]),
                tables: names(&["hidden", "partial", "invisible"]),
                expected: vec![
                    entry(SCHEMA, "hidden", TableKind::Table),
                    entry(SCHEMA, "partial", TableKind::Table),
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

/// The table's `object_id`, its source identifier.
fn object_id(namespace: &str, name: &str) -> Reported<String> {
    Reported::Value(scalar(&format!(
        "SELECT OBJECT_ID(N'[{namespace}].[{name}]')"
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

/// Columns in order, each with its position as ordinal and `column_id`.
fn columns(columns: Vec<(&str, bool, ColumnType)>) -> Vec<Column> {
    columns
        .into_iter()
        .zip(1..)
        .map(|((name, nullable, ty), ordinal)| Column {
            name: name.to_owned(),
            ordinal,
            nullable,
            source_id: Reported::Value(ordinal.to_string()),
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
        "value_types",
        TableKind::Table,
        columns(vec![
            ("id", false, mapped("int", Int)),
            ("flag", true, mapped("bit", Boolean)),
            ("tiny", true, mapped("tinyint", UnsignedByte)),
            ("small", true, mapped("smallint", SmallInt)),
            ("regular", true, mapped("int", Int)),
            ("big", true, mapped("bigint", BigInt)),
            ("single", true, mapped("real", Float)),
            ("ratio", true, mapped("float", Double)),
            ("exact", true, mapped("decimal(38,4)", decimal(38, 4))),
            ("narrow", true, mapped("numeric(9,2)", decimal(9, 2))),
            ("cash", true, mapped("money", decimal(19, 4))),
            ("small_cash", true, mapped("smallmoney", decimal(10, 4))),
            ("fixed", true, mapped("char(5)", Text)),
            ("bounded", true, mapped("varchar(10)", Text)),
            ("unicode_fixed", true, mapped("nchar(5)", Text)),
            ("wide", true, mapped("nvarchar(20)", Text)),
            ("bytes", true, mapped("binary(2)", Blob)),
            ("varbytes", true, mapped("varbinary(10)", Blob)),
            ("day", true, mapped("date", Date)),
            ("clock", true, mapped("time(7)", Time)),
            ("stamp", true, mapped("datetime2(7)", Timestamp)),
            ("stamp3", true, mapped("datetime2(3)", Timestamp)),
            ("legacy", true, mapped("datetime", Timestamp)),
            ("small_stamp", true, mapped("smalldatetime", Timestamp)),
            ("instant", true, mapped("datetimeoffset(7)", TimestampTz)),
            ("external_id", true, mapped("uniqueidentifier", Uuid)),
            ("code", true, mapped("conformance.code", Text)),
            ("label", true, mapped("sysname", Text)),
        ]),
        Reported::Value(vec![primary_key("value_types_pkey", &["id"])]),
    )
}

/// A table or view with one `int` column `id`, and its primary key, if any.
fn single_column(namespace: &str, name: &str, primary: Option<&str>) -> TableMetadata {
    TableMetadata::new(
        reference(namespace, name),
        TableKind::Table,
        object_id(namespace, name),
        columns(vec![("id", false, mapped("int", AnalyticsType::Int))]),
        false,
        Reported::Value(
            primary
                .map(|primary| primary_key(primary, &["id"]))
                .into_iter()
                .collect(),
        ),
    )
}

/// The CHECK expression of `parent`, as SQL Server stores it: 500
/// comparisons, longer than one piece of a long text read.
fn long_check() -> String {
    let mut expression = "([amount]>=(0)".to_owned();
    for n in 1..=500 {
        expression.push_str(&format!(" AND [amount]<>({n})"));
    }
    expression.push(')');
    expression
}

fn parent() -> TableMetadata {
    use AnalyticsType::*;
    use UnsupportedReason::{MappingNotImplemented, NotMapped};
    table(
        "parent",
        TableKind::Table,
        columns(vec![
            ("id", false, mapped("int", Int)),
            ("code", false, mapped("conformance.code", Text)),
            ("amount", false, mapped("decimal(10,2)", decimal(10, 2))),
            ("doubled", true, mapped("decimal(12,2)", decimal(12, 2))),
            ("tripled", true, mapped("decimal(12,2)", decimal(12, 2))),
            ("deleted_at", true, mapped("datetime2(7)", Timestamp)),
            ("slot", true, mapped("int", Int)),
            (
                "note",
                true,
                unsupported("nvarchar(max)", MappingNotImplemented),
            ),
            (
                "payload",
                true,
                unsupported("varbinary(max)", MappingNotImplemented),
            ),
            ("old_text", true, unsupported("text", MappingNotImplemented)),
            (
                "old_ntext",
                true,
                unsupported("ntext", MappingNotImplemented),
            ),
            ("picture", true, unsupported("image", MappingNotImplemented)),
            ("document", true, unsupported("xml", MappingNotImplemented)),
            ("anything", true, unsupported("sql_variant", NotMapped)),
            ("node", true, unsupported("hierarchyid", NotMapped)),
            ("place", true, unsupported("geography", NotMapped)),
            ("version", false, mapped("timestamp", Blob)),
            ("name", true, mapped("sysname", Text)),
        ]),
        Reported::Value(vec![
            primary_key("parent_pkey", &["id"]),
            Constraint {
                name: "parent_code_key".to_owned(),
                enforced: true,
                kind: ConstraintKind::Unique {
                    columns: vec!["code".to_owned()],
                    kind: UniqueKind::Constraint,
                    condition: None,
                },
            },
            Constraint {
                name: "parent_slot_key".to_owned(),
                enforced: true,
                kind: ConstraintKind::Unique {
                    columns: vec!["slot".to_owned()],
                    kind: UniqueKind::Index,
                    condition: Some("([deleted_at] IS NULL)".to_owned()),
                },
            },
            Constraint {
                name: "parent_amount_check".to_owned(),
                enforced: true,
                kind: ConstraintKind::Check {
                    expression: long_check(),
                },
            },
        ]),
    )
}

fn child() -> TableMetadata {
    let int = || mapped("int", AnalyticsType::Int);
    let foreign_key = |name: &str, column: &str, namespace: &str, enforced| Constraint {
        name: name.to_owned(),
        enforced,
        kind: ConstraintKind::ForeignKey {
            columns: vec![column.to_owned()],
            referenced_table: reference(namespace, "parent"),
            referenced_columns: vec!["id".to_owned()],
        },
    };
    table(
        "child",
        TableKind::Table,
        columns(vec![
            ("id", false, int()),
            ("parent_id", true, int()),
            ("other_id", true, int()),
        ]),
        Reported::Value(vec![
            primary_key("child_pkey", &["id"]),
            foreign_key("child_parent_fkey", "parent_id", SCHEMA, true),
            foreign_key("child_other_fkey", "other_id", OTHER_SCHEMA, false),
        ]),
    )
}

fn parent_view() -> TableMetadata {
    table(
        "parent_view",
        TableKind::View,
        columns(vec![
            ("id", false, mapped("int", AnalyticsType::Int)),
            (
                "code",
                false,
                mapped("conformance.code", AnalyticsType::Text),
            ),
        ]),
        Reported::Value(Vec::new()),
    )
}

fn secret_view() -> TableMetadata {
    table(
        "secret_view",
        TableKind::View,
        columns(vec![("id", false, mapped("int", AnalyticsType::Int))]),
        Reported::Value(Vec::new()),
    )
}

fn hidden() -> TableMetadata {
    table(
        "hidden",
        TableKind::Table,
        columns(vec![
            ("id", false, mapped("int", AnalyticsType::Int)),
            ("label", true, mapped("nvarchar(20)", AnalyticsType::Text)),
        ]),
        Reported::Value(vec![primary_key("hidden_pkey", &["id"])]),
    )
}

fn partial() -> TableMetadata {
    table(
        "partial",
        TableKind::Table,
        columns(vec![
            ("id", false, mapped("int", AnalyticsType::Int)),
            ("secret", true, mapped("int", AnalyticsType::Int)),
        ]),
        Reported::Missing {
            reason: "SQL Server hides constraints without VIEW DEFINITION on the table".to_owned(),
        },
    )
}

fn aliased() -> TableMetadata {
    let mut columns = columns(vec![("id", false, mapped("int", AnalyticsType::Int))]);
    columns.push(Column {
        name: "code".to_owned(),
        ordinal: 2,
        nullable: true,
        source_id: Reported::Value("2".to_owned()),
        ty: ColumnType::Unread {
            reason: "SQL Server hides an alias type the credential has no permission on".to_owned(),
        },
    });
    table(
        "aliased",
        TableKind::Table,
        columns,
        Reported::Missing {
            reason: "SQL Server hides constraints without VIEW DEFINITION on the table".to_owned(),
        },
    )
}
