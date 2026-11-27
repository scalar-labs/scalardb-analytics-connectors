//! The metadata the contract expects of the fixture's objects.
//!
//! The `full_data_type` spellings below are those Databricks documents; the
//! suite checks them, including that a collated string names its collation.

use std::collections::BTreeSet;

use analytics_types::{AnalyticsField, AnalyticsType, ArrayType, DecimalType, MapType, StructType};
use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Listed, NameFilter, NamespacePath, Reported,
    TableEntry, TableKind, TableMetadata, UnsupportedReason,
    conformance::{ExpectedRecord, MetadataCase, MetadataRequest},
};

use super::DatabricksFixture;

pub fn cases(fixture: &DatabricksFixture) -> Vec<MetadataCase> {
    let (schema, other) = (fixture.schema.as_str(), fixture.other_schema.as_str());
    let catalog = fixture.workspace.catalog.as_str();
    let case = |name, request| MetadataCase {
        name,
        options: fixture.credential_options(),
        request,
    };
    let namespace = |schema: &str| {
        Listed::Item(NamespacePath {
            names: vec![catalog.to_owned(), schema.to_owned()],
        })
    };
    let entry = |schema: &str, name: &str, kind| {
        Listed::Item(TableEntry {
            table: fixture.reference(schema, name),
            kind,
        })
    };
    let upper_schema = schema.to_uppercase();
    vec![
        case(
            "schemas are listed in the catalog, without information_schema",
            MetadataRequest::Namespaces {
                filter: names(&[schema, other, &upper_schema, "information_schema"]),
                expected: vec![namespace(schema), namespace(other)],
            },
        ),
        case(
            "a schema's tables are listed with their kinds",
            MetadataRequest::Tables {
                namespaces: names(&[schema]),
                tables: names(&["parent", "parent_view", "value_types", "PARENT"]),
                expected: vec![
                    entry(schema, "parent", TableKind::Table),
                    entry(schema, "parent_view", TableKind::View),
                    entry(schema, "value_types", TableKind::Table),
                ],
            },
        ),
        case(
            "table names are matched in every listed schema",
            MetadataRequest::Tables {
                namespaces: names(&[schema, other]),
                tables: names(&["parent"]),
                expected: vec![
                    entry(schema, "parent", TableKind::Table),
                    entry(other, "parent", TableKind::Table),
                ],
            },
        ),
        case(
            "table metadata follows the type mapping",
            MetadataRequest::TableMetadata {
                tables: vec![
                    fixture.reference(schema, "value_types"),
                    fixture.reference(schema, "parent"),
                    fixture.reference(schema, "child"),
                    fixture.reference(schema, "parent_view"),
                    fixture.reference(other, "parent"),
                    fixture.reference(schema, "missing"),
                    fixture.reference(&upper_schema, "parent"),
                    fixture.reference("information_schema", "tables"),
                ],
                expected: vec![
                    ExpectedRecord::Read(value_types(fixture)),
                    ExpectedRecord::Read(parent(fixture)),
                    ExpectedRecord::Read(child(fixture)),
                    ExpectedRecord::Read(parent_view(fixture)),
                    ExpectedRecord::Read(table(
                        fixture,
                        other,
                        "parent",
                        TableKind::Table,
                        columns(vec![("id", true, mapped("int", AnalyticsType::Int))]),
                        Vec::new(),
                    )),
                    // Unity Catalog shows only what the principal may see,
                    // and stores schema names in lower case; the connector
                    // does not read information_schema itself.
                    ExpectedRecord::NotFound,
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

fn mapped(data_source_type: &str, analytics_type: AnalyticsType) -> ColumnType {
    ColumnType::Mapped {
        data_source_type: data_source_type.to_owned(),
        analytics_type,
    }
}

fn decimal(precision: u8, scale: i8) -> AnalyticsType {
    AnalyticsType::Decimal(DecimalType::new(precision, scale).unwrap())
}

/// Columns numbered from 1; Unity Catalog offers no column identifiers
/// through SQL.
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

/// Table metadata as the connector derives it: no identifiers, no hidden
/// columns, and declared constraints.
fn table(
    fixture: &DatabricksFixture,
    schema: &str,
    name: &str,
    kind: TableKind,
    columns: Vec<Column>,
    constraints: Vec<Constraint>,
) -> TableMetadata {
    TableMetadata::new(
        fixture.reference(schema, name),
        kind,
        Reported::NotOffered,
        columns,
        false,
        Reported::Value(constraints),
    )
}

fn value_types(fixture: &DatabricksFixture) -> TableMetadata {
    use AnalyticsType::*;
    let point = Struct(StructType::new(vec![
        AnalyticsField::new("x", Int, true),
        AnalyticsField::new("y", Text, true),
    ]));
    table(
        fixture,
        &fixture.schema,
        "value_types",
        TableKind::Table,
        columns(vec![
            ("id", false, mapped("int", Int)),
            ("tiny", true, mapped("tinyint", Byte)),
            ("small", true, mapped("smallint", SmallInt)),
            ("regular", true, mapped("int", Int)),
            ("big", true, mapped("bigint", BigInt)),
            ("single", true, mapped("float", Float)),
            ("ratio", true, mapped("double", Double)),
            ("narrow", true, mapped("decimal(5,2)", decimal(5, 2))),
            ("medium", true, mapped("decimal(18,4)", decimal(18, 4))),
            ("widest", true, mapped("decimal(38,4)", decimal(38, 4))),
            ("label", true, mapped("string", Text)),
            ("bounded", true, mapped("varchar(10)", Text)),
            ("fixed", true, mapped("char(5)", Text)),
            ("bytes", true, mapped("binary", Blob)),
            ("day", true, mapped("date", Date)),
            ("stamp", true, mapped("timestamp_ntz", Timestamp)),
            ("instant", true, mapped("timestamp", TimestampTz)),
            ("flag", true, mapped("boolean", Boolean)),
            (
                "tags",
                true,
                mapped("array<int>", Array(ArrayType::new(Int, true))),
            ),
            (
                "attributes",
                true,
                mapped("map<string,int>", Map(MapType::new(Text, Int, true))),
            ),
            ("point", true, mapped("struct<x:int,y:string>", point)),
            ("folded", true, mapped("string collate UTF8_LCASE", Text)),
            (
                "document",
                true,
                ColumnType::Unsupported {
                    data_source_type: "variant".to_owned(),
                    reason: UnsupportedReason::NotMapped,
                },
            ),
        ]),
        Vec::new(),
    )
}

fn primary_key(name: &str) -> Constraint {
    Constraint {
        name: name.to_owned(),
        enforced: false,
        kind: ConstraintKind::PrimaryKey {
            columns: vec!["id".to_owned()],
        },
    }
}

fn parent(fixture: &DatabricksFixture) -> TableMetadata {
    table(
        fixture,
        &fixture.schema,
        "parent",
        TableKind::Table,
        columns(vec![
            ("id", false, mapped("bigint", AnalyticsType::BigInt)),
            ("label", true, mapped("string", AnalyticsType::Text)),
        ]),
        vec![primary_key("parent_pk")],
    )
}

fn child(fixture: &DatabricksFixture) -> TableMetadata {
    table(
        fixture,
        &fixture.schema,
        "child",
        TableKind::Table,
        columns(vec![
            ("id", false, mapped("bigint", AnalyticsType::BigInt)),
            ("parent_id", true, mapped("bigint", AnalyticsType::BigInt)),
        ]),
        vec![
            primary_key("child_pk"),
            Constraint {
                name: "child_parent_fk".to_owned(),
                enforced: false,
                kind: ConstraintKind::ForeignKey {
                    columns: vec!["parent_id".to_owned()],
                    referenced_table: fixture.reference(&fixture.schema, "parent"),
                    referenced_columns: vec!["id".to_owned()],
                },
            },
        ],
    )
}

fn parent_view(fixture: &DatabricksFixture) -> TableMetadata {
    table(
        fixture,
        &fixture.schema,
        "parent_view",
        TableKind::View,
        columns(vec![("id", false, mapped("bigint", AnalyticsType::BigInt))]),
        Vec::new(),
    )
}
