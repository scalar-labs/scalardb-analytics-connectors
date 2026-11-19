//! Namespaces, tables, and table metadata from `ListTables` and
//! `DescribeTable`.

use std::collections::BTreeSet;

use analytics_types::AnalyticsType;
use connector_dynamodb::{DynamoDbConnector, DynamoDbTable, NAMESPACE};
use connector_sdk::{
    ColumnType, ConstraintKind, ErrorKind, Listed, NamespacePath, Reported, TableKind, TableRecord,
    TableReference, UnsupportedReason,
    runtime::{BuiltInConnector, TableRead},
};
use serde_json::{Value, json};

use super::{MockDynamoDb, error, ok, session};

fn reference(name: &str) -> TableReference {
    TableReference::new(vec![NAMESPACE.to_owned(), name.to_owned()]).unwrap()
}

fn names(names: &[&str]) -> Option<BTreeSet<String>> {
    Some(names.iter().map(|name| (*name).to_owned()).collect())
}

/// The `DescribeTable` response of `orders`, with the volatile figures a
/// table's description carries beside its definition.
pub fn orders(item_count: u64) -> Value {
    json!({ "Table": {
        "TableName": "orders",
        "TableId": "4b2c6f0e-0b7a-4d4e-9d7c-1f2a3b4c5d6e",
        "TableArn": "arn:aws:dynamodb:us-east-1:123456789012:table/orders",
        "KeySchema": [
            { "AttributeName": "sk", "KeyType": "RANGE" },
            { "AttributeName": "pk", "KeyType": "HASH" }
        ],
        "AttributeDefinitions": [
            { "AttributeName": "pk", "AttributeType": "S" },
            { "AttributeName": "sk", "AttributeType": "B" },
            { "AttributeName": "customer", "AttributeType": "S" }
        ],
        "GlobalSecondaryIndexes": [{
            "IndexName": "by_customer",
            "KeySchema": [{ "AttributeName": "customer", "KeyType": "HASH" }],
            "Projection": { "ProjectionType": "INCLUDE", "NonKeyAttributes": ["total"] },
            "IndexStatus": "ACTIVE",
            "ItemCount": item_count,
            "IndexSizeBytes": item_count * 100
        }],
        "BillingModeSummary": { "BillingMode": "PAY_PER_REQUEST" },
        "StreamSpecification": { "StreamEnabled": true, "StreamViewType": "NEW_IMAGE" },
        "TableStatus": "ACTIVE",
        "ItemCount": item_count,
        "TableSizeBytes": item_count * 100,
        "CreationDateTime": 1.7e9,
        "ProvisionedThroughput": {
            "ReadCapacityUnits": 0,
            "WriteCapacityUnits": 0,
            "NumberOfDecreasesToday": 0
        }
    }})
}

/// The `DescribeTable` response of a table keyed by one number.
fn counters() -> Value {
    json!({ "Table": {
        "TableName": "counters",
        "TableId": "0f0e0d0c-0b0a-0908-0706-050403020100",
        "KeySchema": [{ "AttributeName": "id", "KeyType": "HASH" }],
        "AttributeDefinitions": [{ "AttributeName": "id", "AttributeType": "N" }]
    }})
}

fn read(record: &TableRecord<TableRead>) -> &TableRead {
    match record {
        TableRecord::Read { metadata, .. } => metadata,
        other => panic!("not read: {other:?}"),
    }
}

#[test]
fn the_one_namespace_is_listed_without_a_request() {
    let mock = MockDynamoDb::start();
    let session = session(&mock);
    let before = mock.requests().len();
    let namespace = Listed::Item(NamespacePath {
        names: vec![NAMESPACE.to_owned()],
    });
    assert_eq!(
        session.namespace_list(&None, None, None).unwrap(),
        vec![namespace.clone()]
    );
    assert_eq!(
        session
            .namespace_list(&names(&[NAMESPACE, "other"]), None, None)
            .unwrap(),
        vec![namespace]
    );
    assert!(
        session
            .namespace_list(&names(&["Default"]), None, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(mock.requests().len(), before);
}

#[test]
fn tables_are_listed_across_list_tables_pages() {
    let mock = MockDynamoDb::start();
    mock.on(
        "ListTables",
        Some(json!({ "ExclusiveStartTableName": "b" })),
        ok(json!({ "TableNames": ["c"] })),
    );
    let session = session(&mock);
    mock.on(
        "ListTables",
        Some(json!({})),
        ok(json!({ "TableNames": ["a", "b"], "LastEvaluatedTableName": "b" })),
    );
    let tables = |namespaces, tables| {
        session
            .table_list(&namespaces, &tables, None, None)
            .unwrap()
            .into_iter()
            .map(|listed| match listed {
                Listed::Item(entry) => {
                    assert_eq!(entry.kind, TableKind::Table);
                    entry.table.names()[1].clone()
                }
                Listed::Unreadable { .. } => panic!("unreadable"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(tables(None, None), ["a", "b", "c"]);
    assert_eq!(tables(names(&[NAMESPACE]), names(&["c", "A", "x"])), ["c"]);
    assert!(tables(names(&["other"]), None).is_empty());
}

#[test]
fn credentials_that_may_not_list_tables_make_the_namespace_unreadable() {
    let mock = MockDynamoDb::start();
    let session = session(&mock);
    mock.on(
        "ListTables",
        Some(json!({})),
        error(
            400,
            "AccessDeniedException",
            "not authorized: dynamodb:ListTables",
        ),
    );
    let listed = session.table_list(&None, &None, None, None).unwrap();
    let [Listed::Unreadable { scope, .. }] = listed.as_slice() else {
        panic!("{listed:?}");
    };
    assert_eq!(scope.names, [NAMESPACE]);
}

#[test]
fn the_columns_are_the_key_attributes_then_the_whole_item() {
    let mock = MockDynamoDb::start();
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "orders" })),
        ok(orders(42)),
    );
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "counters" })),
        ok(counters()),
    );
    let session = session(&mock);
    let result = session
        .table_metadata(&[reference("orders"), reference("counters")], None, None)
        .unwrap();

    let orders = read(&result.records[0]);
    let metadata = &orders.table_metadata;
    assert!(metadata.is_complete());
    assert_eq!(metadata.kind(), &TableKind::Table);
    assert_eq!(
        metadata.source_id(),
        &Reported::Value("4b2c6f0e-0b7a-4d4e-9d7c-1f2a3b4c5d6e".to_owned())
    );
    let columns: Vec<_> = metadata
        .columns()
        .iter()
        .map(|column| {
            (
                column.name.as_str(),
                column.ordinal,
                column.nullable,
                &column.ty,
            )
        })
        .collect();
    assert_eq!(
        columns,
        [
            (
                "pk",
                1,
                false,
                &ColumnType::Mapped {
                    data_source_type: "S".to_owned(),
                    analytics_type: AnalyticsType::Text,
                }
            ),
            (
                "sk",
                2,
                false,
                &ColumnType::Mapped {
                    data_source_type: "B".to_owned(),
                    analytics_type: AnalyticsType::Blob,
                }
            ),
            (
                "item",
                3,
                false,
                &ColumnType::Mapped {
                    data_source_type: "item".to_owned(),
                    analytics_type: AnalyticsType::Json,
                }
            ),
        ]
    );
    let Reported::Value(constraints) = metadata.constraints() else {
        panic!("constraints not read");
    };
    let [constraint] = constraints.as_slice() else {
        panic!("{constraints:?}");
    };
    assert_eq!(constraint.name, "orders");
    assert!(constraint.enforced);
    assert_eq!(
        constraint.kind,
        ConstraintKind::PrimaryKey {
            columns: vec!["pk".to_owned(), "sk".to_owned()],
        }
    );

    let counters = read(&result.records[1]);
    assert_eq!(
        counters.table_metadata.columns()[0].ty,
        ColumnType::Unsupported {
            data_source_type: "N".to_owned(),
            reason: UnsupportedReason::NotMapped,
        }
    );
}

#[test]
fn a_key_attribute_named_item_leaves_no_whole_item_column() {
    let mock = MockDynamoDb::start();
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "events" })),
        ok(json!({ "Table": {
            "TableName": "events",
            "TableId": "00000000-0000-0000-0000-000000000001",
            "KeySchema": [{ "AttributeName": "item", "KeyType": "HASH" }],
            "AttributeDefinitions": [{ "AttributeName": "item", "AttributeType": "S" }]
        }})),
    );
    let session = session(&mock);
    let result = session
        .table_metadata(&[reference("events")], None, None)
        .unwrap();
    let columns: Vec<_> = read(&result.records[0])
        .table_metadata
        .columns()
        .iter()
        .map(|column| (column.name.as_str(), &column.ty))
        .collect();
    assert_eq!(
        columns,
        [(
            "item",
            &ColumnType::Mapped {
                data_source_type: "S".to_owned(),
                analytics_type: AnalyticsType::Text,
            }
        )]
    );
}

#[test]
fn source_metadata_holds_the_definition_and_not_the_volatile_figures() {
    let mock = MockDynamoDb::start();
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "orders" })),
        ok(orders(42)),
    );
    let session = session(&mock);
    let first = session
        .table_metadata(&[reference("orders")], None, None)
        .unwrap();
    let source = &read(&first.records[0]).source_metadata;
    assert_eq!(source.connector, "dynamodb");
    let document: Value = serde_json::from_str(&source.document).unwrap();
    assert_eq!(
        document,
        json!({
            "name": "orders",
            "table_id": "4b2c6f0e-0b7a-4d4e-9d7c-1f2a3b4c5d6e",
            "arn": "arn:aws:dynamodb:us-east-1:123456789012:table/orders",
            "key_schema": [
                { "attribute_name": "sk", "key_type": "RANGE" },
                { "attribute_name": "pk", "key_type": "HASH" }
            ],
            "attribute_definitions": [
                { "attribute_name": "pk", "attribute_type": "S" },
                { "attribute_name": "sk", "attribute_type": "B" },
                { "attribute_name": "customer", "attribute_type": "S" }
            ],
            "global_secondary_indexes": [{
                "name": "by_customer",
                "key_schema": [{ "attribute_name": "customer", "key_type": "HASH" }],
                "projection": { "projection_type": "INCLUDE", "non_key_attributes": ["total"] }
            }],
            "local_secondary_indexes": [],
            "billing_mode": "PAY_PER_REQUEST",
            "table_class": null,
            "stream": { "enabled": true, "view_type": "NEW_IMAGE" }
        })
    );
    let schema = BuiltInConnector::of::<DynamoDbConnector>()
        .source_metadata_schema
        .to_value();
    assert!(
        jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&document)
    );
    serde_json::from_value::<DynamoDbTable>(document).unwrap();

    // Writes change the item count and size, not the table.
    mock.reset();
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "orders" })),
        ok(orders(43)),
    );
    let second = session
        .table_metadata(&[reference("orders")], None, None)
        .unwrap();
    assert_eq!(read(&second.records[0]), read(&first.records[0]));
}

#[test]
fn tables_that_cannot_be_described_have_the_records_iam_allows() {
    let mock = MockDynamoDb::start();
    let denied = || {
        error(
            400,
            "AccessDeniedException",
            "not authorized: dynamodb:DescribeTable",
        )
    };
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "missing" })),
        error(
            400,
            "ResourceNotFoundException",
            "Requested resource not found",
        ),
    );
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "listed" })),
        denied(),
    );
    mock.on(
        "DescribeTable",
        Some(json!({ "TableName": "unlisted" })),
        denied(),
    );
    let session = session(&mock);
    mock.on(
        "ListTables",
        Some(json!({})),
        ok(json!({ "TableNames": ["listed"] })),
    );
    let tables = [
        reference("missing"),
        reference("listed"),
        reference("unlisted"),
        // Names that cannot name a table are not sent.
        reference("a b"),
        reference("arn:aws:dynamodb:us-east-1:123456789012:table/listed"),
        TableReference::new(vec!["other".to_owned(), "listed".to_owned()]).unwrap(),
        TableReference::new(vec!["listed".to_owned()]).unwrap(),
    ];
    let result = session.table_metadata(&tables, None, None).unwrap();
    let states: Vec<_> = result
        .records
        .iter()
        .map(|record| match record {
            TableRecord::Read { .. } => "read",
            TableRecord::DoesNotExist { .. } => "does not exist",
            TableRecord::PermissionDenied { .. } => "permission denied",
            TableRecord::NotFound { .. } => "not found",
        })
        .collect();
    assert_eq!(
        states,
        [
            "does not exist",
            "permission denied",
            "not found",
            "does not exist",
            "does not exist",
            "does not exist",
            "does not exist",
        ]
    );
    let described: Vec<_> = mock
        .bodies("DescribeTable")
        .into_iter()
        .map(|body| body["TableName"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(described, ["missing", "listed", "unlisted"]);
}

#[test]
fn a_table_iam_hides_from_every_list_is_not_found() {
    let mock = MockDynamoDb::start();
    mock.on(
        "DescribeTable",
        None,
        error(
            400,
            "AccessDeniedException",
            "not authorized: dynamodb:DescribeTable",
        ),
    );
    let session = session(&mock);
    mock.on(
        "ListTables",
        Some(json!({})),
        error(
            400,
            "AccessDeniedException",
            "not authorized: dynamodb:ListTables",
        ),
    );
    let result = session
        .table_metadata(&[reference("orders")], None, None)
        .unwrap();
    assert!(matches!(result.records[0], TableRecord::NotFound { .. }));
}

#[test]
fn other_describe_failures_fail_the_request() {
    let mock = MockDynamoDb::start();
    mock.on(
        "DescribeTable",
        None,
        error(
            400,
            "UnrecognizedClientException",
            "The security token is invalid.",
        ),
    );
    let session = session(&mock);
    let error = session
        .table_metadata(&[reference("orders")], None, None)
        .err()
        .unwrap();
    assert_eq!(error.kind(), ErrorKind::Authentication);
    super::assert_no_secret(&error);
}
