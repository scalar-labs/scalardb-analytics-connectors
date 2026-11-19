//! The metadata the contract expects of the fixture's tables.

use std::collections::BTreeSet;

use analytics_types::AnalyticsType;
use connector_dynamodb::NAMESPACE;
use connector_sdk::{
    Column, ColumnType, Constraint, ConstraintKind, Listed, NameFilter, NamespacePath, Reported,
    TableEntry, TableKind, TableMetadata, TableReference, UnsupportedReason,
    conformance::{ExpectedRecord, MetadataCase, MetadataRequest},
};

use super::{DynamoDbFixture, Policy, reference};

pub fn cases(fixture: &DynamoDbFixture) -> Vec<MetadataCase> {
    let case = |name, policy, request| MetadataCase {
        name,
        options: fixture.options(policy),
        request,
    };
    let other = |name: &str| TableReference::new(vec!["other".to_owned(), name.to_owned()]);
    vec![
        case(
            "the one namespace is matched exactly",
            Policy::Reader,
            MetadataRequest::Namespaces {
                filter: names(&[NAMESPACE, "Default", "other"]),
                expected: vec![Listed::Item(namespace())],
            },
        ),
        case(
            "table names are matched exactly and listed in ListTables order",
            Policy::Reader,
            MetadataRequest::Tables {
                namespaces: names(&[NAMESPACE]),
                // The fixture creates `cased` before `locked`, so the order
                // is the same whether DynamoDB Local lists tables by name, as
                // DynamoDB does, or in the order they were created.
                tables: names(&["cased", "CASED", "locked", "missing"]),
                expected: vec![entry("cased"), entry("locked")],
            },
        ),
        case(
            "no table is in another namespace",
            Policy::Reader,
            MetadataRequest::Tables {
                namespaces: names(&["other"]),
                tables: NameFilter::None,
                expected: Vec::new(),
            },
        ),
        case(
            "table metadata follows the type mapping",
            Policy::Reader,
            MetadataRequest::TableMetadata {
                tables: vec![
                    reference("texts"),
                    reference("blobs"),
                    reference("numbers"),
                    reference("cased"),
                    reference("missing"),
                    other("texts").unwrap(),
                    // Not a table name: shorter than three characters.
                    reference("ab"),
                ],
                expected: vec![
                    ExpectedRecord::Read(metadata(
                        fixture,
                        "texts",
                        &[("pk", text()), ("sk", text())],
                    )),
                    ExpectedRecord::Read(metadata(
                        fixture,
                        "blobs",
                        &[("pk", text()), ("sk", blob())],
                    )),
                    ExpectedRecord::Read(metadata(
                        fixture,
                        "numbers",
                        &[(
                            "id",
                            ColumnType::Unsupported {
                                data_source_type: "N".to_owned(),
                                reason: UnsupportedReason::NotMapped,
                            },
                        )],
                    )),
                    ExpectedRecord::Read(metadata(fixture, "cased", &[("pk", text())])),
                    ExpectedRecord::DoesNotExist,
                    ExpectedRecord::DoesNotExist,
                    ExpectedRecord::DoesNotExist,
                ],
            },
        ),
        case(
            "tables the credentials may not describe",
            Policy::Outsider,
            MetadataRequest::TableMetadata {
                tables: vec![reference("locked"), reference("unlisted")],
                expected: vec![ExpectedRecord::PermissionDenied, ExpectedRecord::NotFound],
            },
        ),
        case(
            "credentials that may not list tables",
            Policy::Unlister,
            MetadataRequest::Tables {
                namespaces: NameFilter::None,
                tables: NameFilter::None,
                expected: vec![Listed::Unreadable {
                    scope: namespace(),
                    reason: "the credentials may not list DynamoDB tables".to_owned(),
                }],
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

fn namespace() -> NamespacePath {
    NamespacePath {
        names: vec![NAMESPACE.to_owned()],
    }
}

fn entry(name: &str) -> Listed<TableEntry> {
    Listed::Item(TableEntry {
        table: reference(name),
        kind: TableKind::Table,
    })
}

fn text() -> ColumnType {
    ColumnType::Mapped {
        data_source_type: "S".to_owned(),
        analytics_type: AnalyticsType::Text,
    }
}

fn blob() -> ColumnType {
    ColumnType::Mapped {
        data_source_type: "B".to_owned(),
        analytics_type: AnalyticsType::Blob,
    }
}

/// The table metadata of `name` keyed by `keys`: the key columns, then
/// `item`, with the primary key named after the table and the `TableId`
/// DynamoDB Local reports as the source identifier.
fn metadata(fixture: &DynamoDbFixture, name: &str, keys: &[(&str, ColumnType)]) -> TableMetadata {
    let item = (
        "item",
        ColumnType::Mapped {
            data_source_type: "item".to_owned(),
            analytics_type: AnalyticsType::Json,
        },
    );
    let columns = keys
        .iter()
        .chain([&item])
        .zip(1..)
        .map(|((column, ty), ordinal)| Column {
            name: (*column).to_owned(),
            ordinal,
            nullable: false,
            source_id: Reported::NotOffered,
            ty: ty.clone(),
        })
        .collect();
    let source_id = match fixture.admin(async |admin| admin.table_id(name).await) {
        Some(id) => Reported::Value(id),
        None => Reported::Missing {
            reason: "DescribeTable returned no TableId".to_owned(),
        },
    };
    TableMetadata::new(
        reference(name),
        TableKind::Table,
        source_id,
        columns,
        false,
        Reported::Value(vec![Constraint {
            name: name.to_owned(),
            enforced: true,
            kind: ConstraintKind::PrimaryKey {
                columns: keys
                    .iter()
                    .map(|(column, _)| (*column).to_owned())
                    .collect(),
            },
        }]),
    )
}
